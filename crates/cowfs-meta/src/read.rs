//! Read paths shared by read transactions and by writers inside a batch.

use crate::node::NodeId;
use crate::ptree::{self, Cursor};
use crate::types::*;
use crate::{Error, Result};
use cowfs_store::ChunkRef;
use redb::ReadOnlyTable;

pub(crate) type Entry = (Vec<u8>, Vec<u8>);

/// Ordered key-value access to one tree.
pub(crate) trait Reader {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;

    /// First entry with a key at least `key`.
    fn seek_ge(&self, key: &[u8]) -> Result<Option<Entry>>;

    /// Up to `limit` entries with keys at least `from` that start with `prefix`.
    fn scan(&self, from: &[u8], prefix: &[u8], limit: usize) -> Result<Vec<Entry>> {
        let mut out = Vec::new();
        let mut from = from.to_vec();
        while out.len() < limit {
            let Some((k, v)) = self.seek_ge(&from)? else {
                break;
            };
            if !k.starts_with(prefix) {
                break;
            }
            from.clone_from(&k);
            from.push(0);
            out.push((k, v));
        }
        Ok(out)
    }
}

/// A tree read through a redb read transaction.
pub(crate) struct RoView {
    pub(crate) nodes: ReadOnlyTable<[u8; 32], &'static [u8]>,
    pub(crate) root: NodeId,
}

impl Reader for RoView {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        ptree::get(&self.nodes, &self.root, key)
    }

    fn seek_ge(&self, key: &[u8]) -> Result<Option<Entry>> {
        Cursor::seek(&self.nodes, &self.root, key)?.next(&self.nodes)
    }

    fn scan(&self, from: &[u8], prefix: &[u8], limit: usize) -> Result<Vec<Entry>> {
        let mut cur = Cursor::seek(&self.nodes, &self.root, from)?;
        let mut out = Vec::new();
        while out.len() < limit {
            match cur.next(&self.nodes)? {
                Some((k, v)) if k.starts_with(prefix) => out.push((k, v)),
                _ => break,
            }
        }
        Ok(out)
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

pub(crate) fn lookup<R: Reader>(r: &R, dir: Ino, name: &[u8]) -> Result<Attr> {
    let drec = dir_inode(r, dir)?;
    if name == b"." {
        return Ok(drec.attr(dir));
    }
    if name == b".." {
        return getattr(r, Ino(drec.parent));
    }
    if name.is_empty() {
        return Err(Error::Invalid("bad name"));
    }
    if name.len() > NAME_MAX {
        return Err(Error::NameTooLong);
    }
    let (child, _, _) = entry(r, dir, name)?.ok_or(Error::NotFound)?;
    getattr(r, child)
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

pub(crate) fn chunks<R: Reader>(r: &R, ino: Ino) -> Result<Vec<ChunkRef>> {
    match inode(r, ino)?.kind {
        FileType::File => {}
        FileType::Dir => return Err(Error::IsDir),
        FileType::Symlink => return Err(Error::Invalid("not a regular file")),
    }
    let prefix = key(ino, K_CHUNK, &[]);
    let mut out = Vec::new();
    for (_, v) in r.scan(&prefix, &prefix, usize::MAX)? {
        out.extend(decode_chunks(&v)?);
    }
    Ok(out)
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
