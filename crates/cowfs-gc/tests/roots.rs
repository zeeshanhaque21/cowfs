//! What the collector does when the reference side cannot answer, or answers about itself.
//!
//! The contract is that an answer is exact or it is an error. There is no partial answer and no
//! error that means "probably nothing is pinned". A core that returns an empty vector while a
//! writer holds a node lock is telling the collector to free whatever that writer has in flight,
//! so this file proves the collector frees nothing in that situation and everything when the
//! answer is right.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Fixture, Roots};
use cowfs_gc::{Barrier, ExtraRoots, RootsError, SkipReason};
use cowfs_store::BlockId;

fn body(n: usize, seed: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut h = seed.wrapping_mul(2654435761).wrapping_add(1);
    for _ in 0..n {
        h = h.wrapping_mul(1664525).wrapping_add(1013904223);
        out.push(if h >> 29 == 0 {
            b'a'.wrapping_add((h >> 8) as u8)
        } else {
            (h >> 16) as u8
        });
    }
    out
}

/// A store with enough garbage for a sweep to be worth running, and one live file.
fn garbage() -> Fixture {
    let f = Fixture::eager(32 << 10);
    let snap = f.meta.new_snapshot("s").expect("snapshot");
    f.write(&snap, b"keep", &body(60_000, 1));
    for i in 0..40u32 {
        f.store.put(&body(4000, i)).expect("put");
    }
    f.store.sync().expect("sync");
    f.meta.sync().expect("meta sync");
    f
}

struct Noop;
impl Barrier for Noop {}

/// A reference side that answers until a poll counter reaches a set point, then fails forever.
///
/// The counter lets a test put the failure at a chosen point in the cycle: before the mark, during
/// it, between the mark and the sweep, or on the poll just before the unlinks.
struct Failing {
    /// Fail from this poll onwards.
    from: usize,
    polls: AtomicUsize,
    barrier: bool,
    barrier_fails_from: usize,
    barrier_polls: AtomicUsize,
    pinned: Vec<BlockId>,
}

impl Failing {
    fn new(from: usize) -> Self {
        Self {
            from,
            polls: AtomicUsize::new(0),
            barrier: true,
            barrier_fails_from: usize::MAX,
            barrier_polls: AtomicUsize::new(0),
            pinned: Vec::new(),
        }
    }

    fn failing_barrier(from: usize) -> Self {
        Self {
            from: usize::MAX,
            polls: AtomicUsize::new(0),
            barrier: true,
            barrier_fails_from: from,
            barrier_polls: AtomicUsize::new(0),
            pinned: Vec::new(),
        }
    }

    fn barrier_polls(&self) -> usize {
        self.barrier_polls.load(Relaxed)
    }
}

impl ExtraRoots for Failing {
    fn pinned_blocks(&self) -> Result<Vec<BlockId>, RootsError> {
        let n = self.polls.fetch_add(1, Relaxed);
        if n >= self.from {
            return Err(RootsError::Busy);
        }
        Ok(self.pinned.clone())
    }

    fn reference_barrier(&self) -> Result<Option<Box<dyn Barrier>>, RootsError> {
        let n = self.barrier_polls.fetch_add(1, Relaxed);
        if n >= self.barrier_fails_from {
            return Err(RootsError::Unavailable);
        }
        Ok(self.barrier.then(|| Box::new(Noop) as Box<dyn Barrier>))
    }
}

/// Nothing is freed and nothing is copied when the reference side will not answer, whatever poll
/// it stops answering at.
///
/// A garbage store that a healthy collector reclaims is the control: these assertions are about the
/// cycle doing nothing, so the store has to be one where doing something is easy.
#[test]
fn an_error_from_the_roots_aborts_the_sweep_at_every_position() {
    // Poll 0 is the freeze, 1 is the re-poll after the mark, 2 is the last one before the copy.
    // Poll 3 onwards is the per-pack poll under the barrier, which by then is after the copy, so
    // those positions are covered by their own test rather than asserted to copy nothing.
    for from in [0usize, 1, 2] {
        let f = garbage();
        let roots = Failing::new(from);
        let before = f.store_files();
        let packs_before = f.store.stats().packs;

        let r = f.gc.collect(Some(&roots)).expect("collect");

        assert_eq!(
            r.roots_error,
            Some(RootsError::Busy),
            "the report names the failure, poll {from}"
        );
        assert_eq!(r.freed_bytes, 0, "nothing freed, poll {from}");
        assert_eq!(r.packs_unlinked, 0, "nothing unlinked, poll {from}");
        assert_eq!(r.packs_rewritten, 0, "nothing copied, poll {from}");
        assert_eq!(
            f.store_files(),
            before,
            "the store is byte-identical, poll {from}"
        );
        assert_eq!(f.store.stats().packs, packs_before, "poll {from}");
        assert!(
            r.errors.iter().any(|e| e.contains("would not answer")),
            "the report explains itself, poll {from}: {:?}",
            r.errors
        );
        for b in f.live_blocks() {
            assert!(f.store.get(b).is_ok(), "a live block lost, poll {from}");
        }
    }
}

/// A reference side that fails only at the poll under the barrier, after the copy has run, still
/// stops every unlink. The copies stay, because they are written and indexed and a later cycle
/// reuses them, but not one condemned block is freed on an answer that was never given.
#[test]
fn a_failure_at_the_poll_under_the_barrier_stops_every_unlink() {
    let f = garbage();
    // Poll 0 freeze, 1 after the mark, 2 before the copy, 3 is the first poll under the barrier.
    let roots = Failing::new(3);
    let r = f.gc.collect(Some(&roots)).expect("collect");

    assert_eq!(r.roots_error, Some(RootsError::Busy), "{r:?}");
    assert_eq!(r.freed_bytes, 0, "nothing freed");
    assert_eq!(r.packs_unlinked, 0, "nothing unlinked");
    assert!(
        r.skipped
            .iter()
            .any(|s| s.reason == SkipReason::RootsUnavailable),
        "the packs are reported as skipped for the right reason: {:?}",
        r.skipped
    );
    assert!(
        r.errors.iter().any(|e| e.contains("would not answer")),
        "and the report explains itself"
    );
    for b in f.live_blocks() {
        assert!(
            f.store.get(b).is_ok(),
            "a live block was freed on a failed answer"
        );
    }
}

/// A barrier the caller will not promise is no barrier, and the cycle frees nothing.
#[test]
fn a_failing_barrier_stops_the_cycle_before_it_marks_anything() {
    let f = garbage();
    let roots = Failing::failing_barrier(0);
    let before = f.store_files();

    let r = f.gc.collect(Some(&roots)).expect("collect");

    assert_eq!(r.roots_error, Some(RootsError::Unavailable));
    assert!(!r.barrier, "a failed barrier is not a held barrier");
    assert_eq!(r.freed_bytes, 0);
    assert_eq!(r.packs_unlinked, 0);
    assert_eq!(r.packs_rewritten, 0);
    assert_eq!(f.store_files(), before);
    assert_eq!(roots.barrier_polls(), 1);
}

/// The mark still runs, so the report explains what was there and what was not freed.
#[test]
fn a_cycle_that_aborted_still_reports_what_it_marked() {
    let f = garbage();
    let roots = Failing::new(0);
    let r = f.gc.collect(Some(&roots)).expect("collect");

    assert_eq!(r.roots_error, Some(RootsError::Busy));
    assert!(r.marked > 0, "the mark ran before the failure: {r:?}");
    assert!(r.store_blocks > 0, "and the store was counted");
    assert!(
        r.candidates > 0,
        "and the candidates were found, so refusing to copy is a decision"
    );
    // Packs can also be skipped for their own reasons, so check the ones that reached the abort.
    assert!(
        r.candidates > 0
            && r.skipped
                .iter()
                .filter(|s| s.reason == SkipReason::RootsUnavailable)
                .count() as u64
                == r.candidates,
        "every candidate is skipped because the roots would not answer: {:?}",
        r.skipped
    );
}

/// The contract forbids omitting a pinned block. This is the shape of the bug the contract exists
/// to prevent, so it is worth spelling out: an answer that is empty while a block is pinned is
/// indistinguishable from "nothing is pinned", and the collector is entitled to free it.
///
/// The mitigation the collector does have is the re-poll after the mark, so an implementation that
/// reports a block late still protects it.
#[test]
fn a_repoll_after_the_mark_protects_a_block_that_appears_pinned() {
    let f = garbage();
    // A block that only exists in the reference side: nothing in the metadata names it.
    let late = f.store.ingest_bytes(&body(5000, 77)).expect("ingest")[0].id;
    f.store.sync().unwrap();

    struct Late {
        polls: AtomicUsize,
        late: BlockId,
    }
    impl ExtraRoots for Late {
        fn pinned_blocks(&self) -> Result<Vec<BlockId>, RootsError> {
            // Poll 0 (the freeze) omits it, which the contract forbids. Poll 1 reports it.
            let n = self.polls.fetch_add(1, Relaxed);
            Ok(if n == 0 { Vec::new() } else { vec![self.late] })
        }
        fn reference_barrier(&self) -> Result<Option<Box<dyn Barrier>>, RootsError> {
            Ok(Some(Box::new(Noop) as Box<dyn Barrier>))
        }
    }
    let roots = Late {
        polls: AtomicUsize::new(0),
        late,
    };

    let r = f.gc.collect(Some(&roots)).expect("collect");

    assert_eq!(r.roots_error, None, "an omission is not an error");
    assert!(r.pinned >= 1, "the re-poll was unioned in: {r:?}");
    assert!(
        f.store.get(late).is_ok(),
        "the block that appeared pinned after the mark survived"
    );
    assert!(
        roots.polls.load(Relaxed) >= 2,
        "the collector polls more than once, so a late report is still seen"
    );
}

/// Two polls that disagree protect the union, so a block reported on one poll and dropped on the
/// next is still live.
#[test]
fn two_polls_that_differ_protect_the_union_of_both() {
    let f = garbage();
    let a = f.store.ingest_bytes(&body(5000, 11)).expect("ingest")[0].id;
    let b = f.store.ingest_bytes(&body(5000, 12)).expect("ingest")[0].id;
    f.store.sync().unwrap();

    struct Disagree {
        polls: AtomicUsize,
        a: BlockId,
        b: BlockId,
    }
    impl ExtraRoots for Disagree {
        fn pinned_blocks(&self) -> Result<Vec<BlockId>, RootsError> {
            let n = self.polls.fetch_add(1, Relaxed);
            Ok(match n {
                0 => vec![self.a],
                _ => vec![self.b],
            })
        }
        fn reference_barrier(&self) -> Result<Option<Box<dyn Barrier>>, RootsError> {
            Ok(Some(Box::new(Noop) as Box<dyn Barrier>))
        }
    }
    let roots = Disagree {
        polls: AtomicUsize::new(0),
        a,
        b,
    };

    f.gc.collect(Some(&roots)).expect("collect");

    assert!(
        f.store.get(a).is_ok(),
        "the block from the first poll survived"
    );
    assert!(
        f.store.get(b).is_ok(),
        "and so did the one from the second, because the answer is unioned"
    );
}

/// The same set polled twice is idempotent: nothing is double counted and nothing is freed.
#[test]
fn the_same_answer_twice_is_stable() {
    let f = garbage();
    let roots = Roots::new();
    let pinned = f.store.ingest_bytes(&body(5000, 55)).expect("ingest")[0].id;
    f.store.sync().unwrap();
    roots.pin(pinned);

    let r = f.gc.collect(Some(&*roots)).expect("collect");

    assert_eq!(r.pinned, 1, "one block, however many polls: {r:?}");
    assert!(f.store.get(pinned).is_ok(), "and it survived");
}

/// A reference side that is busy while a writer is mid-commit must not cost the writer anything,
/// and must not cost the collector a block.
#[test]
fn a_writer_racing_a_busy_reference_side_loses_nothing() {
    let f = garbage();
    let parts = f.parts();
    let parts = &parts;
    let roots = Roots::new();

    /// Busy on every other poll, so a cycle catches it mid-flight and one after another does not.
    struct Alternating {
        n: AtomicUsize,
        inner: Arc<Roots>,
    }
    impl ExtraRoots for Alternating {
        fn pinned_blocks(&self) -> Result<Vec<BlockId>, RootsError> {
            let n = self.n.fetch_add(1, Relaxed);
            if n % 2 == 1 {
                return Err(RootsError::Busy);
            }
            self.inner.pinned_blocks()
        }
        fn reference_barrier(&self) -> Result<Option<Box<dyn Barrier>>, RootsError> {
            self.inner.reference_barrier()
        }
    }
    let alt = Alternating {
        n: AtomicUsize::new(0),
        inner: Arc::clone(&roots),
    };

    let base = f.meta.new_snapshot("base").unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let written = Arc::new(Mutex::new(Vec::new()));

    std::thread::scope(|sc| {
        let written = Arc::clone(&written);
        let finish = Arc::clone(&stop);
        sc.spawn(move || {
            for i in 0..30u32 {
                let name = format!("w{i:02}");
                let data = body(9000, 400 + i);
                roots.write(|| parts.write(&base, name.as_bytes(), &data));
                written.lock().unwrap().push(name);
                parts.meta.sync().unwrap();
            }
            finish.store(true, Relaxed);
        });

        while !stop.load(Relaxed) {
            let r = parts.gc.collect(Some(&alt)).expect("collect");
            assert_eq!(r.freed_bytes, 0, "a busy reference side frees nothing");
            assert_eq!(r.packs_unlinked, 0);
            for b in parts.live() {
                assert!(
                    parts.store.get(b).is_ok(),
                    "a block a live snapshot names was lost while the roots were busy"
                );
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });

    let names = written.lock().unwrap().clone();
    assert!(!names.is_empty(), "the writer did something");
    parts.meta.sync().unwrap();
    for b in parts.live() {
        assert!(
            parts.store.get(b).is_ok(),
            "every block a live snapshot names survived the whole race"
        );
    }
}

/// A collector with no roots at all frees nothing, because nothing promises to protect it.
#[test]
fn no_roots_at_all_frees_nothing() {
    let f = garbage();
    let before = f.store_files();
    let r = f.gc.collect(None).expect("collect");

    assert_eq!(r.freed_bytes, 0);
    assert_eq!(r.packs_unlinked, 0);
    assert_eq!(f.store_files(), before);
    assert!(r.pinned == 0);
}

/// A healthy reference side still reclaims, so the aborting tests above are not passing because the
/// fixture has nothing to reclaim.
#[test]
fn a_healthy_roots_still_reclaims() {
    let f = garbage();
    let roots = Roots::new();
    let mut total = 0;
    for _ in 0..8 {
        let r = f.gc.collect(Some(&*roots)).expect("collect");
        assert_eq!(r.roots_error, None);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        total += r.freed_bytes;
        if r.freed_bytes == 0 {
            break;
        }
    }
    assert!(total > 0, "a healthy reference side does reclaim");
}
