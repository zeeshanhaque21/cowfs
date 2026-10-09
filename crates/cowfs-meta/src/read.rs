//! Read paths shared by read views and by writers inside a batch.

use crate::ptree::{Entry, MemTree, NodeSource};
use crate::types::*;
use crate::{Error, Result};
use cowfs_store::ChunkRef;

/// Ordered key-value access to one tree.
pub(crate) trait Reader {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;

    /// First entry with a key at least `key`.
    fn seek_ge(&self, key: &[u8]) -> Result<Option<Entry>>;

    /// Up to `limit` entries with keys at least `from` that start with `prefix`.
    fn scan(&self, from: &[u8], prefix: &[u8], limit: usize) -> Result<Vec<Entry>>;
}

/// A tree read through a node source.
pub(crate) struct View<'a> {
    pub(crate) tree: &'a MemTree,
    pub(crate) src: &'a dyn NodeSource,
}

impl Reader for View<'_> {
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

pub(crate) fn inode<R: Reader>(r: &R, ino: Ino) -> Result<InodeRec> {
    let v = r.get(&key(ino, K_INODE, &[]))?.ok_or(Error::NotFound)?;
    InodeRec::decode(&v)
}

pub(crate) fn dir_inode<R: Reader>(r: &R, ino: Ino) -> Result<InodeRec> {
    let rec = inode(r, ino)?;
    if rec.kind == FileType::Dir {
        Ok(rec)
    } else {
        Err(Error::NotDir)
    }
}

pub(crate) fn entry<R: Reader>(
    r: &R,
    dir: Ino,
    name: &[u8],
) -> Result<Option<(Ino, FileType, u64)>> {
    r.get(&key(dir, K_NAME, name))?
        .map(|v| decode_name_val(&v))
        .transpose()
}

pub(crate) fn getattr<R: Reader>(r: &R, ino: Ino) -> Result<Attr> {
    Ok(inode(r, ino)?.attr(ino))
}

/// Two descents on a hit (directory entry, child inode). The directory's own record is read only
/// to pick the right error on a miss.
pub(crate) fn lookup<R: Reader>(r: &R, dir: Ino, name: &[u8]) -> Result<Attr> {
    if name == b"." || name == b".." {
        let drec = dir_inode(r, dir)?;
        return if name == b"." {
            Ok(drec.attr(dir))
        } else {
            getattr(r, Ino(drec.parent))
        };
    }
    if name.is_empty() {
        return Err(Error::Invalid("bad name"));
    }
    if name.len() > NAME_MAX {
        return Err(Error::NameTooLong);
    }
    match entry(r, dir, name)? {
        Some((child, _, _)) => getattr(r, child),
        None => {
            dir_inode(r, dir)?;
            Err(Error::NotFound)
        }
    }
}

pub(crate) fn readdir<R: Reader>(r: &R, dir: Ino, cookie: u64, max: usize) -> Result<ReadDir> {
    dir_inode(r, dir)?;
    let max = max.max(1);
    let prefix = key(dir, K_COOKIE, &[]);
    let from = key(dir, K_COOKIE, &cookie.saturating_add(1).to_be_bytes());
    let mut rows = r.scan(&from, &prefix, max + 1)?;
    let end = rows.len() <= max;
    rows.truncate(max);
    let mut entries = Vec::with_capacity(rows.len());
    for (k, v) in rows {
        let (_, _, suffix) = split_key(&k)?;
        let c = u64::from_be_bytes(
            <[u8; 8]>::try_from(suffix).map_err(|_| Error::Corrupt("bad cookie key".into()))?,
        );
        let (ino, kind, name) = decode_cookie_val(&v)?;
        entries.push(DirEntry {
            name: name.to_vec(),
            ino,
            kind,
            cookie: c,
        });
    }
    let next_cookie = entries.last().map_or(cookie, |e| e.cookie);
    Ok(ReadDir {
        entries,
        next_cookie,
        end,
    })
}

pub(crate) fn readlink<R: Reader>(r: &R, ino: Ino) -> Result<Vec<u8>> {
    if inode(r, ino)?.kind != FileType::Symlink {
        return Err(Error::Invalid("not a symlink"));
    }
    r.get(&key(ino, K_LINK, &[]))?
        .ok_or_else(|| Error::Corrupt("symlink without target".into()))
}

fn file_inode<R: Reader>(r: &R, ino: Ino) -> Result<InodeRec> {
    let rec = inode(r, ino)?;
    match rec.kind {
        FileType::File => Ok(rec),
        FileType::Dir => Err(Error::IsDir),
        _ => Err(Error::Invalid("not a regular file")),
    }
}

const PAGE: usize = 512;

/// Extents of `ino` with offset in `[start, end)`, in offset order.
pub(crate) fn extents<R: Reader>(
    r: &R,
    ino: Ino,
    start: u64,
    end: u64,
) -> Result<Vec<(u64, ChunkRef)>> {
    let prefix = key(ino, K_CHUNK, &[]);
    let mut from = key(ino, K_CHUNK, &start.to_be_bytes());
    let mut out = Vec::new();
    loop {
        let rows = r.scan(&from, &prefix, PAGE)?;
        let n = rows.len();
        for (k, v) in rows {
            let (_, _, suffix) = split_key(&k)?;
            let off = u64::from_be_bytes(
                <[u8; 8]>::try_from(suffix).map_err(|_| Error::Corrupt("bad extent key".into()))?,
            );
            if off >= end {
                return Ok(out);
            }
            let mut c = decode_chunks(&v)?;
            if c.len() != 1 {
                return Err(Error::Corrupt("extent must hold one chunk".into()));
            }
            out.push((off, c.remove(0)));
            from = k;
            from.push(0);
        }
        if n < PAGE {
            return Ok(out);
        }
    }
}

pub(crate) fn chunks<R: Reader>(r: &R, ino: Ino) -> Result<Vec<ChunkRef>> {
    file_inode(r, ino)?;
    Ok(extents(r, ino, 0, u64::MAX)?
        .into_iter()
        .map(|(_, c)| c)
        .collect())
}

pub(crate) fn chunk_range<R: Reader>(r: &R, ino: Ino, start: u64, end: u64) -> Result<ChunkRange> {
    let rec = file_inode(r, ino)?;
    Ok(ChunkRange {
        version: rec.cversion,
        size: rec.size,
        covered: rec.covered,
        chunks: extents(r, ino, start, end)?,
    })
}

pub(crate) fn content_version<R: Reader>(r: &R, ino: Ino) -> Result<u64> {
    Ok(file_inode(r, ino)?.cversion)
}

pub(crate) fn getxattr<R: Reader>(r: &R, ino: Ino, name: &[u8]) -> Result<Vec<u8>> {
    inode(r, ino)?;
    r.get(&key(ino, K_XATTR, name))?.ok_or(Error::NoAttr)
}

pub(crate) fn listxattr<R: Reader>(r: &R, ino: Ino) -> Result<Vec<Vec<u8>>> {
    inode(r, ino)?;
    let prefix = key(ino, K_XATTR, &[]);
    Ok(r.scan(&prefix, &prefix, usize::MAX)?
        .into_iter()
        .map(|(k, _)| k[prefix.len()..].to_vec())
        .collect())
}
