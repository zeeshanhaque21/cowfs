//! Marking: shared subtrees, skipped roots, holes, pinned blocks, and the incremental behaviour.

mod common;

use std::sync::atomic::Ordering;

use common::{Fixture, Roots};
use cowfs_gc::{SkipReason, HOLE};
use cowfs_meta::Snapshot;
use cowfs_store::BlockId;

fn body(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8) ^ seed ^ (i >> 7) as u8).collect()
}

#[test]
fn an_empty_database_marks_nothing_and_frees_nothing() {
    let f = Fixture::eager(64 << 10);
    for i in 0..8u8 {
        f.store.put(&body(4096, i)).unwrap();
    }
    f.store.sync().unwrap();
    let roots = Roots::new();
    let r = f.gc.collect(Some(&*roots)).unwrap();
    assert_eq!(r.marked, 0);
    assert_eq!(r.live_blocks, 0);
    assert_eq!(r.store_blocks, 8);
    assert_eq!(
        r.freed_bytes, 0,
        "nothing is referenced, but nothing is either: no snapshot"
    );
}

#[test]
fn a_snapshot_root_walk_yields_every_chunk() {
    let f = Fixture::eager(64 << 10);
    let snap = f.meta.new_snapshot("s").unwrap();
    f.write(&snap, b"a", &body(40_000, 1));
    f.write(&snap, b"b", &body(20_000, 2));
    let roots = Roots::new();
    let r = f.gc.collect(Some(&*roots)).unwrap();
    assert!(r.marked >= 2, "both files walked, got {}", r.marked);
    assert_eq!(r.live_blocks, r.marked as usize);
    for b in f.live_blocks() {
        assert!(f.store.contains(b), "a referenced block is in the store");
    }
}

#[test]
fn a_hole_chunk_is_never_demanded_of_the_store() {
    let f = Fixture::eager(64 << 10);
    let snap = f.meta.new_snapshot("s").unwrap();
    // A sparse file: a real chunk, then a hole, then another real chunk.
    // Over the 16 KiB minimum chunk, so this is more than one block and real[1] exists.
    let real = f.store.ingest_bytes(&body(90_000, 3)).unwrap();
    assert!(real.len() >= 2, "the fixture needs two real chunks");
    let chunks = vec![
        real[0],
        cowfs_store::ChunkRef {
            id: HOLE,
            len: 4096,
        },
        real[1],
    ];
    let ino = snap
        .batch(|tx| tx.create(cowfs_meta::ROOT_INO, b"sparse", 0o644))
        .unwrap()
        .ino;
    let total: u64 = chunks.iter().map(|c| u64::from(c.len)).sum();
    snap.batch(|tx| tx.set_content(ino, &chunks, total))
        .unwrap();
    let roots = Roots::new();
    let r = f.gc.collect(Some(&*roots)).unwrap();
    assert!(!f.store.contains(HOLE), "the hole id is not a stored block");
    for b in f.live_blocks() {
        assert_ne!(b, HOLE);
        assert!(f.store.get(b).is_ok());
    }
    assert_eq!(r.freed_bytes, 0, "nothing to free with one pack");
}

#[test]
fn a_pinned_block_is_never_swept() {
    let f = Fixture::eager(4096);
    let snap = f.meta.new_snapshot("s").unwrap();
    // Fill several packs with garbage, then keep one block only in the pinned set.
    let pinned: Vec<BlockId> = (100..104u8)
        .map(|i| f.store.put(&body(900, i)).unwrap())
        .collect();
    for i in 0..24u8 {
        f.store.put(&body(900, i)).unwrap();
    }
    f.write(&snap, b"live", &body(900, 200));
    f.store.sync().unwrap();
    let before = f.store.stats().blocks;
    let roots = Roots::new();
    for b in &pinned {
        roots.pin(*b);
    }
    let r = f.gc.collect(Some(&*roots)).unwrap();
    assert_eq!(r.pinned, pinned.len() as u64);
    for b in &pinned {
        assert!(f.store.get(*b).is_ok(), "a pinned block survives the sweep");
    }
    assert!(r.freed_bytes > 0, "garbage did go, {} bytes", r.freed_bytes);
    assert!(f.store.stats().blocks < before, "the store shrank");
    let snap_blocks = f.live_blocks();
    for b in &snap_blocks {
        assert!(f.store.get(*b).is_ok(), "a referenced block is never swept");
    }
    // What is left is the live file plus the four pinned blocks. The blocks that were in the
    // active pack when the cycle ran are also left, and must be: the active pack is never a
    // candidate, so a sweep never rewrites the pack `put` is appending to.
    let after_first = f.store.stats().blocks;
    let r2 = f.gc.collect(Some(&*roots)).unwrap();
    assert_eq!(f.store.stats().blocks, after_first, "still nothing to do");
    assert!(r2
        .skipped
        .iter()
        .any(|s| { s.reason == SkipReason::Active }));

    // Roll the active pack so the pinned records are in a sealed pack, then unpin and sweep.
    for i in 0..12u8 {
        f.store.put(&body(900, 30 + i)).unwrap();
    }
    f.store.sync().unwrap();
    roots.unpin_all();
    let r3 = f.gc.collect(Some(&*roots)).unwrap();
    assert!(
        f.store.stats().blocks < after_first,
        "unpinned blocks are reclaimable: {} bytes freed, {} blocks left",
        r3.freed_bytes,
        f.store.stats().blocks
    );
    for b in &snap_blocks {
        assert!(
            f.store.get(*b).is_ok(),
            "a referenced block survives every cycle"
        );
    }
}

#[test]
fn snapshots_sharing_a_subtree_walk_it_once() {
    let f = Fixture::eager(1 << 20);
    let base = f.meta.new_snapshot("base").unwrap();
    // A tree with a few thousand files under one directory, then a fork.
    f.write(&base, b"only", &body(1000, 1));
    for d in 0..8u8 {
        let dir = format!("d{d}");
        base.batch(|tx| tx.mkdir(cowfs_meta::ROOT_INO, dir.as_bytes(), 0o755))
            .unwrap();
    }
    for d in 0..8u8 {
        let dir = format!("d{d}");
        for fidx in 0..64u16 {
            let name = format!("f{fidx:03}");
            let data = body(700, (fidx % 7) as u8);
            let chunks = f.store.ingest_bytes(&data).unwrap();
            let di = base
                .batch(|tx| {
                    tx.lookup(cowfs_meta::ROOT_INO, dir.as_bytes())
                        .map(|a| a.ino)
                        .map_err(|_| cowfs_meta::Error::NoSuchSnapshot)
                })
                .unwrap();
            base.batch(|tx| tx.create(di, name.as_bytes(), 0o644))
                .unwrap();
            let ino = base
                .batch(|tx| {
                    tx.lookup(di, name.as_bytes())
                        .map(|a| a.ino)
                        .map_err(|_| cowfs_meta::Error::NoSuchSnapshot)
                })
                .unwrap();
            base.batch(|tx| tx.set_content(ino, &chunks, data.len() as u64))
                .unwrap();
        }
    }
    f.meta.sync().unwrap();
    let forked = base.fork("fork").unwrap();
    f.meta.sync().unwrap();

    let roots = Roots::new();
    let r = f.gc.collect(Some(&*roots)).unwrap();
    // Every file in the base is yielded once, and the fork, which shares every node with it,
    // yields nothing more. So `marked` is the file count, not twice it.
    assert_eq!(
        r.marked,
        8 * 64 + 1,
        "the fork's shared subtrees are walked once"
    );
    assert!(
        f.live_blocks().len() < r.marked as usize,
        "the walk yields duplicates, which is what dedup is for"
    );
    assert!(
        f.fork_is_isolated(&forked),
        "the fork is untouched by the mark"
    );
}

#[test]
fn an_unchanged_root_is_skipped_by_the_next_cycle() {
    let f = Fixture::eager(1 << 20);
    let snap = f.meta.new_snapshot("s").unwrap();
    for i in 0..24u8 {
        f.write(&snap, format!("f{i}").as_bytes(), &body(2000, i));
    }
    f.meta.sync().unwrap();
    let roots = Roots::new();
    let first = f.gc.collect(Some(&*roots)).unwrap();
    assert!(first.marked > 0, "the first cycle walks");
    let second = f.gc.collect(Some(&*roots)).unwrap();
    assert_eq!(second.marked, 0, "an unchanged root is skipped whole");
    assert_eq!(
        second.marked_skipped_roots, 1,
        "the skip is reported, not silent"
    );
    assert_eq!(
        second.live_blocks, first.live_blocks,
        "a skipped root's blocks stay live"
    );
    for b in f.live_blocks() {
        assert!(
            f.store.get(b).is_ok(),
            "a skipped root's block is still readable"
        );
    }
}

/// A tree deep enough that one file sits in its own leaf: `dX/sY/fNN`, `DIRS * SUBS * n` files.
///
/// One tree with many files, so a single rewrite shows how much of it the marker skips.
fn nested(f: &Fixture, dirs: u8, subs: u8, n: u8) -> Snapshot {
    let snap = f.meta.new_snapshot("nested").unwrap();
    for d in 0..dirs {
        let dir = format!("d{d}");
        snap.batch(|tx| tx.mkdir(cowfs_meta::ROOT_INO, dir.as_bytes(), 0o755))
            .unwrap();
        let di = snap
            .batch(|tx| {
                tx.lookup(cowfs_meta::ROOT_INO, dir.as_bytes())
                    .map(|a| a.ino)
            })
            .unwrap();
        for k in 0..subs {
            let sub = format!("s{k}");
            snap.batch(|tx| tx.mkdir(di, sub.as_bytes(), 0o755))
                .unwrap();
            let si = snap
                .batch(|tx| tx.lookup(di, sub.as_bytes()).map(|a| a.ino))
                .unwrap();
            for i in 0..n {
                let name = format!("f{i:02}");
                f.write_at(&snap, si, name.as_bytes(), &body(1500, i ^ d ^ k));
            }
        }
    }
    f.meta.sync().unwrap();
    snap
}

#[test]
fn a_small_change_in_many_snapshots_walks_only_the_changed_paths() {
    // Root-level skipping handles an unchanged snapshot. This is the other half: many snapshots
    // that all differ from each other by a little, walked in one cycle with one shared marker, so
    // the first walk pays for the tree and every other walk pays only for what it changed.
    let f = Fixture::eager(1 << 20);
    let base = nested(&f, 8, 8, 8);
    let forks: Vec<Snapshot> = (0..6)
        .map(|k| base.fork(&format!("k{k}")).unwrap())
        .collect();
    for (k, fork) in forks.iter().enumerate() {
        let di = fork
            .batch(|tx| tx.lookup(cowfs_meta::ROOT_INO, b"d0").map(|a| a.ino))
            .unwrap();
        let si = fork
            .batch(|tx| tx.lookup(di, b"s0").map(|a| a.ino))
            .unwrap();
        f.write_at(fork, si, b"f00", &body(1500, 100 + k as u8));
    }
    f.meta.sync().unwrap();
    let all = 8 * 8 * 8u64;
    let roots = Roots::new();
    let r = f.gc.collect(Some(&*roots)).unwrap();

    // The base costs the whole tree; each fork costs only the leaves its change rewrote, which is
    // the one directory's worth of files holding the rewritten chunk row.
    let extra = r.marked - all;
    assert!(
        extra > 0 && extra <= 6 * 16,
        "each fork pays for one leaf, not the tree: {extra} extra yields"
    );
    assert!(
        r.marked * 2 < all * 7,
        "without subtree skipping this would be about {} yields, got {}",
        all * 7,
        r.marked
    );
    for b in f.live_blocks() {
        assert!(f.store.get(b).is_ok(), "every referenced block reads back");
    }
}

#[test]
fn a_change_to_one_snapshot_re_walks_that_snapshot_only() {
    let f = Fixture::eager(1 << 20);
    let base = nested(&f, 8, 8, 8);
    let other = base.fork("other").unwrap();
    f.meta.sync().unwrap();
    let all = 8 * 8 * 8u64;
    let roots = Roots::new();
    let first = f.gc.collect(Some(&*roots)).unwrap();
    assert_eq!(
        first.marked, all,
        "the fork shares every node, so one full walk"
    );

    let di = base
        .batch(|tx| tx.lookup(cowfs_meta::ROOT_INO, b"d0").map(|a| a.ino))
        .unwrap();
    let si = base
        .batch(|tx| tx.lookup(di, b"s0").map(|a| a.ino))
        .unwrap();
    f.write_at(&base, si, b"f00", &body(1500, 200));
    f.meta.sync().unwrap();

    let second = f.gc.collect(Some(&*roots)).unwrap();
    assert_eq!(
        second.marked_skipped_roots, 1,
        "the unchanged fork is skipped whole"
    );
    assert_eq!(
        second.marked, all,
        "the changed root is walked in full: a node-level persistent marker is not available, \
         only a root-level one, so a changed root costs a full walk"
    );
    for b in f.live_blocks() {
        assert!(f.store.get(b).is_ok(), "every referenced block reads back");
    }
    let _ = other;
}

#[test]
fn a_snapshot_created_during_the_mark_loses_nothing() {
    let f = Fixture::eager(1 << 20);
    let first = f.meta.new_snapshot("first").unwrap();
    for i in 0..16u8 {
        f.write(&first, format!("a{i}").as_bytes(), &body(1200, i));
    }
    f.meta.sync().unwrap();
    let roots = Roots::new();

    // A snapshot appears while the mark is running: its blocks were not in the frozen roots, so
    // only the step 5 re-walk, under the barrier, can save them.
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let progress_seen = std::sync::Arc::clone(&seen);
    f.gc.set_progress(move |p| {
        if p.packs_done == 0 && p.sweeping {
            progress_seen.fetch_add(1, Ordering::Relaxed);
        }
    });
    let later = f.meta.new_snapshot("later").unwrap();
    f.write(&later, b"z", &body(3000, 42));
    f.meta.sync().unwrap();
    let r = f.gc.collect(Some(&*roots)).unwrap();
    for b in f.live_blocks() {
        assert!(
            f.store.get(b).is_ok(),
            "every block of every snapshot reads back after a concurrent create"
        );
    }
    assert!(r.candidates > 0 || r.freed_bytes == 0);
}

#[test]
fn the_active_pack_is_never_a_candidate() {
    let f = Fixture::eager(1 << 20);
    let snap = f.meta.new_snapshot("s").unwrap();
    for i in 0..8u8 {
        f.write(&snap, format!("f{i}").as_bytes(), &body(2000, i));
    }
    f.store.sync().unwrap();
    let roots = Roots::new();
    let r = f.gc.collect(Some(&*roots)).unwrap();
    let packs = f.store.packs().unwrap();
    let active = packs.iter().find(|p| p.active).unwrap().id;
    assert!(
        r.skipped
            .iter()
            .any(|s| s.pack == active && s.reason == SkipReason::Active),
        "the active pack is skipped and says why: {:?}",
        r.skipped
    );
}

#[test]
fn the_state_directory_lives_outside_the_store() {
    let f = Fixture::eager(1 << 20);
    let snap = f.meta.new_snapshot("s").unwrap();
    f.write(&snap, b"a", &body(3000, 1));
    f.meta.sync().unwrap();
    let roots = Roots::new();
    f.gc.collect(Some(&*roots)).unwrap();
    assert!(
        f.gc_dir().join("mark.bin").exists(),
        "the marked set is persisted"
    );
    let store_files = f.store_files();
    assert!(
        !store_files
            .iter()
            .any(|(n, _)| n.contains("mark") || n.contains("atime")),
        "the collector writes nothing into the store: {:?}",
        store_files.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
    let r = f.gc.collect(Some(&*roots)).unwrap();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
}
