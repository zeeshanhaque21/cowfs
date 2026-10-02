//! Regressions for the critic's findings on PR #39.
//!
//! Each test here failed on the reviewed head and pins one defect. The names say which, because a
//! test that does not say what it holds is a test nobody can trust when it breaks.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;

use common::{eager, Fixture, Roots};
use cowfs_gc::{Barrier, ExtraRoots, Held, RootsError};
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

struct Noop;
impl Barrier for Noop {
    fn take(&mut self) -> Option<Box<dyn Held>> {
        Some(Box::new(HeldNoop))
    }
}
struct HeldNoop;
impl Held for HeldNoop {}

/// F1: the persisted mark set must not pin a block whose snapshot has gone.
///
/// The order that shows it is collect, then remove, then collect. The reverse order passes on the
/// broken build because `mark.bin` is empty when the doomed root would be recorded.
#[test]
fn a_removed_snapshots_blocks_are_reclaimed_on_the_next_cycle() {
    let f = Fixture::eager(32 << 10);
    let roots = Roots::new();

    let doomed = f.meta.new_snapshot("doomed").expect("snapshot");
    for i in 0..12u32 {
        f.write(&doomed, format!("d{i:02}").as_bytes(), &body(6000, 300 + i));
    }
    let keep = f.meta.new_snapshot("keep").expect("snapshot");
    f.write(&keep, b"keep", &body(6000, 1));
    f.meta.sync().unwrap();
    f.store.sync().unwrap();

    // Cycle 1 walks both roots and records them.
    let r1 = f.gc.collect(Some(&*roots)).expect("collect");
    assert!(r1.marked > 0, "the first cycle marks: {r1:?}");

    let before = f.store.stats().pack_bytes;
    assert!(before > 0);

    // The doomed snapshot goes away. Its blocks are unreachable, so a later cycle must free them.
    f.meta.remove_snapshot(doomed.id()).unwrap();
    f.meta.reap_all().unwrap();
    f.meta.sync().unwrap();

    let mut freed = 0;
    let mut report = cowfs_gc::GcReport::default();
    for _ in 0..6 {
        let r = f.gc.collect(Some(&*roots)).expect("collect");
        freed += r.freed_bytes;
        let done = r.freed_bytes == 0;
        report = r;
        if done {
            break;
        }
    }
    assert!(
        freed > 0,
        "a removed snapshot's blocks were never reclaimed: {report:?}"
    );
    assert!(
        f.store.stats().pack_bytes < before,
        "the store shrank: {} -> {}",
        before,
        f.store.stats().pack_bytes
    );

    // And the snapshot that stayed is untouched.
    for b in f.live_blocks() {
        assert!(
            f.store.get(b).is_ok(),
            "a still-live snapshot's block was freed: {b:?}"
        );
    }
    assert!(f.store.fsck().expect("fsck").is_clean());
    assert!(!f.store.recovery().has_corruption());
}

/// F1: the persisted root set must shrink too, or the walk is skipped forever.
#[test]
fn a_removed_snapshots_root_is_not_skipped_forever() {
    let f = Fixture::eager(32 << 10);
    let roots = Roots::new();
    let doomed = f.meta.new_snapshot("doomed").expect("snapshot");
    f.write(&doomed, b"x", &body(6000, 5));
    f.meta.sync().unwrap();

    f.gc.collect(Some(&*roots)).expect("collect");
    assert!(
        f.gc_dir().join("mark.bin").exists(),
        "the first cycle recorded the root"
    );

    f.meta.remove_snapshot(doomed.id()).unwrap();
    f.meta.reap_all().unwrap();
    f.meta.sync().unwrap();
    f.gc.collect(Some(&*roots)).expect("collect");

    // With the root still recorded, the next cycle would skip it and report nothing marked.
    let again = f.meta.new_snapshot("again").expect("snapshot");
    f.write(&again, b"y", &body(6000, 6));
    f.meta.sync().unwrap();
    let r = f.gc.collect(Some(&*roots)).expect("collect");
    assert!(
        r.marked > 0,
        "a new snapshot must be walked, not skipped as already seen: {r:?}"
    );
}

/// F2: the barrier must not be held while the copies run.
///
/// It is acquired in `take`, so this counts the progress callbacks that happen with it held.
#[test]
fn the_barrier_is_not_held_while_packs_are_copied() {
    let f = Fixture::eager(32 << 10);
    let roots = Roots::new();
    let snap = f.meta.new_snapshot("s").expect("snapshot");
    f.write(&snap, b"keep", &body(60_000, 1));
    for i in 0..40u32 {
        f.store.put(&body(4000, i)).expect("put");
    }
    f.store.sync().unwrap();
    f.meta.sync().unwrap();

    let during_copy = Arc::new(AtomicUsize::new(0));
    let callbacks = Arc::new(AtomicUsize::new(0));
    {
        let r = Arc::clone(&roots);
        let during_copy = Arc::clone(&during_copy);
        let callbacks = Arc::clone(&callbacks);
        f.gc.set_progress(move |p| {
            // A copy-phase callback is one that has not freed anything yet. The callbacks after
            // the first unlink are legitimately inside the barrier.
            if p.sweeping && p.freed_bytes == 0 {
                callbacks.fetch_add(1, Relaxed);
                if r.barrier_live() {
                    during_copy.fetch_add(1, Relaxed);
                }
            }
        });
        let report = f.gc.collect(Some(&*roots)).expect("collect");
        assert!(report.packs_rewritten > 0, "packs were copied: {report:?}");
    }
    let callbacks = callbacks.load(Relaxed);
    let during_copy = during_copy.load(Relaxed);
    assert!(callbacks > 0, "progress was reported during the sweep");
    assert_eq!(
        during_copy, 0,
        "the barrier was held for {during_copy} of {callbacks} progress callbacks, so a copy \
         is a write stall"
    );
}

/// F3: a pack whose file is already gone must still leave the writer's map, or `fsck` is broken
/// for good.
///
/// The race is the second collector's: the first one unlinked the file between this caller's
/// metadata read and its own, so this caller takes the early return.
#[test]
fn a_second_discard_of_the_same_pack_leaves_fsck_clean() {
    let f = Fixture::eager(32 << 10);
    // Garbage alone in the first packs, the live blocks after, so a pack that holds nothing live
    // exists to discard. Same shape the collector chooses.
    for i in 20..24u32 {
        f.store.put(&body(4096, i)).unwrap();
    }
    for i in 40..70u32 {
        f.store.put(&body(4096, i)).unwrap();
    }
    let live: Vec<BlockId> = (100..106u32)
        .map(|i| f.store.put(&body(4096, i)).unwrap())
        .collect();
    f.store.sync().unwrap();

    let keep: std::collections::HashSet<BlockId> = live.iter().copied().collect();
    let mut chosen = None;
    for info in f.store.packs().unwrap() {
        if info.active {
            continue;
        }
        let mut ids = Vec::new();
        let plan = f
            .store
            .plan_pack(info.id, &|b| keep.contains(&b), &mut ids)
            .expect("plan");
        if plan.live_bytes == 0 && plan.records > 0 {
            let mut in_pack = Vec::new();
            f.store
                .plan_pack(info.id, &|_| true, &mut in_pack)
                .expect("plan");
            chosen = Some((info.id, in_pack));
            break;
        }
    }
    let (pack, condemned) = chosen.expect("a sealed pack holding only garbage");

    // The file goes first, as it would if another collector won the race.
    std::fs::remove_file(f.store_dir().join(format!("packs/pack-{pack:08}.cpk"))).expect("gone");

    let freed = f
        .store
        .discard_pack(pack, &condemned)
        .expect("discard over a pack that is already gone");
    assert_eq!(freed, 0, "there was nothing left to free");

    assert!(
        f.store.fsck().expect("fsck").is_clean(),
        "fsck is still usable after a lost race"
    );
    assert!(!f.store.recovery().has_corruption());
    for b in &live {
        assert!(f.store.get(*b).is_ok(), "a live block lost: {b:?}");
    }
}

/// F5: the became-live check is the only thing between a block that became reachable during the
/// cycle and the unlink of its only copy. A writer that dedups onto a condemned pack and commits
/// while the copies run must still be protected.
#[test]
fn a_writer_that_dedups_onto_a_rewritten_pack_keeps_the_block() {
    let f = Fixture::eager(32 << 10);
    let parts = f.parts();
    let parts = &parts;
    let roots = Roots::new();
    let snap = f.meta.new_snapshot("s").expect("snapshot");
    parts.write(&snap, b"keep", &body(60_000, 1));
    for i in 0..40u32 {
        parts.store.put(&body(4000, i)).unwrap();
    }
    parts.store.sync().unwrap();
    parts.meta.sync().unwrap();

    // Bytes a writer will put while the copies run. Each is a block that lives in a pack the sweep
    // is about to condemn, so putting it again deduplicates onto a record that is about to go.
    let late: Arc<Vec<Vec<u8>>> = Arc::new((0..24u32).map(|i| body(4000, i)).collect());
    let done = Arc::new(AtomicUsize::new(0));

    std::thread::scope(|sc| {
        let done = Arc::clone(&done);
        let writer = Arc::clone(&roots);
        let payload = Arc::clone(&late);
        sc.spawn(move || {
            let late = payload;
            for d in late.iter() {
                writer.write(|| {
                    parts.store.ingest_bytes(d).expect("put during the sweep");
                });
                done.fetch_add(1, Relaxed);
            }
        });

        let r = parts.gc.collect(Some(&*roots)).expect("collect");
        assert!(r.errors.is_empty(), "{r:?}");
        // Whatever the cycle decided, every block a writer handed it must still read.
        for d in late.iter() {
            for c in parts.store.ingest_bytes(d).expect("ids") {
                assert!(
                    parts.store.get(c.id).is_ok(),
                    "a block a writer deduplicated onto during the sweep is gone: {:?}",
                    c.id
                );
            }
        }
    });
    assert!(done.load(Relaxed) > 0, "the writer ran");
    for b in parts.live() {
        assert!(parts.store.get(b).is_ok(), "a live block lost: {b:?}");
    }
    assert!(parts.store.fsck().expect("fsck").is_clean());
}

/// F5: `ExtraRoots` answers are unioned, so a block reported on one poll and not the next is
/// still protected for the whole cycle.
#[test]
fn a_block_reported_on_one_poll_only_is_still_protected() {
    let f = Fixture::eager(32 << 10);
    let only_once = f.store.ingest_bytes(&body(5000, 77)).expect("ingest")[0].id;
    f.store.sync().unwrap();

    struct Once {
        polls: AtomicUsize,
        id: BlockId,
    }
    impl ExtraRoots for Once {
        fn pinned_blocks(&self) -> Result<Vec<BlockId>, RootsError> {
            let n = self.polls.fetch_add(1, Relaxed);
            Ok(if n == 0 { vec![self.id] } else { Vec::new() })
        }
        fn reference_barrier(&self) -> Result<Option<Box<dyn Barrier>>, RootsError> {
            Ok(Some(Box::new(Noop)))
        }
    }
    let roots = Once {
        polls: AtomicUsize::new(0),
        id: only_once,
    };

    for _ in 0..3 {
        f.gc.collect(Some(&roots)).expect("collect");
    }
    assert!(
        f.store.get(only_once).is_ok(),
        "a block reported on the first poll and dropped after was freed"
    );
}

/// F5: a hole is the zero id, and it is never demanded of the store or treated as live data.
#[test]
fn a_hole_is_never_swept() {
    let f = Fixture::eager(32 << 10);
    let snap = f.meta.new_snapshot("s").expect("snapshot");
    f.write(&snap, b"keep", &body(60_000, 1));
    for i in 0..40u32 {
        f.store.put(&body(4000, i)).unwrap();
    }
    f.store.sync().unwrap();
    f.meta.sync().unwrap();

    struct WithHole;
    impl ExtraRoots for WithHole {
        fn pinned_blocks(&self) -> Result<Vec<BlockId>, RootsError> {
            Ok(vec![cowfs_gc::HOLE, BlockId::of(b"real")])
        }
        fn reference_barrier(&self) -> Result<Option<Box<dyn Barrier>>, RootsError> {
            Ok(Some(Box::new(Noop)))
        }
    }

    let r = f.gc.collect(Some(&WithHole)).expect("collect");
    assert_eq!(r.pinned, 1, "the hole is dropped, the real id is not");
    assert!(r.errors.is_empty(), "{r:?}");
    assert!(f.store.fsck().expect("fsck").is_clean());
}

/// F5: a pack at or above the epoch watermark was written after the freeze, so the mark cannot have
/// seen a reference to it and it is never a candidate.
#[test]
fn a_pack_at_or_above_the_epoch_is_never_a_candidate() {
    let f = Fixture::eager(32 << 10);
    let roots = Roots::new();
    let snap = f.meta.new_snapshot("s").expect("snapshot");
    f.write(&snap, b"keep", &body(60_000, 1));
    for i in 0..40u32 {
        f.store.put(&body(4000, i)).unwrap();
    }
    f.store.sync().unwrap();
    f.meta.sync().unwrap();

    // Put after the epoch was taken, with no metadata commit naming it: unreachable, but written
    // after the freeze.
    let after = f.store.ingest_bytes(&body(4000, 200)).expect("ingest")[0].id;
    let r = f.gc.collect(Some(&*roots)).expect("collect");
    assert!(r.errors.is_empty(), "{r:?}");
    assert!(
        f.store.get(after).is_ok(),
        "a block written after the freeze was freed"
    );
    assert!(f.store.fsck().expect("fsck").is_clean());
}

/// F5: a pack below the dead-bytes threshold is left alone even though it is garbage.
#[test]
fn a_pack_below_the_dead_threshold_is_left_alone() {
    let f = Fixture::eager(32 << 10);
    let roots = Roots::new();
    let snap = f.meta.new_snapshot("s").expect("snapshot");
    f.write(&snap, b"keep", &body(60_000, 1));
    f.store.sync().unwrap();
    f.meta.sync().unwrap();
    // Two bytes of garbage in a 32 KiB pack.
    f.store.put(&body(1, 1)).unwrap();
    f.store.put(&body(1, 2)).unwrap();
    f.store.sync().unwrap();

    let gc = cowfs_gc::Gc::open(
        f.gc_dir(),
        Arc::clone(&f.store),
        Arc::clone(&f.meta),
        cowfs_gc::Options {
            dead_ratio: 0.9,
            min_dead_bytes: 1 << 20,
            ..eager()
        },
    )
    .expect("gc");
    let r = gc.collect(Some(&*roots)).expect("collect");
    assert_eq!(r.freed_bytes, 0, "a tiny dead region is not worth a copy");
    assert!(
        r.skipped
            .iter()
            .any(|s| s.reason == cowfs_gc::SkipReason::BelowThreshold),
        "and says so: {:?}",
        r.skipped
    );
    for b in f.live_blocks() {
        assert!(f.store.get(b).is_ok());
    }
}

/// F5: the dead *ratio* threshold is its own guard. `min_dead_bytes` above is set to 1, so only the
/// ratio can hold this pack back.
#[test]
fn a_pack_below_the_dead_ratio_is_left_alone() {
    let f = Fixture::eager(32 << 10);
    let roots = Roots::new();
    let snap = f.meta.new_snapshot("s").expect("snapshot");
    f.write(&snap, b"keep", &body(60_000, 1));
    // Garbage first, so the first pack is sealed and holds a few dead records among live ones.
    for i in 0..4u32 {
        f.store.put(&body(500, i)).unwrap();
    }
    // Enough filler to roll several packs, so the pack holding that garbage is not the active one.
    // The filler is written through the snapshot, so it is live: the only garbage is the four
    // records above, and the assertion that nothing is freed means something.
    for i in 0..24u32 {
        f.write(&snap, format!("f{i:02}").as_bytes(), &body(4_000, 500 + i));
    }
    f.store.sync().unwrap();
    f.meta.sync().unwrap();

    let gc = cowfs_gc::Gc::open(
        f.gc_dir(),
        Arc::clone(&f.store),
        Arc::clone(&f.meta),
        cowfs_gc::Options {
            dead_ratio: 0.95,
            min_dead_bytes: 1,
            ..eager()
        },
    )
    .expect("gc");
    let r = gc.collect(Some(&*roots)).expect("collect");
    assert!(
        f.store.packs().unwrap().len() > 1,
        "the store rolled, so a sealed pack exists to hold the garbage back: {:?}",
        f.store.packs().unwrap()
    );
    assert_eq!(r.freed_bytes, 0, "a pack that is mostly live is not swept");
    assert!(
        r.skipped
            .iter()
            .any(|s| s.reason == cowfs_gc::SkipReason::BelowThreshold),
        "and says the threshold held it back: {:?}",
        r.skipped
    );
    for b in f.live_blocks() {
        assert!(f.store.get(b).is_ok());
    }
}
