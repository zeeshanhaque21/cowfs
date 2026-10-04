//! The durability contract of the NFS transport: what the adapter makes durable before it
//! acknowledges a namespace RPC, and what it deliberately leaves alone. Issue #90.
//!
//! The crash itself is `cowfs-daemon`'s `namespace_durability` test, over a real mount. These are
//! the same claim at the seam, so a mistake here is caught without a mount, in seconds, and the
//! two together pin the order: the barrier follows the mutation it is making durable, and a
//! barrier that fails is an error rather than a success.
mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use common::*;
use cowfs_vfs::{
    Attr, Error, FileHandle, ReadDir, ReadDirPlus, Result, SetAttr, StatFs, Vfs, XattrFlags,
};
use cowfs_vfs_test::MemVfs;
use nfsserve::nfs::sattr3;

/// One event the server asked the `Vfs` for, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    Fsync(u64),
    SyncNs(u64),
    Rename,
    Create,
    Unlink,
    Write,
}

/// A `MemVfs` that records the durability calls it was asked for and can be told to fail the
/// namespace barrier only, so the failure cannot be confused with a failed mutation.
struct Watched {
    inner: Arc<MemVfs>,
    calls: Mutex<Vec<Call>>,
    fail_ns: AtomicBool,
    fail_mutation: AtomicBool,
}

impl Watched {
    fn new() -> Arc<Watched> {
        Arc::new(Watched {
            inner: Arc::new(MemVfs::new()),
            calls: Mutex::new(Vec::new()),
            fail_ns: AtomicBool::new(false),
            fail_mutation: AtomicBool::new(false),
        })
    }

    fn note(&self, c: Call) {
        self.calls.lock().unwrap().push(c);
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn forget(&self) {
        self.calls.lock().unwrap().clear();
    }

    /// Every barrier seen since the last `forget`.
    fn barriers(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| matches!(c, Call::SyncNs(_) | Call::Fsync(_)))
            .collect()
    }
}

impl Vfs for Watched {
    fn lookup(&self, p: u64, n: &[u8]) -> Result<Attr> {
        self.inner.lookup(p, n)
    }
    fn getattr(&self, i: u64) -> Result<Attr> {
        self.inner.getattr(i)
    }
    fn setattr(&self, i: u64, c: SetAttr) -> Result<Attr> {
        self.inner.setattr(i, c)
    }
    fn readlink(&self, i: u64) -> Result<Vec<u8>> {
        self.inner.readlink(i)
    }
    fn create(&self, p: u64, n: &[u8], m: u32) -> Result<Attr> {
        if self.fail_mutation.load(Ordering::Relaxed) {
            return Err(Error::PermissionDenied);
        }
        self.note(Call::Create);
        self.inner.create(p, n, m)
    }
    fn mkdir(&self, p: u64, n: &[u8], m: u32) -> Result<Attr> {
        self.inner.mkdir(p, n, m)
    }
    fn symlink(&self, p: u64, n: &[u8], t: &[u8]) -> Result<Attr> {
        self.inner.symlink(p, n, t)
    }
    fn link(&self, i: u64, p: u64, n: &[u8]) -> Result<Attr> {
        self.inner.link(i, p, n)
    }
    fn unlink(&self, p: u64, n: &[u8]) -> Result<()> {
        if self.fail_mutation.load(Ordering::Relaxed) {
            return Err(Error::PermissionDenied);
        }
        self.note(Call::Unlink);
        self.inner.unlink(p, n)
    }
    fn rmdir(&self, p: u64, n: &[u8]) -> Result<()> {
        self.inner.rmdir(p, n)
    }
    fn rename(
        &self,
        p: u64,
        n: &[u8],
        np: u64,
        nn: &[u8],
        f: cowfs_vfs::RenameFlags,
    ) -> Result<()> {
        if self.fail_mutation.load(Ordering::Relaxed) {
            return Err(Error::PermissionDenied);
        }
        self.note(Call::Rename);
        self.inner.rename(p, n, np, nn, f)
    }
    fn open(&self, i: u64) -> Result<FileHandle> {
        self.inner.open(i)
    }
    fn release(&self, h: FileHandle) -> Result<()> {
        self.inner.release(h)
    }
    fn read(&self, i: u64, o: u64, s: u32) -> Result<Vec<u8>> {
        self.inner.read(i, o, s)
    }
    fn write(&self, i: u64, o: u64, d: &[u8]) -> Result<u32> {
        self.note(Call::Write);
        self.inner.write(i, o, d)
    }
    fn flush(&self, i: u64) -> Result<()> {
        self.inner.flush(i)
    }
    fn fsync(&self, i: u64, data_only: bool) -> Result<()> {
        assert!(!data_only, "COMMIT is never data only");
        self.note(Call::Fsync(i));
        self.inner.fsync(i, data_only)
    }
    fn sync_namespace(&self, ino: u64) -> Result<()> {
        self.note(Call::SyncNs(ino));
        if self.fail_ns.load(Ordering::Relaxed) {
            return Err(Error::Io("store sync failed".into()));
        }
        self.inner.sync_namespace(ino)
    }
    fn readdir(&self, d: u64, c: u64, m: usize) -> Result<ReadDir> {
        self.inner.readdir(d, c, m)
    }
    fn readdir_attrs(&self, d: u64, c: u64, m: usize) -> Result<ReadDirPlus> {
        self.inner.readdir_attrs(d, c, m)
    }
    fn statfs(&self) -> Result<StatFs> {
        self.inner.statfs()
    }
    fn getxattr(&self, i: u64, n: &[u8]) -> Result<Vec<u8>> {
        self.inner.getxattr(i, n)
    }
    fn setxattr(&self, i: u64, n: &[u8], v: &[u8], f: XattrFlags) -> Result<()> {
        self.inner.setxattr(i, n, v, f)
    }
    fn listxattr(&self, i: u64) -> Result<Vec<Vec<u8>>> {
        self.inner.listxattr(i)
    }
    fn removexattr(&self, i: u64, n: &[u8]) -> Result<()> {
        self.inner.removexattr(i, n)
    }
}

fn start(vfs: Arc<Watched>) -> (cowfs_nfs::Server, Nfs) {
    serve(vfs, translated())
}

fn translated() -> cowfs_nfs::MountOptions {
    cowfs_nfs::MountOptions {
        appledouble: cowfs_nfs::AppleDoubleMode::Translate,
        ..cowfs_nfs::MountOptions::default()
    }
}

/// The rename the issue measured as lost: one `RENAME`, and the barrier has to follow the rename
/// and use the directory that was renamed in, so a caller that syncs the parent gets durability
/// without ever asking again.
#[test]
fn a_rename_is_durable_before_it_is_acknowledged() {
    let vfs = Watched::new();
    let (_s, mut c) = start(vfs.clone());
    let root = c.root.clone();
    let a = c.create_file(&root, "a");
    let d = c.mkdir(&root, "d").1.expect("mkdir");
    vfs.forget();

    assert_eq!(c.rename(&root, "a", &d, "b"), OK);
    assert_eq!(
        vfs.calls(),
        vec![Call::Rename, Call::SyncNs(c.attrs(&root).fileid)],
        "the barrier must follow the rename and name the source directory, so a caller that syncs \
         the parent directory gets the name"
    );
    // The name really is there, and only under its new one.
    let (st, _, _, _) = c.lookup(&d, "b");
    assert_eq!(st, OK);
    assert_eq!(c.lookup(&root, "a").0, NOENT);
    assert_eq!(
        a.data,
        c.must_lookup(&d, "b").data,
        "the handle must be the same inode under the new name"
    );
}

/// One namespace RPC to drive, and the name to report if it forgets its barrier.
type Case<'a> = (&'a str, Box<dyn FnOnce(&mut Nfs) + 'a>);

/// Every namespace RPC, so a new mutating procedure cannot join this list by being forgotten.
#[test]
fn every_namespace_rpc_is_durable_before_it_is_acknowledged() {
    let vfs = Watched::new();
    let (_s, mut c) = start(vfs.clone());
    let root = c.root.clone();
    let f = c.create_file(&root, "f");
    c.write(&f, 0, b"x", 2);
    c.mkdir(&root, "d").1.expect("mkdir");
    c.symlink(&root, "l", "f");
    let (st, _) = c.link(&f, &root, "hard");
    assert_eq!(st, OK);

    let cases: Vec<Case> = vec![
        (
            "create",
            Box::new(|c| {
                c.create_file(&root, "n1");
            }),
        ),
        (
            "create exclusive",
            Box::new(|c| {
                let (st, _, _) = c.create(&root, "n1x", 2, sattr3::default(), [7; 8]);
                assert_eq!(st, OK, "exclusive create");
            }),
        ),
        (
            "mkdir",
            Box::new(|c| {
                c.mkdir(&root, "n2");
            }),
        ),
        (
            "symlink",
            Box::new(|c| {
                c.symlink(&root, "n3", "f");
            }),
        ),
        (
            "link",
            Box::new(|c| {
                c.link(&f, &root, "n4");
            }),
        ),
        (
            "setattr",
            Box::new(|c| {
                c.setattr(&f, sattr_mtime(7, 0));
            }),
        ),
        (
            "remove",
            Box::new(|c| {
                c.remove(&root, "f");
            }),
        ),
        (
            "rmdir",
            Box::new(|c| assert_eq!(c.rmdir(&root, "d"), OK, "rmdir of an empty dir")),
        ),
        (
            "rename",
            Box::new(|c| {
                c.rename(&root, "l", &root, "l2");
            }),
        ),
    ];
    for (name, run) in cases {
        vfs.forget();
        run(&mut c);
        let barriers = vfs.barriers();
        assert_eq!(
            barriers.len(),
            1,
            "{name} must make the namespace durable exactly once"
        );
        assert!(
            matches!(barriers[0], Call::SyncNs(_)),
            "{name} must use the namespace barrier, not a data fsync: {:?}",
            vfs.calls()
        );
    }
}

/// WRITE stays unstable and READ stays cheap: the repair is a barrier at the acknowledgement of a
/// name, not a flush per byte or per lookup.
#[test]
fn a_write_and_a_read_are_not_a_namespace_barrier() {
    const UNSTABLE: u32 = 0;
    const FILE_SYNC: u32 = 2;
    let vfs = Watched::new();
    let (_s, mut c) = start(vfs.clone());
    let root = c.root.clone();
    let f = c.create_file(&root, "f");
    vfs.forget();

    assert_eq!(c.write(&f, 0, b"payload", UNSTABLE).0, OK);
    assert_eq!(
        vfs.barriers(),
        Vec::new(),
        "an UNSTABLE write must stay unstable: the barrier is at the acknowledgement of a name, \
         not per byte"
    );

    let (st, got, _) = c.read(&f, 0, 7);
    assert_eq!((st, got.as_slice()), (OK, &b"payload"[..]));
    assert_eq!(vfs.barriers(), Vec::new(), "READ must stay cheap");

    // A client that asks for a stable write gets the data sync it asked for, and that is still
    // the file's own fsync rather than the namespace barrier.
    assert_eq!(c.write(&f, 7, b"!", FILE_SYNC).0, OK);
    assert_eq!(
        vfs.barriers(),
        vec![Call::Fsync(c.attrs(&f).fileid)],
        "a stable write syncs that file's data, and no namespace with it"
    );

    vfs.forget();
    assert_eq!(c.commit(&f), OK);
    assert_eq!(
        vfs.barriers(),
        vec![Call::Fsync(c.attrs(&f).fileid)],
        "COMMIT of a file stays a data fsync of that file"
    );
}

/// The failure path: a barrier that cannot be made durable must not be reported as success, and
/// must not be papered over by syncing some other file's data, which would not make the name
/// durable and would cost a healthy file its write-back cache.
#[test]
fn a_failed_barrier_is_an_error_not_a_success() {
    let vfs = Watched::new();
    let (_s, mut c) = start(vfs.clone());
    let root = c.root.clone();
    c.create_file(&root, "f");
    let d = c.mkdir(&root, "d").1.expect("mkdir");
    vfs.forget();
    vfs.fail_ns.store(true, Ordering::Relaxed);

    assert_eq!(c.rename(&root, "f", &d, "g"), IO);
    assert_eq!(
        vfs.barriers(),
        vec![Call::SyncNs(c.attrs(&root).fileid)],
        "the barrier was attempted once and nothing else was flushed to hide it"
    );

    // A failed mutation is still a failed mutation, and it does not reach the barrier.
    vfs.forget();
    vfs.fail_mutation.store(true, Ordering::Relaxed);
    assert_eq!(c.rename(&d, "g", &root, "h"), ACCES);
    assert_eq!(
        vfs.barriers(),
        Vec::new(),
        "a refused rename needs no barrier"
    );
}

const IO: u32 = nfsserve::nfs::nfsstat3::NFS3ERR_IO as u32;
const ACCES: u32 = nfsserve::nfs::nfsstat3::NFS3ERR_ACCES as u32;
