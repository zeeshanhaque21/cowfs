//! The adapter against the sentences of the frozen `Vfs` trait: what it must require, and the
//! paths that are easy to get wrong (COMMIT of the root, READDIRPLUS attributes, an error the
//! client should retry).
mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use common::*;
use cowfs_nfs::Server;
use cowfs_vfs::{
    Attr, DirEntryPlus, Error, FileHandle, ReadDir, ReadDirPlus, Result, Vfs, ROOT_INO,
};
use cowfs_vfs_test::MemVfs;
use nfsserve::nfs::nfsstat3;

/// A `MemVfs` that records what the adapter asked for, and can be told to fail.
struct Watched {
    inner: Arc<MemVfs>,
    fsyncs: Mutex<Vec<(u64, bool)>>,
    list_attrs: AtomicU64,
    fail_with: Mutex<Option<Error>>,
}

impl Watched {
    fn new() -> Arc<Watched> {
        Arc::new(Watched {
            inner: Arc::new(MemVfs::new()),
            fsyncs: Mutex::new(Vec::new()),
            list_attrs: AtomicU64::new(0),
            fail_with: Mutex::new(None),
        })
    }

    fn fsyncs(&self) -> Vec<(u64, bool)> {
        self.fsyncs.lock().unwrap().clone()
    }
}

impl Vfs for Watched {
    fn lookup(&self, p: u64, n: &[u8]) -> Result<Attr> {
        match self.fail_with.lock().unwrap().clone() {
            Some(e) => Err(e),
            None => self.inner.lookup(p, n),
        }
    }
    fn getattr(&self, i: u64) -> Result<Attr> {
        self.inner.getattr(i)
    }
    fn setattr(&self, i: u64, c: cowfs_vfs::SetAttr) -> Result<Attr> {
        self.inner.setattr(i, c)
    }
    fn readlink(&self, i: u64) -> Result<Vec<u8>> {
        self.inner.readlink(i)
    }
    fn create(&self, p: u64, n: &[u8], m: u32) -> Result<Attr> {
        self.inner.create(p, n, m)
    }
    fn mkdir(&self, p: u64, n: &[u8], m: u32) -> Result<Attr> {
        self.inner.mkdir(p, n, m)
    }
    fn mknod(&self, p: u64, n: &[u8], k: cowfs_vfs::FileKind, m: u32, r: u64) -> Result<Attr> {
        self.inner.mknod(p, n, k, m, r)
    }
    fn symlink(&self, p: u64, n: &[u8], t: &[u8]) -> Result<Attr> {
        self.inner.symlink(p, n, t)
    }
    fn link(&self, i: u64, p: u64, n: &[u8]) -> Result<Attr> {
        self.inner.link(i, p, n)
    }
    fn unlink(&self, p: u64, n: &[u8]) -> Result<()> {
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
        self.inner.write(i, o, d)
    }
    fn flush(&self, i: u64) -> Result<()> {
        self.inner.flush(i)
    }
    fn fsync(&self, i: u64, data_only: bool) -> Result<()> {
        self.fsyncs.lock().unwrap().push((i, data_only));
        self.inner.fsync(i, data_only)
    }
    /// Recorded apart from `fsync`, so these tests stay about COMMIT. What the namespace barrier
    /// does, and when, is `ns_durability`'s subject.
    fn sync_namespace(&self, ino: u64) -> Result<()> {
        self.inner.sync_namespace(ino)
    }
    fn readdir(&self, d: u64, c: u64, m: usize) -> Result<ReadDir> {
        self.inner.readdir(d, c, m)
    }
    /// The Vfs may produce attributes with the listing, and the adapter has to use that instead
    /// of asking for them one at a time.
    fn readdir_attrs(&self, d: u64, c: u64, m: usize) -> Result<ReadDirPlus> {
        self.list_attrs.fetch_add(1, Ordering::Relaxed);
        let listing = self.readdir(d, c, m)?;
        let mut entries = Vec::with_capacity(listing.entries.len());
        for entry in listing.entries {
            match self.getattr(entry.ino) {
                Ok(attr) => entries.push(DirEntryPlus { entry, attr }),
                Err(Error::Stale | Error::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(ReadDirPlus {
            entries,
            eof: listing.eof,
        })
    }
    fn statfs(&self) -> Result<cowfs_vfs::StatFs> {
        self.inner.statfs()
    }
    fn getxattr(&self, i: u64, n: &[u8]) -> Result<Vec<u8>> {
        self.inner.getxattr(i, n)
    }
    fn setxattr(&self, i: u64, n: &[u8], v: &[u8], f: cowfs_vfs::XattrFlags) -> Result<()> {
        self.inner.setxattr(i, n, v, f)
    }
    fn listxattr(&self, i: u64) -> Result<Vec<Vec<u8>>> {
        self.inner.listxattr(i)
    }
    fn removexattr(&self, i: u64, n: &[u8]) -> Result<()> {
        self.inner.removexattr(i, n)
    }
}

fn translated() -> cowfs_nfs::MountOptions {
    cowfs_nfs::MountOptions {
        appledouble: cowfs_nfs::AppleDoubleMode::Translate,
        ..cowfs_nfs::MountOptions::default()
    }
}

fn setup() -> (Arc<Watched>, Server, Nfs) {
    let vfs = Watched::new();
    let mut opts = translated();
    opts.check_peer_uid = false;
    let server = cowfs_nfs::Server::start(vfs.clone(), &opts, None).unwrap();
    let c = Nfs::connect(server.port(), server.export_name());
    (vfs, server, c)
}

#[test]
fn commit_of_the_root_handle_is_the_whole_mount_barrier() {
    let (vfs, _s, mut c) = setup();
    let root = c.root.clone();
    let f = c.create_file(&root, "f");
    c.write(&f, 0, b"x", 2);

    // COMMIT of a file.
    let (st, _) = c.call(
        21,
        Args::new()
            .put(&f)
            .put(&0u64)
            .put(&1u32)
            .put(&1u32)
            .put(&b"x".to_vec()),
    );
    assert_eq!(st, OK);
    let file = c.attrs(&f).fileid;
    assert_ne!(file, ROOT_INO, "the test needs a file that is not the root");
    assert!(
        vfs.fsyncs().iter().all(|(i, _)| *i == file),
        "COMMIT of a file fsyncs only that file: {:?}",
        vfs.fsyncs()
    );

    // COMMIT of the root: fsync(ROOT_INO, false), the whole-mount barrier.
    let (st, _) = c.call(21, Args::new().put(&root).put(&0u64).put(&0u32));
    assert_eq!(st, OK);
    assert_eq!(
        vfs.fsyncs().last().copied(),
        Some((ROOT_INO, false)),
        "the root handle must be the mount barrier, not a data-only sync"
    );
    assert!(
        vfs.fsyncs().iter().all(|(_, data_only)| !data_only),
        "COMMIT is never data only: the trait makes the name durable either way"
    );
}

#[test]
fn readdirplus_asks_the_vfs_for_the_attributes_in_one_call() {
    let (vfs, _s, mut c) = setup();
    let root = c.root.clone();
    for i in 0..5 {
        c.create_file(&root, &format!("f{i}"));
    }
    let before = vfs.list_attrs.load(Ordering::Relaxed);
    let (st, page, _) = c.readdir_page(&root, 0, true, 4096);
    assert_eq!(st, OK);
    assert_eq!(page.len(), 5, "and the attributes come with the listing");
    assert!(
        vfs.list_attrs.load(Ordering::Relaxed) > before,
        "READDIRPLUS must go through readdir_attrs"
    );

    // Plain READDIR has no attributes, so it must not ask for them.
    let before = vfs.list_attrs.load(Ordering::Relaxed);
    let (st, _, _) = c.readdir_page(&root, 0, false, 4096);
    assert_eq!(st, OK);
    assert_eq!(vfs.list_attrs.load(Ordering::Relaxed), before);
}

/// The status a client is told, and that it stops being told once the fault is gone.
///
/// The first half is the mapping: a `Vfs` that says "worth repeating" becomes `NFS3ERR_JUKEBOX`.
///
/// The second half is the state after the fault is cleared, which is where this test used to carry
/// `assert!(post.is_none() || true)`. That expression cannot reject any value, and worse, `post` was
/// not what its name implied: the fourth element of the helper's tuple is the *post-op directory*
/// attributes, not the object's, and it is always `Some` on an error reply. So the assertion was
/// `false || true` and was checking nothing at all.
///
/// What replaces it is the contract that is actually there and is worth pinning. A retry status is a
/// consequence of the injected fault, not state the adapter keeps: once the `Vfs` stops asking for a
/// retry, the same client, on the same connection, must get the truth. An NFS error reply also has to
/// carry the post-op directory attributes, which is the field the old assertion was aimed at.
///
/// Nothing here claims atomicity. `LOOKUP` mutates nothing, and this branch adds no guarantee about
/// partially applied operations; the barrier and error-precedence guarantees from #90 are asserted
/// elsewhere and are untouched.
#[test]
fn an_error_the_vfs_asks_to_retry_becomes_the_retry_status() {
    let (vfs, _s, mut c) = setup();
    let root = c.root.clone();
    *vfs.fail_with.lock().unwrap() = Some(Error::Retry);
    let (st, fh, obj, _) = c.lookup(&root, "anything");
    assert_eq!(
        st,
        nfsstat3::NFS3ERR_JUKEBOX as u32,
        "the client must be told to come back, not that the file is missing"
    );
    assert!(
        fh.is_none() && obj.is_none(),
        "a JUKEBOX reply carries no object, so a client cannot mistake it for a hit"
    );

    // The fault is gone. The retry status must go with it.
    *vfs.fail_with.lock().unwrap() = None;
    let (st, fh, obj, dir) = c.lookup(&root, "nope");
    assert_eq!(
        st,
        nfsstat3::NFS3ERR_NOENT as u32,
        "with the fault cleared an absent name is NOENT, not JUKEBOX: a cached retry status would \
         tell a client to keep coming back for a name that will never exist"
    );
    assert!(
        fh.is_none() && obj.is_none(),
        "an absent name has no handle and no attributes"
    );
    assert!(
        dir.is_some(),
        "an error reply still carries the post-op directory attributes, which is the field the \
         removed tautology was pointing at and the reason it read as false"
    );
}
