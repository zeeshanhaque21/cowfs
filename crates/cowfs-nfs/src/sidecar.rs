//! AppleDouble translation: `._name` files are not stored. The adapter serves them as a view of
//! the extended attributes of `name`, so they never become inodes and never show up on other
//! mounts of the same data.
//!
//! - A sidecar is named by a file id of its own, not by a mark on the file's inode number: the
//!   `Vfs` owns the whole `u64` inode space and may use every bit of it (issue #60). Where that id
//!   comes from is [`crate::adapter::Id`].
//! - Reading synthesises the bytes from the current xattrs.
//! - Writing (the client writes and truncates the file in pieces) is buffered per file. Whenever
//!   the buffer is a well formed AppleDouble file its attributes are applied to the `Vfs`. The
//!   buffer stays as the file's content until the file's ctime moves, so the client's next
//!   partial write lands on the layout it wrote itself.
//! - The buffers are bounded in count and bytes; the oldest are dropped first.
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use cowfs_vfs::{Attr, Error, FileKind, Ino, SetAttr, Timestamp, XattrFlags};
use nfsserve::nfs::{fattr3, nfsstat3, sattr3};

use crate::adapter::{is_appledouble, lock, stat, Adapter, Id, NfsResult};
use crate::appledouble::{is_plain_attr, is_plausible_prefix, Sidecar, FINDER_INFO, RESOURCE_FORK};
use crate::convert::set_attr;
use crate::handle::Kind;

const MAX_SIDECAR: usize = 8 << 20;
const MAX_BUFFERS: usize = 1024;
const MAX_BYTES: usize = 32 << 20;

#[derive(Debug)]
struct Pending {
    buf: Vec<u8>,
    dirty: bool,
    ctime: Timestamp,
    tick: u64,
}

/// Sidecar buffers of the files being written.
#[derive(Debug, Default)]
pub struct SidecarBuffers {
    map: HashMap<Ino, Pending>,
    bytes: usize,
    tick: u64,
}

impl SidecarBuffers {
    pub fn remove(&mut self, ino: Ino) {
        if let Some(p) = self.map.remove(&ino) {
            self.bytes -= p.buf.len();
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    fn put(&mut self, ino: Ino, buf: Vec<u8>, dirty: bool, ctime: Timestamp) {
        self.remove(ino);
        self.tick += 1;
        self.bytes += buf.len();
        self.map.insert(
            ino,
            Pending {
                buf,
                dirty,
                ctime,
                tick: self.tick,
            },
        );
        while self.map.len() > MAX_BUFFERS || self.bytes > MAX_BYTES {
            let oldest = self
                .map
                .iter()
                .filter(|(i, _)| **i != ino)
                .min_by_key(|(_, p)| (p.dirty, p.tick))
                .map(|(i, _)| *i);
            match oldest {
                Some(i) => self.remove(i),
                None => break,
            }
        }
    }

    /// The buffer of `ino` if it is still current for a file whose ctime is `ctime`.
    fn current(&mut self, ino: Ino, ctime: Timestamp) -> Option<Vec<u8>> {
        self.tick += 1;
        let tick = self.tick;
        let keep = match self.map.get_mut(&ino) {
            None => return None,
            Some(p) if p.dirty || p.ctime == ctime => {
                p.tick = tick;
                return Some(p.buf.clone());
            }
            Some(_) => false,
        };
        if !keep {
            self.remove(ino);
        }
        None
    }
}

/// One lock per inode, so two writers of the same sidecar cannot lose each other's chunks while
/// writers of different files do not wait for each other. The map holds weak references, so an
/// entry disappears with its last user.
#[derive(Debug, Default)]
pub(crate) struct PerIno {
    map: HashMap<Ino, Weak<Mutex<()>>>,
}

impl PerIno {
    fn of(&mut self, ino: Ino) -> Arc<Mutex<()>> {
        if let Some(l) = self.map.get(&ino).and_then(Weak::upgrade) {
            return l;
        }
        let l = Arc::new(Mutex::new(()));
        self.map.insert(ino, Arc::downgrade(&l));
        if self.map.len() > 4096 {
            self.map.retain(|_, w| w.strong_count() > 0);
        }
        l
    }
}

fn side_of(name: &[u8]) -> Option<&[u8]> {
    is_appledouble(name).then(|| &name[2..])
}

impl Adapter {
    /// True if `._name` is a name this adapter might translate: the mode says so, and no real
    /// file of that name exists. A real file wins, so a `._x` that arrived before `x` (every zip,
    /// tar and git checkout that tracks one) stays a real file.
    pub(crate) fn translating(&self, dir: Ino, name: &[u8]) -> bool {
        self.opts.appledouble == crate::AppleDoubleMode::Translate
            && is_appledouble(name)
            && self.peek(dir, name).is_err()
    }

    /// True if `._name` is the sidecar of an existing `name` rather than a real file of that
    /// name. Both halves matter: with no `name` to hold the attributes there is nothing to
    /// translate, so CREATE stores a real file instead of refusing, which is what an archive
    /// extraction and a checkout of a tree that tracks `._*` need.
    pub(crate) fn side_of(&self, dir: Ino, name: &[u8]) -> bool {
        // The sidecar of a sidecar is a real file, not a view of another view.
        self.translating(dir, name)
            && !is_appledouble(&name[2..])
            && self.main_of(dir, name).is_ok()
    }

    /// The main file a sidecar name belongs to.
    fn main_of(&self, dir: Ino, name: &[u8]) -> Result<Attr, Error> {
        self.peek(dir, name.get(2..).unwrap_or_default())
    }

    fn buffers(&self) -> MutexGuard<'_, SidecarBuffers> {
        lock(&self.sidecars)
    }

    /// Number of sidecar buffers held (for tests and monitoring).
    pub fn sidecar_buffers(&self) -> usize {
        self.buffers().len()
    }

    /// The file `name` belongs to: `._x` is the sidecar of `x`.
    fn side_target(&self, dir: Ino, name: &[u8]) -> NfsResult<Attr> {
        let target = side_of(name).ok_or(nfsstat3::NFS3ERR_INVAL)?;
        if is_appledouble(target) {
            return Err(nfsstat3::NFS3ERR_NOENT);
        }
        self.peek(dir, target).map_err(stat)
    }

    /// The managed xattrs of `ino` in sidecar form.
    fn xattr_sidecar(&self, ino: Ino) -> NfsResult<Sidecar> {
        let names = match self.vfs.listxattr(ino) {
            Ok(n) => n,
            Err(Error::NotSupported | Error::NoAttr) => Vec::new(),
            Err(e) => return Err(stat(e)),
        };
        let mut xs = Vec::new();
        for n in names {
            if n == FINDER_INFO || n == RESOURCE_FORK || is_plain_attr(&n) {
                match self.vfs.getxattr(ino, &n) {
                    Ok(v) => xs.push((n, v)),
                    Err(Error::NoAttr) => {}
                    Err(e) => return Err(stat(e)),
                }
            }
        }
        Ok(Sidecar::from_xattrs(xs))
    }

    /// The content of the sidecar of `t`, or `None` if it has none.
    fn side_bytes(&self, t: &Attr) -> NfsResult<Option<Vec<u8>>> {
        if let Some(buf) = self.buffers().current(t.ino, t.ctime) {
            return Ok(Some(buf));
        }
        let sc = self.xattr_sidecar(t.ino)?;
        Ok((!sc.is_empty()).then(|| sc.encode()))
    }

    fn side_attr(&self, t: &Attr, size: usize) -> NfsResult<fattr3> {
        let size = size as u64;
        self.fa(
            &Attr {
                ino: t.ino,
                kind: FileKind::Regular,
                mode: 0o644,
                nlink: 1,
                size,
                blocks: size.div_ceil(512),
                atime: t.atime,
                mtime: t.ctime,
                ..*t
            },
            Kind::Sidecar,
        )
    }

    /// Makes the xattrs of `ino` equal to those in `sc`.
    fn sync_xattrs(&self, ino: Ino, sc: &Sidecar) -> NfsResult<()> {
        let want: BTreeMap<Vec<u8>, Vec<u8>> = sc.to_xattrs().into_iter().collect();
        let have: BTreeMap<Vec<u8>, Vec<u8>> =
            self.xattr_sidecar(ino)?.to_xattrs().into_iter().collect();
        for (name, value) in &want {
            if have.get(name) != Some(value) {
                self.vfs
                    .setxattr(ino, name, value, XattrFlags::default())
                    .map_err(stat)?;
            }
        }
        for name in have.keys().filter(|n| !want.contains_key(*n)) {
            match self.vfs.removexattr(ino, name) {
                Ok(()) | Err(Error::NoAttr) => {}
                Err(e) => return Err(stat(e)),
            }
        }
        Ok(())
    }

    /// Stores `buf` as the sidecar content of `t` and applies it if it parses.
    fn side_store(&self, t: &Attr, buf: Vec<u8>) -> NfsResult<Attr> {
        let mut cur = *t;
        let dirty = match Sidecar::decode(&buf) {
            Some(sc) => {
                self.sync_xattrs(t.ino, &sc)?;
                cur = self.vfs.getattr(t.ino).map_err(stat)?;
                false
            }
            None => true,
        };
        let len = buf.len();
        self.buffers().put(t.ino, buf, dirty, cur.ctime);
        cur.size = len as u64;
        Ok(cur)
    }

    fn side_file(&self, ino: Ino) -> NfsResult<(Attr, Vec<u8>)> {
        let t = self.vfs.getattr(ino).map_err(stat)?;
        let bytes = self.side_bytes(&t)?.ok_or(nfsstat3::NFS3ERR_STALE)?;
        Ok((t, bytes))
    }

    pub(crate) fn side_lookup(&self, dir: Ino, name: &[u8]) -> NfsResult<(Id, fattr3)> {
        let t = self.side_target(dir, name)?;
        let bytes = self.side_bytes(&t)?.ok_or(nfsstat3::NFS3ERR_NOENT)?;
        Ok((Id::side(t.ino), self.side_attr(&t, bytes.len())?))
    }

    pub(crate) fn side_getattr(&self, ino: Ino) -> NfsResult<fattr3> {
        let (t, bytes) = self.side_file(ino)?;
        self.side_attr(&t, bytes.len())
    }

    pub(crate) fn side_read(
        &self,
        ino: Ino,
        offset: u64,
        count: u32,
    ) -> NfsResult<(Vec<u8>, bool)> {
        let (_, bytes) = self.side_file(ino)?;
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let end = start.saturating_add(count as usize).min(bytes.len());
        Ok((bytes[start..end].to_vec(), end == bytes.len()))
    }

    pub(crate) fn side_write(
        &self,
        ino: Ino,
        offset: u64,
        data: &[u8],
    ) -> NfsResult<(u32, fattr3)> {
        let one = {
            let mut locks = lock(&self.sidecar_locks);
            locks.of(ino)
        };
        let _held = one.lock().unwrap_or_else(PoisonError::into_inner);
        let (t, mut buf) = self.side_file(ino)?;
        let start = usize::try_from(offset).map_err(|_| nfsstat3::NFS3ERR_FBIG)?;
        let end = start
            .checked_add(data.len())
            .filter(|e| *e <= MAX_SIDECAR)
            .ok_or(nfsstat3::NFS3ERR_FBIG)?;
        if buf.len() < end {
            buf.resize(end, 0);
        }
        buf[start..end].copy_from_slice(data);
        if offset == 0 && !is_plausible_prefix(&buf) {
            // Bytes that can never be a sidecar are a real file under a reserved name. Refuse
            // them instead of accepting data the adapter would only drop.
            return Err(nfsstat3::NFS3ERR_NOTSUPP);
        }
        let cur = self.side_store(&t, buf)?;
        let n = u32::try_from(data.len()).unwrap_or(u32::MAX);
        Ok((n, self.side_attr(&cur, cur.size as usize)?))
    }

    /// SETATTR on a sidecar: only the size matters, mode and times are accepted and ignored.
    pub(crate) fn side_setattr(&self, ino: Ino, s: &sattr3) -> NfsResult<fattr3> {
        let one = {
            let mut locks = lock(&self.sidecar_locks);
            locks.of(ino)
        };
        let _held = one.lock().unwrap_or_else(PoisonError::into_inner);
        let (t, mut buf) = self.side_file(ino)?;
        let changes: SetAttr = set_attr(s);
        let Some(size) = changes.size else {
            return self.side_attr(&t, buf.len());
        };
        let size = usize::try_from(size)
            .ok()
            .filter(|s| *s <= MAX_SIDECAR)
            .ok_or(nfsstat3::NFS3ERR_FBIG)?;
        buf.resize(size, 0);
        let cur = self.side_store(&t, buf)?;
        self.side_attr(&cur, cur.size as usize)
    }

    /// CREATE of `._name`: the sidecar exists from now on, empty if it had no attributes.
    pub(crate) fn side_create(
        &self,
        dir: Ino,
        name: &[u8],
        attr: &sattr3,
        guarded: bool,
    ) -> NfsResult<(Id, fattr3)> {
        let t = self.side_target(dir, name)?;
        let existing = self.side_bytes(&t)?;
        let truncate = set_attr(attr).size == Some(0);
        let len = match existing {
            Some(_) if guarded => return Err(nfsstat3::NFS3ERR_EXIST),
            Some(b) if !truncate => b.len(),
            _ => {
                self.buffers().put(t.ino, Vec::new(), true, t.ctime);
                0
            }
        };
        Ok((Id::side(t.ino), self.side_attr(&t, len)?))
    }

    /// CREATE with mode EXCLUSIVE: a retry finds the file it created.
    pub(crate) fn side_create_exclusive(&self, dir: Ino, name: &[u8]) -> NfsResult<(Id, fattr3)> {
        self.side_create(dir, name, &sattr3::default(), false)
    }

    /// REMOVE of `._name`: the file loses all the attributes a sidecar can hold.
    pub(crate) fn side_remove(&self, dir: Ino, name: &[u8]) -> NfsResult<()> {
        let t = self.side_target(dir, name)?;
        if self.side_bytes(&t)?.is_none() {
            return Err(nfsstat3::NFS3ERR_NOENT);
        }
        self.sync_xattrs(t.ino, &Sidecar::default())?;
        self.buffers().remove(t.ino);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::AdapterOptions;
    use cowfs_vfs::ROOT_INO;
    use cowfs_vfs_test::MemVfs;

    fn adapter() -> Adapter {
        Adapter::new(
            Arc::new(MemVfs::new()),
            AdapterOptions {
                appledouble: crate::AppleDoubleMode::Translate,
                ..AdapterOptions::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn more_attributes_than_the_format_holds_are_refused() {
        let a = adapter();
        a.create(ROOT_INO, b"f", &sattr3::default(), true).unwrap();
        let (id, _) = a
            .create(ROOT_INO, b"._f", &sattr3::default(), true)
            .unwrap();
        let mut over = Sidecar::default();
        for i in 0..300 {
            over.attrs
                .insert(format!("user.a{i:03}").into_bytes(), vec![1; 100]);
        }
        let st = a.write(id, 0, &Sidecar::encode_all(&over));
        assert_eq!(
            st.map(|_| ()).unwrap_err(),
            nfsstat3::NFS3ERR_NOTSUPP,
            "past the cap the write is refused, not accepted and dropped"
        );
        let ino = a.main_of(ROOT_INO, b"._f").unwrap().ino;
        assert!(
            a.vfs.listxattr(ino).unwrap().is_empty(),
            "and nothing was stored as attributes"
        );
    }
}
