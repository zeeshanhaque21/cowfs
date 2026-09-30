//! A `Vfs` wrapper that counts lookup references, to prove the adapter balances them.
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use cowfs_vfs::*;
use cowfs_vfs_test::MemVfs;

#[derive(Debug)]
pub struct CountingVfs {
    inner: Arc<MemVfs>,
    refs: Mutex<HashMap<Ino, i64>>,
    seen: Mutex<BTreeSet<Ino>>,
    pub lookups: std::sync::atomic::AtomicU64,
}

impl CountingVfs {
    pub fn new() -> Arc<CountingVfs> {
        Arc::new(CountingVfs {
            inner: Arc::new(MemVfs::new()),
            refs: Mutex::new(HashMap::new()),
            seen: Mutex::new(BTreeSet::new()),
            lookups: Default::default(),
        })
    }

    fn got(&self, a: Attr) -> Attr {
        *self
            .refs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(a.ino)
            .or_insert(0) += 1;
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(a.ino);
        a
    }

    /// References handed out and not yet forgotten, summed over all inodes.
    pub fn outstanding(&self) -> i64 {
        self.refs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .sum()
    }

    /// Inodes ever handed out that the file system still holds.
    pub fn live(&self) -> usize {
        let seen = self
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        seen.into_iter()
            .filter(|i| !matches!(self.inner.getattr(*i), Err(Error::Stale)))
            .count()
    }
}

impl Vfs for CountingVfs {
    fn lookup(&self, p: Ino, n: &[u8]) -> Result<Attr> {
        self.lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.lookup(p, n).map(|a| self.got(a))
    }
    fn forget(&self, ino: Ino, count: u64) {
        *self
            .refs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(ino)
            .or_insert(0) -= count as i64;
        self.inner.forget(ino, count);
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
        self.inner.create(p, n, m).map(|a| self.got(a))
    }
    fn mkdir(&self, p: Ino, n: &[u8], m: u32) -> Result<Attr> {
        self.inner.mkdir(p, n, m).map(|a| self.got(a))
    }
    fn symlink(&self, p: Ino, n: &[u8], t: &[u8]) -> Result<Attr> {
        self.inner.symlink(p, n, t).map(|a| self.got(a))
    }
    fn link(&self, i: Ino, p: Ino, n: &[u8]) -> Result<Attr> {
        self.inner.link(i, p, n).map(|a| self.got(a))
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
    fn read(&self, i: Ino, o: u64, s: u32) -> Result<Vec<u8>> {
        self.inner.read(i, o, s)
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
    fn readdir(&self, d: Ino, c: u64, m: usize) -> Result<ReadDir> {
        self.inner.readdir(d, c, m)
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
