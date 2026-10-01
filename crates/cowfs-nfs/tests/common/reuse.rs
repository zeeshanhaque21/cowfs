//! A `Vfs` that hands out inode numbers the way a careless backend might: the number of a
//! reclaimed inode goes to the next file created. The adapter must still never serve one file's
//! bytes through another file's old handle.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use cowfs_vfs::*;
use cowfs_vfs_test::MemVfs;

#[derive(Debug, Default)]
struct Ids {
    to_ext: HashMap<Ino, Ino>,
    to_int: HashMap<Ino, Ino>,
    free: Vec<Ino>,
    next: Ino,
}

#[derive(Debug)]
pub struct ReusingVfs {
    inner: Arc<MemVfs>,
    ids: Mutex<Ids>,
}

impl ReusingVfs {
    pub fn new() -> Arc<ReusingVfs> {
        let mut ids = Ids {
            next: ROOT_INO + 1,
            ..Ids::default()
        };
        ids.to_ext.insert(ROOT_INO, ROOT_INO);
        ids.to_int.insert(ROOT_INO, ROOT_INO);
        Arc::new(ReusingVfs {
            inner: Arc::new(MemVfs::new()),
            ids: Mutex::new(ids),
        })
    }

    fn ids(&self) -> std::sync::MutexGuard<'_, Ids> {
        self.ids.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn int(&self, ext: Ino) -> Result<Ino> {
        self.ids().to_int.get(&ext).copied().ok_or(Error::Stale)
    }

    fn ext_of(&self, int: Ino) -> Ino {
        let mut g = self.ids();
        if let Some(e) = g.to_ext.get(&int) {
            return *e;
        }
        let e = g.free.pop().unwrap_or_else(|| {
            g.next += 1;
            g.next - 1
        });
        g.to_ext.insert(int, e);
        g.to_int.insert(e, int);
        e
    }

    /// Frees the numbers of inodes the inner file system has reclaimed.
    fn sweep(&self) {
        let mut g = self.ids();
        let gone: Vec<Ino> = g
            .to_ext
            .keys()
            .copied()
            .filter(|i| *i != ROOT_INO && matches!(self.inner.getattr(*i), Err(Error::Stale)))
            .collect();
        for int in gone {
            if let Some(ext) = g.to_ext.remove(&int) {
                g.to_int.remove(&ext);
                g.free.push(ext);
            }
        }
    }

    fn out(&self, mut a: Attr) -> Attr {
        a.ino = self.ext_of(a.ino);
        a
    }
}

impl Vfs for ReusingVfs {
    fn lookup(&self, p: Ino, n: &[u8]) -> Result<Attr> {
        self.inner.lookup(self.int(p)?, n).map(|a| self.out(a))
    }
    fn forget(&self, ino: Ino, count: u64) {
        let Ok(int) = self.int(ino) else { return };
        self.inner.forget(int, count);
        self.sweep();
    }
    fn getattr(&self, i: Ino) -> Result<Attr> {
        self.inner.getattr(self.int(i)?).map(|a| self.out(a))
    }
    fn setattr(&self, i: Ino, c: SetAttr) -> Result<Attr> {
        self.inner.setattr(self.int(i)?, c).map(|a| self.out(a))
    }
    fn readlink(&self, i: Ino) -> Result<Vec<u8>> {
        self.inner.readlink(self.int(i)?)
    }
    fn create(&self, p: Ino, n: &[u8], m: u32) -> Result<Attr> {
        self.inner.create(self.int(p)?, n, m).map(|a| self.out(a))
    }
    fn mkdir(&self, p: Ino, n: &[u8], m: u32) -> Result<Attr> {
        self.inner.mkdir(self.int(p)?, n, m).map(|a| self.out(a))
    }
    fn symlink(&self, p: Ino, n: &[u8], t: &[u8]) -> Result<Attr> {
        self.inner.symlink(self.int(p)?, n, t).map(|a| self.out(a))
    }
    fn link(&self, i: Ino, p: Ino, n: &[u8]) -> Result<Attr> {
        self.inner
            .link(self.int(i)?, self.int(p)?, n)
            .map(|a| self.out(a))
    }
    fn unlink(&self, p: Ino, n: &[u8]) -> Result<()> {
        let r = self.inner.unlink(self.int(p)?, n);
        self.sweep();
        r
    }
    fn rmdir(&self, p: Ino, n: &[u8]) -> Result<()> {
        let r = self.inner.rmdir(self.int(p)?, n);
        self.sweep();
        r
    }
    fn rename(&self, p: Ino, n: &[u8], np: Ino, nn: &[u8], f: RenameFlags) -> Result<()> {
        let r = self.inner.rename(self.int(p)?, n, self.int(np)?, nn, f);
        self.sweep();
        r
    }
    fn open(&self, i: Ino) -> Result<FileHandle> {
        self.inner.open(self.int(i)?)
    }
    fn release(&self, h: FileHandle) -> Result<()> {
        self.inner.release(h)
    }
    fn read(&self, i: Ino, o: u64, s: u32) -> Result<Vec<u8>> {
        self.inner.read(self.int(i)?, o, s)
    }
    fn write(&self, i: Ino, o: u64, d: &[u8]) -> Result<u32> {
        self.inner.write(self.int(i)?, o, d)
    }
    fn flush(&self, i: Ino) -> Result<()> {
        self.inner.flush(self.int(i)?)
    }
    fn fsync(&self, i: Ino, d: bool) -> Result<()> {
        self.inner.fsync(self.int(i)?, d)
    }
    fn readdir(&self, d: Ino, c: u64, m: usize) -> Result<ReadDir> {
        let mut r = self.inner.readdir(self.int(d)?, c, m)?;
        for e in &mut r.entries {
            e.ino = self.ext_of(e.ino);
        }
        Ok(r)
    }
    fn statfs(&self) -> Result<StatFs> {
        self.inner.statfs()
    }
    fn getxattr(&self, i: Ino, n: &[u8]) -> Result<Vec<u8>> {
        self.inner.getxattr(self.int(i)?, n)
    }
    fn setxattr(&self, i: Ino, n: &[u8], v: &[u8], f: XattrFlags) -> Result<()> {
        self.inner.setxattr(self.int(i)?, n, v, f)
    }
    fn listxattr(&self, i: Ino) -> Result<Vec<Vec<u8>>> {
        self.inner.listxattr(self.int(i)?)
    }
    fn removexattr(&self, i: Ino, n: &[u8]) -> Result<()> {
        self.inner.removexattr(self.int(i)?, n)
    }
}
