//! The collector over the real core: real packs reclaimed, real reads after a reopen.
//!
//! Design: `docs/gc-core-integration.md`. Every test uses a private temp directory.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use cowfs_core::{Collector, Core, CoreRoots, Options as CoreOptions, SnapshotView};
use cowfs_gc::{Barrier, ExtraRoots, GcReport, Held, Options as GcOptions, RootsError, SkipReason};
use cowfs_store::BlockId;
use cowfs_vfs::{Vfs, ROOT_INO};

fn core_opts(background: bool) -> CoreOptions {
    CoreOptions {
        background,
        store: cowfs_store::Options {
            max_pack_size: 96 << 10,
            ..cowfs_store::Options::default()
        },
        file_flush_bytes: 32 << 10,
        flush_interval: Duration::from_millis(20),
        sync_interval: Duration::from_millis(50),
        ..CoreOptions::default()
    }
}

fn gc_opts() -> GcOptions {
    GcOptions {
        dead_ratio: 0.0,
        min_dead_bytes: 1,
        io_budget_bytes: 0,
        batch_bytes: 4096,
        ..GcOptions::default()
    }
}

fn body(n: usize, seed: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut h = seed.wrapping_mul(2654435761).wrapping_add(1);
    for _ in 0..n {
        h = h.wrapping_mul(1664525).wrapping_add(1013904223);
        out.push((h >> 16) as u8);
    }
    out
}

fn seeded(seed: u32) -> Vec<u8> {
    body(20_000 + 7_000 * (seed as usize % 4), seed)
}

fn put_file(v: &SnapshotView, name: &str, data: &[u8]) {
    let a = v.create(ROOT_INO, name.as_bytes(), 0o644).expect("create");
    assert_eq!(v.write(a.ino, 0, data).expect("write") as usize, data.len());
    v.forget(a.ino, 1);
}

fn read_file(v: &SnapshotView, name: &str) -> Result<Vec<u8>, cowfs_vfs::Error> {
    let a = v.lookup(ROOT_INO, name.as_bytes())?;
    let mut out = Vec::new();
    let mut err = None;
    while (out.len() as u64) < a.size {
        match v.read(a.ino, out.len() as u64, 1 << 20) {
            Ok(part) if part.is_empty() => break,
            Ok(part) => out.extend(part),
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    v.forget(a.ino, 1);
    err.map_or(Ok(out), Err)
}

type Files = Vec<(String, Vec<u8>)>;

struct Plan {
    keep: Files,
    dropped: Files,
}

/// Two snapshots whose files interleave in the packs, then one is removed. Some packs are mixed,
/// some are all dead, and a tail pushes the durable watermark past all of them.
fn build(core: &Core) -> Plan {
    core.create_snapshot("keep").expect("keep");
    core.create_snapshot("drop").expect("drop");
    let kv = core.snapshot_view("keep").expect("view");
    let dv = core.snapshot_view("drop").expect("view");
    let mut keep = Vec::new();
    let mut dropped = Vec::new();
    for i in 0..16u32 {
        let k = (format!("k{i:02}"), body(40_000, i));
        let d = (format!("d{i:02}"), body(40_000, 1000 + i));
        put_file(&kv, &k.0, &k.1);
        put_file(&dv, &d.0, &d.1);
        keep.push(k);
        dropped.push(d);
        core.sync().expect("sync");
    }
    for i in 16..26u32 {
        let d = (format!("d{i:02}"), body(40_000, 1000 + i));
        put_file(&dv, &d.0, &d.1);
        dropped.push(d);
        core.sync().expect("sync");
    }
    add_tail(core, &mut keep, "tail", 5);
    core.remove_snapshot("drop").expect("remove drop");
    core.sync().expect("sync");
    Plan { keep, dropped }
}

fn add_tail(core: &Core, keep: &mut Files, prefix: &str, n: u32) {
    let kv = core.snapshot_view("keep").expect("view");
    for i in 0..n {
        let k = (format!("{prefix}{i:02}"), body(40_000, 5000 + i));
        put_file(&kv, &k.0, &k.1);
        keep.push(k);
        core.sync().expect("sync");
    }
}

fn verify(core: &Core, files: &Files) {
    core.drop_caches();
    let v = core.snapshot_view("keep").expect("view");
    for (n, d) in files {
        let got = read_file(&v, n).unwrap_or_else(|e| panic!("{n} does not read: {e}"));
        assert!(got == *d, "{n} reads back different bytes");
    }
}

fn fsck_clean(core: &Core) {
    let r = core.fsck().expect("fsck");
    assert!(r.damage.is_empty(), "fsck found damage: {:?}", r.damage);
}

fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
            } else if let Ok(b) = std::fs::read(&p) {
                out.insert(p.strip_prefix(root).unwrap().display().to_string(), b);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn packs(dir: &Path) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir.join("store").join("packs"))
        .expect("packs dir")
        .flatten()
    {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with("pack-") && n.ends_with(".cpk") {
            out.insert(n, e.metadata().unwrap().len());
        }
    }
    out
}

fn bytes(p: &BTreeMap<String, u64>) -> u64 {
    p.values().sum()
}

fn clean(r: &GcReport) {
    assert!(r.errors.is_empty(), "cycle errors: {:?}", r.errors);
    assert!(r.roots_error.is_none(), "roots error: {:?}", r.roots_error);
}

fn reopen(dir: &Path) -> Core {
    Core::open(dir, core_opts(false)).expect("reopen")
}

#[test]
fn a_real_cycle_reclaims_dead_packs_and_survivors_read_after_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    let plan = build(&core);
    let before = packs(dir.path());
    let blocks_before = core.store().stats().blocks;
    assert!(
        before.len() >= 6,
        "the scenario made several packs: {}",
        before.len()
    );

    let c = core.collector(gc_opts()).unwrap();
    let r = c.collect().unwrap();
    clean(&r);
    assert!(r.barrier, "core offers a barrier");
    assert!(r.packs_unlinked > 0, "dead packs were reclaimed: {r:?}");
    assert!(r.freed_bytes > 0);

    let after = packs(dir.path());
    assert!(
        after.len() < before.len(),
        "pack files: {} -> {}",
        before.len(),
        after.len()
    );
    assert!(
        bytes(&after) < bytes(&before),
        "{} -> {}",
        bytes(&before),
        bytes(&after)
    );
    assert!(bytes(&after) + r.freed_bytes >= bytes(&before));
    assert!(core.store().stats().blocks < blocks_before);
    verify(&core, &plan.keep);

    let r2 = c.collect().unwrap();
    clean(&r2);
    assert_eq!(
        r2.packs_unlinked, 0,
        "a second cycle has nothing left: {r2:?}"
    );

    drop(c);
    core.close().expect("close with no collector alive");
    let core = reopen(dir.path());
    verify(&core, &plan.keep);
    fsck_clean(&core);
    core.close().unwrap();
}

#[test]
fn writing_a_dead_block_again_revives_it_before_the_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    let mut plan = build(&core);
    let kv = core.snapshot_view("keep").unwrap();
    for (n, d) in plan.dropped.iter().take(6) {
        let name = format!("again-{n}");
        put_file(&kv, &name, d);
        plan.keep.push((name, d.clone()));
    }
    core.sync().unwrap();
    drop(kv);
    let c = core.collector(gc_opts()).unwrap();
    clean(&c.collect().unwrap());
    verify(&core, &plan.keep);
    drop(c);
    core.close().unwrap();
    let core = reopen(dir.path());
    verify(&core, &plan.keep);
    fsck_clean(&core);
}

#[test]
fn a_dedup_that_is_only_queued_keeps_the_block_alive() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    let mut plan = build(&core);
    let kv = core.snapshot_view("keep").unwrap();
    // 40 KB is past `file_flush_bytes`, so the chunks are stored now and the chunk list is queued,
    // but nothing is committed: only the node in memory names the block.
    for (n, d) in plan.dropped.iter().take(6) {
        let name = format!("queued-{n}");
        put_file(&kv, &name, d);
        plan.keep.push((name, d.clone()));
    }
    assert!(
        !core.pinned_blocks().unwrap().is_empty(),
        "the queued chunk lists are pinned"
    );
    let dirty = body(20_000, 77);
    put_file(&kv, "dirty", &dirty);
    drop(kv);

    let c = core.collector(gc_opts()).unwrap();
    let r = c.collect().unwrap();
    clean(&r);
    assert!(r.pinned > 0, "{r:?}");
    core.sync().unwrap();
    plan.keep.push(("dirty".into(), dirty));
    verify(&core, &plan.keep);
    drop(c);
    core.close().unwrap();
    let core = reopen(dir.path());
    verify(&core, &plan.keep);
    fsck_clean(&core);
}

#[test]
fn an_open_unlinked_file_survives_and_is_reclaimed_after_the_last_close() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    core.create_snapshot("keep").unwrap();
    let kv = core.snapshot_view("keep").unwrap();
    let orphan = body(60_000, 9);
    put_file(&kv, "orphan", &orphan);
    core.sync().unwrap();
    let a = kv.lookup(ROOT_INO, b"orphan").unwrap();
    let h = kv.open(a.ino).unwrap();
    kv.unlink(ROOT_INO, b"orphan").unwrap();
    core.sync().unwrap();
    let mut keep = Vec::new();
    add_tail(&core, &mut keep, "tail", 5);
    assert!(
        !core.pinned_blocks().unwrap().is_empty(),
        "the orphan is pinned"
    );

    let c = core.collector(gc_opts()).unwrap();
    let r = c.collect().unwrap();
    clean(&r);
    assert!(r.pinned > 0);
    let got = kv
        .read(a.ino, 0, 60_000)
        .expect("the open file still reads");
    assert!(got == orphan);
    verify(&core, &keep);

    kv.release(h).unwrap();
    kv.forget(a.ino, 1 << 20);
    core.sync().unwrap();
    assert!(
        core.pinned_blocks().unwrap().is_empty(),
        "nothing pins it any more"
    );
    let r = c.collect().unwrap();
    clean(&r);
    assert!(
        r.packs_unlinked > 0,
        "the orphan's pack is reclaimed once it is closed: {r:?}"
    );
    verify(&core, &keep);
    drop(c);
    drop(kv);
    core.close().unwrap();
}

enum Mode {
    NoRoots,
    NoBarrier,
    BusyPins,
    BarrierError,
    TakeFails,
}

struct NoTake;
impl Barrier for NoTake {
    fn take(&mut self) -> Option<Box<dyn Held>> {
        None
    }
}

struct Wrapped {
    inner: CoreRoots,
    mode: Mode,
}

impl ExtraRoots for Wrapped {
    fn pinned_blocks(&self) -> Result<Vec<BlockId>, RootsError> {
        match self.mode {
            Mode::BusyPins => Err(RootsError::Busy),
            _ => self.inner.pinned_blocks(),
        }
    }
    fn reference_barrier(&self) -> Result<Option<Box<dyn Barrier>>, RootsError> {
        match self.mode {
            Mode::NoBarrier => Ok(None),
            Mode::BarrierError => Err(RootsError::Unavailable),
            Mode::TakeFails => Ok(Some(Box::new(NoTake))),
            _ => self.inner.reference_barrier(),
        }
    }
}

#[test]
fn no_barrier_busy_or_an_error_keeps_every_block() {
    for (label, mode) in [
        ("no roots at all", Mode::NoRoots),
        ("no barrier", Mode::NoBarrier),
        ("busy pins", Mode::BusyPins),
        ("barrier error", Mode::BarrierError),
        ("take fails", Mode::TakeFails),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), core_opts(false)).unwrap();
        let plan = build(&core);
        let before = packs(dir.path());
        let before_tree = tree(&dir.path().join("store").join("packs"));
        let c = core.collector(gc_opts()).unwrap();
        let wrapped = Wrapped {
            inner: c.roots().clone(),
            mode,
        };
        let r = match wrapped.mode {
            Mode::NoRoots => c.gc().collect(None),
            _ => c.gc().collect(Some(&wrapped)),
        }
        .unwrap();
        assert_eq!(r.freed_bytes, 0, "{label}: {r:?}");
        assert_eq!(r.packs_unlinked, 0, "{label}: {r:?}");
        let after = packs(dir.path());
        for name in before.keys() {
            assert!(after.contains_key(name), "{label}: {name} was removed");
        }
        if !matches!(wrapped.mode, Mode::TakeFails) {
            assert_eq!(
                before_tree,
                tree(&dir.path().join("store").join("packs")),
                "{label}: nothing is copied when nothing can be freed"
            );
        }
        verify(&core, &plan.keep);
        drop(wrapped);
        drop(c);
        core.close().unwrap();
        let core = reopen(dir.path());
        verify(&core, &plan.keep);
        fsck_clean(&core);
    }
}

#[test]
fn a_dry_run_changes_no_store_bytes_and_no_roots() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    let plan = build(&core);
    let c = core
        .collector(GcOptions {
            dry_run: true,
            ..gc_opts()
        })
        .unwrap();
    let store_before = tree(&dir.path().join("store"));
    let gc_before = tree(&dir.path().join("gc"));
    let snaps_before = core.list_snapshots().unwrap();
    let r = c.collect().unwrap();
    clean(&r);
    assert!(r.dry_run);
    assert!(
        r.candidates > 0,
        "a dry run reports what it would do: {r:?}"
    );
    assert_eq!(r.freed_bytes, 0);
    assert_eq!(r.packs_rewritten, 0);
    assert!(
        store_before == tree(&dir.path().join("store")),
        "the store changed"
    );
    assert!(
        gc_before == tree(&dir.path().join("gc")),
        "the collector state changed"
    );
    assert_eq!(snaps_before, core.list_snapshots().unwrap());
    verify(&core, &plan.keep);
    drop(c);
    core.close().unwrap();
}

#[test]
fn a_cancel_before_the_cycle_copies_nothing_and_resume_finishes_the_job() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    let plan = build(&core);
    let before = packs(dir.path());
    let c = core.collector(gc_opts()).unwrap();
    c.gc().cancel();
    let r = c.collect().unwrap();
    assert_eq!(r.packs_rewritten, 0, "{r:?}");
    assert_eq!(packs(dir.path()), before);
    verify(&core, &plan.keep);
    c.gc().resume();
    let r = c.collect().unwrap();
    clean(&r);
    assert!(r.packs_unlinked > 0);
    verify(&core, &plan.keep);
    drop(c);
    core.close().unwrap();
}

#[test]
fn a_cancel_mid_cycle_stops_new_work_and_leaves_the_store_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    let plan = build(&core);
    let c = Arc::new(core.collector(gc_opts()).unwrap());
    let weak: Weak<Collector> = Arc::downgrade(&c);
    c.gc().set_progress(move |_| {
        if let Some(c) = weak.upgrade() {
            c.gc().cancel();
        }
    });
    let r = c.collect().unwrap();
    assert!(
        r.skipped.iter().any(|s| s.reason == SkipReason::NotReached),
        "the cancel left candidates unstarted: {r:?}"
    );
    assert_eq!(
        r.packs_unlinked, r.packs_rewritten,
        "what was copied is finished: {r:?}"
    );
    verify(&core, &plan.keep);
    fsck_clean(&core);
    c.gc().resume();
    c.gc().set_progress(|_| {});
    let r2 = c.collect().unwrap();
    clean(&r2);
    verify(&core, &plan.keep);
    drop(c);
    core.close().unwrap();
    let core = reopen(dir.path());
    verify(&core, &plan.keep);
    fsck_clean(&core);
}

#[test]
fn the_collector_is_dropped_before_close_and_a_live_one_makes_close_refuse() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    let plan = build(&core);
    let c = core.collector(gc_opts()).unwrap();
    clean(&c.collect().unwrap());
    let err = core.close().expect_err("a live collector holds the mount");
    assert_eq!(err, cowfs_vfs::Error::Stale);
    // nothing was closed: the collector's mount still works
    let live = c.roots().core().clone();
    verify(&live, &plan.keep);
    let kv = live.snapshot_view("keep").unwrap();
    put_file(&kv, "after-refused-close", b"still writable");
    live.sync().unwrap();
    drop(kv);
    drop(live);
    drop(c);
    let core = reopen(dir.path());
    verify(&core, &plan.keep);
    let kv = core.snapshot_view("keep").unwrap();
    assert_eq!(
        read_file(&kv, "after-refused-close").unwrap(),
        b"still writable"
    );
    drop(kv);
    fsck_clean(&core);
    core.close().unwrap();
}

#[test]
fn collections_run_beside_writers_and_forks_without_deadlock_or_loss() {
    const RUN: Duration = Duration::from_secs(6);
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(true)).unwrap();
    core.create_snapshot("main").unwrap();
    let main = core.snapshot_view("main").unwrap();
    let expected: Arc<Mutex<BTreeMap<String, u32>>> = Arc::default();
    let stop = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let beat = Arc::new(AtomicUsize::new(0));
    let cycles = Arc::new(AtomicUsize::new(0));
    let unlinked = Arc::new(AtomicUsize::new(0));
    let c = core.collector(gc_opts()).unwrap();

    std::thread::scope(|s| {
        {
            let (finished, beat) = (finished.clone(), beat.clone());
            s.spawn(move || {
                let mut last = (beat.load(SeqCst), Instant::now());
                while !finished.load(SeqCst) {
                    std::thread::sleep(Duration::from_millis(250));
                    let now = beat.load(SeqCst);
                    if now != last.0 {
                        last = (now, Instant::now());
                    } else if last.1.elapsed() > Duration::from_secs(60) {
                        eprintln!("DEADLOCK: no operation completed for 60 s");
                        std::process::exit(101);
                    }
                }
            });
        }
        let mut handles = Vec::new();
        for t in 0..3u32 {
            let (main, expected, stop, beat, core) = (
                main.clone(),
                expected.clone(),
                stop.clone(),
                beat.clone(),
                core.clone(),
            );
            handles.push(s.spawn(move || {
                let mut i = 0u32;
                while !stop.load(SeqCst) {
                    let name = format!("w{t}_{}", i % 6);
                    // every fourth write repeats the content of one that was overwritten by now, so
                    // it deduplicates onto a block that is garbage or about to be reclaimed
                    let seed = if i % 4 == 3 {
                        (t * 10_000 + i).saturating_sub(8).max(t * 10_000)
                    } else {
                        t * 10_000 + i
                    };
                    if read_file(&main, &name).is_ok() {
                        main.unlink(ROOT_INO, name.as_bytes()).expect("unlink");
                        expected.lock().unwrap().remove(&name);
                    }
                    put_file(&main, &name, &seeded(seed));
                    expected.lock().unwrap().insert(name, seed);
                    if i.is_multiple_of(3) {
                        core.sync().expect("sync");
                    }
                    beat.fetch_add(1, SeqCst);
                    i += 1;
                }
            }));
        }
        {
            let (core, stop, beat) = (core.clone(), stop.clone(), beat.clone());
            handles.push(s.spawn(move || {
                let mut i = 0;
                while !stop.load(SeqCst) {
                    let n = format!("t{}", i % 3);
                    core.fork_snapshot("main", &n).expect("fork");
                    core.remove_snapshot(&n).expect("remove");
                    beat.fetch_add(1, SeqCst);
                    i += 1;
                }
            }));
        }
        {
            let (stop, beat, cycles, unlinked) =
                (stop.clone(), beat.clone(), cycles.clone(), unlinked.clone());
            let c = &c;
            handles.push(s.spawn(move || {
                while !stop.load(SeqCst) {
                    let r = c.collect().expect("collect");
                    if r.roots_error.is_none() {
                        assert!(r.errors.is_empty(), "cycle errors: {:?}", r.errors);
                    }
                    cycles.fetch_add(1, SeqCst);
                    unlinked.fetch_add(r.packs_unlinked as usize, SeqCst);
                    beat.fetch_add(1, SeqCst);
                }
            }));
        }
        std::thread::sleep(RUN);
        stop.store(true, SeqCst);
        let panicked = handles
            .into_iter()
            .map(|h| h.join().is_err())
            .filter(|p| *p)
            .count();
        finished.store(true, SeqCst);
        assert_eq!(panicked, 0, "{panicked} workers panicked");
    });
    eprintln!(
        "concurrent: {} cycles, {} packs unlinked, {} operations",
        cycles.load(SeqCst),
        unlinked.load(SeqCst),
        beat.load(SeqCst)
    );
    assert!(cycles.load(SeqCst) > 0);
    drop(c);
    drop(main);
    core.sync().unwrap();
    let want: Vec<(String, u32)> = expected
        .lock()
        .unwrap()
        .iter()
        .map(|(n, s)| (n.clone(), *s))
        .collect();
    assert!(!want.is_empty());
    let check = |core: &Core, label: &str| {
        core.drop_caches();
        let v = core.snapshot_view("main").unwrap();
        for (n, seed) in &want {
            let got = read_file(&v, n).unwrap_or_else(|e| panic!("{n} lost {label}: {e}"));
            assert!(got == seeded(*seed), "{n} changed {label}");
        }
    };
    check(&core, "before the reopen");
    fsck_clean(&core);
    core.close().unwrap();
    let core = reopen(dir.path());
    check(&core, "after the reopen");
    fsck_clean(&core);
}

#[test]
fn a_commit_between_the_freeze_listing_and_a_walks_the_listed_root() {
    // The mark phase lists durable `(root, id)` pairs, then walks each snapshot. If a writer commits
    // to a snapshot in between, `snapshot_by_id` returns the *new* root. Recording the listed root
    // while walking the new one claims a root the walk never descended, and a fork still on the
    // listed root is skipped as covered: its blocks sit in no live set and its pack is unlinked.
    // The hook below puts that commit in the exact window, deterministically, with no timing.
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();

    // Garbage to reclaim: two snapshots whose blocks interleave, one removed before the cycle.
    let plan = build(&core);

    // `src` is created before `fork`, so `src` has the smaller id and is listed first. `fork` shares
    // `src`'s root, so the bytes of `F` live only in that shared root until `src` moves off it.
    core.create_snapshot("src").unwrap();
    let sv = core.snapshot_view("src").unwrap();
    let x = body(40_000, 7001);
    put_file(&sv, "F", &x);
    core.sync().unwrap();
    core.fork_snapshot("src", "fork").unwrap();
    core.sync().unwrap();

    // A tail after the shared root pushes the durable watermark past the fork's blocks, so a pack
    // holding them is eligible once the mark says they are dead.
    let mut keep = plan.keep.clone();
    add_tail(&core, &mut keep, "tail-src", 6);

    let y = body(40_000, 7002);
    let y_in = y.clone();
    let c = core.collector(gc_opts()).unwrap();
    let sv2 = core.snapshot_view("src").unwrap();
    let ino = sv2.lookup(ROOT_INO, b"F").unwrap().ino;
    let wcore = core.clone();
    let wsv2 = sv2.clone();
    c.gc().set_between_list_and_walk(Box::new(move || {
        // Overwrite `F` in the smaller-id snapshot, moving it to a new root. The commit lands
        // after the freeze listing and before the walk.
        wsv2.write(ino, 0, &y_in).unwrap();
        wcore.sync().unwrap();
    }));

    let r = c.gc().collect(Some(c.roots())).unwrap();
    clean(&r);

    // The regression: the fork's `F` must still read back as the original bytes.
    core.drop_caches();
    let fv = core.snapshot_view("fork").unwrap();
    assert_eq!(
        read_file(&fv, "F").expect("fork's F reads"),
        x,
        "the fork's block survived the commit race: {r:?}"
    );
    assert_eq!(
        read_file(&sv2, "F").expect("src's F reads"),
        y,
        "src sees its own commit"
    );
    // And the cycle still reclaimed real garbage: a "never GC" fix would hide the loss.
    assert!(
        r.packs_unlinked >= 1 && r.freed_bytes > 0,
        "the race did not disable reclaim: {r:?}"
    );

    drop(fv);
    drop(sv2);
    drop(sv);
    drop(c);
    core.close().unwrap();

    let core = reopen(dir.path());
    let fv = core.snapshot_view("fork").unwrap();
    assert_eq!(
        read_file(&fv, "F").expect("fork's F reads after reopen"),
        x,
        "the fork's block survived a reopen"
    );
    drop(fv);
    verify(&core, &keep);
    fsck_clean(&core);
    core.close().unwrap();
}

/// B2: a `mark.bin` written by a collector *before* the walked-root fix can pair a listed root with
/// a different, newly committed root's blocks. Loading it would let a cycle skip a real root's walk
/// and free the blocks only that root referenced. The fix is a marks-format magic bump, so the whole
/// old file is ignored and every root is walked in full.
///
/// This drives a real store: `src` and its fork share a root; `F` is overwritten in `src`, so `src`
/// moves to a new root while the fork stays on the old one. A hand-written old-format cache names the
/// fork's root but omits the block its `F` chunk lives in - exactly the wrong association the old
/// collector produced. The corrected collector must reject the file, keep the fork readable after a
/// reopen, and still free real dead packs.
#[test]
fn an_old_format_marks_file_from_before_the_walk_fix_is_not_reused() {
    use std::fs;

    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    // Garbage for the reclaim to have something real to free, and a tail so the watermark passes it.
    let plan = build(&core);
    core.create_snapshot("src").unwrap();
    let sv = core.snapshot_view("src").unwrap();
    let x = body(40_000, 7001);
    let y = body(40_000, 7002);
    put_file(&sv, "F", &x);
    core.sync().unwrap();
    drop(sv);
    core.fork_snapshot("src", "fork").unwrap();
    core.sync().unwrap();

    // The fork's current root, the key the old collector would have recorded.
    let fork_root = core
        .list_snapshots()
        .unwrap()
        .into_iter()
        .find(|s| s.name == "fork")
        .expect("fork listed")
        .root;
    let fork_key = *fork_root.as_bytes();

    // Move `src` to a new root by overwriting F, which is what makes the fork's key stale-but-live.
    let sv = core.snapshot_view("src").unwrap();
    let a = sv.lookup(ROOT_INO, b"F").unwrap();
    assert_eq!(sv.write(a.ino, 0, &y).unwrap() as usize, y.len());
    sv.forget(a.ino, 1);
    core.sync().unwrap();
    drop(sv);
    let mut keep = plan.keep.clone();
    add_tail(&core, &mut keep, "tail-src", 6);

    // Hand-build the old-format file: the fork's root named, with a single unrelated block that is
    // not its F chunk. A collector that trusted it would skip the fork's walk and free F's block.
    let decoy = core
        .pinned_blocks()
        .unwrap()
        .into_iter()
        .next()
        .unwrap_or_else(|| BlockId::of(b"decoy"));
    let mut poison = Vec::new();
    poison.extend_from_slice(b"COWMARK1");
    poison.extend_from_slice(&1u64.to_le_bytes()); // one root
    poison.extend_from_slice(&1u64.to_le_bytes()); // one block
    poison.extend_from_slice(&fork_key);
    poison.extend_from_slice(decoy.as_bytes());
    fs::create_dir_all(dir.path().join("gc")).unwrap();
    fs::write(dir.path().join("gc").join("mark.bin"), &poison).unwrap();

    let c = core.collector(gc_opts()).unwrap();
    let r = c.collect().unwrap();
    clean(&r);
    assert!(
        r.packs_unlinked >= 1 && r.freed_bytes > 0,
        "the cycle still reclaimed real dead packs: {r:?}"
    );

    drop(c);
    core.close().unwrap();

    let core = reopen(dir.path());
    let fv = core.snapshot_view("fork").unwrap();
    assert_eq!(
        read_file(&fv, "F").expect("fork's F reads after reopen"),
        x,
        "the old-format cache was rejected, so the fork's block survived"
    );
    drop(fv);
    verify(&core, &keep);
    fsck_clean(&core);
    core.close().unwrap();
}
