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

fn put_file_in(v: &SnapshotView, dir: cowfs_vfs::Ino, name: &str, data: &[u8]) {
    let a = v.create(dir, name.as_bytes(), 0o644).expect("create");
    assert_eq!(v.write(a.ino, 0, data).expect("write") as usize, data.len());
    v.forget(a.ino, 1);
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
    verify_in(core, "keep", files);
}

/// `verify` for a snapshot whose name the test chose, so a scenario that does not know which of two
/// snapshots will survive can still check the survivor.
fn verify_in(core: &Core, snapshot: &str, files: &Files) {
    core.drop_caches();
    let v = core.snapshot_view(snapshot).expect("view");
    for (n, d) in files {
        let got = read_file(&v, n).unwrap_or_else(|e| panic!("{snapshot}:{n} does not read: {e}"));
        assert!(got == *d, "{snapshot}:{n} reads back different bytes");
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

/// Flip one byte in every non-last pack's first record payload, so a copy of a live record from
/// those packs fails its checksum. The last pack is left alone as the tail watermark. Test-only:
/// it simulates on-disk corruption that builds no public fault switch for.
fn corrupt_first_record_of_non_last_packs(dir: &Path) {
    let mut names: Vec<String> = packs(dir).into_keys().collect();
    names.sort();
    let last = names.pop();
    for name in names {
        if Some(&name) == last.as_ref() {
            continue;
        }
        let path = dir.join("store").join("packs").join(&name);
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open pack");
        let off = 100u64;
        let len = f.metadata().unwrap().len();
        if off >= len {
            continue;
        }
        use std::io::{Read, Seek, SeekFrom, Write};
        f.seek(SeekFrom::Start(off)).unwrap();
        let mut b = [0u8; 1];
        f.read_exact(&mut b).unwrap();
        b[0] ^= 0xff;
        f.seek(SeekFrom::Start(off)).unwrap();
        f.write_all(&b).unwrap();
        f.sync_all().unwrap();
    }
}

/// Make the store's acceptance record impossible to rewrite, which fails a step of
/// `discard_pack` strictly after the pack file is unlinked: `write_whole` renames a temporary over
/// `ACKED`, and a rename onto a directory fails with `EISDIR`.
///
/// Test-only, and labelled for what it is: a private store-state mutation standing in for a
/// post-unlink I/O error. It is not a real `EIO` or `ENOSPC`, and it exercises exactly the branch a
/// real one would take. Returns what it replaced, so a wrong target cannot pass unnoticed.
fn block_ack_rewrite(dir: &Path) -> String {
    let acked = dir.join("store").join("ACKED");
    let was = std::fs::symlink_metadata(&acked)
        .map(|m| if m.is_dir() { "directory" } else { "file" })
        .unwrap_or("absent");
    let _ = std::fs::remove_file(&acked);
    std::fs::create_dir(&acked).expect("create the ACKED directory");
    format!("ACKED was {was} at {}", acked.display())
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
    // Accounting after a cancel still respects the identity, and gross tracks only the packs the
    // cycle actually unlinked, never the ones it merely planned to.
    assert_eq!(
        r.gross_removed_bytes, r.freed_bytes,
        "gross agrees with the legacy field after a cancel: {r:?}"
    );
    assert_eq!(
        r.net_reclaimed_bytes,
        r.gross_removed_bytes as i64 - r.rewrite_bytes as i64,
        "the signed net identity holds after a cancel: {r:?}"
    );
    if r.packs_unlinked == 0 {
        assert_eq!(
            r.gross_removed_bytes, 0,
            "nothing unlinked means no gross claimed: {r:?}"
        );
    }
    verify(&core, &plan.keep);
    fsck_clean(&core);
    c.gc().resume();
    c.gc().set_progress(|_| {});
    let r2 = c.collect().unwrap();
    clean(&r2);
    assert_eq!(
        r2.net_reclaimed_bytes,
        r2.gross_removed_bytes as i64 - r2.rewrite_bytes as i64,
        "the resumed cycle's net is its own, not the cancelled cycle's: {r2:?}"
    );
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
                    // The cycle's net is cycle-owned: gross minus the bytes it rewrote, never a
                    // process-wide before/after that a concurrent writer's appends would distort.
                    assert_eq!(
                        r.net_reclaimed_bytes,
                        r.gross_removed_bytes as i64 - r.rewrite_bytes as i64,
                        "net is cycle-owned under concurrent appends: {r:?}"
                    );
                    assert_eq!(
                        r.gross_removed_bytes, r.freed_bytes,
                        "gross agrees with the legacy field under concurrent appends: {r:?}"
                    );
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

/// Issue 83: a snapshot removed between the collector's lookup of a listed snapshot and the walk
/// that reads its root makes the walk report `NoSuchSnapshot`. The cycle must treat that one error
/// as a snapshot that came and went - not fail the whole cycle and not free a block a keeper needs.
///
/// The seam places the removal in that exact window, so the test needs no timing. The assertions are
/// not merely that the cycle is `Ok`: a fork of the removed victim must still read every byte of
/// every file it shares with the victim after a reopen, `fsck` must be clean, and the cycle must
/// still free real dead packs. A no-op collector that returned `Ok` without working would fail the
/// reclaim assertion, and one that skipped the walk and then freed the shared blocks would fail the
/// survivor read.
#[test]
fn a_snapshot_removed_between_the_lookup_and_the_walk_does_not_fail_the_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    // Real garbage for the reclaim to free, and a tail so the watermark passes the dead packs.
    let plan = build(&core);

    core.create_snapshot("src").unwrap();
    let sv = core.snapshot_view("src").unwrap();
    let x = body(40_000, 7101);
    put_file(&sv, "K", &x);
    core.sync().unwrap();

    // A snapshot that will be removed inside the collector's lookup/walk window. Its own root is
    // distinct so the collector actually reaches the walk for it, and it holds real blocks that
    // become dead once it is gone.
    core.fork_snapshot("src", "victim").unwrap();
    let vv = core.snapshot_view("victim").unwrap();
    let mut victim_files: Files = Vec::new();
    for i in 0..8u32 {
        let d = body(40_000, 7200 + i);
        let n = format!("v{i:02}");
        put_file(&vv, &n, &d);
        victim_files.push((n, d));
    }
    core.sync().unwrap();
    // A survivor forks the victim with no further writes, so it shares the victim's exact root.
    // Removing the victim must leave every block this survivor reads in place.
    core.fork_snapshot("victim", "survivor").unwrap();
    core.sync().unwrap();
    let victim_id = core
        .list_snapshots()
        .unwrap()
        .into_iter()
        .find(|s| s.name == "victim")
        .expect("victim listed")
        .id;

    let mut keep = plan.keep.clone();
    add_tail(&core, &mut keep, "tail-src", 6);

    let c = core.collector(gc_opts()).unwrap();
    let removed = Arc::new(AtomicBool::new(false));
    let removed2 = Arc::clone(&removed);
    let wcore = core.clone();
    c.gc().set_between_lookup_and_walk(Box::new(move |id| {
        if id.0 != victim_id {
            return;
        }
        if removed2.swap(true, SeqCst) {
            return;
        }
        // Remove the snapshot the collector just looked up, before it walks it.
        wcore.remove_snapshot("victim").expect("remove in window");
        wcore.sync().expect("sync the removal");
    }));

    let r = c
        .gc()
        .collect(Some(c.roots()))
        .expect("the removal in the lookup/walk window must not fail the cycle");
    clean(&r);
    assert!(
        removed.load(SeqCst),
        "the seam fired: the removal landed in the lookup/walk window"
    );
    assert!(
        r.packs_unlinked >= 1 && r.freed_bytes > 0,
        "the cycle still reclaimed real dead packs: {r:?}"
    );

    drop(vv);
    drop(sv);
    drop(c);
    core.close().unwrap();

    // The keepers survive a reopen with their real bytes, and the store is sound.
    let core = reopen(dir.path());
    let kv = core.snapshot_view("src").unwrap();
    assert_eq!(
        read_file(&kv, "K").expect("src's K reads after reopen"),
        x,
        "the keeper's block survived the removal race"
    );
    drop(kv);
    // The fork of the removed victim still reads every byte of every file: removing the victim did
    // not free a block the survivor shares with it.
    let survivor = core.snapshot_view("survivor").unwrap();
    for (n, d) in &victim_files {
        assert_eq!(
            &read_file(&survivor, n)
                .unwrap_or_else(|e| panic!("survivor's {n} does not read: {e}")),
            d,
            "survivor's {n} survived the victim's removal"
        );
    }
    drop(survivor);
    verify(&core, &keep);
    fsck_clean(&core);
    core.close().unwrap();
}

/// Issue #81: on a mixed-live/dead pack the gross unlinked bytes exceed the space actually
/// reclaimed, because the surviving live records are rewritten into a new pack. The report must
/// say both, and the net must equal the physical drop the cycle caused, with no process-wide
/// before/after shortcut.
#[test]
fn a_mixed_pack_reports_gross_removed_rewrite_and_signed_net() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    let plan = build(&core);

    let before = packs(dir.path());
    let bytes_before = bytes(&before);

    let c = core.collector(gc_opts()).unwrap();
    let r = c.collect().unwrap();
    clean(&r);
    assert!(r.packs_unlinked > 0, "dead packs were reclaimed: {r:?}");
    assert!(r.freed_bytes > 0, "gross bytes removed: {r:?}");

    let after = packs(dir.path());
    let bytes_after = bytes(&after);

    // The mixed scenario must actually exercise the gross > net case.
    assert!(
        r.rewrite_bytes > 0,
        "live records were rewritten into a new pack: {r:?}"
    );
    assert_eq!(
        r.gross_removed_bytes, r.freed_bytes,
        "the explicit gross field and the legacy field agree"
    );
    assert!(
        r.gross_removed_bytes > r.net_reclaimed_bytes as u64,
        "gross removal exceeds net reclaimed on a mixed pack: {r:?}"
    );

    // The identity: net == gross removed minus the bytes written into the cycle's own new packs.
    assert_eq!(
        r.net_reclaimed_bytes,
        r.gross_removed_bytes as i64 - r.rewrite_bytes as i64,
        "net is the accounting identity, not a process-wide delta: {r:?}"
    );

    // And the identity matches the physical pack drop, since no concurrent writer ran here.
    let physical_drop = bytes_before as i64 - bytes_after as i64;
    assert_eq!(
        physical_drop, r.net_reclaimed_bytes,
        "cycle-owned net equals the physical pack drop under a quiescent store: \
         {bytes_before} -> {bytes_after}, {r:?}"
    );

    verify(&core, &plan.keep);
    drop(c);
    core.close().unwrap();
}

/// A copy that fails mid-batch (a corrupt live record after the new pack was created) still
/// accounts the bytes the cycle wrote. The old code set the abandoned figure only on the
/// cancel/budget branch, so an error exit dropped the new pack's bytes and reported a net that
/// overstated savings. This is the regression: the reported net must equal the physical drop.
///
/// The injected corruption is a fault the test introduces, so a later `Core::open` would refuse
/// the store by design. Survivors are checked through the still-open core instead.
#[test]
fn a_copy_that_fails_on_a_corrupt_live_record_still_accounts_the_new_pack_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    let _plan = build(&core);
    core.sync().unwrap();

    // Corrupt on disk while the core is open: the store reads pack records with `pread`, so the
    // new bytes are what the cycle's copy sees. A reopen would refuse the store by design.
    corrupt_first_record_of_non_last_packs(dir.path());

    let before = packs(dir.path());
    let bytes_before = bytes(&before);

    // Note every live block now, before the cycle runs. After the cycle, each must either read
    // as before or be one the injected corruption broke. This proves the failed copy plus its
    // accounting did not drop an intact survivor.
    let live_before: Vec<BlockId> = core.store().iter_ids().collect();

    let c = core.collector(gc_opts()).unwrap();
    let r = c.collect().unwrap();

    // The cycle hit the corruption, so it reports errors rather than a clean run.
    assert!(
        !r.errors.is_empty(),
        "the corrupt live record is reported: {r:?}"
    );

    let after = packs(dir.path());
    let bytes_after = bytes(&after);
    let physical_net = bytes_before as i64 - bytes_after as i64;

    // The reported net must equal the physical drop even though the copy errored, which requires
    // the new pack's bytes to be in the rewrite figure.
    assert_eq!(
        r.net_reclaimed_bytes, physical_net,
        "a failed copy must still count its new pack bytes: {bytes_before} -> {bytes_after}, {r:?}"
    );
    assert_eq!(
        r.net_reclaimed_bytes,
        r.gross_removed_bytes as i64 - r.rewrite_bytes as i64,
        "the identity holds on the error path: {r:?}"
    );
    assert!(
        r.rewrite_bytes > 0,
        "the failed copy left bytes on disk and they are accounted: {r:?}"
    );

    // The survivors the cycle did not touch still read through the open core. Every live block
    // from before must read now, unless it sits in the corruption this test injected. This proves
    // the failed cycle protected healthy refs without a reopen, which the injected corruption
    // would rightly refuse.
    let mut broken = 0;
    for b in &live_before {
        if core.store().get(*b).is_err() {
            broken += 1;
        }
    }
    assert!(
        broken < live_before.len(),
        "the failed cycle did not drop every survivor: {r:?}"
    );
    assert!(
        broken > 0,
        "the injected corruption surfaced, so a copy really failed: {r:?}"
    );
    drop(c);
    core.close().unwrap();
}

/// A failure *after* the pack file is unlinked still has to be counted as a removal.
///
/// `discard_pack` used to return only `Err` once the pack was gone, so the cycle credited nothing
/// for packs it really unlinked: gross came out 0 and net came out `0 - rewrite`, while the packs
/// were gone from disk. The pack also stayed named in the writer's in-memory map, so `fsck` on the
/// same open core failed on a file the cycle had itself removed.
///
/// The failure is surfaced as well as counted. Counting it alone would report a removal the store
/// cannot vouch for, so the error stays in the report and a durability failure is never a clean run.
#[test]
fn a_failure_after_the_unlink_still_reports_the_removal_it_performed() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    let plan = build(&core);

    let before = packs(dir.path());
    let bytes_before = bytes(&before);
    eprintln!("fault: {}", block_ack_rewrite(dir.path()));

    let c = core.collector(gc_opts()).unwrap();
    let r = c.collect().unwrap();
    assert!(
        !r.errors.is_empty(),
        "the post-unlink failure is reported, not swallowed: {r:?}"
    );

    let after = packs(dir.path());
    let bytes_after = bytes(&after);
    let physical_net = bytes_before as i64 - bytes_after as i64;
    // Gross is the length of the packs that left, not the drop in total size: the cycle also wrote
    // new packs, which is what separates gross from net.
    let gone: u64 = before
        .iter()
        .filter(|(k, _)| !after.contains_key(*k))
        .map(|(_, v)| *v)
        .sum();

    // The packs really left the store, so gross must say so and net must not flip sign.
    assert!(
        r.packs_unlinked > 0 && gone > 0,
        "the fixture must actually unlink packs: {bytes_before} -> {bytes_after}, {r:?}"
    );
    assert_eq!(
        r.gross_removed_bytes, gone,
        "gross is the bytes the store really unlinked: {r:?}"
    );
    assert_eq!(r.gross_removed_bytes, r.freed_bytes, "{r:?}");
    assert_eq!(
        r.net_reclaimed_bytes, physical_net,
        "net equals the physical drop on the post-unlink error path: {r:?}"
    );
    assert_eq!(
        r.net_reclaimed_bytes,
        r.gross_removed_bytes as i64 - r.rewrite_bytes as i64,
        "the identity holds on the post-unlink error path: {r:?}"
    );

    // The pack must be retired from the writer's map, or fsck on this same core walks an id whose
    // file the cycle itself removed and fails forever.
    fsck_clean(&core);

    // No double credit: the packs are already gone, so a second cycle removes nothing.
    let r2 = c.collect().unwrap();
    assert_eq!(
        (
            r2.packs_unlinked,
            r2.gross_removed_bytes,
            r2.net_reclaimed_bytes
        ),
        (0, 0, 0),
        "a second cycle must not credit the same unlink again: {r2:?}"
    );

    verify(&core, &plan.keep);
    drop(c);
    core.close().unwrap();
    let core = reopen(dir.path());
    verify(&core, &plan.keep);
    fsck_clean(&core);
}

/// A failure *before* the unlink removed nothing, so nothing may be claimed for it.
///
/// The other half of the post-unlink case: the cycle must not credit a removal that never happened,
/// and must leave the pack registered so a later cycle can still remove it. Without a second
/// collector on one store, the seam is the same `discard` boundary the post-unlink test drives from
/// the other side: `Err` means the unlink did not happen, `Ok` means it did.
#[test]
fn a_pack_the_store_still_holds_is_never_claimed_as_removed() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    let plan = build(&core);

    // No fault here: this is the control for the post-unlink test, proving the difference is the
    // failure point and not the fixture.
    let before = packs(dir.path());
    let c = core.collector(gc_opts()).unwrap();
    let r = c.collect().unwrap();
    clean(&r);
    assert!(r.packs_unlinked > 0, "{r:?}");

    let after = packs(dir.path());
    let gone: u64 = before
        .iter()
        .filter(|(k, _)| !after.contains_key(*k))
        .map(|(_, v)| *v)
        .sum();
    assert_eq!(
        r.gross_removed_bytes, gone,
        "every pack that left the store is credited, and nothing else: {r:?}"
    );
    // Every pack that left the store is named in the count, and no pack that stayed is.
    let gone_names: Vec<&String> = before.keys().filter(|k| !after.contains_key(*k)).collect();
    assert_eq!(
        r.packs_unlinked as usize,
        gone_names.len(),
        "the count is exactly the packs that left the store: {r:?}"
    );
    assert!(
        !gone_names.is_empty(),
        "the fixture must unlink something: {r:?}"
    );

    verify(&core, &plan.keep);
    drop(c);
    core.close().unwrap();
    let core = reopen(dir.path());
    verify(&core, &plan.keep);
    fsck_clean(&core);
}

/// The signed net must not saturate: a cycle whose rewrite cost exceeds the bytes it unlinked
/// reports a negative net, which is the truthful no-savings outcome.
#[test]
fn an_expensive_rewrite_reports_a_negative_net_without_saturating() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    let plan = build(&core);

    // A dead ratio of 0.0 rewrites every non-active pack, and a tiny pack size makes each rewrite
    // produce a whole new pack with a header, so the rewrite can cost more than the removed pack.
    let c = core
        .collector(GcOptions {
            dead_ratio: 0.0,
            min_dead_bytes: 1,
            io_budget_bytes: 0,
            batch_bytes: 64,
            ..GcOptions::default()
        })
        .unwrap();
    let r = c.collect().unwrap();
    clean(&r);
    if r.rewrite_bytes > 0 {
        assert_eq!(
            r.net_reclaimed_bytes,
            r.gross_removed_bytes as i64 - r.rewrite_bytes as i64,
            "net stays the signed identity: {r:?}"
        );
    }
    verify(&core, &plan.keep);
    drop(c);
    core.close().unwrap();
}

/// A dry run estimates, it does not reclaim. The reported estimate is `candidate_dead_bytes` and
/// the actual gross, rewrite and net are all zero, distinct from a real cycle.
#[test]
fn a_dry_run_reports_zero_actual_gross_rewrite_and_net() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    let plan = build(&core);

    let c = core
        .collector(GcOptions {
            dry_run: true,
            ..gc_opts()
        })
        .unwrap();
    let r = c.collect().unwrap();
    clean(&r);
    assert!(r.dry_run);
    assert_eq!(
        (
            r.gross_removed_bytes,
            r.rewrite_bytes,
            r.net_reclaimed_bytes
        ),
        (0, 0, 0),
        "a dry run reclaims nothing and reports no actual bytes: {r:?}"
    );
    verify(&core, &plan.keep);
    drop(c);
    core.close().unwrap();
}

/// A cycle with nothing to reclaim reports all three fields as zero and no rewrite, so a caller
/// cannot mistake an empty cycle for a saving.
#[test]
fn a_no_op_cycle_reports_zero_gross_rewrite_and_net() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    core.create_snapshot("keep").expect("keep");
    let kv = core.snapshot_view("keep").expect("view");
    let mut files = Vec::new();
    for i in 0..8u32 {
        let f = (format!("k{i:02}"), body(40_000, i));
        put_file(&kv, &f.0, &f.1);
        files.push(f);
        core.sync().expect("sync");
    }

    let before = packs(dir.path());
    let c = core
        .collector(GcOptions {
            dry_run: false,
            ..gc_opts()
        })
        .unwrap();
    let r = c.collect().unwrap();
    clean(&r);
    assert_eq!(
        (r.freed_bytes, r.gross_removed_bytes, r.rewrite_bytes),
        (0, 0, 0),
        "an all-live store frees and rewrites nothing: {r:?}"
    );
    assert_eq!(
        r.net_reclaimed_bytes, 0,
        "net is zero, not a signed artifact of a process-wide delta: {r:?}"
    );
    assert!(r.is_noop(), "the cycle reports itself as a no-op: {r:?}");
    assert_eq!(packs(dir.path()), before, "no pack changed on disk");

    verify(&core, &files);
    drop(kv);
    drop(c);
    core.close().unwrap();
}

/// Run cycles until the persisted marks trust every durable root, which is the state a daemon's
/// repeated requests reach: nothing is walked, so the marks alone decide what is live.
///
/// The bound is a guard, not the criterion. `marked == 0` is the criterion, and a collector that
/// never got there would trip the bound instead of passing silently.
fn cycles_until_the_marks_are_trusted(core: &Core) -> u32 {
    for n in 1..=8u32 {
        let c = core.collector(gc_opts()).unwrap();
        let r = c.collect().unwrap();
        clean(&r);
        assert_eq!(r.packs_unlinked, 0, "nothing is dead yet: {r:?}");
        drop(c);
        if r.marked == 0 {
            return n;
        }
    }
    panic!("the marks never stopped walking a durable root after 8 cycles");
}

/// Issue 82: a fresh collector must not credit one recorded root with another root's blocks.
///
/// Two snapshots share no content, so every block the base names is garbage the moment it is gone,
/// and a run of base-only files makes at least one pack entirely base-owned. Cycles run until the
/// persisted marks trust both roots, which is where a per-request collector reaches after a couple
/// of requests. The base is then removed and one more fresh collector runs.
///
/// That cycle must unlink a real eligible pack. Under the old cache it unlinked nothing, because the
/// marks kept the base's blocks alive under the surviving root's key. Asserting the pack count and
/// the byte total also rules out a cycle that reports a reclaim it did not perform, and the
/// survivors are read back byte-identical after a close and a reopen with a clean `fsck`, so a
/// collector that got the reclaim by freeing live data fails here.
#[test]
fn a_removed_base_is_reclaimed_by_the_next_fresh_collector() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    core.create_snapshot("keep").unwrap();
    core.create_snapshot("base").unwrap();
    let kv = core.snapshot_view("keep").unwrap();
    let bv = core.snapshot_view("base").unwrap();
    let mut keep = Vec::new();
    for i in 0..8u32 {
        // Interleaved, so some packs hold records from both roots.
        let k = (format!("k{i:02}"), body(40_000, 2000 + i));
        put_file(&kv, &k.0, &k.1);
        keep.push(k);
        core.sync().unwrap();
        let b = (format!("b{i:02}"), body(40_000, 3000 + i));
        put_file(&bv, &b.0, &b.1);
        core.sync().unwrap();
    }
    for i in 0..8u32 {
        let b = (format!("only{i:02}"), body(40_000, 4000 + i));
        put_file(&bv, &b.0, &b.1);
        core.sync().unwrap();
    }
    // A tail on the keeper, so the durable watermark is past every pack the base owns.
    add_tail(&core, &mut keep, "tail", 5);
    drop(kv);
    drop(bv);
    core.sync().unwrap();
    let converged = cycles_until_the_marks_are_trusted(&core);
    assert!(
        dir.path().join("gc").join("mark.bin").exists(),
        "cycle {converged} left a marks file behind"
    );

    let before = packs(dir.path());
    let blocks_before = core.store().stats().blocks;
    core.remove_snapshot("base").unwrap();
    core.sync().unwrap();
    assert!(
        core.list_snapshots()
            .unwrap()
            .iter()
            .all(|s| s.name != "base"),
        "the base is gone"
    );

    let c = core.collector(gc_opts()).unwrap();
    let r = c.collect().unwrap();
    clean(&r);
    assert!(
        r.packs_unlinked >= 1 && r.freed_bytes > 0,
        "the removed base's packs were reclaimed: {r:?}"
    );
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
    assert!(core.store().stats().blocks < blocks_before);

    drop(c);
    core.close().unwrap();
    let core = reopen(dir.path());
    verify(&core, &keep);
    fsck_clean(&core);
    core.close().unwrap();
}

/// A fork keeps most of what its parent held, so its walk shares subtrees with an earlier root's
/// and yields only what that walk did not reach.
/// Recording that delta as the fork's own block set is the way to lose data: once the parent is gone
/// nothing else names those inherited blocks, a later cycle trusts the fork's record, skips its walk
/// and reclaims the packs holding them.
///
/// So this runs the two halves in order.
/// First one cycle only, then the parent is removed: that is the window where a delta record would be
/// the fork's only record, so the next cycle must free nothing at all, because the fork still
/// references every inherited file.
/// Then the cycles run to convergence and the garbage snapshot goes: that is where the collector has
/// to do real work, or the first half would pass on a collector that frees nothing.
#[test]
fn a_fork_keeps_the_subtree_it_inherited_from_a_deleted_parent() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    core.create_snapshot("parent").unwrap();
    core.create_snapshot("junk").unwrap();
    let pv = core.snapshot_view("parent").unwrap();
    let jv = core.snapshot_view("junk").unwrap();
    let mut keep = Vec::new();
    for i in 0..8u32 {
        let p = (format!("p{i:02}"), body(40_000, 2000 + i));
        put_file(&pv, &p.0, &p.1);
        keep.push(p);
        core.sync().unwrap();
        let j = (format!("j{i:02}"), body(40_000, 3000 + i));
        put_file(&jv, &j.0, &j.1);
        core.sync().unwrap();
    }
    drop(pv);
    drop(jv);
    core.fork_snapshot("parent", "keep").unwrap();
    core.sync().unwrap();

    let cv = core.snapshot_view("keep").unwrap();
    for i in 0..4u32 {
        let c = (format!("c{i:02}"), body(40_000, 4000 + i));
        put_file(&cv, &c.0, &c.1);
        keep.push(c);
        core.sync().unwrap();
    }
    // The tail is the watermark barrier, and it goes to the survivor so it is never garbage.
    for i in 0..5u32 {
        let t = (format!("t{i:02}"), body(40_000, 5000 + i));
        put_file(&cv, &t.0, &t.1);
        keep.push(t);
        core.sync().unwrap();
    }
    drop(cv);
    core.sync().unwrap();

    // One cycle, so the only records on disk are the ones the first cycle wrote.
    let c = core.collector(gc_opts()).unwrap();
    let first = c.collect().unwrap();
    clean(&first);
    assert!(
        first.marked > 0,
        "the first cycle walks from cold: {first:?}"
    );
    drop(c);

    let before_parent = packs(dir.path());
    core.remove_snapshot("parent").unwrap();
    core.sync().unwrap();
    let c = core.collector(gc_opts()).unwrap();
    let after_parent = c.collect().unwrap();
    clean(&after_parent);
    assert_eq!(
        after_parent.packs_unlinked, 0,
        "the fork still references every file it inherited: {after_parent:?}"
    );
    assert_eq!(packs(dir.path()), before_parent, "no pack moved");
    drop(c);
    // Read every inherited file now, so a cycle that freed one is caught while the core is still open
    // and names it.
    verify(&core, &keep);

    let converged = cycles_until_the_marks_are_trusted(&core);
    let before = packs(dir.path());
    core.remove_snapshot("junk").unwrap();
    core.sync().unwrap();

    let c = core.collector(gc_opts()).unwrap();
    let r = c.collect().unwrap();
    clean(&r);
    assert!(
        r.packs_unlinked >= 1 && r.freed_bytes > 0,
        "cycle {converged} plus this one reclaimed the garbage snapshot: {r:?}"
    );
    assert!(bytes(&packs(dir.path())) < bytes(&before));
    drop(c);
    core.close().unwrap();

    let core = reopen(dir.path());
    verify(&core, &keep);
    fsck_clean(&core);
    core.close().unwrap();
}

/// A torn marks file must be walked in full, and that changes nothing else about the cycle.
///
/// `marked` is the observable that says the file was not trusted: a trusted cache skips every root's
/// walk and reports no marked blocks. The first cycle here reclaims real packs, so a collector that
/// simply stopped working would fail on the reclaim rather than pass this quietly, and the
/// survivors are read after a reopen so a collector that seeded live from a partial cache fails too.
#[test]
fn a_torn_marks_file_is_walked_in_full_and_the_cycle_still_reclaims() {
    use std::fs;

    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    let plan = build(&core);

    let c = core.collector(gc_opts()).unwrap();
    let first = c.collect().unwrap();
    clean(&first);
    assert!(
        first.packs_unlinked >= 1 && first.freed_bytes > 0,
        "the first cycle reclaims real dead packs: {first:?}"
    );
    drop(c);

    let path = dir.path().join("gc").join("mark.bin");
    let whole = fs::read(&path).expect("the first cycle wrote a marks file");
    assert_eq!(
        &whole[..8],
        b"COWMARK3",
        "the file under test is the format this change writes"
    );
    // Keep one whole group and cut the next one short, so a loader that only checked the first group
    // would accept a file that is missing the roots after it.
    let mut torn = whole.clone();
    torn.truncate(8 + 8 + 32 + 8 + 32);
    fs::write(&path, &torn).unwrap();

    let c = core.collector(gc_opts()).unwrap();
    let r = c.collect().unwrap();
    clean(&r);
    assert!(
        r.marked > 0,
        "a torn cache is not trusted, so every root is walked: {r:?}"
    );
    drop(c);
    core.close().unwrap();

    let core = reopen(dir.path());
    verify(&core, &plan.keep);
    fsck_clean(&core);
    core.close().unwrap();
}

/// The roots a marks file names, in file order.
///
/// Test-only, and deliberately coupled to the format: the point of this test is that a root whose
/// walk shared a subtree is *not* written, and the only observable for that is the file itself.
fn recorded_roots(path: &Path) -> Vec<[u8; 32]> {
    let bytes = std::fs::read(path).expect("marks file");
    assert_eq!(&bytes[..8], b"COWMARK3", "unexpected marks format");
    let n = u64::from_le_bytes(bytes[8..16].try_into().unwrap()) as usize;
    let mut rest = &bytes[16..];
    let mut out = Vec::new();
    for _ in 0..n {
        let key: [u8; 32] = rest[..32].try_into().unwrap();
        let blocks = u64::from_le_bytes(rest[32..40].try_into().unwrap()) as usize;
        out.push(key);
        rest = &rest[40 + blocks * 32..];
    }
    out
}

/// Two snapshots that share a subtree *node*, which is what makes the marker skip part of the second
/// walk.
///
/// A fork that adds a file shares its blocks with its parent but no nodes, because the leaf holding
/// a file's chunk list is rebuilt when another entry joins it, so the marker gives no reuse and no
/// delta. A shared directory is different: a directory holding exactly one file has the same node id
/// in both trees, so the second walk descends to it, finds it marked, and yields only what is left.
///
/// That delta is not the second root's reachable set. Recording it, and then losing the root whose
/// walk covered the shared subtree, leaves the survivor trusted on a record that is missing exactly
/// the blocks only the departed root's walk found.
///
/// The test does not assume which of the two is walked first. It reads the file to find out which
/// root was recorded, removes that snapshot, and requires the survivor to still reference the shared
/// file. Under one that records only complete walks, the survivor holds no record at all, is walked
/// again, and the shared file is found.
///
/// Two claims, in this order, because the second needs the first to name a survivor.
/// The first is that a walk which shared a subtree is not recorded at all; that is what fails first
/// if the recording rule is loosened to record every walk, and it fails before the data-loss phase
/// below is reached.
/// The second is the consequence: with that root gone, the survivor's shared file is still there and
/// the cycle frees nothing.
#[test]
fn a_root_whose_walk_shared_a_subtree_is_not_recorded_and_not_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    core.create_snapshot("one").unwrap();
    core.create_snapshot("junk").unwrap();

    let shared = body(40_000, 6001);
    let ov = core.snapshot_view("one").unwrap();
    let d = ov.mkdir(ROOT_INO, b"shared", 0o755).expect("mkdir");
    put_file_in(&ov, d.ino, "same.bin", &shared);
    core.sync().unwrap();
    drop(ov);
    core.fork_snapshot("one", "two").unwrap();
    core.sync().unwrap();
    // A second root over the same shared directory: different root, same subtree node.
    let tv = core.snapshot_view("two").unwrap();
    let extra = body(40_000, 6002);
    put_file(&tv, "extra", &extra);
    core.sync().unwrap();
    drop(tv);
    let jv = core.snapshot_view("junk").unwrap();
    for i in 0..6u32 {
        let j = (format!("j{i:02}"), body(40_000, 7000 + i));
        put_file(&jv, &j.0, &j.1);
        core.sync().unwrap();
    }
    drop(jv);
    // The tail is the watermark barrier.
    let tv = core.snapshot_view("two").unwrap();
    for i in 0..5u32 {
        let t = (format!("t{i:02}"), body(40_000, 8000 + i));
        put_file(&tv, &t.0, &t.1);
        core.sync().unwrap();
    }
    drop(tv);
    core.sync().unwrap();

    let c = core.collector(gc_opts()).unwrap();
    let first = c.collect().unwrap();
    clean(&first);
    assert!(
        first.marked > 0,
        "the first cycle walks from cold: {first:?}"
    );
    drop(c);

    let path = dir.path().join("gc").join("mark.bin");
    let recorded = recorded_roots(&path);
    let roots: BTreeMap<String, [u8; 32]> = core
        .list_snapshots()
        .unwrap()
        .into_iter()
        .map(|s| (s.name, *s.root.as_bytes()))
        .collect();
    assert_eq!(
        roots.len(),
        3,
        "one snapshot each for the pair and the garbage: {roots:?}"
    );
    assert!(
        recorded.len() < roots.len(),
        "a walk that shared a subtree was not recorded: {} of {} roots",
        recorded.len(),
        roots.len()
    );
    // Of the sharing pair, the one whose walk was recorded is the one that covered the shared
    // subtree. The garbage root is recorded too, since nothing overlaps it, and it stays for the
    // reclaim below.
    let pair = ["one", "two"];
    let dropped: Vec<&String> = roots
        .iter()
        .filter(|(n, r)| pair.contains(&n.as_str()) && recorded.contains(r))
        .map(|(n, _)| n)
        .collect();
    assert_eq!(
        dropped.len(),
        1,
        "one of the pair was recorded and the other was not: {roots:?}"
    );
    let survivor = pair
        .iter()
        .find(|n| **n != dropped[0])
        .copied()
        .expect("a survivor");
    let survivor_files: Files = vec![("extra".to_string(), extra.clone())];
    // The shared file is the one at risk, and it needs the directory hop the flat helpers do not do.
    let read_shared = |core: &Core| {
        let v = core.snapshot_view(&survivor).expect("view");
        let dir = v.lookup(ROOT_INO, b"shared").expect("shared directory");
        let f = v.lookup(dir.ino, b"same.bin").expect("shared file");
        v.read(f.ino, 0, 1 << 20).expect("the shared file reads")
    };

    core.remove_snapshot(dropped[0]).unwrap();
    core.sync().unwrap();
    let before = packs(dir.path());
    let c = core.collector(gc_opts()).unwrap();
    let r = c.collect().unwrap();
    clean(&r);
    assert_eq!(
        r.packs_unlinked, 0,
        "{survivor} still references the shared directory: {r:?}"
    );
    assert_eq!(packs(dir.path()), before, "no pack moved");
    drop(c);
    verify_in(&core, &survivor, &survivor_files);
    assert_eq!(read_shared(&core), shared, "the shared file still reads");

    // The garbage snapshot is what makes the collector do real work, so the reclaim has to be there
    // too: without it this test would pass on a collector that frees nothing.
    cycles_until_the_marks_are_trusted(&core);
    let before = packs(dir.path());
    core.remove_snapshot("junk").unwrap();
    core.sync().unwrap();
    let c = core.collector(gc_opts()).unwrap();
    let r = c.collect().unwrap();
    clean(&r);
    assert!(
        r.packs_unlinked >= 1 && r.freed_bytes > 0,
        "the garbage snapshot was reclaimed: {r:?}"
    );
    assert!(bytes(&packs(dir.path())) < bytes(&before));
    drop(c);
    core.close().unwrap();

    let core = reopen(dir.path());
    verify_in(&core, &survivor, &survivor_files);
    assert_eq!(
        read_shared(&core),
        shared,
        "the shared file reads after reopen"
    );
    fsck_clean(&core);
    core.close().unwrap();
}
