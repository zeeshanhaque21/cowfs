//! Regressions for the critic's findings on PR #39.
//!
//! Each test here failed on the reviewed head and pins one defect. The names say which, because a
//! test that does not say what it holds is a test nobody can trust when it breaks.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
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

/// F3a/F5: a writer that deduplicates onto a condemned record **and commits** keeps the block.
///
/// Both halves matter. Ingesting alone leaves the bytes unreferenced, so the block really is
/// garbage and freeing it is correct; the earlier version of this test asserted after re-ingesting,
/// which put the bytes back and made it pass on a store that had already lost them. Committing is
/// what makes the reference real, and the reference is what the step-5 re-read has to find.
#[test]
fn a_committing_writer_that_dedups_onto_a_rewritten_pack_keeps_the_block() {
    let f = Fixture::eager(32 << 10);
    let parts = f.parts();
    let roots = Roots::new();
    let snap = f.meta.new_snapshot("s").expect("snapshot");
    parts.write(&snap, b"keep", &body(60_000, 1));
    for i in 0..40u32 {
        parts.store.put(&body(4000, i)).expect("put");
    }
    parts.store.sync().unwrap();
    parts.meta.sync().unwrap();

    let late: Arc<Vec<Vec<u8>>> = Arc::new((0..24u32).map(|i| body(4000, i)).collect());
    // The ids are read once, before the cycle. Re-deriving them afterwards would put the bytes back.
    let ids: Vec<BlockId> = late
        .iter()
        .flat_map(|d| parts.store.ingest_bytes(d).expect("ids"))
        .map(|c| c.id)
        .collect();
    assert_eq!(ids.len(), late.len(), "each payload is one block");
    parts.store.sync().expect("sync");

    let meta_h = Arc::clone(&parts.meta);
    let store_h = Arc::clone(&parts.store);
    let done = Arc::new(AtomicUsize::new(0));

    std::thread::scope(|sc| {
        let done = Arc::clone(&done);
        let writer = roots.clone();
        let payload = Arc::clone(&late);
        sc.spawn(move || {
            let late = payload;
            for (i, d) in late.iter().enumerate() {
                let name = format!("late{i}");
                writer.write(|| {
                    let s = meta_h.new_snapshot(&name).expect("snapshot");
                    let got = store_h.ingest_bytes(d).expect("put during the sweep");
                    assert_eq!(got.len(), 1, "the put deduplicated rather than stored");
                    let ino = s
                        .batch(|tx| tx.create(cowfs_meta::ROOT_INO, name.as_bytes(), 0o644))
                        .expect("create")
                        .ino;
                    s.batch(|tx| tx.set_content(ino, &got, d.len() as u64))
                        .expect("set content");
                    meta_h.sync().expect("sync");
                });
                done.fetch_add(1, Relaxed);
            }
        });

        let r = parts.gc.collect(Some(&*roots)).expect("collect");
        assert!(r.errors.is_empty(), "{r:?}");
    });

    assert_eq!(done.load(Relaxed), late.len(), "every writer commit ran");
    for b in &ids {
        assert!(
            parts.live().contains(b),
            "the committed snapshots do not name {b:?}, so this test proves nothing"
        );
        assert!(
            parts.store.get(*b).is_ok(),
            "a block a committing writer deduplicated onto during the sweep is gone: {b:?}"
        );
    }
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

/// A pack that holds one condemned record, and the fixture that builds one.
///
/// Returned alongside the id of a record that no live snapshot references, in a sealed pack that
/// the collector will choose.
fn one_condemned(f: &Fixture) -> BlockId {
    let keep = f.live_blocks();
    for info in f.store.packs().expect("packs") {
        if info.active {
            continue;
        }
        let mut live = Vec::new();
        let plan = f
            .store
            .plan_pack(info.id, &|b| keep.contains(&b), &mut live)
            .expect("plan");
        // A pack with nothing live in it, so the collector really does choose to rewrite it.
        if plan.live_bytes != 0 || plan.dead_bytes == 0 {
            continue;
        }
        let mut all = Vec::new();
        f.store
            .plan_pack(info.id, &|_| true, &mut all)
            .expect("plan");
        if let Some(b) = all.into_iter().find(|b| !keep.contains(b)) {
            return b;
        }
    }
    panic!("no sealed pack holds a condemned record");
}

/// A fixture with one live snapshot and one condemned record in a sealed pack.
fn with_one_condemned() -> Fixture {
    let f = Fixture::eager(32 << 10);
    let snap = f.meta.new_snapshot("s").expect("snapshot");
    f.write(&snap, b"keep", &body(60_000, 1));
    for i in 0..40u32 {
        f.store.put(&body(4_000, 900 + i)).expect("put");
    }
    f.store.sync().unwrap();
    f.meta.sync().unwrap();
    f
}

/// F5: a block that becomes reachable while the copies run is only spared by the became-live check
/// at the unlink.
///
/// `pinned_blocks` is polled at the freeze, after the mark, before the copy, and again before every
/// unlink. Reporting the block only from the fourth poll on is exactly a write that lands during
/// the copy, so a collector that does not re-check at the unlink frees it.
#[test]
fn a_block_that_becomes_live_during_the_copy_is_spared_the_unlink() {
    let f = with_one_condemned();
    let condemned = one_condemned(&f);

    struct Late {
        polls: AtomicUsize,
        id: BlockId,
    }
    impl ExtraRoots for Late {
        fn pinned_blocks(&self) -> Result<Vec<BlockId>, RootsError> {
            let n = self.polls.fetch_add(1, Relaxed);
            // Polls 1 to 3 are the freeze, the end of the mark and the start of the copy. A write
            // that lands during the copy is first seen by the poll before the first unlink.
            Ok(if n >= 3 { vec![self.id] } else { Vec::new() })
        }
        fn reference_barrier(&self) -> Result<Option<Box<dyn Barrier>>, RootsError> {
            Ok(Some(Box::new(Noop)))
        }
    }
    let roots = Late {
        polls: AtomicUsize::new(0),
        id: condemned,
    };

    let r = f.gc.collect(Some(&roots)).expect("collect");
    assert!(r.errors.is_empty(), "the cycle reported errors: {r:?}");
    assert!(
        r.skipped
            .iter()
            .any(|s| s.reason == cowfs_gc::SkipReason::BecameLive),
        "the pack holding a block that became live was not spared: {:?} {r:?}",
        r.skipped
    );
    assert!(
        f.store.get(condemned).is_ok(),
        "a block that became live while the copies ran was freed"
    );
}

/// F5: the polls are unioned. A collector that keeps only the first answer drops everything the
/// later polls reported, so a block named on the second poll and after is freed.
#[test]
fn a_block_reported_on_a_later_poll_only_is_still_protected() {
    let f = with_one_condemned();
    let condemned = one_condemned(&f);

    struct Later {
        polls: AtomicUsize,
        first: BlockId,
        later: BlockId,
    }
    impl ExtraRoots for Later {
        fn pinned_blocks(&self) -> Result<Vec<BlockId>, RootsError> {
            let n = self.polls.fetch_add(1, Relaxed);
            Ok(if n == 0 {
                vec![self.first]
            } else {
                vec![self.later]
            })
        }
        fn reference_barrier(&self) -> Result<Option<Box<dyn Barrier>>, RootsError> {
            Ok(Some(Box::new(Noop)))
        }
    }
    let roots = Later {
        polls: AtomicUsize::new(0),
        first: BlockId::of(b"first poll only"),
        later: condemned,
    };
    let r = f.gc.collect(Some(&roots)).expect("collect");
    assert!(r.errors.is_empty(), "{r:?}");
    assert!(
        f.store.get(condemned).is_ok(),
        "a block reported only on a later poll was freed: {r:?}"
    );
}

/// F5: the persisted set must not keep naming a block whose snapshot is gone.
///
/// Without the prune the set grows for ever, so a later cycle still treats the block as live. The
/// store is the observable, and `mark.bin` is the thing being asserted about.
#[test]
fn the_persisted_set_forgets_a_block_whose_snapshot_is_gone() {
    let f = Fixture::eager(32 << 10);
    let doomed = f.meta.new_snapshot("doomed").expect("snapshot");
    for i in 0..8u32 {
        f.write(&doomed, format!("d{i}").as_bytes(), &body(6_000, 40 + i));
    }
    let keep = f.meta.new_snapshot("keep").expect("snapshot");
    f.write(&keep, b"keep", &body(6_000, 3));
    f.meta.sync().unwrap();
    f.store.sync().unwrap();

    let roots = Roots::new();
    f.gc.collect(Some(&*roots)).expect("first collect");
    let mut marker = cowfs_meta::Marker::new();
    let gone: Vec<BlockId> = doomed
        .live_blocks(&mut marker)
        .expect("walk")
        .map(|b| b.expect("block"))
        .filter(|b| *b != cowfs_gc::HOLE)
        .collect();
    assert!(!gone.is_empty(), "the doomed snapshot holds blocks");
    let recorded = mark_bytes(&f);
    assert!(
        gone.iter().all(|b| has(&recorded, b.as_bytes())),
        "the first cycle recorded every block of the doomed snapshot"
    );

    f.meta.remove_snapshot(doomed.id()).unwrap();
    f.meta.reap_all().unwrap();
    f.meta.sync().unwrap();
    f.gc.collect(Some(&*roots)).expect("second collect");

    let after = mark_bytes(&f);
    for b in &gone {
        assert!(
            !has(&after, b.as_bytes()),
            "mark.bin still names a block whose snapshot is gone: {b:?}"
        );
    }
    for b in f.live_blocks() {
        assert!(f.store.get(b).is_ok(), "a live block was freed: {b:?}");
    }
}

/// F5: the persisted root set must not keep naming a root that is gone, or every later cycle
/// believes it has already walked it.
#[test]
fn the_persisted_set_forgets_a_root_that_is_gone() {
    let f = Fixture::eager(32 << 10);
    let doomed = f.meta.new_snapshot("doomed").expect("snapshot");
    f.write(&doomed, b"x", &body(6_000, 11));
    f.meta.sync().unwrap();
    let roots = Roots::new();
    f.gc.collect(Some(&*roots)).expect("first collect");

    let doomed_root = *f
        .meta
        .snapshot_by_id(doomed.id())
        .unwrap()
        .root()
        .unwrap()
        .as_bytes();
    let recorded = mark_bytes(&f);
    assert!(
        recorded.windows(32).any(|w| w == doomed_root),
        "the first cycle recorded the root"
    );

    f.meta.remove_snapshot(doomed.id()).unwrap();
    f.meta.reap_all().unwrap();
    f.meta.sync().unwrap();
    f.gc.collect(Some(&*roots)).expect("second collect");

    let after = mark_bytes(&f);
    assert!(
        !after.windows(32).any(|w| w == doomed_root),
        "mark.bin still names a root that is gone"
    );
}

/// F5: the durable roots are read after the metadata sync, or a snapshot this cycle makes durable is
/// invisible to its own freeze and its blocks are freed.
#[test]
fn a_snapshot_this_cycle_makes_durable_is_not_swept() {
    let f = Fixture::eager(32 << 10);
    let snap = f.meta.new_snapshot("fresh").expect("snapshot");
    let refs = f.write(&snap, b"fresh", &body(20_000, 77));
    let blocks: Vec<BlockId> = refs.iter().map(|c| c.id).collect();
    assert!(!blocks.is_empty(), "the file produced blocks");
    // No metadata sync: the collector's own sync is what makes this snapshot durable.
    f.store.sync().unwrap();
    for i in 0..30u32 {
        f.store.put(&body(4_000, 300 + i)).expect("put");
    }
    f.store.sync().unwrap();

    let roots = Roots::new();
    let r = f.gc.collect(Some(&*roots)).expect("collect");
    assert!(r.errors.is_empty(), "{r:?}");
    for b in &blocks {
        assert!(
            f.store.get(*b).is_ok(),
            "a block of a snapshot the cycle itself made durable was freed: {b:?} {r:?}"
        );
    }
    assert!(f.store.fsck().expect("fsck").is_clean());
}

/// True when `needle` appears as a whole 32 byte entry in `hay`.
fn has(hay: &[u8], needle: &[u8; 32]) -> bool {
    hay.windows(32).any(|w| w == needle)
}

/// The bytes of the collector's persisted mark set, which is a root list then a block list.
fn mark_bytes(f: &Fixture) -> Vec<u8> {
    std::fs::read(f.gc_dir().join("mark.bin")).unwrap_or_default()
}

/// F1: a snapshot committed while the copies run must keep its blocks.
///
/// The commit is placed on the collector's own thread from the progress callback, so the ordering
/// is forced rather than raced for. Before the fix the step-5 pass was handed the root list from
/// the freeze with every root already walked, so it returned nothing and the pack was unlinked:
///
/// ```text
/// DATA LOSS: a durable snapshot references BlockId(e5eea69c..10fbef1), the collector freed it,
/// and the cycle reported 5 packs unlinked and no error. freed_bytes=159132 errors=[]
/// ```
#[test]
fn a_snapshot_committed_during_the_copy_keeps_its_blocks() {
    let f = Fixture::eager(32 << 10);
    let parts = f.parts();
    let base = f.meta.new_snapshot("base").expect("snapshot");
    parts.write(&base, b"keep", &body(60_000, 1));

    let victim = body(4000, 7);
    let vid = parts.store.put(&victim).expect("put");
    for i in 0..40u32 {
        parts.store.put(&body(4000, i)).expect("put");
    }
    parts.store.sync().unwrap();
    parts.meta.sync().unwrap();
    assert!(
        parts.store.get(vid).is_ok(),
        "the block is there before the cycle"
    );

    let roots = Roots::new();
    let committed = Arc::new(AtomicUsize::new(0));
    let meta_h = Arc::clone(&parts.meta);
    let store_h = Arc::clone(&parts.store);
    {
        let committed = Arc::clone(&committed);
        let victim = victim.clone();
        let roots = roots.clone();
        f.gc.set_progress(move |p| {
            // Fires once per pack copied, inside the copy phase, which runs with no barrier. A
            // commit now lands before every unlink.
            if p.sweeping && committed.load(Relaxed) == 0 {
                roots.write(|| {
                    let snap = meta_h.new_snapshot("late").expect("snapshot");
                    let chunks = store_h.ingest_bytes(&victim).expect("ingest");
                    assert_eq!(chunks[0].id, vid, "the commit deduplicated onto the victim");
                    let ino = snap
                        .batch(|tx| tx.create(cowfs_meta::ROOT_INO, b"late", 0o644))
                        .expect("create")
                        .ino;
                    snap.batch(|tx| tx.set_content(ino, &chunks, victim.len() as u64))
                        .expect("set content");
                    meta_h.sync().expect("sync");
                    committed.fetch_add(1, Relaxed);
                });
            }
        });
    }

    let r = f.gc.collect(Some(&*roots)).expect("collect");
    assert!(r.errors.is_empty(), "the cycle reported no error: {r:?}");
    assert_eq!(
        committed.load(Relaxed),
        1,
        "the commit ran inside the copy phase"
    );
    assert!(r.packs_unlinked > 0, "the cycle unlinked packs: {r:?}");

    assert!(
        parts.live().contains(&vid),
        "the snapshot committed mid-cycle is not in the durable live set, so this test proves \
         nothing: {r:?}"
    );
    let got = parts.store.get(vid);
    assert!(
        got.is_ok(),
        "DATA LOSS: a durable snapshot references {vid:?}, the collector freed it, and the cycle \
         reported {} packs unlinked and no error. freed_bytes={} errors={:?}",
        r.packs_unlinked,
        r.freed_bytes,
        r.errors
    );
    assert_eq!(
        got.unwrap(),
        victim,
        "and the bytes are right when they survive"
    );
}

/// F1: the same commit, but after the last copy and before the unlinks, on another thread, which
/// is what a real writer does. Several rounds, because this one is a race.
#[test]
fn a_snapshot_committed_while_the_cycle_runs_keeps_its_blocks() {
    for round in 0..3u32 {
        let f = Fixture::eager(32 << 10);
        let parts = f.parts();
        let base = f.meta.new_snapshot("base").expect("snapshot");
        parts.write(&base, b"keep", &body(60_000, 900 + round));
        let victim = body(4000, 500 + round);
        let vid = parts.store.put(&victim).expect("put");
        for i in 0..60u32 {
            parts.store.put(&body(4000, round * 100 + i)).expect("put");
        }
        parts.store.sync().unwrap();
        parts.meta.sync().unwrap();

        let roots = Roots::new();
        let stop = Arc::new(AtomicBool::new(false));
        let committed = Arc::new(AtomicUsize::new(0));
        let roots_w = roots.clone();
        let stop_w = Arc::clone(&stop);
        let committed_w = Arc::clone(&committed);
        let victim_w = victim.clone();

        std::thread::scope(|sc| {
            let parts = &parts;
            sc.spawn(move || {
                let mut i = 0u32;
                while !stop_w.load(Relaxed) && i < 80 {
                    let name = format!("late{i}");
                    let victim = &victim_w;
                    roots_w.write(|| {
                        let snap = parts.meta.new_snapshot(&name).expect("snapshot");
                        let chunks = parts.store.ingest_bytes(victim).expect("ingest");
                        let ino = snap
                            .batch(|tx| tx.create(cowfs_meta::ROOT_INO, name.as_bytes(), 0o644))
                            .expect("create")
                            .ino;
                        snap.batch(|tx| tx.set_content(ino, &chunks, victim.len() as u64))
                            .expect("set content");
                        parts.meta.sync().expect("sync");
                        committed_w.fetch_add(1, Relaxed);
                    });
                    i += 1;
                }
                stop_w.store(true, Relaxed);
            });
            let r = f.gc.collect(Some(&*roots)).expect("collect");
            stop.store(true, Relaxed);
            assert!(r.errors.is_empty(), "round {round}: {r:?}");
        });

        assert!(
            committed.load(Relaxed) > 0,
            "round {round}: the writer committed"
        );
        assert!(
            parts.live().contains(&vid),
            "round {round}: the committed snapshots do not name the victim, so this test proves \
             nothing"
        );
        assert!(
            parts.store.get(vid).is_ok(),
            "round {round}: DATA LOSS, a durable snapshot references {vid:?} and the store cannot \
             produce it"
        );
    }
}

/// F2: the barrier guard must be alive from the last root read through the discard it gates.
///
/// It used to be bound inside the `if let` that acquired it, so it dropped before both. Measured
/// then: `barrier taken: 1, polls with a guard alive: 0, polls with NO guard alive: 8, packs
/// unlinked: 5`.
#[test]
fn the_barrier_is_held_across_every_discard_it_gates() {
    let f = Fixture::eager(32 << 10);
    let parts = f.parts();
    let base = f.meta.new_snapshot("base").expect("snapshot");
    parts.write(&base, b"keep", &body(60_000, 1));
    for i in 0..40u32 {
        parts.store.put(&body(4000, i)).expect("put");
    }
    parts.store.sync().unwrap();
    parts.meta.sync().unwrap();

    let roots = Roots::new();
    let live_at_discard = Arc::new(AtomicUsize::new(0));
    let unheld_at_discard = Arc::new(AtomicUsize::new(0));
    {
        let live_at_discard = Arc::clone(&live_at_discard);
        let unheld_at_discard = Arc::clone(&unheld_at_discard);
        let r = roots.clone();
        // Fired once per pack unlinked. A guard has to be alive here, or the became-live check
        // that ran just before was not gated by anything.
        f.gc.set_progress(move |p| {
            if p.freed_bytes > 0 {
                if r.barrier_live() {
                    live_at_discard.fetch_add(1, Relaxed);
                } else {
                    unheld_at_discard.fetch_add(1, Relaxed);
                }
            }
        });
        let report = f.gc.collect(Some(&*roots)).expect("collect");
        assert!(
            report.packs_unlinked > 0,
            "the cycle unlinked packs: {report:?}"
        );
    }
    assert_eq!(
        unheld_at_discard.load(Relaxed),
        0,
        "a discard ran with no barrier alive"
    );
    assert!(
        live_at_discard.load(Relaxed) > 0,
        "no discard ran, so this test proves nothing"
    );
}

/// F1, second shape: a snapshot forked during the cycle from one that was walked before it. The
/// fork shares most of its tree with a walked root, so only the changed path is re-walked.
#[test]
fn a_snapshot_forked_during_the_cycle_keeps_its_blocks() {
    let f = Fixture::eager(32 << 10);
    let parts = f.parts();
    let base = f.meta.new_snapshot("base").expect("snapshot");
    parts.write(&base, b"keep", &body(60_000, 1));
    let victim = body(4000, 31);
    parts.store.put(&victim).expect("put");
    for i in 0..40u32 {
        parts.store.put(&body(4000, i)).expect("put");
    }
    parts.store.sync().unwrap();
    parts.meta.sync().unwrap();

    let roots = Roots::new();
    let forked = Arc::new(AtomicBool::new(false));
    let meta_h = Arc::clone(&parts.meta);
    let store_h = Arc::clone(&parts.store);
    let base_snap = parts.meta.snapshot("base").expect("base");
    {
        let forked = Arc::clone(&forked);
        let roots = roots.clone();
        let victim = victim.clone();
        let base_snap = base_snap.clone();
        f.gc.set_progress(move |p| {
            if p.sweeping && !forked.swap(true, Relaxed) {
                roots.write(|| {
                    // A fork of the root the collector already walked: most of its tree is
                    // unchanged, so only the new path is re-walked.
                    let fork = base_snap.fork("fork").expect("fork");
                    let chunks = store_h.ingest_bytes(&victim).expect("ingest");
                    let ino = fork
                        .batch(|tx| tx.create(cowfs_meta::ROOT_INO, b"fork", 0o644))
                        .expect("create")
                        .ino;
                    fork.batch(|tx| tx.set_content(ino, &chunks, victim.len() as u64))
                        .expect("set content");
                    meta_h.sync().expect("sync");
                });
            }
        });
    }
    let r = f.gc.collect(Some(&*roots)).expect("collect");
    assert!(r.errors.is_empty(), "{r:?}");
    assert!(forked.load(Relaxed), "the fork ran inside the copy phase");
    assert!(
        parts.store.fsck().expect("fsck").is_clean(),
        "fsck is clean: {r:?}"
    );
    for b in parts.live() {
        assert!(
            parts.store.get(b).is_ok(),
            "a block a snapshot forked during the cycle references was freed: {b:?}"
        );
    }
}
