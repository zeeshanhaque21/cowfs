use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

/// Opaque inode number, unique per file within one mount.
///
/// - `0` is never a valid `Ino`.
/// - An `Ino` is never reused for a different file for the lifetime of the mount: NFS file
///   handles and kernel dentry caches outlive the file, and a reused number would let a
///   stale handle read another file's bytes. Every implementation must guarantee this.
/// - An `Ino` that has been handed out keeps meaning the same inode for the rest of the mount
///   session, for as long as that inode exists. The number of an inode is not a function of
///   when the caller last mentioned it: two lookups of one inode in one session return the
///   same number, and a number does not revert to a different form when the implementation
///   commits it. Hardlinks and renames keep that number too, so a client never sees two
///   numbers for one inode in a session.
/// - The only thing that makes a number `Stale` is the inode going away: unlinked with no
///   other name and no open handle. A protocol that cannot say when it is done with a number,
///   such as NFS where a filehandle is just the number, relies on this rule; see `forget`.
/// - Two snapshots that share content still report different numbers, so tools never
///   mistake them for hardlinks. Hardlinks within one snapshot share one `Ino`.
/// - Uniqueness is per mount, not per host. Whether numbers survive a restart is up to the
///   implementation, but a number from a previous run must never name a different file:
///   it must be `Stale` or the same file.
/// - `ROOT_INO` is the mount root.
pub type Ino = u64;

/// The mount root.
pub const ROOT_INO: Ino = 1;

/// Longest allowed file name in bytes.
pub const NAME_MAX: usize = 255;

/// Permission bits a `mode` may carry (rwx for user, group, other, plus setuid, setgid, sticky).
pub const MODE_MASK: u32 = 0o7777;

/// New kinds may be added, so match with a wildcard arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum FileKind {
    Regular,
    Directory,
    Symlink,
}

/// A point in time with nanosecond resolution. `nanos` is below 1_000_000_000. Protocols with
/// narrower ranges (NFSv3 carries `u32` seconds) clamp in the adapter, not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Timestamp {
    pub secs: i64,
    pub nanos: u32,
}

impl Timestamp {
    pub fn now() -> Self {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => Self {
                secs: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
                nanos: d.subsec_nanos(),
            },
            Err(_) => Self::default(),
        }
    }
}

/// How `setattr` should set a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetTime {
    /// The backend's current time.
    Now,
    At(Timestamp),
}

/// File attributes. Everything is owned by the mounter, so uid and gid are reported as the
/// mounter's. `SetAttr` has no uid or gid. The NFS adapter answers a `chown` to another uid with
/// `EPERM` and accepts one naming the current uid as a no-op, and so does the FUSE adapter. Both
/// accept and ignore any gid.
///
/// Every operation that adds, removes or renames a name (`create`, `mkdir`, `symlink`,
/// `link`, `unlink`, `rmdir`, `rename`) sets the mtime and ctime of the parent directory
/// (both parents for `rename`) and the ctime of the affected file, and leaves the file's
/// mtime alone. `create`, `mkdir` and `setattr` mask `mode` with `MODE_MASK`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attr {
    pub ino: Ino,
    pub kind: FileKind,
    /// Permission bits only (`mode & MODE_MASK`).
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    /// Size in bytes: file length, symlink target length (in bytes), or an implementation
    /// defined value for directories.
    pub size: u64,
    /// 512-byte units of logical allocation of non-hole data, for `st_blocks`. Allocation
    /// granularity is the backend's: one byte of data may report several blocks.
    pub blocks: u64,
    pub atime: Timestamp,
    pub mtime: Timestamp,
    pub ctime: Timestamp,
}

/// Attribute changes. `None` leaves the field alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SetAttr {
    pub mode: Option<u32>,
    pub size: Option<u64>,
    pub atime: Option<SetTime>,
    pub mtime: Option<SetTime>,
}

/// One directory entry. `cookie` is the position just after this entry: pass it back to
/// `readdir` to continue listing after it. A cookie is never `0` (that means "from the
/// start"), is opaque, and is valid only for the directory it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub ino: Ino,
    pub kind: FileKind,
    pub name: Vec<u8>,
    pub cookie: u64,
}

/// A directory entry together with the attributes of its target, as `getattr` would return
/// them. Lets adapters answer READDIRPLUS-style requests without one `getattr` per entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntryPlus {
    pub entry: DirEntry,
    pub attr: Attr,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadDirPlus {
    pub entries: Vec<DirEntryPlus>,
    /// True when no entries remain after the last one returned.
    pub eof: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadDir {
    pub entries: Vec<DirEntry>,
    /// True when no entries remain after the last one returned.
    pub eof: bool,
}

/// A field of `0` means unknown, not zero (btrfs reports `files` as 0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StatFs {
    pub block_size: u32,
    pub blocks: u64,
    pub blocks_free: u64,
    pub blocks_available: u64,
    pub files: u64,
    pub files_free: u64,
    pub name_max: u32,
}

/// Pins an inode: an unlinked file stays readable and writable until its last handle is
/// released. A handle is not a capability and carries no access mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileHandle(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenameFlags {
    /// Fail with `Error::Exists` if the destination already exists.
    pub no_replace: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct XattrFlags {
    /// Fail with `Error::Exists` if the attribute already exists.
    pub create: bool,
    /// Fail with `Error::NoAttr` if the attribute does not exist.
    pub replace: bool,
}

/// Checks a single path component: non-empty, at most `NAME_MAX` bytes, no `/` and no NUL,
/// and not `.` or `..`. Every operation that takes a name returns exactly these errors for
/// a bad one: `InvalidArgument` for empty, `.`, `..`, `/` or NUL, and `NameTooLong` over
/// `NAME_MAX`.
pub fn validate_name(name: &[u8]) -> Result<()> {
    if name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') || name.contains(&0)
    {
        return Err(Error::InvalidArgument);
    }
    if name.len() > NAME_MAX {
        return Err(Error::NameTooLong);
    }
    Ok(())
}
