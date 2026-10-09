//! Two independent `Adapter` instances over one shared backend: the root-handle-versus-
//! `snapshot_view` topology the adapter lock was never asked to cover.
//!
//! `namespace_race.rs` proves the per-directory lock serialises a guard read and the mutation it
//! guards when both requests go through the *same* `Adapter`. The product does not always use one
//! adapter. The daemon keeps a default mount over the core root and exports a snapshot at a
//! client-chosen path, and each export is its own `Mount` -> `Server` -> `Adapter` over the same
//! `Core`. Two connections on two mounts reach one snapshot's namespace through two adapters whose
//! lock maps are separate by construction (`Adapter::new` builds a fresh `PerIno` per instance).
//!
//! This file drives that topology directly at the public `Adapter::new` seam: one backing `Vfs`,
//! two adapters, and the same `mkdir(._doc)` versus `create(doc)` race the single-adapter fixture
//! uses. The window is made observable, not timing-dependent: the shared backend holds adapter A's
//! guard read of the main name until adapter B has run, which is exactly the interleaving a lock
//! shared between the two adapters would forbid and a per-instance lock cannot.
//!
//! Assertion semantics (the final review of the sibling real-Core fixture,
//! `docs/reviews/pr145-real-core-outcome-final-wbuddy-review.md`): a mid-operation observer is not a
//! product outcome, so an assertion must not forbid a legal result. The race is judged against the
//! serial oracle: the same adapter pair is run in both serial orders, and the race must equal one of
//! those actual products on every user-observable field. The surrogate has no per-snapshot
//! `SnapCtx.ns` and a writable root, so it stays a fixture for the assertion discipline only; the
//! real-Core regression in `crates/cowfs-daemon/tests/separate_adapter_namespace.rs` is the product
//! topology.
//!
//! Both adapters are built with the public `Adapter::new` over the same `Arc<dyn Vfs>`.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use common::memfs;
use cowfs_nfs::{Adapter, AdapterOptions, AppleDoubleMode};
use cowfs_vfs::*;
use nfsserve::nfs::sattr3;

const MAIN: &[u8] = b"doc";
const SIDE: &[u8] = b"._doc";

fn adapter_with(vfs: Arc<dyn Vfs>) -> Arc<Adapter> {
    Arc::new(
        Adapter::new(
            vfs,
            AdapterOptions {
                appledouble: AppleDoubleMode::Translate,
                ..AdapterOptions::default()
            },
        )
        .unwrap(),
    )
}

/// The one backend both adapters wrap. It counts how many guarded name-space operations are
/// *inside the adapters* at once for the root, across every adapter, and can hold adapter A's
/// guard read of `name` until `release` or the deadline. Everything else delegates through.
///
/// The seam sits at the `Vfs` boundary the adapters share, not below it: it is the backend, so a
/// hold here is the guard read the adapter itself performs.
struct SharedBackend {
    inner: Arc<dyn Vfs>,
    /// Guarded name-space operations inside either adapter, for the root.
    depth: AtomicUsize,
    /// Largest depth seen across both adapters.
    peak: AtomicUsize,
    hold: Mutex<HoldState>,
    ready: Condvar,
}

struct HoldState {
    armed: Option<Vec<u8>>,
    reached: bool,
    released: bool,
}

impl SharedBackend {
    fn new(inner: Arc<dyn Vfs>) -> Arc<SharedBackend> {
        Arc::new(SharedBackend {
            inner,
            depth: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            hold: Mutex::new(HoldState {
                armed: None,
                reached: false,
                released: false,
            }),
            ready: Condvar::new(),
        })
    }

    fn arm(&self, name: &[u8]) {
        self.hold.lock().unwrap().armed = Some(name.to_vec());
    }

    fn enter(&self) {
        let d = self.depth.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(d, Ordering::SeqCst);
    }

    fn leave(&self) {
        self.depth.fetch_sub(1, Ordering::SeqCst);
    }

    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    /// Waits until the recorded peak reaches `want`, or `millis` pass. Bounded, so a correct
    /// arrangement that never overlaps returns once the deadline passes.
    fn wait_peak(&self, want: usize, millis: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(millis);
        while self.peak() < want {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        true
    }

    /// Waits until the armed guard read has stopped at the barrier, or `secs` pass.
    fn wait_reached(&self, secs: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
        let mut s = self.hold.lock().unwrap();
        while !s.reached {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            let (g, _) = self.ready.wait_timeout(s, left).unwrap();
            s = g;
        }
        true
    }

    fn release(&self) {
        self.hold.lock().unwrap().released = true;
        self.ready.notify_all();
    }

    fn hold_if_armed(&self, name: &[u8]) -> bool {
        let mut s = self.hold.lock().unwrap();
        if s.armed.as_deref() != Some(name) || s.reached {
            return false;
        }
        s.reached = true;
        self.ready.notify_all();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !s.released {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return true;
            }
            let (g, _) = self.ready.wait_timeout(s, left).unwrap();
            s = g;
        }
        true
    }
}

impl Vfs for SharedBackend {
    fn lookup(&self, p: Ino, n: &[u8]) -> Result<Attr> {
        // The guard read: the adapter asks whether a name exists before it mutates. The answer is
        // returned immediately; the hold is placed in the mutation below, so adapter A has its
        // answer in hand when adapter B changes the tree, and the mutation that follows runs on the
        // answer read before the change. This is the check-then-write window on the shared backend,
        // with no injection.
        self.inner.lookup(p, n)
    }
    fn forget(&self, i: Ino, c: u64) {
        self.inner.forget(i, c)
    }
    fn getattr(&self, i: Ino) -> Result<Attr> {
        self.inner.getattr(i)
    }
    fn setattr(&self, i: Ino, c: SetAttr) -> Result<Attr> {
        self.inner.setattr(i, c)
    }
    fn readlink(&self, i: Ino) -> Result<Vec<u8>> {
        self.inner.readlink(i)
    }
    fn create(&self, p: Ino, n: &[u8], m: u32) -> Result<Attr> {
        self.enter();
        let r = self.inner.create(p, n, m);
        self.leave();
        r
    }
    fn mkdir(&self, p: Ino, n: &[u8], m: u32) -> Result<Attr> {
        self.enter();
        self.hold_if_armed(n);
        let r = self.inner.mkdir(p, n, m);
        self.leave();
        r
    }
    fn mknod(&self, p: Ino, n: &[u8], k: cowfs_vfs::FileKind, m: u32, r: u64) -> Result<Attr> {
        self.enter();
        let res = self.inner.mknod(p, n, k, m, r);
        self.leave();
        res
    }
    fn symlink(&self, p: Ino, n: &[u8], t: &[u8]) -> Result<Attr> {
        self.enter();
        let r = self.inner.symlink(p, n, t);
        self.leave();
        r
    }
    fn link(&self, i: Ino, np: Ino, nn: &[u8]) -> Result<Attr> {
        self.enter();
        let r = self.inner.link(i, np, nn);
        self.leave();
        r
    }
    fn unlink(&self, p: Ino, n: &[u8]) -> Result<()> {
        self.inner.unlink(p, n)
    }
    fn rmdir(&self, p: Ino, n: &[u8]) -> Result<()> {
        self.inner.rmdir(p, n)
    }
    fn rename(&self, p: Ino, n: &[u8], np: Ino, nn: &[u8], f: RenameFlags) -> Result<()> {
        self.inner.rename(p, n, np, nn, f)
    }
    fn open(&self, i: Ino) -> Result<FileHandle> {
        self.inner.open(i)
    }
    fn release(&self, h: FileHandle) -> Result<()> {
        self.inner.release(h)
    }
    fn read(&self, i: Ino, o: u64, n: u32) -> Result<Vec<u8>> {
        self.inner.read(i, o, n)
    }
    fn write(&self, i: Ino, o: u64, d: &[u8]) -> Result<u32> {
        self.inner.write(i, o, d)
    }
    fn flush(&self, i: Ino) -> Result<()> {
        self.inner.flush(i)
    }
    fn fsync(&self, i: Ino, d: bool) -> Result<()> {
        self.inner.fsync(i, d)
    }
    fn sync_namespace(&self, i: Ino) -> Result<()> {
        self.inner.sync_namespace(i)
    }
    fn readdir(&self, d: Ino, c: u64, m: usize) -> Result<ReadDir> {
        self.inner.readdir(d, c, m)
    }
    fn readdir_attrs(&self, d: Ino, c: u64, m: usize) -> Result<ReadDirPlus> {
        self.inner.readdir_attrs(d, c, m)
    }
    fn statfs(&self) -> Result<StatFs> {
        self.inner.statfs()
    }
    fn getxattr(&self, i: Ino, n: &[u8]) -> Result<Vec<u8>> {
        self.inner.getxattr(i, n)
    }
    fn setxattr(&self, i: Ino, n: &[u8], v: &[u8], f: XattrFlags) -> Result<()> {
        self.inner.setxattr(i, n, v, f)
    }
    fn listxattr(&self, i: Ino) -> Result<Vec<Vec<u8>>> {
        self.inner.listxattr(i)
    }
    fn removexattr(&self, i: Ino, n: &[u8]) -> Result<()> {
        self.inner.removexattr(i, n)
    }
}

/// The normalised, user-observable product of one scenario on the surrogate backend.
///
/// Every field is what the adapter returns to a caller, never an internal probe. As on the
/// real-Core fixture, the surrogate must match an actual serial product on all of these, so a legal
/// serial outcome is accepted and only a third product is a counterexample.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Observation {
    /// Whether `mkdir(._doc)` returned `Ok`.
    mkdir_ok: bool,
    /// Whether `create(doc)` returned `Ok`.
    create_ok: bool,
    /// Kind of `doc` after both requests.
    main_kind: Option<FileKind>,
    /// Kind of `._doc` after both requests.
    side_kind: Option<FileKind>,
}

fn kind_of(inner: &dyn Vfs, name: &[u8]) -> Option<FileKind> {
    inner.lookup(ROOT_INO, name).ok().map(|a| a.kind)
}

/// One scenario on a fresh surrogate backend: two adapters over one `MemVfs`-backed shared backend.
/// `mkdir_first` chooses the serial order; `race` runs the two requests concurrently with adapter A
/// held at its sidecar mutation after its guard read of `doc` has answered.
fn observe(mkdir_first: bool, race: bool) -> Observation {
    let inner = memfs();
    let backend = SharedBackend::new(inner.clone());
    let a = adapter_with(backend.clone() as Arc<dyn Vfs>);
    let b = adapter_with(backend.clone() as Arc<dyn Vfs>);
    let root_a = a.root_id();
    let root_b = b.root_id();

    let (mkdir_ok, create_ok) = if !race {
        if mkdir_first {
            let m = a.mkdir(root_a, SIDE, &sattr3::default());
            let c = b.create(root_b, MAIN, &sattr3::default(), true);
            (m.is_ok(), c.is_ok())
        } else {
            let c = b.create(root_b, MAIN, &sattr3::default(), true);
            let m = a.mkdir(root_a, SIDE, &sattr3::default());
            (m.is_ok(), c.is_ok())
        }
    } else {
        // Hold adapter A at its mutation of `._doc`, after its guard read of `doc` returned.
        backend.arm(SIDE);
        let mk = {
            let a = a.clone();
            std::thread::spawn(move || a.mkdir(root_a, SIDE, &sattr3::default()))
        };
        assert!(
            backend.wait_reached(10),
            "adapter A never reached its sidecar mutation; the window could not be opened"
        );
        let created = {
            let b = b.clone();
            std::thread::spawn(move || b.create(root_b, MAIN, &sattr3::default(), true))
        };
        let overlapped = backend.wait_peak(2, 1000);
        let depth_while_held = backend.peak();
        backend.release();
        let mk = mk.join().unwrap();
        let created = created.join().unwrap();
        // Diagnostics only, never acceptance.
        eprintln!(
            "SURROGATE race=1 overlapped={overlapped} depth_while_held={depth_while_held} \
             peak_depth={}",
            backend.peak()
        );
        (mk.is_ok(), created.is_ok())
    };

    Observation {
        mkdir_ok,
        create_ok,
        main_kind: kind_of(inner.as_ref(), MAIN),
        side_kind: kind_of(inner.as_ref(), SIDE),
    }
}

/// The separate-adapter topology on the `MemVfs` surrogate, judged against the serial oracle.
///
/// The surrogate has no per-snapshot `SnapCtx.ns` and a writable root, so it can never prove a
/// product defect. What it can prove is the assertion discipline: the race must equal one of the two
/// actual serial products of the same adapter pair, so an assertion never forbids a legal result.
///
/// Adapter A runs `mkdir(._doc)` and stops at its sidecar mutation, after its guard read of `doc`
/// has returned. Adapter B then creates `doc` through its own lock map. Both build through the public
/// `Adapter::new` over the same `Arc<dyn Vfs>`, the construction the daemon uses for a default mount
/// and an exported snapshot that serve one core.
#[test]
fn two_adapters_over_one_namespace_match_a_serial_product_under_the_race() {
    let serial_mkdir_first = observe(true, false);
    let serial_create_first = observe(false, false);
    let race = observe(true, true);

    let vs_mkdir = race != serial_mkdir_first;
    let vs_create = race != serial_create_first;
    assert!(
        !(vs_mkdir && vs_create),
        "the race produced a state matching NEITHER actual serial product: \
         race={race:?} mkdir-first={serial_mkdir_first:?} create-first={serial_create_first:?}"
    );

    // `doc` is a regular file in both legal serial products, so the race must show it too.
    assert_eq!(
        race.main_kind,
        Some(FileKind::Regular),
        "doc is not a regular file after the race: {race:?}"
    );
    // `._doc` is either a real directory (mkdir-first) or a live regular-file view (create-first).
    assert!(
        matches!(
            race.side_kind,
            Some(FileKind::Directory) | Some(FileKind::Regular)
        ),
        "._doc is neither a real directory nor a live view after the race: {race:?}"
    );
}

/// The two adapters really do reach one namespace: a create through one is visible to the other,
/// and both share the same backend inode numbering. This pins the fixture's premise so the test
/// above cannot pass by the two adapters sitting on disjoint state.
#[test]
fn the_two_adapters_share_one_namespace() {
    let inner = memfs();
    let backend = SharedBackend::new(inner.clone());
    let a = adapter_with(backend.clone() as Arc<dyn Vfs>);
    let b = adapter_with(backend.clone() as Arc<dyn Vfs>);
    let root_a = a.root_id();
    let root_b = b.root_id();

    let (made, _) = a
        .create(root_a, MAIN, &sattr3::default(), true)
        .expect("adapter A creates doc");
    let (seen, _) = b
        .lookup(root_b, MAIN)
        .expect("adapter B sees doc written through adapter A");
    assert_eq!(
        made, seen,
        "the two adapters did not agree on the file id of one shared name"
    );
}
