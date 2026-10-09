//! A `Vfs` that hands out inode numbers the way a careless backend might: the number of a
//! reclaimed inode goes to the next file created. The adapter must still never serve one file's
//! bytes through another file's old handle. `virtuals` adds an offset, so every number it reports
//! has the top bit set, the way `cowfs-core` numbers a virtual inode (issue #60).
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use cowfs_vfs::*;
use cowfs_vfs_test::MemVfs;

/// What `cowfs-core` puts on the top bit of a virtual inode number.
pub const VIRT: Ino = 1 << 63;

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
    /// Added to every number the inner file system reports.
    offset: Ino,
    /// Whether the numbers are handed out from the top of the `u64` space downwards, which is
    /// where the adapter takes the file ids of sidecars that are not inodes (issue #60).
    descending: bool,
}

impl ReusingVfs {
    pub fn new() -> Arc<ReusingVfs> {
        Self::at(0)
    }

    /// The same file system with every inode number but the root's shifted by `offset`.
    pub fn at(offset: Ino) -> Arc<ReusingVfs> {
        Self::shape(offset, false)
    }

    /// Every inode number but the root's has its top bit set, the way `cowfs-core` numbers a
    /// virtual inode (issue #60). The root of a mount is always `ROOT_INO`.
    pub fn virtuals() -> Arc<ReusingVfs> {
        Self::at(VIRT)
    }

    /// Inode numbers from `u64::MAX` downwards, so the file ids an adapter gives to sidecars that
    /// are not inodes collide with a real inode.
    pub fn from_the_top() -> Arc<ReusingVfs> {
        Self::shape(0, true)
    }

    fn shape(offset: Ino, descending: bool) -> Arc<ReusingVfs> {
        let mut ids = Ids {
            next: if descending { 0 } else { ROOT_INO + 1 },
            ..Ids::default()
        };
        ids.to_ext.insert(ROOT_INO, ROOT_INO);
        ids.to_int.insert(ROOT_INO, ROOT_INO);
        Arc::new(ReusingVfs {
            inner: Arc::new(MemVfs::new()),
            ids: Mutex::new(ids),
            offset,
            descending,
        })
    }

    pub fn offset(&self) -> Ino {
        self.offset
    }

    fn ids(&self) -> std::sync::MutexGuard<'_, Ids> {
        self.ids.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn int(&self, ext: Ino) -> Result<Ino> {
        self.ids().to_int.get(&ext).copied().ok_or(Error::Stale)
    }

    fn ext_of(&self, int: Ino) -> Ino {
        if int == ROOT_INO {
            return ROOT_INO;
        }
        let mut g = self.ids();
        if let Some(e) = g.to_ext.get(&int) {
            return *e;
        }
        let e = g.free.pop().unwrap_or_else(|| {
            if self.descending {
                g.next = g.next.checked_sub(1).unwrap_or(Ino::MAX);
                g.next
            } else {
                g.next += 1;
                g.next - 1 + self.offset
            }
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
    fn mknod(&self, p: Ino, n: &[u8], k: cowfs_vfs::FileKind, m: u32, r: u64) -> Result<Attr> {
        self.inner
            .mknod(self.int(p)?, n, k, m, r)
            .map(|a| self.out(a))
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
