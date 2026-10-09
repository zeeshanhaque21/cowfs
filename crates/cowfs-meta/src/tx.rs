//! Mutating operations, applied to a snapshot's in-memory tree.

use crate::ptree::{Entry, MemTree, NodeSource};
use crate::read::{self, Reader};
use crate::types::*;
use crate::{Error, Result};
use cowfs_store::ChunkRef;
use std::collections::{HashMap, HashSet};

#[cfg(test)]
std::thread_local! {
    pub(crate) static SKIP_INODE_LIMIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Hands out inode numbers below a durable high-water mark.
#[derive(Debug)]
pub(crate) struct InoAlloc {
    pub(crate) next: u64,
    pub(crate) reserved: u64,
    pub(crate) block: u64,
}

/// A batch of operations on one snapshot, applied atomically.
///
/// Obtained from [`Snapshot::batch`](crate::Snapshot::batch). Other readers see nothing until the
/// closure returns `Ok`; on `Err` every change to the tree is discarded (inode numbers handed out
/// inside the closure are still consumed and never reused).
pub struct Tx<'a> {
    pub(crate) tree: &'a mut MemTree,
    pub(crate) src: &'a dyn NodeSource,
    pub(crate) ino: &'a mut InoAlloc,
    pub(crate) reserve: &'a dyn Fn(u64) -> Result<()>,
    /// Identity of the store this transaction runs against, compared with a [`ReservedIno`]'s.
    pub(crate) store: u64,
    /// The session's outstanding reserved numbers. A selected create is admitted only for a number
    /// in here, which is what refuses a ticket from another store or a closed session.
    pub(crate) reserved: &'a HashSet<Ino>,
    /// Numbers already spent inside this one transaction, so a batch cannot create two inodes at
    /// the same reserved number.
    pub(crate) spent: &'a mut HashSet<Ino>,
    /// Set when this transaction creates a special file, so the commit that persists it also
    /// records the format version that only builds that know special files can open.
    pub(crate) special: &'a mut bool,
    pub(crate) now: Timestamp,
}

impl std::fmt::Debug for Tx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tx").finish_non_exhaustive()
    }
}

impl Reader for Tx<'_> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.tree.get(self.src, key)
    }

    fn seek_ge(&self, key: &[u8]) -> Result<Option<Entry>> {
        self.tree.seek_ge(self.src, key)
    }

    fn scan(&self, from: &[u8], prefix: &[u8], limit: usize) -> Result<Vec<Entry>> {
        self.tree.scan(self.src, from, prefix, limit)
    }
}

const PAGE: usize = 512;

impl Tx<'_> {
    fn put(&mut self, key: Vec<u8>, val: Vec<u8>) -> Result<()> {
        self.tree.insert(self.src, &key, val)
    }

    fn del(&mut self, key: &[u8]) -> Result<()> {
        self.tree.remove(self.src, key).map(|_| ())
    }

    fn put_inode(&mut self, ino: Ino, rec: &InodeRec) -> Result<()> {
        self.put(key(ino, K_INODE, &[]), rec.encode())
    }

    fn alloc(&mut self) -> Result<Ino> {
        let a = &mut *self.ino;
        let exhausted = a.next >= INO_LIMIT;
        #[cfg(test)]
        let exhausted = exhausted && !SKIP_INODE_LIMIT.with(|c| c.get());
        if exhausted {
            return Err(Error::LimitExceeded("inode numbers exhausted"));
        }
        if a.next >= a.reserved {
            let new = (a.next + a.block.max(1)).min(INO_LIMIT);
            (self.reserve)(new)?;
            self.ino.reserved = new;
        }
        let ino = Ino(self.ino.next);
        self.ino.next += 1;
        Ok(ino)
    }

    fn new_rec(&self, kind: FileType, mode: u32, parent: Ino) -> InodeRec {
        InodeRec {
            kind,
            mode: mode & 0o7777,
            nlink: if kind == FileType::Dir { 2 } else { 1 },
            size: 0,
            atime: self.now,
            mtime: self.now,
            ctime: self.now,
            parent: parent.0,
            next_cookie: 1,
            covered: 0,
            cversion: 0,
            rdev: 0,
        }
    }

    fn add_entry(
        &mut self,
        dir: Ino,
        name: &[u8],
        child: Ino,
        kind: FileType,
        nlink_delta: i64,
    ) -> Result<()> {
        let mut d = read::inode(self, dir)?;
        let cookie = d.next_cookie;
        d.next_cookie += 1;
        d.nlink = apply_delta(d.nlink, nlink_delta)?;
        d.mtime = self.now;
        d.ctime = self.now;
        self.put(key(dir, K_NAME, name), name_val(child, kind, cookie))?;
        self.put(
            key(dir, K_COOKIE, &cookie.to_be_bytes()),
            cookie_val(child, kind, name),
        )?;
        self.put_inode(dir, &d)
    }

    fn remove_entry(&mut self, dir: Ino, name: &[u8], cookie: u64, nlink_delta: i64) -> Result<()> {
        let mut d = read::inode(self, dir)?;
        d.nlink = apply_delta(d.nlink, nlink_delta)?;
        d.mtime = self.now;
        d.ctime = self.now;
        self.del(&key(dir, K_NAME, name))?;
        self.del(&key(dir, K_COOKIE, &cookie.to_be_bytes()))?;
        self.put_inode(dir, &d)
    }

    fn delete_prefix(&mut self, prefix: &[u8]) -> Result<()> {
        loop {
            let rows = self.scan(prefix, prefix, PAGE)?;
            if rows.is_empty() {
                return Ok(());
            }
            for (k, _) in rows {
                self.del(&k)?;
            }
        }
    }

    fn drop_inode(&mut self, ino: Ino, rec: &InodeRec) -> Result<Vec<ChunkRef>> {
        let chunks = if rec.kind == FileType::File {
            read::chunks(self, ino)?
        } else {
            Vec::new()
        };
        self.delete_prefix(&ino.0.to_be_bytes())?;
        Ok(chunks)
    }

    fn unref(&mut self, ino: Ino) -> Result<Removed> {
        let mut rec = read::inode(self, ino)?;
        rec.nlink = rec
            .nlink
            .checked_sub(1)
            .ok_or_else(|| Error::Corrupt("nlink underflow".into()))?;
        if rec.nlink == 0 {
            let chunks = self.drop_inode(ino, &rec)?;
            return Ok(Removed {
                attr: rec.attr(ino),
                freed: true,
                chunks,
            });
        }
        rec.ctime = self.now;
        self.put_inode(ino, &rec)?;
        Ok(Removed {
            attr: rec.attr(ino),
            freed: false,
            chunks: Vec::new(),
        })
    }

    fn free_dir(&mut self, ino: Ino) -> Result<Removed> {
        let mut rec = read::inode(self, ino)?;
        self.drop_inode(ino, &rec)?;
        rec.nlink = 0;
        Ok(Removed {
            attr: rec.attr(ino),
            freed: true,
            chunks: Vec::new(),
        })
    }

    fn new_child(
        &mut self,
        dir: Ino,
        name: &[u8],
        kind: FileType,
        mode: u32,
        target: Option<&[u8]>,
        rdev: u64,
    ) -> Result<Attr> {
        validate_name(name)?;
        read::dir_inode(self, dir)?;
        if read::entry(self, dir, name)?.is_some() {
            return Err(Error::Exists);
        }
        if let Some(t) = target {
            if t.is_empty() {
                return Err(Error::Invalid("empty symlink target"));
            }
            if t.len() > SYMLINK_MAX {
                return Err(Error::TooBig);
            }
        }
        let ino = self.alloc()?;
        let mut rec = self.new_rec(kind, mode, dir);
        rec.rdev = rdev;
        *self.special |= kind.is_special();
        let delta = i64::from(kind == FileType::Dir);
        if let Some(t) = target {
            rec.size = t.len() as u64;
            self.put(key(ino, K_LINK, &[]), t.to_vec())?;
        }
        self.put_inode(ino, &rec)?;
        self.add_entry(dir, name, ino, kind, delta)?;
        Ok(rec.attr(ino))
    }

    /// [`Tx::new_child`], but at a number taken from a reservation instead of a fresh allocation.
    ///
    /// The number comes only from `ticket`, a [`ReservedIno`] this store's session minted and has
    /// not spent. `ticket.store` must equal this transaction's store and `ticket.ino` must be in the
    /// session's outstanding set, so a ticket from another store or a session that has closed is
    /// refused. The number is then spent once: the inode record must not already exist, and a second
    /// create at the same number in this transaction is refused.
    #[allow(clippy::too_many_arguments)] // the one extra argument is the device number
    fn new_child_at(
        &mut self,
        dir: Ino,
        name: &[u8],
        kind: FileType,
        mode: u32,
        target: Option<&[u8]>,
        rdev: u64,
        ticket: &ReservedIno,
    ) -> Result<Attr> {
        validate_name(name)?;
        if ticket.store != self.store {
            return Err(Error::Invalid(
                "reserved number belongs to a different store",
            ));
        }
        let ino = ticket.ino;
        if !self.reserved.contains(&ino) {
            return Err(Error::Invalid(
                "reserved number was not issued by this store's open session",
            ));
        }
        if self.spent.contains(&ino) {
            return Err(Error::Exists);
        }
        read::dir_inode(self, dir)?;
        if read::entry(self, dir, name)?.is_some() {
            return Err(Error::Exists);
        }
        if read::inode(self, ino).is_ok() {
            return Err(Error::Exists);
        }
        if let Some(t) = target {
            if t.is_empty() {
                return Err(Error::Invalid("empty symlink target"));
            }
            if t.len() > SYMLINK_MAX {
                return Err(Error::TooBig);
            }
        }
        let mut rec = self.new_rec(kind, mode, dir);
        rec.rdev = rdev;
        *self.special |= kind.is_special();
        let delta = i64::from(kind == FileType::Dir);
        if let Some(t) = target {
            rec.size = t.len() as u64;
            self.put(key(ino, K_LINK, &[]), t.to_vec())?;
        }
        self.put_inode(ino, &rec)?;
        self.add_entry(dir, name, ino, kind, delta)?;
        self.spent.insert(ino);
        Ok(rec.attr(ino))
    }

    /// The time every inode this transaction touches is stamped with.
    ///
    /// Defaults to the wall clock at the moment the transaction opened, which is what a caller that
    /// applies changes as it makes them wants. A caller replaying operations that happened earlier
    /// sets it per operation, so a deferred change records when it happened rather than when the
    /// batch that carried it committed.
    ///
    /// A newly created inode takes all three of its times from this value. An inode that already
    /// exists takes only its `ctime` from it, and its `atime` and `mtime` are whatever the caller
    /// asked for. A time given explicitly in a `setattr` is never replaced, including on a create.
    pub fn set_now(&mut self, now: Timestamp) {
        self.now = now;
    }

    /// Creates an empty regular file.
    pub fn create(&mut self, dir: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        self.new_child(dir, name, FileType::File, mode, None, 0)
    }

    /// Creates an empty directory.
    pub fn mkdir(&mut self, dir: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        self.new_child(dir, name, FileType::Dir, mode, None, 0)
    }

    /// Creates a fifo, socket, character device or block device. `kind` must be one of those
    /// four, and `rdev` must be `0` unless it is a device.
    pub fn mknod(
        &mut self,
        dir: Ino,
        name: &[u8],
        kind: FileType,
        mode: u32,
        rdev: u64,
    ) -> Result<Attr> {
        check_mknod_args(kind, rdev)?;
        self.new_child(dir, name, kind, mode, None, rdev)
    }

    /// Creates a symbolic link holding `target`.
    pub fn symlink(&mut self, dir: Ino, name: &[u8], target: &[u8]) -> Result<Attr> {
        self.new_child(dir, name, FileType::Symlink, 0o777, Some(target), 0)
    }

    /// Creates an empty regular file at a number taken from a reservation.
    ///
    /// The number is `ticket`'s, which only a reservation on this same open store minted. Refused if
    /// the ticket is foreign, already spent, or names an inode that already exists.
    pub fn create_at(
        &mut self,
        dir: Ino,
        name: &[u8],
        mode: u32,
        ticket: &ReservedIno,
    ) -> Result<Attr> {
        self.new_child_at(dir, name, FileType::File, mode, None, 0, ticket)
    }

    /// Creates an empty directory at a number taken from a reservation. See [`Tx::create_at`].
    pub fn mkdir_at(
        &mut self,
        dir: Ino,
        name: &[u8],
        mode: u32,
        ticket: &ReservedIno,
    ) -> Result<Attr> {
        self.new_child_at(dir, name, FileType::Dir, mode, None, 0, ticket)
    }

    /// Creates a symbolic link holding `target`, at a number taken from a reservation. See
    /// [`Tx::create_at`].
    pub fn symlink_at(
        &mut self,
        dir: Ino,
        name: &[u8],
        target: &[u8],
        ticket: &ReservedIno,
    ) -> Result<Attr> {
        self.new_child_at(dir, name, FileType::Symlink, 0o777, Some(target), 0, ticket)
    }

    /// [`Tx::mknod`] at a number taken from a reservation. See [`Tx::create_at`].
    pub fn mknod_at(
        &mut self,
        dir: Ino,
        name: &[u8],
        kind: FileType,
        mode: u32,
        rdev: u64,
        ticket: &ReservedIno,
    ) -> Result<Attr> {
        check_mknod_args(kind, rdev)?;
        self.new_child_at(dir, name, kind, mode, None, rdev, ticket)
    }

    /// Adds another name for a file or symlink. Directories cannot be hardlinked.
    pub fn link(&mut self, ino: Ino, dir: Ino, name: &[u8]) -> Result<Attr> {
        validate_name(name)?;
        let mut rec = read::inode(self, ino)?;
        read::dir_inode(self, dir)?;
        if rec.kind == FileType::Dir {
            return Err(Error::Invalid("cannot hardlink a directory"));
        }
        if read::entry(self, dir, name)?.is_some() {
            return Err(Error::Exists);
        }
        rec.nlink = apply_delta(rec.nlink, 1)?;
        rec.ctime = self.now;
        self.put_inode(ino, &rec)?;
        self.add_entry(dir, name, ino, rec.kind, 0)?;
        Ok(rec.attr(ino))
    }

    /// Removes a name of a file or symlink; the inode is deleted with its last name.
    pub fn unlink(&mut self, dir: Ino, name: &[u8]) -> Result<Removed> {
        validate_name(name)?;
        read::dir_inode(self, dir)?;
        let (child, kind, cookie) = read::entry(self, dir, name)?.ok_or(Error::NotFound)?;
        if kind == FileType::Dir {
            return Err(Error::IsDir);
        }
        self.remove_entry(dir, name, cookie, 0)?;
        self.unref(child)
    }

    /// Removes an empty directory.
    pub fn rmdir(&mut self, dir: Ino, name: &[u8]) -> Result<Removed> {
        validate_name(name)?;
        read::dir_inode(self, dir)?;
        let (child, kind, cookie) = read::entry(self, dir, name)?.ok_or(Error::NotFound)?;
        if kind != FileType::Dir {
            return Err(Error::NotDir);
        }
        if self.dir_has_entries(child)? {
            return Err(Error::NotEmpty);
        }
        self.remove_entry(dir, name, cookie, -1)?;
        self.free_dir(child)
    }

    fn dir_has_entries(&self, dir: Ino) -> Result<bool> {
        let p = key(dir, K_NAME, &[]);
        Ok(!self.scan(&p, &p, 1)?.is_empty())
    }

    /// Renames atomically, replacing a file, a symlink or an empty directory at the destination.
    ///
    /// Returns what the replacement removed. Renaming a name onto another name of the same inode
    /// does nothing. A directory cannot move into its own subtree.
    ///
    /// Within one directory the entry keeps its cookie, so it keeps its place in listings and a
    /// listing that renames every entry it sees still terminates. Moving to another directory gives
    /// the entry a new cookie there (it appears once more at the end of that directory).
    pub fn rename(
        &mut self,
        from_dir: Ino,
        from_name: &[u8],
        to_dir: Ino,
        to_name: &[u8],
    ) -> Result<Option<Removed>> {
        validate_name(from_name)?;
        validate_name(to_name)?;
        read::dir_inode(self, from_dir)?;
        read::dir_inode(self, to_dir)?;
        let (src, skind, scookie) =
            read::entry(self, from_dir, from_name)?.ok_or(Error::NotFound)?;
        if from_dir == to_dir && from_name == to_name {
            return Ok(None);
        }
        let dst = read::entry(self, to_dir, to_name)?;
        if let Some((dino, dkind, _)) = dst {
            if dino == src {
                return Ok(None);
            }
            match (skind, dkind) {
                (FileType::Dir, FileType::Dir) => {
                    if self.dir_has_entries(dino)? {
                        return Err(Error::NotEmpty);
                    }
                }
                (FileType::Dir, _) => return Err(Error::NotDir),
                (_, FileType::Dir) => return Err(Error::IsDir),
                _ => {}
            }
        }
        if skind == FileType::Dir {
            let mut cur = to_dir;
            loop {
                if cur == src {
                    return Err(Error::Invalid("rename into own subtree"));
                }
                if cur == ROOT_INO {
                    break;
                }
                cur = Ino(read::dir_inode(self, cur)?.parent);
            }
        }
        let is_dir = i64::from(skind == FileType::Dir);
        let mut replaced = None;
        if let Some((dino, dkind, dcookie)) = dst {
            let d = i64::from(dkind == FileType::Dir);
            self.remove_entry(to_dir, to_name, dcookie, -d)?;
            replaced = Some(if dkind == FileType::Dir {
                self.free_dir(dino)?
            } else {
                self.unref(dino)?
            });
        }
        if from_dir == to_dir {
            self.del(&key(from_dir, K_NAME, from_name))?;
            self.put(key(to_dir, K_NAME, to_name), name_val(src, skind, scookie))?;
            self.put(
                key(to_dir, K_COOKIE, &scookie.to_be_bytes()),
                cookie_val(src, skind, to_name),
            )?;
            let mut d = read::inode(self, to_dir)?;
            d.mtime = self.now;
            d.ctime = self.now;
            self.put_inode(to_dir, &d)?;
        } else {
            self.remove_entry(from_dir, from_name, scookie, -is_dir)?;
            self.add_entry(to_dir, to_name, src, skind, is_dir)?;
        }
        let mut rec = read::inode(self, src)?;
        rec.ctime = self.now;
        if skind == FileType::Dir {
            rec.parent = to_dir.0;
        }
        self.put_inode(src, &rec)?;
        Ok(replaced)
    }

    /// Changes mode, times, or size.
    ///
    /// Growing makes a trailing hole. Shrinking a file below its chunk-covered length must land on
    /// a chunk boundary, otherwise it fails with [`Error::NeedsRechunk`]: the caller re-chunks the
    /// tail, `put`s the new tail block and calls `splice_content` (or `set_content`).
    pub fn setattr(&mut self, ino: Ino, set: SetAttr) -> Result<Attr> {
        let mut rec = read::inode(self, ino)?;
        if let Some(size) = set.size {
            match rec.kind {
                FileType::Dir => return Err(Error::IsDir),
                FileType::Symlink => return Err(Error::Invalid("cannot resize a symlink")),
                FileType::File => {}
                _ => return Err(Error::Invalid("cannot resize a special file")),
            }
            if size != rec.size {
                if size < rec.covered {
                    let at = key(ino, K_CHUNK, &size.to_be_bytes());
                    match self.seek_ge(&at)? {
                        Some((k, _)) if k == at => {}
                        _ => return Err(Error::NeedsRechunk),
                    }
                    self.remove_extents_from(ino, size)?;
                    rec.covered = size;
                }
                rec.size = size;
                rec.mtime = self.now;
                rec.cversion += 1;
            }
        }
        if let Some(mode) = set.mode {
            rec.mode = mode & 0o7777;
        }
        if let Some(t) = set.atime {
            rec.atime = t;
        }
        if let Some(t) = set.mtime {
            rec.mtime = t;
        }
        rec.ctime = self.now;
        self.put_inode(ino, &rec)?;
        Ok(rec.attr(ino))
    }

    fn remove_extents_from(&mut self, ino: Ino, from_off: u64) -> Result<()> {
        let prefix = key(ino, K_CHUNK, &[]);
        let from = key(ino, K_CHUNK, &from_off.to_be_bytes());
        loop {
            let rows = self.scan(&from, &prefix, PAGE)?;
            if rows.is_empty() {
                return Ok(());
            }
            for (k, _) in rows {
                self.del(&k)?;
            }
        }
    }

    fn put_extent(&mut self, ino: Ino, off: u64, c: &ChunkRef) -> Result<()> {
        self.put(
            key(ino, K_CHUNK, &off.to_be_bytes()),
            encode_chunks(std::slice::from_ref(c))?,
        )
    }

    fn file_rec(&self, ino: Ino) -> Result<InodeRec> {
        let rec = read::inode(self, ino)?;
        match rec.kind {
            FileType::Dir => Err(Error::IsDir),
            FileType::File => Ok(rec),
            _ => Err(Error::Invalid("not a regular file")),
        }
    }

    /// Replaces a file's whole chunk list and size. `size` may exceed the chunk total (a trailing
    /// hole). Last writer wins; use [`Tx::splice_content`] for a compare-and-swap.
    pub fn set_content(&mut self, ino: Ino, chunks: &[ChunkRef], size: u64) -> Result<Attr> {
        let mut rec = self.file_rec(ino)?;
        if chunks.iter().any(|c| c.len == 0) {
            return Err(Error::Invalid("zero-length chunk ref"));
        }
        let total: u64 = chunks.iter().map(|c| u64::from(c.len)).sum();
        if size < total {
            return Err(Error::Invalid("size smaller than chunk list"));
        }
        let old: HashMap<u64, ChunkRef> =
            read::extents(self, ino, 0, u64::MAX)?.into_iter().collect();
        let mut keep = HashSet::new();
        let mut off = 0u64;
        for c in chunks {
            if old.get(&off) != Some(c) {
                self.put_extent(ino, off, c)?;
            }
            keep.insert(off);
            off += u64::from(c.len);
        }
        for o in old.keys().filter(|o| !keep.contains(o)) {
            self.del(&key(ino, K_CHUNK, &o.to_be_bytes()))?;
        }
        rec.covered = total;
        rec.size = size;
        rec.mtime = self.now;
        rec.ctime = self.now;
        rec.cversion += 1;
        self.put_inode(ino, &rec)?;
        Ok(rec.attr(ino))
    }

    /// Replaces the chunks covering bytes `start..end` of a file with `new_chunks` and sets the
    /// file size, if the file's content version is still `expected_version`.
    ///
    /// `start` and `end` must be chunk boundaries and `end` at most the chunk-covered length. A
    /// splice that does not reach the end of the chunk list must keep the byte length; one that
    /// does (an append or a tail replacement) may change it. Cost is proportional to the chunks
    /// removed and added plus the tree depth, not to the file. Returns the new content version.
    ///
    /// Errors: [`Error::Conflict`] if the version moved on, [`Error::Invalid`] for a bad range.
    pub fn splice_content(
        &mut self,
        ino: Ino,
        expected_version: u64,
        start: u64,
        end: u64,
        new_chunks: &[ChunkRef],
        new_size: u64,
    ) -> Result<u64> {
        let mut rec = self.file_rec(ino)?;
        if rec.cversion != expected_version {
            return Err(Error::Conflict);
        }
        if start > end || end > rec.covered {
            return Err(Error::Invalid("splice range outside the chunk list"));
        }
        if new_chunks.iter().any(|c| c.len == 0) {
            return Err(Error::Invalid("zero-length chunk ref"));
        }
        if start == end
            && start < rec.covered
            && self
                .get(&key(ino, K_CHUNK, &start.to_be_bytes()))?
                .is_none()
        {
            return Err(Error::Invalid("splice range is not on chunk boundaries"));
        }
        let old = read::extents(self, ino, start, end)?;
        let old_len: u64 = old.iter().map(|(_, c)| u64::from(c.len)).sum();
        let on_boundary = old.first().is_none_or(|(o, _)| *o == start);
        if !on_boundary || old_len != end - start {
            return Err(Error::Invalid("splice range is not on chunk boundaries"));
        }
        let new_len: u64 = new_chunks.iter().map(|c| u64::from(c.len)).sum();
        if end < rec.covered && new_len != end - start {
            return Err(Error::Invalid(
                "splice before the end must keep the byte length",
            ));
        }
        let covered = rec.covered - (end - start) + new_len;
        if new_size < covered {
            return Err(Error::Invalid("size smaller than chunk list"));
        }
        let mut off = start;
        let mut want = HashMap::new();
        for c in new_chunks {
            want.insert(off, *c);
            off += u64::from(c.len);
        }
        let old_map: HashMap<u64, ChunkRef> = old.into_iter().collect();
        for o in old_map.keys().filter(|o| !want.contains_key(*o)) {
            self.del(&key(ino, K_CHUNK, &o.to_be_bytes()))?;
        }
        let mut puts: Vec<_> = want
            .iter()
            .filter(|(o, c)| old_map.get(o) != Some(c))
            .collect();
        puts.sort_by_key(|(o, _)| **o);
        for (o, c) in puts {
            self.put_extent(ino, *o, c)?;
        }
        rec.covered = covered;
        rec.size = new_size;
        rec.mtime = self.now;
        rec.ctime = self.now;
        rec.cversion += 1;
        self.put_inode(ino, &rec)?;
        Ok(rec.cversion)
    }

    /// Sets an extended attribute.
    pub fn setxattr(&mut self, ino: Ino, name: &[u8], value: &[u8]) -> Result<()> {
        if name.is_empty() || name.len() > NAME_MAX {
            return Err(Error::Invalid("bad xattr name"));
        }
        if value.len() > XATTR_MAX {
            return Err(Error::TooBig);
        }
        let mut rec = read::inode(self, ino)?;
        rec.ctime = self.now;
        self.put_inode(ino, &rec)?;
        self.put(key(ino, K_XATTR, name), value.to_vec())
    }

    /// Removes an extended attribute.
    pub fn removexattr(&mut self, ino: Ino, name: &[u8]) -> Result<()> {
        let mut rec = read::inode(self, ino)?;
        let k = key(ino, K_XATTR, name);
        if self.get(&k)?.is_none() {
            return Err(Error::NoAttr);
        }
        self.del(&k)?;
        rec.ctime = self.now;
        self.put_inode(ino, &rec)
    }

    /// Looks up a name in a directory (`.` and `..` resolve).
    pub fn lookup(&self, dir: Ino, name: &[u8]) -> Result<Attr> {
        read::lookup(self, dir, name)
    }

    /// Attributes of an inode.
    pub fn getattr(&self, ino: Ino) -> Result<Attr> {
        read::getattr(self, ino)
    }

    /// A page of a directory listing.
    pub fn readdir(&self, dir: Ino, cookie: u64, max: usize) -> Result<ReadDir> {
        read::readdir(self, dir, cookie, max)
    }

    /// A file's whole chunk list.
    pub fn chunks(&self, ino: Ino) -> Result<Vec<ChunkRef>> {
        read::chunks(self, ino)
    }

    /// The chunks starting in byte range `start..end`, with the content version they were read at.
    pub fn chunk_range(&self, ino: Ino, start: u64, end: u64) -> Result<ChunkRange> {
        read::chunk_range(self, ino, start, end)
    }

    /// The content version of a file (see [`Tx::splice_content`]).
    pub fn content_version(&self, ino: Ino) -> Result<u64> {
        read::content_version(self, ino)
    }

    /// Target of a symlink.
    pub fn readlink(&self, ino: Ino) -> Result<Vec<u8>> {
        read::readlink(self, ino)
    }
}

fn apply_delta(v: u32, d: i64) -> Result<u32> {
    u32::try_from(i64::from(v) + d).map_err(|_| Error::Invalid("link count out of range"))
}

fn check_mknod_args(kind: FileType, rdev: u64) -> Result<()> {
    if !kind.is_special() {
        return Err(Error::Invalid("mknod makes only special files"));
    }
    if rdev != 0 && !kind.is_device() {
        return Err(Error::Invalid("only a device has a device number"));
    }
    Ok(())
}
