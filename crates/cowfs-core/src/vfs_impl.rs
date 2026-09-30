//! `Vfs` for `Core`.

use cowfs_vfs::{
    Attr, FileHandle, Ino, ReadDir, RenameFlags, Result, SetAttr, StatFs, Vfs, XattrFlags,
};

use crate::queue::Create;
use crate::Core;

impl Vfs for Core {
    fn lookup(&self, parent: Ino, name: &[u8]) -> Result<Attr> {
        self.inner.op_lookup(parent, name)
    }

    fn forget(&self, ino: Ino, count: u64) {
        self.inner.op_forget(ino, count);
    }

    fn getattr(&self, ino: Ino) -> Result<Attr> {
        self.inner.op_getattr(ino)
    }

    fn setattr(&self, ino: Ino, changes: SetAttr) -> Result<Attr> {
        self.inner.op_setattr(ino, changes)
    }

    fn readlink(&self, ino: Ino) -> Result<Vec<u8>> {
        self.inner.op_readlink(ino)
    }

    fn create(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        self.inner.make(parent, name, Create::File, mode)
    }

    fn mkdir(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        self.inner.make(parent, name, Create::Dir, mode)
    }

    fn symlink(&self, parent: Ino, name: &[u8], target: &[u8]) -> Result<Attr> {
        self.inner.op_symlink(parent, name, target)
    }

    fn link(&self, ino: Ino, new_parent: Ino, new_name: &[u8]) -> Result<Attr> {
        self.inner.op_link(ino, new_parent, new_name)
    }

    fn unlink(&self, parent: Ino, name: &[u8]) -> Result<()> {
        self.inner.op_unlink(parent, name)
    }

    fn rmdir(&self, parent: Ino, name: &[u8]) -> Result<()> {
        self.inner.op_rmdir(parent, name)
    }

    fn rename(
        &self,
        parent: Ino,
        name: &[u8],
        new_parent: Ino,
        new_name: &[u8],
        flags: RenameFlags,
    ) -> Result<()> {
        self.inner
            .op_rename(parent, name, new_parent, new_name, flags)
    }

    fn open(&self, ino: Ino) -> Result<FileHandle> {
        self.inner.op_open(ino)
    }

    fn release(&self, handle: FileHandle) -> Result<()> {
        self.inner.op_release(handle)
    }

    fn read(&self, ino: Ino, offset: u64, size: u32) -> Result<Vec<u8>> {
        self.inner.op_read(ino, offset, size)
    }

    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> Result<u32> {
        self.inner.op_write(ino, offset, data)
    }

    fn flush(&self, ino: Ino) -> Result<()> {
        self.inner.op_flush(ino)
    }

    fn fsync(&self, ino: Ino, _data_only: bool) -> Result<()> {
        self.inner.op_fsync(ino)
    }

    fn readdir(&self, dir: Ino, cookie: u64, max: usize) -> Result<ReadDir> {
        self.inner.op_readdir(dir, cookie, max)
    }

    fn statfs(&self) -> Result<StatFs> {
        self.inner.op_statfs()
    }

    fn getxattr(&self, ino: Ino, name: &[u8]) -> Result<Vec<u8>> {
        self.inner.op_getxattr(ino, name)
    }

    fn setxattr(&self, ino: Ino, name: &[u8], value: &[u8], flags: XattrFlags) -> Result<()> {
        self.inner.op_setxattr(ino, name, value, flags)
    }

    fn listxattr(&self, ino: Ino) -> Result<Vec<Vec<u8>>> {
        self.inner.op_listxattr(ino)
    }

    fn removexattr(&self, ino: Ino, name: &[u8]) -> Result<()> {
        self.inner.op_removexattr(ino, name)
    }
}
