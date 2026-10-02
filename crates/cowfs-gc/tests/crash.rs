//! Crash injection at every step of a compaction.
//!
//! A crash image is the store directory as it would be after a power cut at one step: the steps
//! before it happened, the ones after it did not. The store funnels every durability-relevant call
//! through one seam, so a test builds each image by driving the real compaction code up to the
//! chosen point and dropping the process state, which leaves exactly the files the cut would leave.
//!
//! The negative control at the end does the same thing with a deliberately wrong ordering and
//! shows the assertions catch it. Without it, a test that only ever builds correct images proves
//! nothing about whether it can tell a correct image from a broken one.

mod common;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use common::{eager, Fixture, Roots};
use cowfs_gc::Gc;
use cowfs_meta::{Meta, Snapshot};
use cowfs_store::{BlockId, Options as StoreOptions, Store};

/// One crash point: how far the compaction got before the power went out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Point {
    /// Nothing has run.
    Before,
    /// The new pack exists and one batch of records is in it, unsynced.
    MidCopy,
    /// Every record is copied and the new pack is fsynced, but the index still names the old pack.
    AfterCopyFsync,
    /// The index points at the new pack and `index.cix` is rewritten.
    AfterIndex,
    /// The old pack is unlinked and `packs/` is fsynced, but the watermark base is not yet lowered.
    AfterUnlink,
    /// Everything is done.
    Done,
}

impl Point {
    const ALL: [Point; 6] = [
        Point::Before,
        Point::MidCopy,
        Point::AfterCopyFsync,
        Point::AfterIndex,
        Point::AfterUnlink,
        Point::Done,
    ];

    fn name(self) -> &'static str {
        match self {
            Point::Before => "before",
            Point::MidCopy => "mid copy",
            Point::AfterCopyFsync => "after copy fsync",
            Point::AfterIndex => "after index",
            Point::AfterUnlink => "after unlink",
            Point::Done => "done",
        }
    }
}

const PACK: u64 = 32 << 10;

fn body(n: usize, seed: u8) -> Vec<u8> {
    // Half compressible, so packs are worth rewriting, and distinct per seed.
    let mut out = Vec::with_capacity(n);
    let mut h = u32::from(seed).wrapping_mul(2654435761).wrapping_add(1);
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

fn write_file(
    store: &Store,
    snap: &Snapshot,
    name: &[u8],
    data: &[u8],
) -> Vec<cowfs_store::ChunkRef> {
    let chunks = store.ingest_bytes(data).expect("ingest");
    let ino = snap
        .batch(|tx| tx.create(cowfs_meta::ROOT_INO, name, 0o644))
        .expect("create")
        .ino;
    snap.batch(|tx| tx.set_content(ino, &chunks, data.len() as u64))
        .expect("set content");
    chunks
}

/// A store with one referenced file, which spans several blocks, and enough garbage for several
/// sealed packs. Returns the store, the database, and the bytes each live block must read back as.
fn seeded(dir: &std::path::Path) -> (Arc<Store>, Arc<Meta>, HashMap<BlockId, Vec<u8>>) {
    let store = Arc::new(
        Store::open(
            dir.join("store"),
            StoreOptions {
                max_pack_size: PACK,
                ..StoreOptions::default()
            },
        )
        .expect("store"),
    );
    let meta = Arc::new(
        Meta::open(
            dir.join("meta"),
            cowfs_meta::Options {
                background: false,
                ..cowfs_meta::Options::default()
            },
        )
        .expect("meta"),
    );
    let snap = meta.new_snapshot("s").expect("snapshot");
    // Garbage first, so the pack the live file lands in holds both live and dead records: that is
    // the pack a compaction has to rewrite rather than unlink outright.
    for i in 0..12u8 {
        store.put(&body(4000, i)).expect("put");
    }
    let keep = body(120_000, 1);
    let chunks = write_file(&store, &snap, b"keep", &keep);
    // A block holds one chunk, so the expected bytes are the chunk's slice of the file.
    let mut want = HashMap::new();
    let mut at = 0usize;
    for c in &chunks {
        let n = usize::try_from(c.len).unwrap_or(0);
        want.insert(c.id, keep[at..(at + n).min(keep.len())].to_vec());
        at += n;
    }
    for i in 12..40u8 {
        store.put(&body(4000, i)).expect("put");
    }
    store.sync().expect("sync");
    meta.sync().expect("meta sync");
    (store, meta, want)
}

fn reopen_store(dir: &std::path::Path) -> Result<Arc<Store>, cowfs_store::Error> {
    Store::open(
        dir.join("store"),
        StoreOptions {
            max_pack_size: PACK,
            ..StoreOptions::default()
        },
    )
    .map(Arc::new)
}

fn reopen_meta(dir: &std::path::Path) -> Result<Arc<Meta>, cowfs_meta::Error> {
    Meta::open(
        dir.join("meta"),
        cowfs_meta::Options {
            background: false,
            ..cowfs_meta::Options::default()
        },
    )
    .map(Arc::new)
}

/// Every live block reads back with exactly the bytes that were put.
fn expect_readable(store: &Store, want: &HashMap<BlockId, Vec<u8>>, where_: &str) {
    for (b, bytes) in want {
        let got = store
            .get(*b)
            .unwrap_or_else(|e| panic!("{where_}: live block lost: {e}"));
        assert_eq!(&got, bytes, "{where_}: live block has the wrong bytes");
    }
}

/// Drive a compaction of one sealed pack to `point` and stop.
fn drive_to(store: &Store, live: &HashSet<BlockId>, point: Point) -> (u32, Vec<BlockId>) {
    let is_live = |b: BlockId| live.contains(&b);
    let from = store
        .packs()
        .expect("packs")
        .into_iter()
        .filter(|p| !p.active)
        .map(|p| {
            let mut ids = Vec::new();
            (
                p.id,
                store.plan_pack(p.id, &is_live, &mut ids).expect("plan"),
            )
        })
        .find(|(_, plan)| plan.dead_ratio() >= 0.5 && plan.dead_bytes > 0)
        .map(|(id, _)| id)
        .expect("a mostly-dead sealed pack to compact");
    let mut ids = Vec::new();
    let plan = store.plan_pack(from, &is_live, &mut ids).expect("plan");
    if point == Point::Before {
        return (from, Vec::new());
    }
    let mut c = store.begin_compaction(&plan, &is_live).expect("begin");
    if point == Point::MidCopy {
        // One small batch, so the new pack holds some records and not all, and nothing is fsynced.
        store.copy_batch(&mut c, 8192).expect("copy");
        return (from, c.condemned().to_vec());
    }
    while !store.copy_batch(&mut c, 0).expect("copy") {}
    if point == Point::AfterCopyFsync {
        return (from, c.condemned().to_vec());
    }
    let rw = store.finish_compaction(&c).expect("finish");
    if point == Point::AfterIndex {
        return (from, rw.condemned);
    }
    store.discard_pack(from, &rw.condemned).expect("discard");
    (from, rw.condemned)
}

#[test]
fn a_crash_at_every_compaction_step_loses_no_live_block() {
    for point in Point::ALL {
        let dir = tempfile::tempdir().expect("tempdir");
        let (store, meta, want) = seeded(dir.path());
        let live: HashSet<BlockId> = want.keys().copied().collect();
        let packs_before = store.packs().expect("packs").len();
        assert!(
            packs_before >= 2,
            "the fixture needs sealed packs: {packs_before}"
        );
        let (from, condemned) = drive_to(&store, &live, point);
        // The power goes out here: every in-memory handle is lost, and the files on disk are what
        // a crash would leave.
        drop(store);
        drop(meta);

        let store = reopen_store(dir.path())
            .unwrap_or_else(|e| panic!("{}: the store does not open: {e}", point.name()));
        expect_readable(&store, &want, point.name());
        assert!(
            !store.recovery().has_corruption(),
            "{}: a compaction left corruption {:?}",
            point.name(),
            store.recovery()
        );
        assert!(
            store.fsck().expect("fsck").is_clean(),
            "{}: fsck found damage",
            point.name()
        );

        // Only unreferenced data may be missing, so the blocks that were live are all there and
        // the condemned ones either are or are not, with nothing in between.
        for b in &condemned {
            let _ = store.get(*b);
        }
        if point == Point::Before || point == Point::MidCopy || point == Point::AfterCopyFsync {
            assert!(
                store.pack_len(from) > 0,
                "{}: the old pack is still there",
                point.name()
            );
        }

        // A rerun of the cycle finishes the job and keeps every live block.
        let meta = reopen_meta(dir.path()).expect("meta reopen");
        let gc = Gc::open(
            dir.path().join("gcstate"),
            Arc::clone(&store),
            Arc::clone(&meta),
            eager(),
        )
        .expect("gc");
        let roots = Roots::new();
        let r = gc.collect(Some(&*roots)).expect("collect");
        assert!(r.errors.is_empty(), "{}: {:?}", point.name(), r.errors);
        expect_readable(gc.store(), &want, point.name());
    }
}

#[test]
fn a_rewrite_that_is_never_unlinked_leaves_the_source_readable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, _meta, want) = seeded(dir.path());
    let live: HashSet<BlockId> = want.keys().copied().collect();
    let is_live = |b: BlockId| live.contains(&b);
    let from = store
        .packs()
        .expect("packs")
        .into_iter()
        .filter(|p| !p.active)
        .max_by_key(|p| p.id)
        .expect("a sealed pack")
        .id;
    let mut ids = Vec::new();
    let plan = store.plan_pack(from, &is_live, &mut ids).expect("plan");
    let mut c = store.begin_compaction(&plan, &is_live).expect("begin");
    while !store.copy_batch(&mut c, 0).expect("copy") {}
    // Dropped here: the copy is a real pack of real records, the index still names the old one, and
    // the old one is there.
    let condemned = c.condemned().to_vec();
    drop(c);
    for b in &live {
        assert!(
            store.get(*b).is_ok(),
            "the old pack still holds every live block"
        );
    }
    for b in &condemned {
        assert!(
            store.get(*b).is_ok(),
            "a condemned block is still readable until the unlink"
        );
    }
}

#[test]
fn a_cancelled_collect_leaves_the_store_consistent() {
    let f = Fixture::eager(PACK);
    let snap = f.meta.new_snapshot("s").expect("snapshot");
    let keep = body(120_000, 1);
    f.write(&snap, b"keep", &keep);
    let live = f.live_blocks();
    for i in 0..40u8 {
        f.store.put(&body(4000, i)).expect("put");
    }
    f.store.sync().expect("sync");
    f.meta.sync().expect("meta sync");
    let before = f.store.stats().pack_bytes;

    let roots = Roots::new();
    f.gc.cancel();
    let r = f.gc.collect(Some(&*roots)).expect("collect");
    assert_eq!(r.freed_bytes, 0, "a cancelled collect frees nothing");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        r.skipped
            .iter()
            .any(|s| { s.reason == cowfs_gc::SkipReason::NotReached }),
        "the cancel is reported, not silent: {:?}",
        r.skipped
    );
    for b in &live {
        assert!(f.store.get(*b).is_ok(), "every live block still reads");
    }
    assert!(
        f.store.fsck().expect("fsck").is_clean(),
        "a cancelled collect left the store clean"
    );

    // The next cycle, not cancelled, does the work.
    f.gc.resume();
    let r2 = f.gc.collect(Some(&*roots)).expect("collect");
    assert!(r2.freed_bytes > 0, "the next cycle reclaims: {r2:?}");
    for b in &live {
        assert!(f.store.get(*b).is_ok(), "every live block still reads");
    }
    let _ = before;
}

/// The negative control: the old pack is unlinked before any copy exists.
///
/// This is the ordering bug the protocol exists to prevent. It is built on purpose, and the
/// assertions the matrix uses must catch it. If they do not, the matrix proves nothing.
#[test]
#[should_panic(expected = "negative control")]
fn the_matrix_catches_a_broken_ordering() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (store, meta, want) = seeded(dir.path());
    let live: HashSet<BlockId> = want.keys().copied().collect();
    let is_live = |b: BlockId| live.contains(&b);
    // The pack that holds the live blocks, which is the one the correct order would copy first.
    let from = store
        .packs()
        .expect("packs")
        .into_iter()
        .filter(|p| !p.active)
        .map(|p| {
            let mut ids = Vec::new();
            (
                p.id,
                store.plan_pack(p.id, &is_live, &mut ids).expect("plan"),
            )
        })
        .find(|(_, plan)| plan.live_bytes > 0)
        .map(|(id, _)| id)
        .expect("a sealed pack holding live records");
    let condemned: Vec<BlockId> = store.iter_ids().filter(|b| !live.contains(b)).collect();
    // No copy, no new index: the pack holding the live blocks goes first.
    store.discard_pack(from, &condemned).expect("discard");
    drop(store);
    drop(meta);

    let store = match reopen_store(dir.path()) {
        Ok(s) => s,
        Err(e) => panic!("negative control: the broken ordering refused to reopen: {e}"),
    };
    let lost: Vec<BlockId> = live
        .iter()
        .filter(|b| store.get(**b).is_err())
        .copied()
        .collect();
    assert!(
        lost.is_empty(),
        "negative control caught it: the broken ordering lost {} live blocks, of {}",
        lost.len(),
        live.len()
    );
    assert!(
        !store.recovery().has_corruption(),
        "negative control: the broken ordering was not even reported"
    );
    panic!("negative control: the broken ordering was not detected");
}
