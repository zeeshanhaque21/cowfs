//! A `Vfs` wrapper whose lookup and unlink of the name `hang` block until `release`, to model a
//! handler that never completes (issue 287).
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use cowfs_vfs::*;
use cowfs_vfs_test::MemVfs;

#[derive(Debug)]
pub struct HangVfs {
    inner: Arc<MemVfs>,
    open: Mutex<bool>,
    cv: Condvar,
}

impl HangVfs {
    pub fn new() -> Arc<HangVfs> {
        Arc::new(HangVfs {
            inner: Arc::new(MemVfs::new()),
            open: Mutex::new(false),
            cv: Condvar::new(),
        })
    }

    /// Lets every blocked and future call through.
    pub fn release(&self) {
        *self.open.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.cv.notify_all();
    }

    fn gate(&self, n: &[u8]) {
        if n == b"hang" {
            let mut g = self.open.lock().unwrap_or_else(PoisonError::into_inner);
            while !*g {
                g = self.cv.wait(g).unwrap_or_else(PoisonError::into_inner);
            }
        }
    }
}

impl Vfs for HangVfs {
    fn lookup(&self, p: Ino, n: &[u8]) -> Result<Attr> {
        self.gate(n);
        self.inner.lookup(p, n)
    }
    fn forget(&self, ino: Ino, count: u64) {
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
        self.inner.create(p, n, m)
    }
    fn mkdir(&self, p: Ino, n: &[u8], m: u32) -> Result<Attr> {
        self.inner.mkdir(p, n, m)
    }
    fn mknod(&self, p: Ino, n: &[u8], k: cowfs_vfs::FileKind, m: u32, r: u64) -> Result<Attr> {
        self.inner.mknod(p, n, k, m, r)
    }
    fn symlink(&self, p: Ino, n: &[u8], t: &[u8]) -> Result<Attr> {
        self.inner.symlink(p, n, t)
    }
    fn link(&self, i: Ino, p: Ino, n: &[u8]) -> Result<Attr> {
        self.inner.link(i, p, n)
    }
    fn unlink(&self, p: Ino, n: &[u8]) -> Result<()> {
        self.gate(n);
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
