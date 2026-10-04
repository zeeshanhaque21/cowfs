//! The reference-barrier window over a real core: a writer that runs exactly while the barrier is
//! held, with and without the real gate.
//!
//! This lives here, not in `cowfs-gc`'s tests, because the negative control turns the real `Core`
//! gate into a barrier that does not close, through `Core::set_gate_fault`. That seam is
//! `#[cfg(test)]`-only (a fail-open switch must not be reachable in a production build), and a
//! `cfg(test)` item is only visible to unit tests in this crate. Keeping the control here means no
//! Cargo feature is needed, so nothing can leak it into another package's build.
//!
//! The positive control (`fault = false`) is the real gate: it must park the writer and lose no
//! block, while still unlinking a pack. The negative control (`fault = true`) proves the test is
//! sensitive: without a real barrier the writer's block is lost.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cowfs_gc::{Barrier, ExtraRoots, GcReport, Held, Options as GcOptions, RootsError};
use cowfs_store::BlockId;
use cowfs_vfs::{Vfs, ROOT_INO};

use crate::{Core, CoreRoots, Options as CoreOptions, SnapshotView};

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

fn reopen(dir: &Path) -> Core {
    Core::open(dir, core_opts(false)).expect("reopen")
}

/// The roots of a real core, plus a writer that runs exactly while the barrier is held.
struct Window {
    inner: CoreRoots,
    core: Core,
    late: Files,
    holding: Arc<AtomicBool>,
    fired: AtomicBool,
    writer: Mutex<Option<JoinHandle<()>>>,
    parked: AtomicBool,
}

struct WindowBarrier {
    inner: Box<dyn Barrier>,
    holding: Arc<AtomicBool>,
}

struct WindowHeld {
    _inner: Box<dyn Held>,
    holding: Arc<AtomicBool>,
}

impl Held for WindowHeld {}
impl Drop for WindowHeld {
    fn drop(&mut self) {
        self.holding.store(false, SeqCst);
    }
}

impl Barrier for WindowBarrier {
    fn take(&mut self) -> Option<Box<dyn Held>> {
        let inner = self.inner.take()?;
        self.holding.store(true, SeqCst);
        Some(Box::new(WindowHeld {
            _inner: inner,
            holding: self.holding.clone(),
        }))
    }
}

impl ExtraRoots for Window {
    fn pinned_blocks(&self) -> Result<Vec<BlockId>, RootsError> {
        let answer = self.inner.pinned_blocks();
        // This is the last read of the reference side before the pack is unlinked. A writer that
        // starts here has to be kept out by the barrier, or its blocks are in no answer.
        if self.holding.load(SeqCst) && !self.fired.swap(true, SeqCst) {
            let core = self.core.clone();
            let late = self.late.clone();
            let h = std::thread::spawn(move || {
                let v = core.snapshot_view("late").expect("late view");
                for (n, d) in &late {
                    put_file(&v, n, d);
                }
                core.sync().expect("sync");
            });
            let start = Instant::now();
            while !h.is_finished() && self.core.gate_waiters() == 0 {
                assert!(
                    start.elapsed() < Duration::from_secs(10),
                    "the writer neither ran nor parked"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            self.parked.store(!h.is_finished(), SeqCst);
            *self.writer.lock().unwrap() = Some(h);
        }
        answer
    }
    fn reference_barrier(&self) -> Result<Option<Box<dyn Barrier>>, RootsError> {
        let inner = self
            .inner
            .reference_barrier()?
            .expect("core offers a barrier");
        Ok(Some(Box::new(WindowBarrier {
            inner,
            holding: self.holding.clone(),
        })))
    }
}

/// Runs one cycle with a writer that re-writes every dead block inside the barrier window.
/// Returns how many of the writer's files fail to read after a reopen, and whether the writer
/// was kept out of the window.
fn window(fault: bool) -> (usize, bool, GcReport) {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), core_opts(false)).unwrap();
    let plan = build(&core);
    core.create_snapshot("late").unwrap();
    core.sync().unwrap();
    let c = core.collector(gc_opts()).unwrap();
    let w = Window {
        inner: c.roots().clone(),
        core: core.clone(),
        late: plan.dropped.clone(),
        holding: Arc::default(),
        fired: AtomicBool::new(false),
        writer: Mutex::new(None),
        parked: AtomicBool::new(false),
    };
    core.set_gate_fault(u8::from(fault));
    let r = c.gc().collect(Some(&w)).unwrap();
    core.set_gate_fault(0);
    if let Some(h) = w.writer.lock().unwrap().take() {
        h.join().expect("writer");
    }
    let parked = w.parked.load(SeqCst);
    drop(w);
    drop(c);
    core.close().unwrap();
    let core = reopen(dir.path());
    verify(&core, &plan.keep);
    let v = core.snapshot_view("late").unwrap();
    let failed = plan
        .dropped
        .iter()
        .filter(|(n, d)| read_file(&v, n).map_or(true, |got| got != *d))
        .count();
    if !fault {
        fsck_clean(&core);
    }
    (failed, parked, r)
}

#[test]
fn a_writer_that_dedups_inside_the_barrier_window_waits_for_the_unlink() {
    let (failed, parked, r) = window(false);
    assert!(
        parked,
        "the writer must be parked at the gate while the barrier is held"
    );
    assert_eq!(failed, 0, "no block the writer referenced was lost: {r:?}");
    assert!(
        r.packs_unlinked >= 1,
        "the window really covered an unlink: {r:?}"
    );
}

#[test]
fn negative_control_a_barrier_that_does_not_close_the_gate_loses_data() {
    let (failed, parked, r) = window(true);
    assert!(!parked, "with the fault the writer is not held back");
    assert!(
        failed > 0,
        "without a real barrier the writer's reference is lost, so this test would catch it: {r:?}"
    );
}
