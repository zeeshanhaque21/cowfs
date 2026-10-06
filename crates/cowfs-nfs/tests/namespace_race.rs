//! The guard that refuses a real object under a live sidecar name reads the directory and then
//! mutates it in a separate `Vfs` call. Two NFSv3 calls make up the race: `mkdir(._doc)` and
//! `create(doc)`. This file drives both of them the way a client does, as real RPCs over two
//! connections to the real in-process NFSv3 server, and adds one direct adapter check for the lock
//! contract itself.
//!
//! The window is made observable rather than timing-dependent. A `Vfs` wrapper records how many
//! guarded name-space operations are inside the adapter at once for one directory, and it can hold
//! the first `mkdir` at a barrier while a second request runs. On an adapter whose guard read and
//! mutation are one step against the directory, the depth never exceeds one, the second request
//! waits, and `._doc` never becomes a real object while `doc` exists. On the reviewed head both
//! requests run inside the window, a real directory takes the name, and the attribute channel for
//! `doc` is dead for the rest of the mount.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use common::{memfs, serve, OK};
use cowfs_nfs::{Adapter, AdapterOptions, AppleDoubleMode, MountOptions};
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

fn translated() -> MountOptions {
    MountOptions {
        appledouble: AppleDoubleMode::Translate,
        one_shot_mount: false,
        ..MountOptions::default()
    }
}

/// A `Vfs` that counts how many guarded name-space operations the adapter runs at once for the root
/// directory, and can hold the next `mkdir` at a barrier so a second request has a chance to enter.
/// Everything else delegates straight through.
struct WatchVfs {
    inner: Arc<dyn Vfs>,
    /// Namespace operations currently inside the adapter for the root.
    depth: AtomicUsize,
    /// Largest depth seen, the lock-contract value the test asserts on.
    peak: AtomicUsize,
    /// Holds the first `mkdir` of the armed name until `release` or the deadline.
    hold: Mutex<HoldState>,
    ready: Condvar,
}

struct HoldState {
    armed: Option<Vec<u8>>,
    reached: bool,
    released: bool,
}

impl WatchVfs {
    fn new(inner: Arc<dyn Vfs>) -> Arc<WatchVfs> {
        Arc::new(WatchVfs {
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
    /// adapter that never overlaps returns as soon as the deadline passes.
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

    /// Waits until the armed `mkdir` has stopped at the barrier, or `secs` pass.
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

impl Vfs for WatchVfs {
    fn lookup(&self, p: Ino, n: &[u8]) -> Result<Attr> {
        // The guard read: the adapter asks the backend whether a name exists before it mutates. The
        // hold is placed after the backend has answered, so the guard has its stale answer in hand
        // when the second request changes the tree; the mutation that follows then runs on the
        // answer read before the change. This is the check-then-write window on the real request
        // flow, with no injection below `Vfs`.
        let r = self.inner.lookup(p, n);
        self.hold_if_armed(n);
        r
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

fn is_real_dir(vfs: &dyn Vfs) -> bool {
    matches!(vfs.lookup(ROOT_INO, SIDE), Ok(a) if a.kind == FileKind::Directory)
}

/// The dispatch sample: one bounded, complete raw-NFS run over the real in-process server.
///
/// Two real RPCs on two connections race the guard's window for one directory. The window is opened
/// on the real request flow, with no injection below `Vfs`: the first `mkdir(._doc)` is held at its
/// guard read, the backend lookup that decides whether the name is free. The second connection then
/// creates the main name while the first request is stopped between its read and its write, which is
/// exactly the transition a bare read-then-write cannot cover.
///
/// The outcome that matters is decided by what the guard saw, not by timing. The test records
/// whether the main name was already present in the tree at the moment the held mutation was
/// allowed to run. If it was, the guard read stale state, and a real directory must never take the
/// live view name. If it was not, the `mkdir` was the serial winner and a real `._doc` directory is
/// the correct macOS fallback for a name that had no main file yet. The check is unconditional
/// given the recorded fact, so a run that produces the shadow cannot pass by skipping it.
#[test]
fn a_sidecar_name_never_becomes_a_real_object_under_raw_nfs() {
    let inner = memfs();
    let watch = WatchVfs::new(inner.clone());
    let (server, mut c) = serve(watch.clone(), translated());
    let root = c.root.clone();

    // Hold on the guard read of `doc`, which is the read the `mkdir(._doc)` guard does before it
    // would write. The hold is inside the adapter's window, not below `Vfs`.
    watch.arm(MAIN);

    let mk = {
        let mut c = common::Nfs::connect(server.port(), server.export_name());
        let root = root.clone();
        std::thread::spawn(move || c.mkdir(&root, "._doc"))
    };

    assert!(
        watch.wait_reached(10),
        "mkdir(._doc) never reached its guard read of doc; the window could not be opened"
    );

    // The main name is created on a second connection while the first request is stopped in its
    // window. On an adapter whose read and write are one step this blocks on the directory lock
    // until the first request finishes; on one without the step it runs straight through.
    let created = {
        let mut c = common::Nfs::connect(server.port(), server.export_name());
        let root = root.clone();
        std::thread::spawn(move || c.create(&root, "doc", 1, common::sattr_mode(0o644), [0; 8]))
    };
    // Bounded: give the second request a window to slip in. When it is serialized away, nothing
    // changes and this simply runs to its deadline.
    let overlapped = watch.wait_peak(2, 1000);
    let depth_while_held = watch.peak();

    // The fact the outcome turns on: is the main name in the tree while the held mutation has not
    // run yet? If yes, the guard read it away too early and the write that follows is the defect.
    let doc_present_at_mutation = inner.lookup(ROOT_INO, MAIN).is_ok();
    watch.release();
    let (mk_st, _fh) = mk.join().unwrap();
    let (created, _fh, _) = created.join().unwrap();

    let real = is_real_dir(inner.as_ref());
    let doc_exists = inner.lookup(ROOT_INO, MAIN).is_ok();
    let names = c.names(&root);
    eprintln!(
        "NFS mkdir(._doc)={mk_st} create(doc)={created} overlapped={overlapped} \
         doc_present_at_mutation={doc_present_at_mutation} real_dir_took_the_name={real} \
         doc_exists={doc_exists} depth_while_held={depth_while_held} peak_depth={} names={names:?}",
        watch.peak()
    );

    // The lock contract: the two guarded changes for one directory never overlap. On the fixed
    // adapter the second request cannot enter, so the peak stays one.
    assert_eq!(
        watch.peak(),
        1,
        "two guarded name-space operations on one directory ran at the same time"
    );
    assert!(
        !overlapped,
        "the second request entered the first request's guard window"
    );

    // The illegal transition: the guard held an answer from before the main name landed, so no
    // real object may take the live view name.
    if doc_present_at_mutation {
        assert!(
            !real,
            "a real directory took the live view name: the guard read stale state"
        );
    }
    // A real `._doc` directory from the serial fallback is not the view: the channel must not
    // report it as a usable attribute view.
    if real {
        let (ch, _fh, _) = c.create(&root, "._doc", 1, common::sattr_mode(0o600), [0; 8]);
        assert_ne!(
            ch, OK,
            "a real ._doc directory was reported as a usable attribute view"
        );
    }
}

/// The lock contract itself, on the adapter directly: for one directory the guard read and the
/// mutation that follows it are never interleaved with another guarded name-space operation.
/// On the reviewed head both requests run inside the window and the backend sees depth two; with
/// the per-directory lock the second request cannot enter until the first leaves.
#[test]
fn the_guard_and_its_mutation_are_one_step() {
    let inner = memfs();
    let watch = WatchVfs::new(inner.clone());
    let a = adapter_with(watch.clone());

    watch.arm(SIDE);
    let mk = {
        let a = a.clone();
        std::thread::spawn(move || a.mkdir(ROOT_INO, SIDE, &sattr3::default()))
    };
    assert!(
        watch.wait_reached(10),
        "mkdir(._doc) never reached its mutation"
    );

    // This runs on another thread while `mkdir` is inside its mutation. With the lock it cannot
    // enter the adapter until `mkdir` leaves, so the peak depth stays at one. Give it a bounded
    // chance to enter before the first request is allowed to finish.
    let create = {
        let a = a.clone();
        std::thread::spawn(move || a.create(ROOT_INO, MAIN, &sattr3::default(), true))
    };
    watch.wait_peak(2, 1000);
    let depth_while_held = watch.peak();
    watch.release();
    let _ = mk.join().unwrap();
    let created = create.join().unwrap();
    assert!(created.is_ok(), "the concurrent create(doc) must succeed");

    eprintln!(
        "ADAPTER peak_depth_while_held={depth_while_held} peak_depth={}",
        watch.peak()
    );
    assert_eq!(
        watch.peak(),
        1,
        "two guarded name-space operations on one directory ran at the same time"
    );
}

/// A refused `mkdir` must not have created anything: the sidecar name is still free.
#[test]
fn a_refused_directory_leaves_the_name_free() {
    let inner = memfs();
    let watch = WatchVfs::new(inner.clone());
    let a = adapter_with(watch.clone());

    let dir = a.create(ROOT_INO, MAIN, &sattr3::default(), true);
    assert!(dir.is_ok(), "create(doc)");

    let refused = a.mkdir(ROOT_INO, SIDE, &sattr3::default());
    eprintln!(
        "REFUSE mkdir(._doc) over a live view: {:?}, ._doc real dir={}",
        refused.as_ref().err().map(|e| *e as u32),
        is_real_dir(inner.as_ref())
    );
    assert!(
        refused.is_err(),
        "mkdir(._doc) must be refused while doc exists"
    );
    assert!(
        !is_real_dir(inner.as_ref()),
        "a refused mkdir left ._doc behind"
    );
    assert!(inner.lookup(ROOT_INO, MAIN).is_ok(), "doc survives");
}
