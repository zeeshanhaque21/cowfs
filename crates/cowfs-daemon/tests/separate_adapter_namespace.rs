//! The separate-adapter namespace seam, driven through a real `Core`.
//!
//! `crates/cowfs-nfs/tests/namespace_race.rs` proves the per-adapter guard lock serialises a guard
//! read against the mutation it guards when both requests go through one `Adapter`. The product
//! does not always use one adapter: the daemon keeps a default mount over the core root and exports
//! a snapshot at a client-chosen path, and every export is its own `Mount` -> `Server` -> `Adapter`
//! over the same `Core` (`crates/cowfs-daemon/src/exports.rs` `mount_snapshot` ->
//! `backend.snapshot(name)` -> `Core::snapshot_view`, `crates/cowfs-daemon/src/backend.rs`).
//!
//! This file builds the real topology in the daemon's own dependency closure:
//! `Core::open(tmpdir, Options::default())` -> `create_snapshot("s")` -> two `snapshot_view("s")`
//! of the SAME snapshot -> two `Server::start` (each an `Adapter` over its view, over one `Core`)
//! -> the `mkdir(._doc)` versus `create(doc)` race, driven as real RPCs on two connections.
//!
//! The window is made observable, not timing-dependent: a `Vfs` wrapper under adapter A holds its
//! sidecar mutation after its guard read of `doc` has already answered.
//!
//! Assertion semantics (the final review of this file, `docs/reviews/pr145-real-core-outcome-final-
//! wbuddy-review.md`): the earlier revision asserted an "illegal" outcome from a mid-operation
//! observer, `main_at_mutation == true`, and thereby forbade a legal result. That observer is a
//! third-party read injected at one instant, not the result of any operation in the history; a
//! `mkdir(._doc)` request legitimately spans `[guard read, mutation]`, so a concurrent `create(doc)`
//! may land inside that span while A is still correctly ordered before it at A's guard read.
//!
//! This revision is a serial-oracle regression instead. It drives the same real-Core, two-adapter,
//! public-RPC race, then compares the normalized observation of the race against the normalized
//! observations of BOTH actual serial histories - `mkdir(._doc)` then `create(doc)`, and
//! `create(doc)` then `mkdir(._doc)` - run through the same topology. The race must equal at least
//! one actual serial product on every user-observable field (exact RPC statuses, returned-handle
//! presence, the final kind of each name, the `fileid` identity within the run, handle stability,
//! `doc` bytes written through adapter A and read back through *B's own* handle, and the sidecar
//! channel bytes). `depth`/`peak`/`overlap` and the two mid-operation
//! probes are diagnostics only and are never the pass criterion.
//!
//! A PASS therefore says the race produced one of the two legal serial outcomes; it does not claim
//! an internal linearization instant. A true counterexample is an observation that matches neither
//! serial product, which would be reported with its actual returned statuses and bytes.
//!
//! macOS only: the real topology is the NFS loopback, and `cowfs-nfs` is a `cfg(target_os =
//! "macos")` dependency of this crate, so the file compiles to nothing elsewhere.

#![cfg(target_os = "macos")]

use std::io::{Cursor, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use cowfs_core::{Core, Options};
use cowfs_nfs::{AppleDoubleMode, MountOptions, Server};
use cowfs_vfs::{
    Attr, FileHandle, FileKind, Ino, ReadDir, ReadDirPlus, RenameFlags, SetAttr, StatFs, Vfs,
    XattrFlags, ROOT_INO,
};

const MAIN: &[u8] = b"doc";
const SIDE: &[u8] = b"._doc";

// NFSv3 wire constants. The daemon crate cannot name `nfsserve` types directly (it is not a direct
// dependency and no manifest may change), so the handful of types this client needs are XDR-coded
// here, byte for byte as in `crates/cowfs-nfs/tests/common/mod.rs`.
const NFS_PROG: u32 = 100_003;
const MOUNT_PROG: u32 = 100_005;
const OK: u32 = 0;
const EXIST: u32 = 17;
const NOTSUPP: u32 = 10004;

#[derive(Default)]
struct Args(Vec<u8>);

impl Args {
    fn new() -> Self {
        Args(Vec::new())
    }
    fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u64(mut self, v: u64) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    /// An XDR length-prefixed opaque, padded to four bytes.
    fn opaque(mut self, v: &[u8]) -> Self {
        self = self.u32(v.len() as u32);
        self.0.extend_from_slice(v);
        while !self.0.len().is_multiple_of(4) {
            self.0.push(0);
        }
        self
    }
}

fn ru32(r: &mut Cursor<Vec<u8>>) -> u32 {
    let mut b = [0u8; 4];
    r.read_exact(&mut b).unwrap();
    u32::from_be_bytes(b)
}

fn rb(r: &mut Cursor<Vec<u8>>) -> Vec<u8> {
    let n = ru32(r) as usize;
    let mut b = vec![0u8; n];
    r.read_exact(&mut b).unwrap();
    let pad = (4 - (n % 4)) % 4;
    let mut p = [0u8; 4];
    r.read_exact(&mut p[..pad]).unwrap();
    b
}

fn rbool(r: &mut Cursor<Vec<u8>>) -> bool {
    ru32(r) != 0
}

fn ru64(r: &mut Cursor<Vec<u8>>) -> u64 {
    let mut b = [0u8; 8];
    r.read_exact(&mut b).unwrap();
    u64::from_be_bytes(b)
}

/// Skip `n` words ahead in a reply body.
fn skip_fattr_words(r: &mut Cursor<Vec<u8>>, n: usize) {
    for _ in 0..n {
        ru32(r);
    }
}

/// A `post_op_fh3`: a present flag then, when set, a length-prefixed handle. CREATE and MKDIR
/// return this, unlike LOOKUP which returns a bare `nfs_fh3`.
fn rfh_post(r: &mut Cursor<Vec<u8>>) -> Vec<u8> {
    if !rbool(r) {
        return Vec::new();
    }
    rb(r)
}

/// `sattr3` with only the mode set and every other field void, exactly as `nfsserve` XDR-codes it:
/// each of mode/uid/gid/size is a bool-union (`1, value` or `0`), and atime/mtime are a plain enum
/// (`0` is `DONT_CHANGE`).
fn sattr_mode(mut a: Args, mode: u32) -> Args {
    a = a.u32(1).u32(mode); // set_mode3::mode
    a = a.u32(0).u32(0).u32(0); // uid, gid, size void
    a.u32(0).u32(0) // atime, mtime DONT_CHANGE
}

/// One connection to one server. Enough of the client to mount, look up, create, mkdir, write, read.
struct Client {
    s: TcpStream,
    xid: u32,
    root: Vec<u8>,
}

impl Client {
    fn connect(port: u16, export: &str) -> Client {
        let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_nodelay(true).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let mut c = Client {
            s,
            xid: 0,
            root: Vec::new(),
        };
        // `mount_nfs` is told `localhost:/<export_name>`; the server matches the leading slash.
        let (st, root) = c.mount(&format!("/{export}"));
        assert_eq!(st, 0, "MNT refused");
        c.root = root;
        c
    }

    fn send(&mut self, m: &[u8]) {
        self.s
            .write_all(&((m.len() as u32) | (1 << 31)).to_be_bytes())
            .unwrap();
        self.s.write_all(m).unwrap();
    }

    fn recv(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let mut h = [0u8; 4];
            self.s.read_exact(&mut h).unwrap();
            let h = u32::from_be_bytes(h);
            let mut b = vec![0u8; (h & 0x7fff_ffff) as usize];
            self.s.read_exact(&mut b).unwrap();
            out.extend_from_slice(&b);
            if h >> 31 == 1 {
                return out;
            }
        }
    }

    fn raw(&mut self, prog: u32, proc: u32, args: Args) -> (u32, Cursor<Vec<u8>>) {
        self.xid += 1;
        let mut m = Vec::new();
        for w in [self.xid, 0, 2, prog, 3, proc, 0, 0, 0, 0] {
            m.extend_from_slice(&w.to_be_bytes());
        }
        m.extend_from_slice(&args.0);
        self.send(&m);
        let mut r = Cursor::new(self.recv());
        assert_eq!(ru32(&mut r), self.xid);
        assert_eq!(ru32(&mut r), 1, "reply");
        // Accept body: accepted(0) is a separate word, then verifier flavour and verifier, and only
        // then the accept_stat the program body follows.
        let accepted = ru32(&mut r);
        let _flavor = ru32(&mut r);
        let _verf = rb(&mut r);
        assert_eq!(accepted, 0, "RPC not accepted");
        (ru32(&mut r), r)
    }

    fn call(&mut self, proc: u32, args: Args) -> (u32, Cursor<Vec<u8>>) {
        let (acc, mut r) = self.raw(NFS_PROG, proc, args);
        assert_eq!(acc, 0, "RPC not accepted");
        (ru32(&mut r), r)
    }

    fn mount(&mut self, export: &str) -> (u32, Vec<u8>) {
        self.xid += 1;
        let mut m = Vec::new();
        for w in [self.xid, 0, 2, MOUNT_PROG, 3, 1, 0, 0, 0, 0] {
            m.extend_from_slice(&w.to_be_bytes());
        }
        m.extend_from_slice(&Args::new().opaque(export.as_bytes()).0);
        self.send(&m);
        let mut r = Cursor::new(self.recv());
        assert_eq!(ru32(&mut r), self.xid);
        assert_eq!(ru32(&mut r), 1, "reply");
        assert_eq!(ru32(&mut r), 0, "accepted");
        let _flavor = ru32(&mut r);
        let _verf = rb(&mut r);
        assert_eq!(ru32(&mut r), 0, "accept_stat"); // SUCCESS
        let st = ru32(&mut r);
        let root = if st == 0 { rb(&mut r) } else { Vec::new() };
        (st, root)
    }

    fn fh(&self) -> Vec<u8> {
        self.root.clone()
    }

    fn dirop(&self, name: &str) -> Args {
        Args::new().opaque(&self.root).opaque(name.as_bytes())
    }

    /// LOOKUP: (status, handle, kind).
    fn lookup(&mut self, name: &str) -> (u32, Vec<u8>, Option<u32>) {
        let (st, mut r) = self.call(3, self.dirop(name));
        if st != OK {
            return (st, Vec::new(), None);
        }
        let fh = rb(&mut r);
        let kind = skip_and_kind(&mut r);
        (st, fh, kind)
    }

    /// CREATE, guarded (mode 1). Returns (status, handle).
    fn create(&mut self, name: &str, mode: u32) -> (u32, Vec<u8>) {
        let mut a = self.dirop(name).u32(1); // guard (UNCHECKED=0, GUARDED=1, EXCLUSIVE=2)
        a = sattr_mode(a, mode);
        let (st, mut r) = self.call(8, a);
        if st != OK {
            return (st, Vec::new());
        }
        let fh = rfh_post(&mut r);
        (st, fh)
    }

    /// MKDIR: (status, handle).
    fn mkdir(&mut self, name: &str) -> (u32, Vec<u8>) {
        let a = sattr_mode(self.dirop(name), 0o755);
        let (st, mut r) = self.call(9, a);
        if st != OK {
            return (st, Vec::new());
        }
        let fh = rfh_post(&mut r);
        (st, fh)
    }

    fn write(&mut self, fh: &[u8], off: u64, data: &[u8]) -> u32 {
        let a = Args::new()
            .opaque(fh)
            .u64(off)
            .u32(data.len() as u32)
            .u32(0)
            .opaque(data);
        let (st, mut r) = self.call(7, a);
        if st != OK {
            return st;
        }
        let _wcc = skip_wcc(&mut r);
        let _count = ru32(&mut r);
        let _committed = ru32(&mut r);
        st
    }

    fn read(&mut self, fh: &[u8], off: u64, count: u32) -> (u32, Vec<u8>) {
        let (st, mut r) = self.call(6, Args::new().opaque(fh).u64(off).u32(count));
        if st != OK {
            return (st, Vec::new());
        }
        let _attr = skip_post_op_attr(&mut r);
        let _n = ru32(&mut r);
        let _eof = rbool(&mut r);
        (st, rb(&mut r))
    }

    fn getattr(&mut self, fh: &[u8]) -> (u32, Option<u32>) {
        let (st, mut r) = self.call(1, Args::new().opaque(fh));
        if st != OK {
            return (st, None);
        }
        // GETATTR returns a bare `fattr3`, not a `post_op_attr`.
        (st, Some(skip_fattr(&mut r)))
    }

    /// GETATTR's `fileid3`, the identity the two adapters must agree on for one shared name.
    ///
    /// `fattr3` is `ftype, mode, nlink, uid, gid, size(u64), used(u64), rdev(2 words), fsid(u64),
    /// fileid(u64), ...`, so `fileid3` starts at word 13. Skipping 11 would read `fsid`, a constant
    /// `FSID` for the whole file system, and any two handles would compare equal.
    fn fileid(&mut self, fh: &[u8]) -> u64 {
        let (st, mut r) = self.call(1, Args::new().opaque(fh));
        assert_eq!(st, OK, "getattr for the file id");
        skip_fattr_words(&mut r, 13);
        ru64(&mut r)
    }
}

/// fattr3: type, mode, nlink, uid, gid, size, used, rdev(specdata3), fsid, fileid, 3 times. 21 u32.
fn skip_fattr(r: &mut Cursor<Vec<u8>>) -> u32 {
    let kind = ru32(r);
    for _ in 0..20 {
        ru32(r);
    }
    kind
}

fn skip_and_kind(r: &mut Cursor<Vec<u8>>) -> Option<u32> {
    rbool(r).then(|| skip_fattr(r))
}

#[test]
fn lookup_post_op_attr_decodes_kind_after_the_present_flag() {
    for kind in [FTYPE_DIR, FTYPE_REG] {
        let mut bytes = Vec::new();
        for word in [1u32, kind].into_iter().chain([0; 20]) {
            bytes.extend_from_slice(&word.to_be_bytes());
        }
        bytes.extend_from_slice(&0x12345678u32.to_be_bytes());
        let mut reply = Cursor::new(bytes);
        assert_eq!(skip_and_kind(&mut reply), Some(kind));
        assert_eq!(ru32(&mut reply), 0x12345678);
    }
    let mut reply = Cursor::new(0u32.to_be_bytes().to_vec());
    assert_eq!(skip_and_kind(&mut reply), None);
    assert_eq!(reply.position(), 4);
}

fn skip_post_op_attr(r: &mut Cursor<Vec<u8>>) -> bool {
    let present = rbool(r);
    if present {
        skip_fattr(r);
    }
    present
}

/// wcc_data: before (pre_op_attr: bool + size/mtime/ctime = 7 words when present) + after post_op_attr.
fn skip_pre_op_attr(r: &mut Cursor<Vec<u8>>) -> bool {
    let present = rbool(r);
    if present {
        for _ in 0..6 {
            ru32(r);
        }
    }
    present
}

fn skip_wcc(r: &mut Cursor<Vec<u8>>) -> bool {
    let b = skip_pre_op_attr(r);
    skip_post_op_attr(r);
    b
}

const FTYPE_DIR: u32 = 2;
const FTYPE_REG: u32 = 1;

/// The one `Vfs` a single adapter serves: a `Core::snapshot_view` of snapshot `s`, with a barrier
/// that holds adapter A's mutation of `._doc` after its guard read of `doc` has answered.
///
/// The wrapper sits at the `Vfs` boundary the adapter shares with the core, so a hold here is inside
/// the adapter's guard window, not below it. It also counts how many guarded name-space operations
/// are inside the adapter at once, across every adapter over this one backend: a diagnostic only.
struct GuardedView {
    inner: Arc<dyn Vfs>,
    depth: AtomicUsize,
    peak: AtomicUsize,
    /// Whether the adapter's own guard read of the main name saw it present.
    guard_saw_main: AtomicU8,
    /// Whether the main name was present at the instant the sidecar mutation ran.
    main_present_at_mutation: AtomicU8,
    hold: Mutex<HoldState>,
    ready: Condvar,
}

struct HoldState {
    armed: Option<Vec<u8>>,
    reached: bool,
    released: bool,
}

impl GuardedView {
    fn new(inner: Arc<dyn Vfs>) -> Arc<GuardedView> {
        Arc::new(GuardedView {
            inner,
            depth: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            guard_saw_main: AtomicU8::new(0),
            main_present_at_mutation: AtomicU8::new(0),
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

    /// The adapter's own guard read of `doc`: did it see the main name present?
    fn guard_saw_main(&self) -> bool {
        self.guard_saw_main.load(Ordering::SeqCst) != 0
    }

    /// Ground truth at the mutation: was `doc` a live regular file?
    fn main_present_at_mutation(&self) -> bool {
        self.main_present_at_mutation.load(Ordering::SeqCst) != 0
    }

    fn wait_peak(&self, want: usize, millis: u64) -> bool {
        let deadline = Instant::now() + Duration::from_millis(millis);
        while self.peak() < want {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        true
    }

    fn wait_reached(&self, secs: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        let mut s = self.hold.lock().unwrap();
        while !s.reached {
            let left = deadline.saturating_duration_since(Instant::now());
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
        let deadline = Instant::now() + Duration::from_secs(10);
        while !s.released {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return true;
            }
            let (g, _) = self.ready.wait_timeout(s, left).unwrap();
            s = g;
        }
        true
    }
}

impl Vfs for GuardedView {
    fn lookup(&self, p: Ino, n: &[u8]) -> cowfs_vfs::Result<Attr> {
        let r = self.inner.lookup(p, n);
        // Ground truth for the guard read: did the adapter's own guard read see the main name?
        if n == MAIN {
            self.guard_saw_main
                .store(u8::from(r.is_ok()), Ordering::SeqCst);
        }
        r
    }
    fn forget(&self, i: Ino, c: u64) {
        self.inner.forget(i, c)
    }
    fn getattr(&self, i: Ino) -> cowfs_vfs::Result<Attr> {
        self.inner.getattr(i)
    }
    fn setattr(&self, i: Ino, c: SetAttr) -> cowfs_vfs::Result<Attr> {
        self.inner.setattr(i, c)
    }
    fn readlink(&self, i: Ino) -> cowfs_vfs::Result<Vec<u8>> {
        self.inner.readlink(i)
    }
    fn create(&self, p: Ino, n: &[u8], m: u32) -> cowfs_vfs::Result<Attr> {
        self.enter();
        let r = self.inner.create(p, n, m);
        self.leave();
        r
    }
    fn mkdir(&self, p: Ino, n: &[u8], m: u32) -> cowfs_vfs::Result<Attr> {
        self.enter();
        self.hold_if_armed(n);
        // The instant the mutation runs: ground truth for whether the main name is live now.
        if n == SIDE {
            if let Ok(a) = self.inner.lookup(p, MAIN) {
                self.main_present_at_mutation
                    .store(u8::from(a.kind == FileKind::Regular), Ordering::SeqCst);
            }
        }
        let r = self.inner.mkdir(p, n, m);
        self.leave();
        r
    }
    fn symlink(&self, p: Ino, n: &[u8], t: &[u8]) -> cowfs_vfs::Result<Attr> {
        self.enter();
        let r = self.inner.symlink(p, n, t);
        self.leave();
        r
    }
    fn link(&self, i: Ino, np: Ino, nn: &[u8]) -> cowfs_vfs::Result<Attr> {
        self.enter();
        let r = self.inner.link(i, np, nn);
        self.leave();
        r
    }
    fn unlink(&self, p: Ino, n: &[u8]) -> cowfs_vfs::Result<()> {
        self.inner.unlink(p, n)
    }
    fn rmdir(&self, p: Ino, n: &[u8]) -> cowfs_vfs::Result<()> {
        self.inner.rmdir(p, n)
    }
    fn rename(
        &self,
        p: Ino,
        n: &[u8],
        np: Ino,
        nn: &[u8],
        f: RenameFlags,
    ) -> cowfs_vfs::Result<()> {
        self.inner.rename(p, n, np, nn, f)
    }
    fn open(&self, i: Ino) -> cowfs_vfs::Result<FileHandle> {
        self.inner.open(i)
    }
    fn release(&self, h: FileHandle) -> cowfs_vfs::Result<()> {
        self.inner.release(h)
    }
    fn read(&self, i: Ino, o: u64, n: u32) -> cowfs_vfs::Result<Vec<u8>> {
        self.inner.read(i, o, n)
    }
    fn write(&self, i: Ino, o: u64, d: &[u8]) -> cowfs_vfs::Result<u32> {
        self.inner.write(i, o, d)
    }
    fn flush(&self, i: Ino) -> cowfs_vfs::Result<()> {
        self.inner.flush(i)
    }
    fn fsync(&self, i: Ino, d: bool) -> cowfs_vfs::Result<()> {
        self.inner.fsync(i, d)
    }
    fn sync_namespace(&self, i: Ino) -> cowfs_vfs::Result<()> {
        self.inner.sync_namespace(i)
    }
    fn readdir(&self, d: Ino, c: u64, m: usize) -> cowfs_vfs::Result<ReadDir> {
        self.inner.readdir(d, c, m)
    }
    fn readdir_attrs(&self, d: Ino, c: u64, m: usize) -> cowfs_vfs::Result<ReadDirPlus> {
        self.inner.readdir_attrs(d, c, m)
    }
    fn statfs(&self) -> cowfs_vfs::Result<StatFs> {
        self.inner.statfs()
    }
    fn getxattr(&self, i: Ino, n: &[u8]) -> cowfs_vfs::Result<Vec<u8>> {
        self.inner.getxattr(i, n)
    }
    fn setxattr(&self, i: Ino, n: &[u8], v: &[u8], f: XattrFlags) -> cowfs_vfs::Result<()> {
        self.inner.setxattr(i, n, v, f)
    }
    fn listxattr(&self, i: Ino) -> cowfs_vfs::Result<Vec<Vec<u8>>> {
        self.inner.listxattr(i)
    }
    fn removexattr(&self, i: Ino, n: &[u8]) -> cowfs_vfs::Result<()> {
        self.inner.removexattr(i, n)
    }
    fn fallocate(
        &self,
        i: Ino,
        m: cowfs_vfs::FallocMode,
        o: u64,
        l: u64,
    ) -> cowfs_vfs::Result<Attr> {
        self.inner.fallocate(i, m, o, l)
    }
}

fn translated() -> MountOptions {
    MountOptions {
        appledouble: AppleDoubleMode::Translate,
        one_shot_mount: false,
        check_peer_uid: false,
        ..MountOptions::default()
    }
}

/// The normalised, user-observable product of one scenario, captured through real RPCs on a real
/// `Core`. Every field is a fact the product returns to a client, never an internal probe.
///
/// Numeric `fileid`s are compared only *within* one run (the two adapters must agree on one shared
/// name's id); they are never compared across runs, because two fresh stores assign independently.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Observation {
    /// Exact RPC status of `mkdir(._doc)`.
    mkdir_status: u32,
    /// Exact RPC status of `create(doc)`.
    create_status: u32,
    /// Whether the `mkdir(._doc)` reply carried a file handle.
    mkdir_handle: bool,
    /// Whether the `create(doc)` reply carried a file handle.
    create_handle: bool,
    /// `ftype` of `doc` after both requests, read by LOOKUP through an adapter.
    main_kind_rpc: Option<u32>,
    /// `ftype` of `._doc` after both requests, read by LOOKUP through an adapter.
    side_kind_rpc: Option<u32>,
    /// `doc`'s `fileid3` re-read after the requests stayed the same (identity is stable).
    main_identity_stable: bool,
    /// `doc` written through adapter A and read back through adapter B, resolving MAIN through
    /// *B's own* root: B's handle for the same inode, then the exact bytes. A is written through
    /// A's handle; B never decodes A's handle.
    main_cross_adapter_bytes: bool,
    /// When `._doc` is a live sidecar channel (`ftype` regular): a valid AppleDouble image written to
    /// it reads back unchanged. `None` when `._doc` is a real directory, so no channel exists.
    channel_roundtrips: Option<bool>,
    /// When the channel exists: junk is refused with `NOTSUPP` and the channel still reads the good
    /// bytes afterwards. `None` when there is no channel.
    channel_refuses_junk: Option<bool>,
}

impl Observation {
    /// Human-readable fields where the race and a serial history disagree, for the failure message.
    fn diff(&self, other: &Observation) -> Vec<String> {
        let mut d = Vec::new();
        macro_rules! cmp {
            ($f:ident) => {
                if self.$f != other.$f {
                    d.push(format!(
                        "{}: race={:?} serial={:?}",
                        stringify!($f),
                        self.$f,
                        other.$f
                    ));
                }
            };
        }
        cmp!(mkdir_status);
        cmp!(create_status);
        cmp!(mkdir_handle);
        cmp!(create_handle);
        cmp!(main_kind_rpc);
        cmp!(side_kind_rpc);
        cmp!(main_identity_stable);
        cmp!(main_cross_adapter_bytes);
        cmp!(channel_roundtrips);
        cmp!(channel_refuses_junk);
        d
    }
}

/// What to write into `doc` and read back through the *other* adapter, so the bytes prove one shared
/// namespace rather than a per-adapter cache.
const DOC_BYTES: &[u8] = b"cowfs-separate-adapter-doc-payload";

/// Run one scenario on a fresh real `Core`: two `snapshot_view`s of one snapshot, two `Server`s, two
/// raw-NFS clients. `mkdir_first` chooses the serial order; `race` runs the two requests concurrently
/// with adapter A held at its sidecar mutation after its guard read of `doc` has answered.
fn observe(mkdir_first: bool, race: bool) -> Observation {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), Options::default()).expect("open a core over a fresh store");
    core.create_snapshot("s").expect("create snapshot s");

    let view_a = core.snapshot_view("s").expect("snapshot view a");
    let view_b = core.snapshot_view("s").expect("snapshot view b");
    let a = GuardedView::new(Arc::new(view_a) as Arc<dyn Vfs>);
    let b = GuardedView::new(Arc::new(view_b) as Arc<dyn Vfs>);

    let server_a = Server::start(a.clone() as Arc<dyn Vfs>, &translated(), None).expect("server a");
    let server_b = Server::start(b.clone() as Arc<dyn Vfs>, &translated(), None).expect("server b");

    let mut ca = Client::connect(server_a.port(), server_a.export_name());
    let mut cb = Client::connect(server_b.port(), server_b.export_name());
    let root_a = ca.fh();
    let root_b = cb.fh();

    let (mkdir_status, mkdir_handle, create_status, create_handle) = if !race {
        // A serial history: the two requests run one after the other, no hold, no overlap.
        if mkdir_first {
            let (ms, mfh) = ca.mkdir("._doc");
            let (cs, cfh) = cb.create("doc", 0o644);
            (ms, !mfh.is_empty(), cs, !cfh.is_empty())
        } else {
            let (cs, cfh) = cb.create("doc", 0o644);
            let (ms, mfh) = ca.mkdir("._doc");
            (ms, !mfh.is_empty(), cs, !cfh.is_empty())
        }
    } else {
        // The controlled race. Hold adapter A at its mutation of `._doc` after its guard read of
        // `doc` has answered; run B's `create(doc)` inside that window.
        a.arm(SIDE);
        let mk = {
            let root = root_a.clone();
            let port = server_a.port();
            let export = server_a.export_name().to_string();
            std::thread::spawn(move || {
                let mut c = Client::connect(port, &export);
                c.root = root;
                c.mkdir("._doc")
            })
        };
        assert!(
            a.wait_reached(10),
            "adapter A never reached its sidecar mutation; the window could not be opened"
        );
        let created = {
            let root = root_b.clone();
            let port = server_b.port();
            let export = server_b.export_name().to_string();
            std::thread::spawn(move || {
                let mut c = Client::connect(port, &export);
                c.root = root;
                c.create("doc", 0o644)
            })
        };
        a.release();
        let mk = mk.join().unwrap();
        let created = created.join().unwrap();
        (mk.0, !mk.1.is_empty(), created.0, !created.1.is_empty())
    };

    // Diagnostics only: did the two guarded operations overlap inside the adapters. Never an
    // acceptance constraint: a legal arrangement may show either. Only meaningful under the race.
    let overlapped = race && a.wait_peak(2, 1000);

    // User-observable final state, read by LOOKUP through adapter A (a real RPC), plus the doc
    // handle from a fresh LOOKUP so its `fileid` is compared within this run, not across runs.
    let (main_kind_rpc, main_fh) = match ca.lookup("doc") {
        (OK, fh, Some(k)) => (Some(k), fh),
        (_, _, k) => (k, Vec::new()),
    };
    let (side_kind_rpc, side_fh) = match ca.lookup("._doc") {
        (OK, fh, Some(k)) => (Some(k), fh),
        (_, _, k) => (k, Vec::new()),
    };

    // Identity within one run: the same name's `fileid3` must be stable across two GETATTRs.
    let main_identity_stable = if main_fh.is_empty() {
        false
    } else {
        let id1 = ca.fileid(&main_fh);
        let id2 = ca.fileid(&main_fh);
        id1 == id2
    };

    // Cross-adapter byte proof without crossing capability: resolve `doc` through *B's own* root so
    // B holds B's valid handle for the shared inode; write through A's handle, read through B's.
    // B never decodes A's opaque handle (each adapter mints its own BLAKE3 key). The two adapters
    // must also agree on the shared inode id, so a per-adapter cache cannot masquerade as one
    // namespace. Absent in both the failing-create and the MAIN-absent failed-mkdir cases, where the
    // byte claim does not apply.
    let main_cross_adapter_bytes = match (ca.lookup("doc"), cb.lookup("doc")) {
        ((OK, a_fh, Some(_)), (OK, b_fh, Some(_))) => {
            let wrote = ca.write(&a_fh, 0, DOC_BYTES);
            let (rs, got) = cb.read(&b_fh, 0, 1 << 16);
            wrote == OK && rs == OK && got == DOC_BYTES && ca.fileid(&a_fh) == cb.fileid(&b_fh)
        }
        _ => false,
    };

    // The sidecar channel exists only when `._doc` is a live view (a regular file), not a real
    // directory. When it does, a valid AppleDouble image must round-trip through the handle the
    // product returned for that name, and junk must be refused with NOTSUPP, non-destructively.
    // The channel is inherent to a main file, which both adapters can see, so a valid sidecar
    // resolved through B's own root tolerates a MAIN-name arrangement; elsewhere it stays A's.
    let side_is_channel = side_kind_rpc == Some(FTYPE_REG);
    let blob = valid_sidecar();
    let (side_write_st, side_got, side_junk_st, side_recovered) =
        if side_is_channel && !side_fh.is_empty() {
            let slot = ca.write(&side_fh, 0, &blob);
            let got = match cb.lookup("._doc") {
                (OK, b_side, Some(FTYPE_REG)) if !b_side.is_empty() => {
                    match cb.read(&b_side, 0, 1 << 16) {
                        (OK, g) => Some(g),
                        _ => None,
                    }
                }
                _ => match ca.read(&side_fh, 0, 1 << 16) {
                    (OK, g) => Some(g),
                    _ => None,
                },
            };
            let junk = vec![0u8; 4096];
            let junk_st = ca.write(&side_fh, 0, &junk);
            // The refused junk write must never corrupt the good bytes: read back (A's handle).
            let (rs2, got2) = ca.read(&side_fh, 0, 1 << 16);
            let recovered = rs2 == OK && got2 == blob;
            (Some(slot), got, Some(junk_st), recovered)
        } else {
            (None, None, None, false)
        };

    // The sidecar oracle compares booleans, never raw statuses: an absent channel reads as `None`,
    // matching the serial histories, rather than a raw RPC failure that would falsely diverge.
    let channel_roundtrips =
        side_write_st.map(|st| st == OK && side_got.as_deref() == Some(&blob[..]));
    let channel_refuses_junk = side_junk_st.map(|st| {
        st == NOTSUPP
            && matches!(side_got.as_deref(), Some(g) if g == blob.as_slice())
            && side_recovered
    });

    let obs = Observation {
        mkdir_status,
        create_status,
        mkdir_handle,
        create_handle,
        main_kind_rpc,
        side_kind_rpc,
        main_identity_stable,
        main_cross_adapter_bytes,
        channel_roundtrips,
        channel_refuses_junk,
    };

    // Diagnostics only, never acceptance: the overlap gauge, the funnel depth, and the two
    // mid-operation probes. None of these are the pass criterion.
    eprintln!(
        "REAL-CORE race={race} mkdir_first={mkdir_first} obs={obs:?} \
         overlapped={overlapped} guard_saw_main={} main_at_mutation={} peak_depth={}",
        a.guard_saw_main(),
        a.main_present_at_mutation(),
        a.peak()
    );

    obs
}

/// The separate-adapter race against the serial oracle, driven through a real `Core`.
///
/// Three runs: the two actual serial histories and the race. The race must equal at least one serial
/// history on every user-observable field. A run that matches neither is the counterexample; the
/// assertion reports both serial observations it was compared against, so the failure is the actual
/// returned statuses and kinds, not a label.
///
/// This does not assert the race matched a *particular* serial history: if the request interval
/// overlaps, `mkdir` cannot be forced to linearize at the mutation instant, and both serial orders
/// are legal products. It asserts only that the race landed on one of the two real serial products.
#[test]
fn two_adapters_over_one_snapshot_namespace_match_a_serial_product_under_the_race() {
    // Each serial history is a fresh store, so its numeric `fileid`s never cross-compare.
    let serial_mkdir_first = observe(true, false);
    let serial_create_first = observe(false, false);
    let race = observe(true, true);

    let vs_mkdir = race.diff(&serial_mkdir_first);
    let vs_create = race.diff(&serial_create_first);

    assert!(
        vs_mkdir.is_empty() || vs_create.is_empty(),
        "the race produced a state matching NEITHER actual serial product. \
         vs mkdir-first {vs_mkdir:?}; vs create-first {vs_create:?}. \
         race={race:?} mkdir-first={serial_mkdir_first:?} create-first={serial_create_first:?}"
    );

    // A `doc` must exist as a regular file in both legal products, so the race observation must too
    // if it matched one of them. This is implied by the equality above; kept as a direct statement.
    assert_eq!(
        race.main_kind_rpc,
        Some(FTYPE_REG),
        "doc is not a regular file after the race: {race:?}"
    );
    // Whichever legal product the race matched, `._doc` is either a real directory (mkdir-first) or a
    // live regular-file view (create-first); it is never absent or a third kind.
    assert!(
        matches!(race.side_kind_rpc, Some(FTYPE_DIR) | Some(FTYPE_REG)),
        "._doc is neither a real directory nor a live view after the race: {race:?}"
    );

    // The successful serial history whose `create(doc)` landed must actually *show* one shared
    // namespace: MAIN resolves through both adapters, writing through A and reading through B's own
    // handle returns the exact bytes, and the two adapters agree on the shared inode id. If this
    // were a bug it could never hold; asserting it TRUE stops the oracle from passing on the
    // always-false equality (false == false). The `MAIN`-absent failed-mkdir product legitimately
    // has no `doc` and no byte claim, so it is exempt.
    let successful_serial = if serial_mkdir_first.create_status == OK {
        (serial_mkdir_first, "mkdir-first")
    } else {
        (serial_create_first, "create-first")
    };
    assert_eq!(
        successful_serial.0.main_kind_rpc,
        Some(FTYPE_REG),
        "the successful serial history ({}) did not leave doc a regular file: {:?}",
        successful_serial.1,
        successful_serial.0
    );
    assert!(
        successful_serial.0.main_cross_adapter_bytes,
        "the successful serial history ({}) did not prove the byte round-trip through B's own handle: {:?}",
        successful_serial.1,
        successful_serial.0
    );
    assert!(
        successful_serial.0.main_identity_stable,
        "the successful serial history ({}) reported an unstable doc fileid: {:?}",
        successful_serial.1, successful_serial.0
    );
    assert!(
        race.main_cross_adapter_bytes,
        "the race did not prove the cross-adapter byte round-trip it must share with a legal product: {race:?}"
    );
    assert!(
        race.main_identity_stable,
        "the race reported an unstable doc fileid across two GETATTRs: {race:?}"
    );
    // Since the byte proof is TRUE in the successful serial product, the race matching it must also
    // be TRUE; a `false == false` coincidence is impossible.
    if let Some(channel) = successful_serial.0.channel_roundtrips {
        assert!(
            channel,
            "the successful serial history ({}) did not round-trip a valid sidecar through the channel: {:?}",
            successful_serial.1,
            successful_serial.0
        );
    }
    if let Some(refused) = race.channel_refuses_junk {
        assert!(
            refused,
            "the race channel did not refuse junk non-destructively: {race:?}"
        );
    }
}

/// The premise: the two adapters really do reach one namespace through the one `Core`. A file
/// created on one is visible on the other with the same file id, so the race test above cannot pass
/// on disjoint state.
#[test]
fn the_two_adapters_share_one_snapshot_namespace() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), Options::default()).unwrap();
    core.create_snapshot("s").unwrap();
    let view_a = core.snapshot_view("s").unwrap();
    let view_b = core.snapshot_view("s").unwrap();
    let server_a = Server::start(Arc::new(view_a) as Arc<dyn Vfs>, &translated(), None).unwrap();
    let server_b = Server::start(Arc::new(view_b) as Arc<dyn Vfs>, &translated(), None).unwrap();
    let mut ca = Client::connect(server_a.port(), server_a.export_name());
    let mut cb = Client::connect(server_b.port(), server_b.export_name());

    let (probe_st, _probe_fh, _probe_kind) = ca.lookup("nonexistent");
    assert_ne!(
        probe_st, 10001,
        "the MNT root handle is not accepted by LOOKUP"
    );

    let (made_st, made) = ca.create("doc", 0o644);
    assert_eq!(made_st, OK, "adapter A creates doc");
    let (seen_st, seen, _) = cb.lookup("doc");
    assert_eq!(seen_st, OK, "adapter B sees doc written through adapter A");

    // One shared name, one identity: the file id each adapter reports for `doc` is the same. The
    // raw handles differ because each server mints handles with its own MAC key, so the file id is
    // the identity to compare, not the handle bytes.
    let id_a = ca.fileid(&made);
    let id_b = cb.fileid(&seen);
    assert_eq!(
        id_a, id_b,
        "the two adapters did not agree on the file id of one shared name"
    );

    // The root facade of the same Core also sees it: the snapshot's namespace is one.
    let vfs = core.snapshot_view("s").unwrap();
    assert!(
        vfs.lookup(ROOT_INO, MAIN).is_ok(),
        "the Core view does not see doc"
    );
}

/// The `Core` ROOT facade (`Backend::root` = the `Core` itself, whose root lists snapshots) refuses
/// a real mkdir at the true root: `Core::make(parent=ROOT_INO)` is `ReadOnly`. This pins that the
/// fixture's writes go through a `snapshot_view` (the export path), not the read-only root, so the
/// test cannot pass by a name landing somewhere the product never writes.
#[test]
fn the_core_root_facade_is_read_only_for_make() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), Options::default()).unwrap();
    core.create_snapshot("s").unwrap();
    let err = core.mkdir(ROOT_INO, b"x", 0o755);
    assert!(
        err.is_err(),
        "mkdir at the core root must be refused (the root lists snapshots, it is read-only)"
    );
    // The snapshot's own view root is writable: that is where exports land.
    let vfs = core.snapshot_view("s").unwrap();
    assert!(
        vfs.mkdir(ROOT_INO, b"x", 0o755).is_ok(),
        "a snapshot view's root must accept a directory"
    );
}

/// A valid AppleDouble image over the translating sidecar channel: the form a macOS client writes.
/// The bytes must round-trip through the real Core-backed adapter unchanged, and junk must be
/// refused without corrupting the channel.
#[test]
fn the_sidecar_channel_round_trips_valid_bytes_and_refuses_junk() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), Options::default()).unwrap();
    core.create_snapshot("s").unwrap();
    let vfs = core.snapshot_view("s").unwrap();
    let server = Server::start(Arc::new(vfs) as Arc<dyn Vfs>, &translated(), None).unwrap();
    let mut c = Client::connect(server.port(), server.export_name());

    let (doc_st, _doc_fh) = c.create("doc", 0o644);
    assert_eq!(doc_st, OK, "create(doc)");
    let (ch, fh) = c.create("._doc", 0o600);
    assert_eq!(ch, OK, "the translating sidecar channel did not open");

    // A plausible AppleDouble image: a macOS-written `._doc` always starts with the magic.
    let blob = valid_sidecar();
    assert_eq!(
        c.write(&fh, 0, &blob),
        OK,
        "the channel rejected valid bytes"
    );
    let (rs, got) = c.read(&fh, 0, 1 << 16);
    assert_eq!(rs, OK, "the channel rejected a read");
    assert_eq!(got, blob, "the sidecar bytes did not survive");

    // Junk can never be a sidecar. A whole-file write at offset 0 whose bytes have no plausible
    // AppleDouble prefix is refused with NOTSUPP, so a real file under a reserved name is never
    // accepted and then dropped. This runs on the same live sidecar channel.
    let junk = vec![0u8; 4096];
    assert_eq!(
        c.write(&fh, 0, &junk),
        NOTSUPP,
        "implausible bytes were not refused with NFS3ERR_NOTSUPP"
    );

    // The channel still works after the refusal.
    let (rs2, got2) = c.read(&fh, 0, 1 << 16);
    assert_eq!(rs2, OK, "the channel read failed after the refusal");
    assert_eq!(got2, blob, "the channel did not recover after the refusal");

    // A refused junk write never leaves a real directory under the reserved name: the channel view
    // stays a regular file.
    let (kst, kind) = c.getattr(&fh);
    assert_eq!(kst, OK, "getattr on the channel");
    assert_eq!(kind, Some(FTYPE_REG), "the channel is not a regular file");

    let _ = (EXIST, FTYPE_DIR, doc_st);
}

/// A valid AppleDouble image from the public encoder, matching the accepted `namespace_race.rs`
/// fixture. The daemon crate cannot call `Sidecar::encode` (that type is in `cowfs-nfs`, reachable)
/// so this is the exact minimal AppleDouble layout the client writes for one plain xattr.
fn valid_sidecar() -> Vec<u8> {
    // `cowfs_nfs::Sidecar` is public, so build through the existing encoder rather than hand-roll it.
    cowfs_nfs::Sidecar::from_xattrs([(b"user.k".to_vec(), vec![7u8; 100])]).encode()
}
