use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

/// Opaque inode number. Unique per live file within a mount, including across snapshots:
/// two snapshots that share content still report different numbers, so tools never
/// mistake them for hardlinks. `ROOT_INO` is the mount root.
pub type Ino = u64;

/// The mount root.
pub const ROOT_INO: Ino = 1;

/// Longest allowed file name in bytes.
pub const NAME_MAX: usize = 255;

/// Permission bits a `mode` may carry (rwx for user, group, other, plus setuid, setgid, sticky).
pub const MODE_MASK: u32 = 0o7777;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    Regular,
    Directory,
    Symlink,
}

/// A point in time with nanosecond resolution.
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
/// mounter's and are accepted but ignored by `setattr`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attr {
    pub ino: Ino,
    pub kind: FileKind,
    /// Permission bits only (`mode & MODE_MASK`).
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    /// Size in bytes: file length, symlink target length, or an implementation defined value for directories.
    pub size: u64,
    /// 512-byte units actually stored, for `st_blocks`.
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
/// `readdir` to continue listing after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub ino: Ino,
    pub kind: FileKind,
    pub name: Vec<u8>,
    pub cookie: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadDir {
    pub entries: Vec<DirEntry>,
    /// True when no entries remain after the last one returned.
    pub eof: bool,
}

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

/// Pins an inode open: an unlinked file stays readable and writable until its last handle is released.
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
/// and not `.` or `..`.
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
