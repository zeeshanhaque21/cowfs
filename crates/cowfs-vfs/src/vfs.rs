use crate::error::Result;
use crate::types::{Attr, FileHandle, Ino, ReadDir, RenameFlags, SetAttr, StatFs, XattrFlags};

/// The POSIX-like filesystem a mount adapter serves. Operations are by inode number and
/// are safe to call from many threads at once.
///
/// Semantics follow POSIX. Names are raw bytes: no `/`, no NUL, at most `NAME_MAX` bytes,
/// never `.` or `..` (adapters handle those). Any operation on an inode that does not
/// exist returns `Error::Stale`. Every operation that changes a file updates ctime, and
/// content changes also update mtime.
pub trait Vfs: Send + Sync {
    /// Attributes of the child `name` of directory `parent`. `Error::NotFound` if absent,
    /// `Error::NotDir` if `parent` is not a directory.
    fn lookup(&self, parent: Ino, name: &[u8]) -> Result<Attr>;

    /// Called when an adapter drops `count` of its references to `ino` that were handed out
    /// by `lookup`, `create`, `mkdir`, `symlink` or `link`. An inode with no links and no
    /// references or open handles may then be reclaimed. The default does nothing.
    fn forget(&self, _ino: Ino, _count: u64) {}

    fn getattr(&self, ino: Ino) -> Result<Attr>;

    /// Applies the requested changes atomically and returns the new attributes.
    /// `size` truncates or extends with zeros and is only valid for regular files
    /// (`Error::IsDir` for directories, `Error::InvalidArgument` for symlinks).
    /// `mode` is masked with `MODE_MASK`.
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

    /// Pins `ino` open until `release`. Fails with `Error::IsDir` only if a caller asks to write.
    fn open(&self, ino: Ino) -> Result<FileHandle>;

    fn release(&self, handle: FileHandle) -> Result<()>;

    /// Reads up to `size` bytes at `offset`. A read at or past the end returns an empty vector,
    /// a read that crosses the end returns the bytes up to it. Holes read as zeros.
    fn read(&self, ino: Ino, offset: u64, size: u32) -> Result<Vec<u8>>;

    /// Writes `data` at `offset`, extending the file (with a zero-filled hole if `offset` is
    /// past the end). Returns the number of bytes written, which is `data.len()` on success.
    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> Result<u32>;

    /// Called when a file descriptor is closed. Buffered data may stay buffered.
    fn flush(&self, ino: Ino) -> Result<()>;

    /// Makes previously written data and metadata for `ino` durable. With `data_only`,
    /// metadata that does not affect reading the data may be skipped.
    fn fsync(&self, ino: Ino, data_only: bool) -> Result<()>;

    /// Lists a directory in a stable order, starting after `cookie` (0 means from the start).
    /// Returns at most `max` entries, excluding `.` and `..`. Cookies stay valid while entries
    /// are added or removed: an entry that existed for the whole listing appears exactly once,
    /// and a removed entry is never repeated.
    fn readdir(&self, dir: Ino, cookie: u64, max: usize) -> Result<ReadDir>;

    fn statfs(&self) -> Result<StatFs>;

    /// Extended attribute value. `Error::NoAttr` if absent.
    fn getxattr(&self, ino: Ino, name: &[u8]) -> Result<Vec<u8>>;

    fn setxattr(&self, ino: Ino, name: &[u8], value: &[u8], flags: XattrFlags) -> Result<()>;

    fn listxattr(&self, ino: Ino) -> Result<Vec<Vec<u8>>>;

    /// `Error::NoAttr` if absent.
    fn removexattr(&self, ino: Ino, name: &[u8]) -> Result<()>;
}
