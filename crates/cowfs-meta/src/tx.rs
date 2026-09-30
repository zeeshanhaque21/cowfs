//! Mutating operations, run inside one redb write transaction.

use crate::ptree::{MemTree, NodeWriter};
use crate::read::{self, Entry, Reader};
use crate::types::*;
use crate::{Error, Result};
use cowfs_store::ChunkRef;

/// A batch of operations on one snapshot, committed atomically.
///
/// Obtained from [`Snapshot::batch`](crate::Snapshot::batch). Nothing is visible to other readers
/// until the closure returns `Ok`; on `Err` every change is discarded.
pub struct Tx<'a> {
    pub(crate) w: NodeWriter<'a>,
    pub(crate) tree: MemTree,
    pub(crate) next_ino: u64,
    pub(crate) now: Timestamp,
}

impl std::fmt::Debug for Tx<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tx").finish_non_exhaustive()
    }
}

impl Reader for Tx<'_> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.tree.get(&self.w.nodes, key)
    }

    fn seek_ge(&self, key: &[u8]) -> Result<Option<Entry>> {
        self.tree.seek_ge(&self.w.nodes, key)
    }
}

impl Tx<'_> {
    fn put(&mut self, key: Vec<u8>, val: Vec<u8>) -> Result<()> {
        self.tree.insert(&self.w.nodes, &key, val)
    }

    fn del(&mut self, key: &[u8]) -> Result<()> {
        self.tree.remove(&self.w.nodes, key).map(|_| ())
    }

    fn put_inode(&mut self, ino: Ino, rec: &InodeRec) -> Result<()> {
        self.put(key(ino, K_INODE, &[]), rec.encode())
    }

    fn alloc(&mut self) -> Ino {
        let ino = Ino(self.next_ino);
        self.next_ino += 1;
        ino
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

    fn drop_inode(&mut self, ino: Ino, rec: &InodeRec) -> Result<Vec<ChunkRef>> {
        let chunks = if rec.kind == FileType::File {
            read::chunks(self, ino)?
        } else {
            Vec::new()
        };
        let prefix = ino.0.to_be_bytes();
        for (k, _) in self.scan(&prefix, &prefix, usize::MAX)? {
            self.del(&k)?;
        }
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
        let ino = self.alloc();
        let mut rec = self.new_rec(kind, mode, dir);
        let delta = if kind == FileType::Dir { 1 } else { 0 };
        if let Some(t) = target {
            rec.size = t.len() as u64;
            self.put(key(ino, K_LINK, &[]), t.to_vec())?;
        }
        self.put_inode(ino, &rec)?;
        self.add_entry(dir, name, ino, kind, delta)?;
        Ok(rec.attr(ino))
    }

    /// Creates an empty regular file.
    pub fn create(&mut self, dir: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        self.new_child(dir, name, FileType::File, mode, None)
    }

    /// Creates an empty directory.
    pub fn mkdir(&mut self, dir: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        self.new_child(dir, name, FileType::Dir, mode, None)
    }

    /// Creates a symbolic link holding `target`.
    pub fn symlink(&mut self, dir: Ino, name: &[u8], target: &[u8]) -> Result<Attr> {
        self.new_child(dir, name, FileType::Symlink, 0o777, Some(target))
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
        self.remove_entry(from_dir, from_name, scookie, -is_dir)?;
        self.add_entry(to_dir, to_name, src, skind, is_dir)?;
        let mut rec = read::inode(self, src)?;
        rec.ctime = self.now;
        if skind == FileType::Dir {
            rec.parent = to_dir.0;
        }
        self.put_inode(src, &rec)?;
        Ok(replaced)
    }

    /// Changes mode, times, or size. Shrinking a file must land on a chunk boundary.
    pub fn setattr(&mut self, ino: Ino, set: SetAttr) -> Result<Attr> {
        let mut rec = read::inode(self, ino)?;
        if let Some(size) = set.size {
            match rec.kind {
                FileType::Dir => return Err(Error::IsDir),
                FileType::Symlink => return Err(Error::Invalid("cannot resize a symlink")),
                FileType::File => {}
            }
            if size != rec.size {
                let chunks = read::chunks(self, ino)?;
                let total: u64 = chunks.iter().map(|c| u64::from(c.len)).sum();
                if size < total {
                    let mut acc = 0u64;
                    let mut keep = (size == 0).then_some(0);
                    for (i, c) in chunks.iter().enumerate() {
                        if keep.is_some() || acc >= size {
                            break;
                        }
                        acc += u64::from(c.len);
                        if acc == size {
                            keep = Some(i + 1);
                        }
                    }
                    let keep = keep.ok_or(Error::Invalid("size must be a chunk boundary"))?;
                    self.write_chunks(ino, &chunks[..keep])?;
                }
                rec.size = size;
                rec.mtime = self.now;
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

    fn write_chunks(&mut self, ino: Ino, chunks: &[ChunkRef]) -> Result<()> {
        let prefix = key(ino, K_CHUNK, &[]);
        let old = self.scan(&prefix, &prefix, usize::MAX)?;
        let new: Vec<Entry> = chunks
            .chunks(CHUNKS_PER_SEGMENT)
            .enumerate()
            .map(|(i, seg)| {
                (
                    key(ino, K_CHUNK, &(i as u32).to_be_bytes()),
                    encode_chunks(seg),
                )
            })
            .collect();
        for (i, e) in new.iter().enumerate() {
            if old.get(i) != Some(e) {
                self.put(e.0.clone(), e.1.clone())?;
            }
        }
        for (k, _) in old.iter().skip(new.len()) {
            self.del(k)?;
        }
        Ok(())
    }

    /// Replaces a file's chunk list and size. `size` may exceed the chunk total (a trailing hole).
    pub fn set_content(&mut self, ino: Ino, chunks: &[ChunkRef], size: u64) -> Result<Attr> {
        let mut rec = read::inode(self, ino)?;
        match rec.kind {
            FileType::Dir => return Err(Error::IsDir),
            FileType::Symlink => return Err(Error::Invalid("not a regular file")),
            FileType::File => {}
        }
        let total: u64 = chunks.iter().map(|c| u64::from(c.len)).sum();
        if size < total {
            return Err(Error::Invalid("size smaller than chunk list"));
        }
        self.write_chunks(ino, chunks)?;
        rec.size = size;
        rec.mtime = self.now;
        rec.ctime = self.now;
        self.put_inode(ino, &rec)?;
        Ok(rec.attr(ino))
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

    /// A file's chunk list.
    pub fn chunks(&self, ino: Ino) -> Result<Vec<ChunkRef>> {
        read::chunks(self, ino)
    }
}

fn apply_delta(v: u32, d: i64) -> Result<u32> {
    u32::try_from(i64::from(v) + d).map_err(|_| Error::Invalid("link count out of range"))
}
