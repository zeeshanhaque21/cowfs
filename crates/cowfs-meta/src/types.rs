//! Public value types and the on-disk record encodings.

use crate::{Error, Result};
use cowfs_store::{BlockId, ChunkRef};
use std::fmt;

/// Inode number. Never reused. The root directory is [`ROOT_INO`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Ino(pub u64);

/// The root directory of every snapshot.
pub const ROOT_INO: Ino = Ino(1);

/// A contiguous half-open range of inode numbers reserved before any of them names an inode.
///
/// `end` is exclusive, so the range holds `end - start` numbers and a one-number range is
/// `start..start + 1`. The numbers come from the same allocator ordinary creation draws on, so no
/// other caller is handed one of them, and the durable floor is committed before the range is
/// returned, so a number is not reissued after a reopen even if it never gets used.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InoRange {
    start: Ino,
    end: Ino,
}

impl InoRange {
    /// Builds a range whose first number is `start` and whose end is one past the last.
    pub const fn new(start: Ino, end: Ino) -> Self {
        Self { start, end }
    }

    /// The lowest number in the range.
    pub const fn start(&self) -> Ino {
        self.start
    }

    /// One past the highest number, exclusive.
    pub const fn end(&self) -> Ino {
        self.end
    }

    /// How many numbers the range holds.
    pub const fn len(&self) -> u64 {
        self.end.0 - self.start.0
    }

    /// Always false for a range this crate hands out: asking for zero numbers is refused.
    pub const fn is_empty(&self) -> bool {
        self.end.0 == self.start.0
    }

    /// Whether `ino` is one of the numbers in the range.
    pub const fn contains(&self, ino: Ino) -> bool {
        ino.0 >= self.start.0 && ino.0 < self.end.0
    }

    /// Every number in the range, lowest first.
    pub fn iter(&self) -> impl Iterator<Item = Ino> + '_ {
        (self.start.0..self.end.0).map(Ino)
    }
}

/// One inode number a [`Meta`](crate::Meta) session has reserved and not yet created.
///
/// This is the capability a selected-number create needs. It is deliberately `!Copy` and `!Clone`
/// with a private field, so a caller cannot duplicate it and mint the same number twice, and it
/// carries the store's own identity and the session that minted it, so a create can refuse a ticket
/// from another store or from a session that has since closed. Only [`Meta::reserve_tickets`]
/// mints one, and the session removes it from its outstanding set when the create commits.
///
/// [`Meta::reserve_tickets`]: crate::Meta::reserve_tickets
#[derive(Debug, PartialEq, Eq)]
pub struct ReservedIno {
    pub(crate) store: u64,
    pub(crate) ino: Ino,
}

impl ReservedIno {
    /// The reserved inode number itself. Read-only: reading it does not spend the ticket.
    pub const fn ino(&self) -> Ino {
        self.ino
    }
}

/// Snapshot id. Never reused.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct SnapshotId(pub u64);

/// Longest name of a directory entry or xattr, in bytes.
pub const NAME_MAX: usize = 255;
/// Largest xattr value, in bytes.
pub const XATTR_MAX: usize = 64 * 1024;
/// Longest symlink target, in bytes.
pub const SYMLINK_MAX: usize = 4096;

/// Inode numbers are below this bound (40 bits) so a snapshot id and an inode fit in one `u64`.
pub const INO_LIMIT: u64 = 1 << 40;
/// Snapshot ids are below this bound (24 bits).
pub const SNAPSHOT_LIMIT: u64 = 1 << 24;

/// Kind of an inode.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FileType {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symbolic link.
    Symlink,
}

impl FileType {
    fn code(self) -> u8 {
        match self {
            FileType::File => 1,
            FileType::Dir => 2,
            FileType::Symlink => 3,
        }
    }

    pub(crate) fn from_code(c: u8) -> Result<Self> {
        match c {
            1 => Ok(FileType::File),
            2 => Ok(FileType::Dir),
            3 => Ok(FileType::Symlink),
            _ => Err(Error::Corrupt(format!("unknown file type {c}"))),
        }
    }
}

/// Seconds and nanoseconds since the Unix epoch.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Timestamp {
    /// Whole seconds.
    pub secs: i64,
    /// Nanoseconds within the second.
    pub nanos: u32,
}

impl Timestamp {
    /// The current wall-clock time.
    pub fn now() -> Self {
        let d = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        Self {
            secs: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
            nanos: d.subsec_nanos(),
        }
    }
}

/// Attributes of an inode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attr {
    /// Inode number.
    pub ino: Ino,
    /// File type.
    pub kind: FileType,
    /// Permission bits.
    pub mode: u32,
    /// Number of directory entries naming this inode (directories: 2 plus subdirectories).
    pub nlink: u32,
    /// Size in bytes (files: logical size, symlinks: target length, directories: 0).
    pub size: u64,
    /// Last access time.
    pub atime: Timestamp,
    /// Last modification time.
    pub mtime: Timestamp,
    /// Last status change time.
    pub ctime: Timestamp,
}

/// One directory entry.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DirEntry {
    /// Entry name.
    pub name: Vec<u8>,
    /// Inode the name refers to.
    pub ino: Ino,
    /// Type of that inode.
    pub kind: FileType,
    /// Position cookie: resuming with this value returns the entries after this one.
    pub cookie: u64,
}

/// A page of a directory listing.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ReadDir {
    /// Entries in cookie order.
    pub entries: Vec<DirEntry>,
    /// Cookie to resume from (the last entry's cookie, or the input cookie if none).
    pub next_cookie: u64,
    /// True when no entries remain after this page.
    pub end: bool,
}

/// Fields to change in `setattr`. `None` leaves a field alone.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct SetAttr {
    /// New permission bits.
    pub mode: Option<u32>,
    /// New access time.
    pub atime: Option<Timestamp>,
    /// New modification time.
    pub mtime: Option<Timestamp>,
    /// New size: growing makes a hole, shrinking must land on a chunk boundary.
    pub size: Option<u64>,
}

/// Result of removing a name.
#[derive(Clone, PartialEq, Debug)]
pub struct Removed {
    /// The removed inode's attributes as they were before removal, with `nlink` after removal.
    pub attr: Attr,
    /// True when this removal deleted the inode.
    pub freed: bool,
    /// Chunk list of the deleted inode (empty unless `freed` and a regular file).
    pub chunks: Vec<ChunkRef>,
}

/// A run of a file's chunk list with the content version it was read at.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChunkRange {
    /// Content version of the file when read. Pass it to `splice_content` as `expected_version`.
    pub version: u64,
    /// Logical file size.
    pub size: u64,
    /// Bytes of the file covered by chunks (the rest up to `size` is a trailing hole).
    pub covered: u64,
    /// `(byte offset, chunk)` pairs in offset order.
    pub chunks: Vec<(u64, ChunkRef)>,
}

/// One snapshot as listed by `Meta::snapshots`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SnapshotInfo {
    /// Snapshot id.
    pub id: SnapshotId,
    /// Unique name.
    pub name: String,
    /// Merkle root of the snapshot's tree.
    pub root: crate::NodeId,
    /// Creation time.
    pub created: Timestamp,
    /// Snapshot it was cloned from, if any.
    pub parent: Option<SnapshotId>,
}

impl fmt::Display for Ino {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

// Key layout: inode (8, big-endian), kind (1), suffix.
pub(crate) const K_INODE: u8 = 1;
pub(crate) const K_NAME: u8 = 2;
pub(crate) const K_COOKIE: u8 = 3;
pub(crate) const K_XATTR: u8 = 4;
pub(crate) const K_CHUNK: u8 = 5;
pub(crate) const K_LINK: u8 = 6;

pub(crate) fn key(ino: Ino, kind: u8, suffix: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(9 + suffix.len());
    k.extend(ino.0.to_be_bytes());
    k.push(kind);
    k.extend_from_slice(suffix);
    k
}

pub(crate) fn split_key(k: &[u8]) -> Result<(Ino, u8, &[u8])> {
    let ino = k
        .get(..8)
        .and_then(|b| <[u8; 8]>::try_from(b).ok())
        .ok_or_else(|| Error::Corrupt("short key".into()))?;
    let kind = *k.get(8).ok_or_else(|| Error::Corrupt("short key".into()))?;
    Ok((Ino(u64::from_be_bytes(ino)), kind, &k[9..]))
}

fn rd<const N: usize>(b: &[u8], pos: usize) -> Result<[u8; N]> {
    b.get(pos..pos + N)
        .and_then(|s| <[u8; N]>::try_from(s).ok())
        .ok_or_else(|| Error::Corrupt("short record".into()))
}

/// The stored inode record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct InodeRec {
    pub kind: FileType,
    pub mode: u32,
    pub nlink: u32,
    pub size: u64,
    pub atime: Timestamp,
    pub mtime: Timestamp,
    pub ctime: Timestamp,
    pub parent: u64,
    pub next_cookie: u64,
    /// Bytes covered by chunk extents (files); always at most `size`.
    pub covered: u64,
    /// Bumped by every content change; the compare-and-swap token of `splice_content`.
    pub cversion: u64,
}

const INODE_V: u8 = 2;
const INODE_LEN: usize = 2 + 4 + 4 + 8 + 3 * 12 + 8 + 8 + 8 + 8;

impl InodeRec {
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(INODE_LEN);
        b.push(INODE_V);
        b.push(self.kind.code());
        b.extend(self.mode.to_le_bytes());
        b.extend(self.nlink.to_le_bytes());
        b.extend(self.size.to_le_bytes());
        for t in [self.atime, self.mtime, self.ctime] {
            b.extend(t.secs.to_le_bytes());
            b.extend(t.nanos.to_le_bytes());
        }
        b.extend(self.parent.to_le_bytes());
        b.extend(self.next_cookie.to_le_bytes());
        b.extend(self.covered.to_le_bytes());
        b.extend(self.cversion.to_le_bytes());
        b
    }

    pub(crate) fn decode(b: &[u8]) -> Result<Self> {
        if b.len() != INODE_LEN || b[0] != INODE_V {
            return Err(Error::Corrupt("bad inode record".into()));
        }
        let ts = |pos| -> Result<Timestamp> {
            Ok(Timestamp {
                secs: i64::from_le_bytes(rd(b, pos)?),
                nanos: u32::from_le_bytes(rd(b, pos + 8)?),
            })
        };
        Ok(Self {
            kind: FileType::from_code(b[1])?,
            mode: u32::from_le_bytes(rd(b, 2)?),
            nlink: u32::from_le_bytes(rd(b, 6)?),
            size: u64::from_le_bytes(rd(b, 10)?),
            atime: ts(18)?,
            mtime: ts(30)?,
            ctime: ts(42)?,
            parent: u64::from_le_bytes(rd(b, 54)?),
            next_cookie: u64::from_le_bytes(rd(b, 62)?),
            covered: u64::from_le_bytes(rd(b, 70)?),
            cversion: u64::from_le_bytes(rd(b, 78)?),
        })
    }

    pub(crate) fn attr(&self, ino: Ino) -> Attr {
        Attr {
            ino,
            kind: self.kind,
            mode: self.mode,
            nlink: self.nlink,
            size: self.size,
            atime: self.atime,
            mtime: self.mtime,
            ctime: self.ctime,
        }
    }
}

/// Value of a by-name directory record.
pub(crate) fn name_val(child: Ino, kind: FileType, cookie: u64) -> Vec<u8> {
    let mut b = Vec::with_capacity(17);
    b.extend(child.0.to_le_bytes());
    b.push(kind.code());
    b.extend(cookie.to_le_bytes());
    b
}

pub(crate) fn decode_name_val(b: &[u8]) -> Result<(Ino, FileType, u64)> {
    if b.len() != 17 {
        return Err(Error::Corrupt("bad dirent record".into()));
    }
    Ok((
        Ino(u64::from_le_bytes(rd(b, 0)?)),
        FileType::from_code(b[8])?,
        u64::from_le_bytes(rd(b, 9)?),
    ))
}

/// Value of a by-cookie directory record.
pub(crate) fn cookie_val(child: Ino, kind: FileType, name: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(9 + name.len());
    b.extend(child.0.to_le_bytes());
    b.push(kind.code());
    b.extend_from_slice(name);
    b
}

pub(crate) fn decode_cookie_val(b: &[u8]) -> Result<(Ino, FileType, &[u8])> {
    if b.len() < 9 {
        return Err(Error::Corrupt("bad dirent cookie record".into()));
    }
    Ok((
        Ino(u64::from_le_bytes(rd(b, 0)?)),
        FileType::from_code(b[8])?,
        &b[9..],
    ))
}

pub(crate) fn encode_chunks(chunks: &[ChunkRef]) -> Result<Vec<u8>> {
    let mut b = Vec::with_capacity(chunks.len() * 36);
    for c in chunks {
        // the flag and the sentinel must agree before anything reaches the medium: a hole that
        // named a stored block would lose that block from every walk
        c.validate()
            .map_err(|_| Error::Invalid("chunk ref is neither a hole nor a block"))?;
        b.extend_from_slice(c.id.as_bytes());
        b.extend(c.len.to_le_bytes());
    }
    Ok(b)
}

pub(crate) fn decode_chunks(b: &[u8]) -> Result<Vec<ChunkRef>> {
    if !b.len().is_multiple_of(36) {
        return Err(Error::Corrupt("bad chunk segment".into()));
    }
    b.as_chunks::<36>()
        .0
        .iter()
        .map(|c| {
            let id = BlockId::from_bytes(rd(c, 0)?);
            let len = u32::from_le_bytes(rd(c, 32)?);
            // a store written before the flag existed still says "hole" with the sentinel, so the
            // flag comes from the id here and costs nothing on the medium
            let r = ChunkRef {
                id,
                len,
                hole: id == cowfs_store::HOLE,
            };
            r.validate()
                .map_err(|_| Error::Corrupt("chunk ref is not a hole or a block".into()))?;
            Ok(r)
        })
        .collect()
}

pub(crate) fn validate_name(name: &[u8]) -> Result<()> {
    if name.is_empty() || name == b"." || name == b".." {
        return Err(Error::Invalid("bad name"));
    }
    if name.len() > NAME_MAX {
        return Err(Error::NameTooLong);
    }
    if name.iter().any(|&c| c == b'/' || c == 0) {
        return Err(Error::Invalid("bad name"));
    }
    Ok(())
}
