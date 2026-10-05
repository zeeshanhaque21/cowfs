//! A snapshot shown as a plain filesystem.

use cowfs_vfs::{
    Attr, DirEntry, FileHandle, Ino, ReadDir, RenameFlags, Result, SetAttr, StatFs, Vfs,
    XattrFlags, ROOT_INO,
};

use crate::Core;

/// A `Vfs` whose `ROOT_INO` is the root directory of one snapshot of a [`Core`].
///
/// Inode numbers are the core's own, except that the snapshot root is `ROOT_INO` inside the view.
#[derive(Clone, Debug)]
pub struct SnapshotView {
    core: Core,
    root: Ino,
}

impl SnapshotView {
    pub(crate) fn new(core: Core, root: Ino) -> Self {
        Self { core, root }
    }

    fn i(&self, ino: Ino) -> Ino {
        if ino == ROOT_INO {
            self.root
        } else {
            ino
        }
    }

    fn o(&self, ino: Ino) -> Ino {
        if ino == self.root {
            ROOT_INO
        } else {
            ino
        }
    }

    fn attr(&self, mut a: Attr) -> Attr {
        a.ino = self.o(a.ino);
        a
    }
}

impl Vfs for SnapshotView {
    fn lookup(&self, parent: Ino, name: &[u8]) -> Result<Attr> {
        self.core.lookup(self.i(parent), name).map(|a| self.attr(a))
    }

    fn forget(&self, ino: Ino, count: u64) {
        self.core.forget(self.i(ino), count);
    }

    fn getattr(&self, ino: Ino) -> Result<Attr> {
        self.core.getattr(self.i(ino)).map(|a| self.attr(a))
    }

    fn setattr(&self, ino: Ino, changes: SetAttr) -> Result<Attr> {
        self.core
            .setattr(self.i(ino), changes)
            .map(|a| self.attr(a))
    }

    fn readlink(&self, ino: Ino) -> Result<Vec<u8>> {
        self.core.readlink(self.i(ino))
    }

    fn create(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        self.core
            .create(self.i(parent), name, mode)
            .map(|a| self.attr(a))
    }

    fn mkdir(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        self.core
            .mkdir(self.i(parent), name, mode)
            .map(|a| self.attr(a))
    }

    fn symlink(&self, parent: Ino, name: &[u8], target: &[u8]) -> Result<Attr> {
        self.core
            .symlink(self.i(parent), name, target)
            .map(|a| self.attr(a))
    }

    fn link(&self, ino: Ino, new_parent: Ino, new_name: &[u8]) -> Result<Attr> {
        self.core
            .link(self.i(ino), self.i(new_parent), new_name)
            .map(|a| self.attr(a))
    }

    fn unlink(&self, parent: Ino, name: &[u8]) -> Result<()> {
        self.core.unlink(self.i(parent), name)
    }

    fn rmdir(&self, parent: Ino, name: &[u8]) -> Result<()> {
        self.core.rmdir(self.i(parent), name)
    }

    fn rename(
        &self,
        parent: Ino,
        name: &[u8],
        new_parent: Ino,
        new_name: &[u8],
        flags: RenameFlags,
    ) -> Result<()> {
        self.core
            .rename(self.i(parent), name, self.i(new_parent), new_name, flags)
    }

    fn open(&self, ino: Ino) -> Result<FileHandle> {
        self.core.open(self.i(ino))
    }

    fn release(&self, handle: FileHandle) -> Result<()> {
        self.core.release(handle)
    }

    fn read(&self, ino: Ino, offset: u64, size: u32) -> Result<Vec<u8>> {
        self.core.read(self.i(ino), offset, size)
    }

    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> Result<u32> {
        self.core.write(self.i(ino), offset, data)
    }

    fn flush(&self, ino: Ino) -> Result<()> {
        Vfs::flush(&self.core, self.i(ino))
    }

    fn fsync(&self, ino: Ino, data_only: bool) -> Result<()> {
        self.core.fsync(self.i(ino), data_only)
    }

    fn sync_namespace(&self, ino: Ino) -> Result<()> {
        self.core.sync_namespace(self.i(ino))
    }

    fn readdir(&self, dir: Ino, cookie: u64, max: usize) -> Result<ReadDir> {
        let mut r = self.core.readdir(self.i(dir), cookie, max)?;
        r.entries = r
            .entries
            .into_iter()
            .map(|e| DirEntry {
                ino: self.o(e.ino),
                ..e
            })
            .collect();
        Ok(r)
    }

    fn statfs(&self) -> Result<StatFs> {
        self.core.statfs()
    }

    fn getxattr(&self, ino: Ino, name: &[u8]) -> Result<Vec<u8>> {
        self.core.getxattr(self.i(ino), name)
    }

    fn setxattr(&self, ino: Ino, name: &[u8], value: &[u8], flags: XattrFlags) -> Result<()> {
        self.core.setxattr(self.i(ino), name, value, flags)
    }

    fn listxattr(&self, ino: Ino) -> Result<Vec<Vec<u8>>> {
        self.core.listxattr(self.i(ino))
    }

    fn removexattr(&self, ino: Ino, name: &[u8]) -> Result<()> {
        self.core.removexattr(self.i(ino), name)
    }
}
