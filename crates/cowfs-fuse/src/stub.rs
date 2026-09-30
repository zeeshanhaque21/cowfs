//! Throwaway in-memory `Vfs` for this crate's own tests, until `cowfs-vfs-test::MemVfs` is on the base.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use cowfs_vfs::{
    validate_name, Attr, DirEntry, Error, FileHandle, FileKind, Ino, ReadDir, RenameFlags, Result,
    SetAttr, SetTime, StatFs, Timestamp, Vfs, XattrFlags, MODE_MASK, ROOT_INO,
};

struct Node {
    kind: FileKind,
    mode: u32,
    nlink: u32,
    data: Vec<u8>,
    xattrs: BTreeMap<Vec<u8>, Vec<u8>>,
    times: [Timestamp; 3],
    parent: Ino,
    seq: u64,
    entries: BTreeMap<u64, (Vec<u8>, Ino)>,
    names: HashMap<Vec<u8>, u64>,
    opens: u64,
}

struct Inner {
    nodes: HashMap<Ino, Node>,
    next_ino: Ino,
    handles: HashMap<u64, Ino>,
    next_fh: u64,
}

#[derive(Default)]
pub struct TestVfs {
    inner: Mutex<Option<Inner>>,
    uid: u32,
    gid: u32,
    bad_cookies: AtomicBool,
}

fn node(kind: FileKind, mode: u32, parent: Ino) -> Node {
    let now = Timestamp::now();
    Node {
        kind,
        mode: mode & MODE_MASK,
        nlink: if kind == FileKind::Directory { 2 } else { 1 },
        data: Vec::new(),
        xattrs: BTreeMap::new(),
        times: [now; 3],
        parent,
        seq: 0,
        entries: BTreeMap::new(),
        names: HashMap::new(),
        opens: 0,
    }
}

impl Inner {
    fn get(&mut self, ino: Ino) -> Result<&mut Node> {
        self.nodes.get_mut(&ino).ok_or(Error::Stale)
    }

    fn dir(&mut self, ino: Ino) -> Result<&mut Node> {
        let n = self.get(ino)?;
        if n.kind == FileKind::Directory {
            Ok(n)
        } else {
            Err(Error::NotDir)
        }
    }

    fn touch(&mut self, ino: Ino, content: bool) {
        if let Ok(n) = self.get(ino) {
            let now = Timestamp::now();
            n.times[2] = now;
            if content {
                n.times[1] = now;
            }
        }
    }

    fn insert(&mut self, parent: Ino, name: &[u8], ino: Ino) {
        if let Some(p) = self.nodes.get_mut(&parent) {
            p.seq += 1;
            let seq = p.seq;
            p.entries.insert(seq, (name.to_vec(), ino));
            p.names.insert(name.to_vec(), seq);
        }
        self.touch(parent, true);
    }

    fn remove(&mut self, parent: Ino, name: &[u8]) -> Option<Ino> {
        let p = self.nodes.get_mut(&parent)?;
        let seq = p.names.remove(name)?;
        let (_, ino) = p.entries.remove(&seq)?;
        self.touch(parent, true);
        Some(ino)
    }

    fn drop_link(&mut self, ino: Ino) {
        let Some(n) = self.nodes.get_mut(&ino) else {
            return;
        };
        n.nlink = n.nlink.saturating_sub(1);
        n.times[2] = Timestamp::now();
        if n.nlink == 0 && n.opens == 0 {
            self.nodes.remove(&ino);
        }
    }

    fn add(&mut self, parent: Ino, name: &[u8], n: Node) -> Result<Ino> {
        validate_name(name)?;
        if self.dir(parent)?.names.contains_key(name) {
            return Err(Error::Exists);
        }
        let ino = self.next_ino;
        self.next_ino += 1;
        let is_dir = n.kind == FileKind::Directory;
        self.nodes.insert(ino, n);
        self.insert(parent, name, ino);
        if is_dir {
            self.get(parent)?.nlink += 1;
        }
        Ok(ino)
    }

    fn is_within(&self, mut ino: Ino, ancestor: Ino) -> bool {
        while ino != ROOT_INO {
            if ino == ancestor {
                return true;
            }
            match self.nodes.get(&ino) {
                Some(n) => ino = n.parent,
                None => return false,
            }
        }
        ancestor == ROOT_INO
    }
}

impl TestVfs {
    #[cfg(target_os = "linux")]
    pub fn new(uid: u32, gid: u32) -> Self {
        Self {
            uid,
            gid,
            ..Self::default()
        }
    }

    /// Makes `readdir` return cookies that never advance, to test the adapter's guard.
    pub fn break_cookies(&self) {
        self.bad_cookies.store(true, Ordering::Relaxed);
    }

    fn lock(&self) -> MutexGuard<'_, Option<Inner>> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.get_or_insert_with(|| {
            let mut nodes = HashMap::new();
            nodes.insert(ROOT_INO, node(FileKind::Directory, 0o755, ROOT_INO));
            Inner {
                nodes,
                next_ino: ROOT_INO + 1,
                handles: HashMap::new(),
                next_fh: 1,
            }
        });
        g
    }

    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> Result<T>) -> Result<T> {
        let mut g = self.lock();
        match g.as_mut() {
            Some(i) => f(i),
            None => Err(Error::Stale),
        }
    }

    fn attr(&self, i: &Inner, ino: Ino) -> Result<Attr> {
        let n = i.nodes.get(&ino).ok_or(Error::Stale)?;
        let size = if n.kind == FileKind::Directory {
            4096
        } else {
            n.data.len() as u64
        };
        Ok(Attr {
            ino,
            kind: n.kind,
            mode: n.mode,
            nlink: n.nlink,
            uid: self.uid,
            gid: self.gid,
            size,
            blocks: size.div_ceil(512),
            atime: n.times[0],
            mtime: n.times[1],
            ctime: n.times[2],
        })
    }
}

impl Vfs for TestVfs {
    fn lookup(&self, parent: Ino, name: &[u8]) -> Result<Attr> {
        self.with(|i| {
            let p = i.dir(parent)?;
            let seq = p.names.get(name).ok_or(Error::NotFound)?;
            let ino = p.entries.get(seq).map(|e| e.1).ok_or(Error::Stale)?;
            self.attr(i, ino)
        })
    }

    fn getattr(&self, ino: Ino) -> Result<Attr> {
        self.with(|i| self.attr(i, ino))
    }

    fn setattr(&self, ino: Ino, changes: SetAttr) -> Result<Attr> {
        self.with(|i| {
            let kind = i.get(ino)?.kind;
            if changes.size.is_some() {
                match kind {
                    FileKind::Directory => return Err(Error::IsDir),
                    FileKind::Symlink => return Err(Error::InvalidArgument),
                    FileKind::Regular => {}
                }
            }
            let n = i.get(ino)?;
            let now = Timestamp::now();
            let when = |t: SetTime| match t {
                SetTime::Now => now,
                SetTime::At(t) => t,
            };
            if let Some(m) = changes.mode {
                n.mode = m & MODE_MASK;
            }
            if let Some(s) = changes.size {
                n.data
                    .resize(usize::try_from(s).map_err(|_| Error::Range)?, 0);
                n.times[1] = now;
            }
            if let Some(t) = changes.atime {
                n.times[0] = when(t);
            }
            if let Some(t) = changes.mtime {
                n.times[1] = when(t);
            }
            n.times[2] = now;
            self.attr(i, ino)
        })
    }

    fn readlink(&self, ino: Ino) -> Result<Vec<u8>> {
        self.with(|i| {
            let n = i.get(ino)?;
            if n.kind == FileKind::Symlink {
                Ok(n.data.clone())
            } else {
                Err(Error::InvalidArgument)
            }
        })
    }

    fn create(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        self.with(|i| {
            let ino = i.add(parent, name, node(FileKind::Regular, mode, parent))?;
            self.attr(i, ino)
        })
    }

    fn mkdir(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        self.with(|i| {
            let ino = i.add(parent, name, node(FileKind::Directory, mode, parent))?;
            self.attr(i, ino)
        })
    }

    fn symlink(&self, parent: Ino, name: &[u8], target: &[u8]) -> Result<Attr> {
        self.with(|i| {
            let mut n = node(FileKind::Symlink, 0o777, parent);
            n.data = target.to_vec();
            let ino = i.add(parent, name, n)?;
            self.attr(i, ino)
        })
    }

    fn link(&self, ino: Ino, new_parent: Ino, new_name: &[u8]) -> Result<Attr> {
        self.with(|i| {
            validate_name(new_name)?;
            if i.get(ino)?.kind == FileKind::Directory {
                return Err(Error::PermissionDenied);
            }
            if i.dir(new_parent)?.names.contains_key(new_name) {
                return Err(Error::Exists);
            }
            i.insert(new_parent, new_name, ino);
            let n = i.get(ino)?;
            n.nlink += 1;
            n.times[2] = Timestamp::now();
            self.attr(i, ino)
        })
    }

    fn unlink(&self, parent: Ino, name: &[u8]) -> Result<()> {
        self.with(|i| {
            let p = i.dir(parent)?;
            let seq = p.names.get(name).ok_or(Error::NotFound)?;
            let ino = p.entries.get(seq).map(|e| e.1).ok_or(Error::Stale)?;
            if i.get(ino)?.kind == FileKind::Directory {
                return Err(Error::IsDir);
            }
            i.remove(parent, name);
            i.drop_link(ino);
            Ok(())
        })
    }

    fn rmdir(&self, parent: Ino, name: &[u8]) -> Result<()> {
        self.with(|i| {
            let p = i.dir(parent)?;
            let seq = p.names.get(name).ok_or(Error::NotFound)?;
            let ino = p.entries.get(seq).map(|e| e.1).ok_or(Error::Stale)?;
            if !i.dir(ino)?.entries.is_empty() {
                return Err(Error::NotEmpty);
            }
            i.remove(parent, name);
            i.nodes.remove(&ino);
            i.get(parent)?.nlink -= 1;
            Ok(())
        })
    }

    fn rename(
        &self,
        parent: Ino,
        name: &[u8],
        new_parent: Ino,
        new_name: &[u8],
        flags: RenameFlags,
    ) -> Result<()> {
        self.with(|i| {
            validate_name(new_name)?;
            let src = {
                let p = i.dir(parent)?;
                let seq = p.names.get(name).ok_or(Error::NotFound)?;
                p.entries.get(seq).map(|e| e.1).ok_or(Error::Stale)?
            };
            let dst = {
                let p = i.dir(new_parent)?;
                p.names
                    .get(new_name)
                    .and_then(|s| p.entries.get(s))
                    .map(|e| e.1)
            };
            let src_dir = i.get(src)?.kind == FileKind::Directory;
            if dst == Some(src) {
                return Ok(());
            }
            if src_dir && i.is_within(new_parent, src) {
                return Err(Error::InvalidArgument);
            }
            if let Some(d) = dst {
                if flags.no_replace {
                    return Err(Error::Exists);
                }
                let dst_dir = i.get(d)?.kind == FileKind::Directory;
                match (src_dir, dst_dir) {
                    (true, false) => return Err(Error::NotDir),
                    (false, true) => return Err(Error::IsDir),
                    (true, true) if !i.get(d)?.entries.is_empty() => return Err(Error::NotEmpty),
                    _ => {}
                }
                i.remove(new_parent, new_name);
                if dst_dir {
                    i.nodes.remove(&d);
                    i.get(new_parent)?.nlink -= 1;
                } else {
                    i.drop_link(d);
                }
            }
            i.remove(parent, name);
            i.insert(new_parent, new_name, src);
            if src_dir {
                i.get(parent)?.nlink -= 1;
                i.get(new_parent)?.nlink += 1;
                i.get(src)?.parent = new_parent;
            }
            i.touch(src, false);
            Ok(())
        })
    }

    fn open(&self, ino: Ino) -> Result<FileHandle> {
        self.with(|i| {
            i.get(ino)?.opens += 1;
            let fh = i.next_fh;
            i.next_fh += 1;
            i.handles.insert(fh, ino);
            Ok(FileHandle(fh))
        })
    }

    fn release(&self, handle: FileHandle) -> Result<()> {
        self.with(|i| {
            let ino = i.handles.remove(&handle.0).ok_or(Error::InvalidArgument)?;
            if let Some(n) = i.nodes.get_mut(&ino) {
                n.opens -= 1;
                if n.opens == 0 && n.nlink == 0 {
                    i.nodes.remove(&ino);
                }
            }
            Ok(())
        })
    }

    fn read(&self, ino: Ino, offset: u64, size: u32) -> Result<Vec<u8>> {
        self.with(|i| {
            let n = i.get(ino)?;
            if n.kind == FileKind::Directory {
                return Err(Error::IsDir);
            }
            let len = n.data.len() as u64;
            let start = offset.min(len) as usize;
            let end = offset.saturating_add(u64::from(size)).min(len) as usize;
            Ok(n.data[start..end].to_vec())
        })
    }

    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> Result<u32> {
        self.with(|i| {
            let n = i.get(ino)?;
            if n.kind == FileKind::Directory {
                return Err(Error::IsDir);
            }
            let start = usize::try_from(offset).map_err(|_| Error::Range)?;
            let end = start.checked_add(data.len()).ok_or(Error::Range)?;
            if n.data.len() < end {
                n.data.resize(end, 0);
            }
            n.data[start..end].copy_from_slice(data);
            i.touch(ino, true);
            u32::try_from(data.len()).map_err(|_| Error::Range)
        })
    }

    fn flush(&self, ino: Ino) -> Result<()> {
        self.with(|i| i.get(ino).map(|_| ()))
    }

    fn fsync(&self, ino: Ino, _data_only: bool) -> Result<()> {
        self.with(|i| i.get(ino).map(|_| ()))
    }

    fn readdir(&self, dir: Ino, cookie: u64, max: usize) -> Result<ReadDir> {
        let bad = self.bad_cookies.load(Ordering::Relaxed);
        self.with(|i| {
            let kinds: Vec<_> = {
                let d = i.dir(dir)?;
                d.entries
                    .range(cookie.saturating_add(1)..)
                    .take(max)
                    .map(|(c, (n, ino))| (*c, n.clone(), *ino))
                    .collect()
            };
            let d = i.dir(dir)?;
            let last = kinds.last().map(|e| e.0);
            let eof = last.is_none_or(|l| d.entries.range(l + 1..).next().is_none());
            let mut entries = Vec::with_capacity(kinds.len());
            for (c, name, ino) in kinds {
                let kind = i.get(ino)?.kind;
                entries.push(DirEntry {
                    ino,
                    kind,
                    name,
                    cookie: if bad { 0 } else { c },
                });
            }
            Ok(ReadDir { entries, eof })
        })
    }

    fn statfs(&self) -> Result<StatFs> {
        Ok(StatFs {
            block_size: 4096,
            blocks: 1 << 20,
            blocks_free: 1 << 19,
            blocks_available: 1 << 19,
            files: 1 << 20,
            files_free: 1 << 19,
            name_max: 255,
        })
    }

    fn getxattr(&self, ino: Ino, name: &[u8]) -> Result<Vec<u8>> {
        self.with(|i| i.get(ino)?.xattrs.get(name).cloned().ok_or(Error::NoAttr))
    }

    fn setxattr(&self, ino: Ino, name: &[u8], value: &[u8], flags: XattrFlags) -> Result<()> {
        self.with(|i| {
            let x = &mut i.get(ino)?.xattrs;
            if flags.create && x.contains_key(name) {
                return Err(Error::Exists);
            }
            if flags.replace && !x.contains_key(name) {
                return Err(Error::NoAttr);
            }
            x.insert(name.to_vec(), value.to_vec());
            Ok(())
        })
    }

    fn listxattr(&self, ino: Ino) -> Result<Vec<Vec<u8>>> {
        self.with(|i| Ok(i.get(ino)?.xattrs.keys().cloned().collect()))
    }

    fn removexattr(&self, ino: Ino, name: &[u8]) -> Result<()> {
        self.with(|i| {
            i.get(ino)?
                .xattrs
                .remove(name)
                .map(|_| ())
                .ok_or(Error::NoAttr)
        })
    }
}
