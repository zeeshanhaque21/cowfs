//! Read/write coherence through a real FUSE mount over a real `Core`.
//!
//! `conformance.rs` skips `concurrent_readers_and_writers_of_one_file` for the mount because the
//! Linux page cache owns the bytes a reader sees, so a `Cowfs` level 4 KiB atomicity contract is
//! not observable there (issue #45). That skip is only honest while the invariant still holds where
//! it *is* observable, so this file holds it, on the path production actually runs: a real `Core`
//! under a real mount, reached through the mount's own syscalls.
//!
//! Nothing here asserts the atomicity the skip gives up. A read that mixes two writes while
//! writers are running is the kernel's business, not cowfs'. What is asserted:
//!
//! 1. `core_view_keeps_aligned_4k_blocks_atomic_while_flushing`: with no kernel between the test
//!    and the `Core`, a 4 KiB read never mixes two writes. `file_flush_bytes` is dropped to one
//!    block so every write stores and republishes its chunk list, and the background flusher runs,
//!    because the default 4 MiB threshold is never reached by this 64 KiB file and the default arm
//!    therefore exercises the overlay alone.
//! 2. `mount_and_native_agree_on_acknowledged_write_visibility`: the same single write and read
//!    pairs, run against the mount and against a native tmpfs. A write that returned to its caller
//!    reads back exactly, with and without a barrier, on both.
//! 3. `concurrent_writers_leave_every_block_uniform_and_acknowledged`: after the writers stop, each
//!    block is uniform and holds a value some acknowledged write actually wrote. A block torn
//!    while nothing is writing is corruption no interleaving allows, so this is the assertion that
//!    a concurrent tear rate alone cannot stand in for.
//!
//! Run: `cargo test -p cowfs-fuse -j4 --test coherence -- --ignored --nocapture --test-threads=1`
#![cfg(target_os = "linux")]

mod common;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use cowfs_core::{Core, Options};
use cowfs_fuse::{Mount, MountOptions};
use cowfs_vfs::{Ino, Vfs, ROOT_INO};
use cowfs_vfs_path::PathVfs;
use tempfile::TempDir;

use common::fuse_usable;

const PAGES: u64 = 16;
const PG: u32 = 4096;
const WRITERS: usize = 3;
const READERS: usize = 3;
const ROUNDS: usize = 2000;
/// Each check stops at this, so a slow backend does fewer iterations instead of hanging.
const CAP: Duration = Duration::from_secs(20);
/// Flush a file once its unflushed bytes reach one block, so writes store and republish their
/// chunk list instead of piling up in the overlay. The default is 4 MiB, which this file never
/// reaches.
const FLUSH_BYTES: usize = PG as usize;
/// Every writer's value for round `i` on thread `t`. The three ranges are disjoint and never
/// include `PREFILL`, so a block still holding the pre-fill names a lost write.
const PREFILL: u8 = 1;

fn fill(t: usize, i: usize) -> u8 {
    (t * 60 + i % 50 + 2) as u8
}

#[derive(Debug, Default)]
struct Counters {
    writes: AtomicUsize,
    reads: AtomicUsize,
    torn: AtomicUsize,
}

/// Per block, every value for which a write returned to its caller. Written under one lock after
/// the write, so it needs no ordering between writers to be read at rest.
type Receipts = Mutex<HashMap<u64, BTreeSet<u8>>>;

/// A file of `PAGES` blocks, all `PREFILL`.
fn fresh(fs: &Arc<dyn Vfs>, name: &str) -> Ino {
    let ino = fs.create(ROOT_INO, name.as_bytes(), 0o644).unwrap().ino;
    let n = fs
        .write(ino, 0, &vec![PREFILL; (PAGES * u64::from(PG)) as usize])
        .unwrap();
    assert_eq!(n as u64, PAGES * u64::from(PG), "prefill of {name}");
    ino
}

/// Runs `WRITERS` writers of whole blocks against `READERS` readers of whole blocks, then checks
/// the file at rest. `atomic` says whether a torn read under concurrency is this arm's problem:
/// it is for a `Core` called directly, and not for anything behind a kernel page cache.
fn concurrent(fs: &Arc<dyn Vfs>, ino: Ino, c: &Counters, atomic: bool) {
    let receipts: Receipts = Mutex::new(HashMap::new());
    let start = Instant::now();
    std::thread::scope(|s| {
        for t in 0..WRITERS {
            let (fs, receipts, c) = (fs.clone(), &receipts, c);
            s.spawn(move || {
                for i in 0..ROUNDS {
                    if start.elapsed() > CAP {
                        break;
                    }
                    let off = ((i as u64 * 7 + t as u64) % PAGES) * u64::from(PG);
                    let v = fill(t, i);
                    let n = fs.write(ino, off, &vec![v; PG as usize]).unwrap();
                    assert_eq!(n as usize, PG as usize, "short write at {off}");
                    c.writes.fetch_add(1, Relaxed);
                    receipts
                        .lock()
                        .unwrap()
                        .entry(off / u64::from(PG))
                        .or_default()
                        .insert(v);
                }
            });
        }
        for r in 0..READERS {
            let (fs, c) = (fs.clone(), c);
            s.spawn(move || {
                for i in 0..ROUNDS {
                    if start.elapsed() > CAP {
                        break;
                    }
                    let off = ((i as u64 * 7 + r as u64) % PAGES) * u64::from(PG);
                    let got = fs.read(ino, off, PG).unwrap();
                    assert_eq!(got.len(), PG as usize, "read length at {off}");
                    c.reads.fetch_add(1, Relaxed);
                    if !got.iter().all(|&b| b == got[0]) {
                        c.torn.fetch_add(1, Relaxed);
                    }
                }
            });
        }
    });
    if atomic {
        assert_eq!(
            c.torn.load(Relaxed),
            0,
            "a 4 KiB read mixed two writes, with no kernel between the reader and the Core"
        );
    }
    let receipts = receipts.lock().unwrap();
    for b in 0..PAGES {
        let off = b * u64::from(PG);
        let got = fs.read(ino, off, PG).unwrap();
        assert_eq!(got.len(), PG as usize, "at-rest read length at {off}");
        let v = got[0];
        assert!(
            got.iter().all(|&b| b == v),
            "block {b} is torn at rest with no writer running: it holds {v} and {:?}",
            got.iter().filter(|&&b| b != v).take(4).collect::<Vec<_>>()
        );
        let seen = receipts.get(&b).unwrap_or_else(|| {
            panic!("block {b} was never written, so it must still be {PREFILL}")
        });
        assert!(
            seen.contains(&v),
            "block {b} holds {v} at rest, but no acknowledged write put it there (only {seen:?})"
        );
    }
}

/// A write that returned to its caller reads back exactly, with nothing else running, and again
/// behind a barrier.
fn acknowledged_writes_are_visible(fs: &Arc<dyn Vfs>, ino: Ino, tag: &str) {
    for b in 0..PAGES {
        let off = b * u64::from(PG);
        let v = fill(0, b as usize);
        assert_eq!(
            fs.write(ino, off, &vec![v; PG as usize]).unwrap() as usize,
            PG as usize,
            "{tag}: write at {off}"
        );
        assert_eq!(
            fs.read(ino, off, PG).unwrap(),
            vec![v; PG as usize],
            "{tag}: write then read at {off}"
        );
        let w = fill(1, b as usize);
        assert_eq!(
            fs.write(ino, off, &vec![w; PG as usize]).unwrap() as usize,
            PG as usize,
            "{tag}: write at {off}"
        );
        fs.fsync(ino, false).unwrap();
        assert_eq!(
            fs.read(ino, off, PG).unwrap(),
            vec![w; PG as usize],
            "{tag}: write, fsync, read at {off}"
        );
    }
}

/// A real `Core`, opened with flushes forced and the background flusher running.
fn core() -> (TempDir, Core, Arc<dyn Vfs>) {
    let tmp = TempDir::new().unwrap();
    let opts = Options {
        background: true,
        file_flush_bytes: FLUSH_BYTES,
        ..Options::default()
    };
    let core = Core::open(tmp.path().join("store"), opts).unwrap();
    core.create_snapshot("main").unwrap();
    let view = core.snapshot_view("main").unwrap();
    (tmp, core, Arc::new(view))
}

/// A real mount over a real `Core`, plus the `PathVfs` that reaches it through syscalls.
/// `_mount` is declared first so it unmounts before the directory holding it is removed.
struct Rig {
    _mount: Option<Mount>,
    vfs: Arc<dyn Vfs>,
    _core: Core,
    _tmp: TempDir,
    _watchdog: mpsc::Sender<()>,
}

fn rig() -> Option<Rig> {
    if !fuse_usable() {
        eprintln!("SKIP: /dev/fuse or fusermount3 not usable");
        return None;
    }
    let (tmp, core, view) = core();
    let dir = tmp.path().join("mnt");
    std::fs::create_dir(&dir).unwrap();
    let mount = Mount::new(view, &dir, MountOptions::default()).unwrap();
    let vfs = Arc::new(PathVfs::new(&dir).unwrap());
    let d = dir.clone();
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        if let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(Duration::from_secs(300)) {
            eprintln!("WATCHDOG: hung, unmounting {}", d.display());
            let _ = std::process::Command::new("fusermount3")
                .arg("-u")
                .arg("-z")
                .arg(&d)
                .status();
        }
    });
    Some(Rig {
        _mount: Some(mount),
        vfs,
        _core: core,
        _tmp: tmp,
        _watchdog: tx,
    })
}

/// A native tmpfs directory, falling back to the temporary directory when `/dev/shm` is not
/// writable, so the control arm is a real filesystem and not a `cowfs` one.
fn native() -> PathBuf {
    let shm = Path::new("/dev/shm").join(format!("cowfs-coherence-{}", std::process::id()));
    if std::fs::create_dir_all(&shm).is_ok() {
        return shm;
    }
    std::env::temp_dir()
}

#[test]
fn core_view_keeps_aligned_4k_blocks_atomic_while_flushing() {
    let (_tmp, _core, fs) = core();
    let ino = fresh(&fs, "f");
    let c = Counters::default();
    concurrent(&fs, ino, &c, true);
    println!(
        "core direct: {} writes, {} reads, {} torn",
        c.writes.load(Relaxed),
        c.reads.load(Relaxed),
        c.torn.load(Relaxed)
    );
    assert!(c.reads.load(Relaxed) > 0, "the readers ran no iterations");
}

#[test]
fn mount_and_native_agree_on_acknowledged_write_visibility() {
    let Some(rig) = rig() else { return };
    let mnt = rig.vfs.clone();
    let mine = fresh(&mnt, "f");
    acknowledged_writes_are_visible(&mnt, mine, "mount");

    let dir = native();
    let keep = TempDir::new_in(&dir).unwrap();
    let nat: Arc<dyn Vfs> = Arc::new(PathVfs::new(keep.path()).unwrap());
    let theirs = fresh(&nat, "f");
    acknowledged_writes_are_visible(&nat, theirs, "native tmpfs");

    assert!(keep.path().is_dir(), "the native control directory is gone");
}

#[test]
fn concurrent_writers_leave_every_block_uniform_and_acknowledged() {
    let Some(rig) = rig() else { return };
    let mnt = rig.vfs.clone();
    let ino = fresh(&mnt, "f");
    let c = Counters::default();
    // A torn read under concurrency is the page cache's, so the count is reported, not asserted.
    // `concurrent` still asserts what no interleaving allows: no tear at all without a kernel,
    // and at rest here every block uniform and holding an acknowledged value.
    concurrent(&mnt, ino, &c, false);
    println!(
        "through the mount: {} writes, {} reads, {} torn under concurrency, 0 torn at rest",
        c.writes.load(Relaxed),
        c.reads.load(Relaxed),
        c.torn.load(Relaxed)
    );
    assert!(c.reads.load(Relaxed) > 0, "the readers ran no iterations");
}

/// The matched control for the mount's torn count: this same harness, this same 4096-byte blocks
/// and thread counts, against a native tmpfs with no cowfs code underneath. Nothing here asserts
/// the concurrent count either, because the page cache owns it on every Linux filesystem. It is
/// measured so the mount's number can be read against something. The at-rest assertions do hold,
/// and must, because no write interleaving may leave a block torn once the writers have stopped.
#[test]
fn native_tmpfs_torn_count_is_reported_for_comparison() {
    let dir = native();
    let keep = TempDir::new_in(&dir).unwrap();
    let nat: Arc<dyn Vfs> = Arc::new(PathVfs::new(keep.path()).unwrap());
    let ino = fresh(&nat, "f");
    let c = Counters::default();
    concurrent(&nat, ino, &c, false);
    println!(
        "native tmpfs ({}): {} writes, {} reads, {} torn under concurrency, 0 torn at rest",
        keep.path().display(),
        c.writes.load(Relaxed),
        c.reads.load(Relaxed),
        c.torn.load(Relaxed)
    );
    assert!(c.reads.load(Relaxed) > 0, "the readers ran no iterations");
}
