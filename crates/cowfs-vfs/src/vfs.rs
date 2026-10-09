use crate::error::Result;
use crate::types::FallocMode;
use crate::types::{
    Attr, DirEntryPlus, FileHandle, Ino, ReadDir, ReadDirPlus, RenameFlags, SetAttr, StatFs,
    XattrFlags,
};

/// The POSIX-like filesystem a mount adapter serves. Operations are by inode number and
/// are safe to call from many threads at once.
///
/// Semantics follow POSIX. Names are raw bytes: no `/`, no NUL, at most `NAME_MAX` bytes,
/// never `.` or `..` (adapters handle those). Any operation on an inode that does not
/// exist returns `Error::Stale`. Every operation that changes a file updates its ctime, and
/// content changes also update its mtime. Name-changing operations also update the parent
/// directory's times (see `Attr`).
///
/// # Concurrency
///
/// Every method may be called concurrently from many threads, and each call is atomic:
/// concurrent calls behave as if they ran one at a time in some order (linearizable), and
/// the effects of a call that returned are visible to every call that starts afterwards.
/// An implementation must not rely on the caller to order or serialise calls, including
/// calls on the same inode. Adapters preserve only the order the kernel or protocol
/// imposes, which is that a dependent request is issued after the reply to the request it
/// depends on (write, then fsync, then read). Adapters may run cheap calls inline and slow
/// ones on worker threads, so two independent calls can overlap.
///
/// `ROOT_INO` may be a synthetic, read-only directory: mutating it returns `ReadOnly`, and a
/// rename across two such subtrees returns `CrossDevice`. There are no special files
/// (devices, fifos, sockets) and no permission enforcement: adapters check mode bits.
/// Every name-taking operation validates names with `validate_name`.
pub trait Vfs: Send + Sync {
    /// Attributes of the child `name` of directory `parent`. `Error::NotFound` if absent,
    /// `Error::NotDir` if `parent` is not a directory.
    fn lookup(&self, parent: Ino, name: &[u8]) -> Result<Attr>;

    /// Called when an adapter drops `count` of its references to `ino` that were handed out
    /// by `lookup`, `create`, `mkdir`, `symlink` or `link`.
    ///
    /// This is the adapter saying it is done with its own bookkeeping, not permission for the
    /// inode to disappear. `forget` says nothing about whether the client still holds the
    /// number, and a stateless protocol cannot say: an NFS client hands out filehandles that
    /// are just the number and never tells the server it is finished, so an adapter that
    /// forgets immediately must still get `ino` back for as long as the inode exists (see
    /// `Ino`).
    ///
    /// An inode with no names left, no outstanding references and no open handles may then be
    /// reclaimed, and later operations on it return `Error::Stale`. The default does nothing,
    /// which is correct only for an implementation that never reclaims inodes and therefore
    /// leaks them.
    fn forget(&self, _ino: Ino, _count: u64) {}

    fn getattr(&self, ino: Ino) -> Result<Attr>;

    /// Applies the requested changes atomically and returns the new attributes.
    /// `size` truncates or extends with zeros and is only valid for regular files
    /// (`Error::IsDir` for directories, `Error::InvalidArgument` for symlinks).
    /// `mode` is masked with `MODE_MASK`. `getattr` never follows a symlink. A symlink's size
    /// is its target length in bytes.
    fn setattr(&self, ino: Ino, changes: SetAttr) -> Result<Attr>;

    fn readlink(&self, ino: Ino) -> Result<Vec<u8>>;

    /// Creates an empty regular file. Fails with `Error::Exists` if the name exists.
    fn create(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr>;

    fn mkdir(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr>;

    fn symlink(&self, parent: Ino, name: &[u8], target: &[u8]) -> Result<Attr>;

    /// Hardlink: a new name `new_name` in `new_parent` for the existing non-directory `ino`.
    /// Both names share one inode: later writes through either are visible through both and
    /// `nlink` counts the names. Linking a directory is `Error::PermissionDenied`.
    fn link(&self, ino: Ino, new_parent: Ino, new_name: &[u8]) -> Result<Attr>;

    /// Removes a name for a non-directory (`Error::IsDir` for directories). The data lives on
    /// while other names or open handles remain.
    fn unlink(&self, parent: Ino, name: &[u8]) -> Result<()>;

    /// Removes an empty directory (`Error::NotEmpty`, `Error::NotDir`).
    fn rmdir(&self, parent: Ino, name: &[u8]) -> Result<()>;

    /// Atomic rename. Replaces an existing destination file, or an existing empty directory
    /// when the source is a directory, unless `flags.no_replace`. Renaming a directory into
    /// its own subtree is `Error::InvalidArgument`. Renaming a name onto another name of the
    /// same inode succeeds and does nothing.
    fn rename(
        &self,
        parent: Ino,
        name: &[u8],
        new_parent: Ino,
        new_name: &[u8],
        flags: RenameFlags,
    ) -> Result<()>;

    /// Pins `ino` until `release`. The handle is not a capability: `open` never fails because
    /// of what the caller intends to do. `read` and `write` on a directory return
    /// `Error::IsDir`, and on a symlink `Error::InvalidArgument`. `release` of an unknown or
    /// already released handle is `Error::InvalidArgument`. `flush` and `release` may arrive
    /// in either order.
    fn open(&self, ino: Ino) -> Result<FileHandle>;

    fn release(&self, handle: FileHandle) -> Result<()>;

    /// Reads up to `size` bytes at `offset`. A read at or past the end returns an empty vector,
    /// a read that crosses the end returns the bytes up to it. Holes read as zeros.
    fn read(&self, ino: Ino, offset: u64, size: u32) -> Result<Vec<u8>>;

    /// Writes `data` at `offset`, extending the file (with a zero-filled hole if `offset` is
    /// past the end). Returns the number of bytes written, which is `data.len()` on success.
    /// `data.len()` must not exceed `u32::MAX`.
    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> Result<u32>;

    /// Called when a file descriptor is closed. Buffered data may stay buffered.
    fn flush(&self, ino: Ino) -> Result<()>;

    /// Makes previously written data and metadata for `ino` durable, including the name by
    /// which it was created, so a file whose `fsync` returned survives a crash. With
    /// `data_only`, metadata that does not affect reading the data (times) may be skipped.
    /// `fsync(ROOT_INO, false)` is the whole-mount barrier.
    fn fsync(&self, ino: Ino, data_only: bool) -> Result<()>;

    /// Makes the name and attribute changes already applied in `ino`'s snapshot durable,
    /// without writing file data that is still dirty: a file's unflushed bytes stay unstable,
    /// exactly as after `write`, until its own `fsync`.
    ///
    /// This is for a transport that gets no second chance. A caller that renames a file and then
    /// `fsync`s the parent directory has done the whole POSIX dance, and on a client that sends
    /// no COMMIT for a directory `fsync` the name is still only in memory when that `fsync`
    /// returns. Such a transport calls this before it answers, so the answer means what the
    /// caller was told it means. It is deliberately not a per-`lookup` or per-`write` cost.
    ///
    /// The default is `fsync(ino, false)`, which is at least as strong.
    fn sync_namespace(&self, ino: Ino) -> Result<()> {
        self.fsync(ino, false)
    }

    /// Lists a directory in a stable order, starting after `cookie` (0 means from the start).
    /// Returns at most `max` entries, excluding `.` and `..`; `max == 0` is
    /// `Error::InvalidArgument`. Cookies stay valid while entries are added or removed: an
    /// entry that existed for the whole listing appears exactly once, and a removed entry is
    /// never repeated.
    fn readdir(&self, dir: Ino, cookie: u64, max: usize) -> Result<ReadDir>;

    /// Like `readdir`, with each entry's attributes. Implementations that can produce them
    /// cheaply should override this; the default calls `getattr` per entry and skips entries
    /// that vanished in between.
    fn readdir_attrs(&self, dir: Ino, cookie: u64, max: usize) -> Result<ReadDirPlus> {
        let listing = self.readdir(dir, cookie, max)?;
        let mut entries = Vec::with_capacity(listing.entries.len());
        for entry in listing.entries {
            match self.getattr(entry.ino) {
                Ok(attr) => entries.push(DirEntryPlus { entry, attr }),
                Err(crate::Error::Stale | crate::Error::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(ReadDirPlus {
            entries,
            eof: listing.eof,
        })
    }

    fn statfs(&self) -> Result<StatFs>;

    /// Extended attribute value. `Error::NoAttr` if absent.
    fn getxattr(&self, ino: Ino, name: &[u8]) -> Result<Vec<u8>>;

    fn setxattr(&self, ino: Ino, name: &[u8], value: &[u8], flags: XattrFlags) -> Result<()>;

    fn listxattr(&self, ino: Ino) -> Result<Vec<Vec<u8>>>;

    /// `Error::NoAttr` if absent.
    fn removexattr(&self, ino: Ino, name: &[u8]) -> Result<()>;

    /// Allocates, punches or zeroes `[offset, offset + len)` of a regular file atomically and
    /// returns the new attributes. See `FallocMode` for each mode.
    ///
    /// `len == 0` is `Error::InvalidArgument`. A range past the largest file size is `Error::FileTooBig`
    /// and changes nothing. A directory is `Error::IsDir` and a symlink `Error::InvalidArgument`.
    /// `PunchHole` and both `ZeroRange` modes are content changes, so they set mtime and ctime.
    /// There is no preallocation: no mode consumes space, and `blocks` never grows.
    ///
    /// The default is `Error::NotSupported`. An implementation that forwards calls to another
    /// `Vfs` must forward this one, or it hides the capability.
    fn fallocate(&self, _ino: Ino, _mode: FallocMode, _offset: u64, _len: u64) -> Result<Attr> {
        Err(crate::Error::NotSupported)
    }
}
