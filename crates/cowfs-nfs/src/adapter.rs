//! Translates NFSv3 operations to `Vfs` calls. `Adapter` is the synchronous core, `CowNfs` runs
//! it on blocking tasks behind the async `NFSFileSystem` trait. No lock is held across a `Vfs` call.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use cowfs_vfs::{
    validate_name, Attr, DirEntry, Error, FileKind, Ino, RenameFlags, SetAttr, SetTime, Timestamp,
    Vfs, NAME_MAX, ROOT_INO,
};
use nfsserve::nfs::{
    cookie3, count3, createverf3, fattr3, fileid3, filename3, fsstat3, nfspath3, nfsstat3, sattr3,
    set_mode3,
};
use nfsserve::vfs::{DirEntry as NfsDirEntry, NFSFileSystem, ReadDirResult};

use crate::convert::{fattr, set_attr};
use crate::errors::nfsstat;

type NfsResult<T> = Result<T, nfsstat3>;

const APPLEDOUBLE: &[u8] = b"._";
const SYMLINK_TARGET_MAX: usize = 1024;
const READDIR_PAGE: usize = 512;

/// Adapter behaviour switches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdapterOptions {
    /// Hide `._*` entries from directory listings. The macOS client writes them next to files
    /// that carry extended attributes. They are still stored and can still be looked up, they
    /// are just not listed. Removing a file removes its sidecar, renaming moves it, and removing
    /// a directory that holds only sidecars removes them.
    pub hide_appledouble: bool,
}

impl Default for AdapterOptions {
    fn default() -> Self {
        Self {
            hide_appledouble: true,
        }
    }
}

/// True for AppleDouble sidecar names (`._name`).
pub fn is_appledouble(name: &[u8]) -> bool {
    name.starts_with(APPLEDOUBLE)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn stat(e: Error) -> nfsstat3 {
    nfsstat(&e)
}

/// A name that is about to be created: "." and ".." exist already.
fn new_name(name: &[u8]) -> NfsResult<()> {
    if name == b"." || name == b".." {
        return Err(nfsstat3::NFS3ERR_EXIST);
    }
    check_name(name)
}

fn check_name(name: &[u8]) -> NfsResult<()> {
    validate_name(name).map_err(stat)
}

/// Exclusive-create verifiers are kept in atime and mtime, the way NFS servers usually do.
fn verifier_times(verf: createverf3) -> (Timestamp, Timestamp) {
    let word = |b: &[u8]| Timestamp {
        secs: i64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]])),
        nanos: 0,
    };
    (word(&verf[0..4]), word(&verf[4..8]))
}

/// The synchronous NFS to `Vfs` translation.
pub struct Adapter {
    vfs: Arc<dyn Vfs>,
    opts: AdapterOptions,
    generation: u64,
    parents: Mutex<HashMap<Ino, Ino>>,
    refs: Mutex<HashMap<Ino, u64>>,
}

impl std::fmt::Debug for Adapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Adapter")
            .field("opts", &self.opts)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Adapter {
    pub fn new(vfs: Arc<dyn Vfs>, opts: AdapterOptions) -> Self {
        let generation = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| {
            u64::try_from(d.as_millis()).unwrap_or(u64::MAX).max(1)
        });
        Self {
            vfs,
            opts,
            generation,
            parents: Mutex::new(HashMap::new()),
            refs: Mutex::new(HashMap::new()),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Records a reference handed to the client, and the parent of a directory.
    fn handed_out(&self, parent: Option<Ino>, a: &Attr) {
        *lock(&self.refs).entry(a.ino).or_insert(0) += 1;
        if let (Some(p), FileKind::Directory) = (parent, a.kind) {
            lock(&self.parents).insert(a.ino, p);
        }
    }

    /// Looks a name up for the adapter's own use and gives the reference straight back.
    fn peek(&self, dir: Ino, name: &[u8]) -> Result<Attr, Error> {
        let a = self.vfs.lookup(dir, name)?;
        self.vfs.forget(a.ino, 1);
        Ok(a)
    }

    /// Drops the references held for an inode whose last name was just removed.
    fn reap(&self, ino: Ino) {
        let n = lock(&self.refs).remove(&ino).unwrap_or(0);
        lock(&self.parents).remove(&ino);
        if n > 0 {
            self.vfs.forget(ino, n);
        }
    }

    fn parent_of(&self, dir: Ino) -> NfsResult<Ino> {
        if dir == ROOT_INO {
            return Ok(ROOT_INO);
        }
        lock(&self.parents)
            .get(&dir)
            .copied()
            .ok_or(nfsstat3::NFS3ERR_STALE)
    }

    pub fn getattr(&self, id: fileid3) -> NfsResult<fattr3> {
        self.vfs.getattr(id).map(|a| fattr(&a)).map_err(stat)
    }

    pub fn lookup(&self, dir: fileid3, name: &[u8]) -> NfsResult<(fileid3, fattr3)> {
        match name {
            b"." => {
                let a = self.vfs.getattr(dir).map_err(stat)?;
                if a.kind != FileKind::Directory {
                    return Err(nfsstat3::NFS3ERR_NOTDIR);
                }
                Ok((dir, fattr(&a)))
            }
            b".." => {
                let p = self.parent_of(dir)?;
                Ok((p, self.getattr(p)?))
            }
            _ => {
                check_name(name)?;
                let a = self.vfs.lookup(dir, name).map_err(stat)?;
                self.handed_out(Some(dir), &a);
                Ok((a.ino, fattr(&a)))
            }
        }
    }

    /// Applies `changes` and returns the new attributes, or just the attributes if there are none.
    fn apply(&self, id: Ino, changes: SetAttr) -> NfsResult<Attr> {
        if changes == SetAttr::default() {
            self.vfs.getattr(id).map_err(stat)
        } else {
            self.vfs.setattr(id, changes).map_err(stat)
        }
    }

    pub fn setattr(&self, id: fileid3, s: &sattr3) -> NfsResult<fattr3> {
        self.apply(id, set_attr(s)).map(|a| fattr(&a))
    }

    pub fn readlink(&self, id: fileid3) -> NfsResult<Vec<u8>> {
        self.vfs.readlink(id).map_err(stat)
    }

    pub fn read(&self, id: fileid3, offset: u64, count: count3) -> NfsResult<(Vec<u8>, bool)> {
        let data = self.vfs.read(id, offset, count).map_err(stat)?;
        let len = data.len() as u64;
        let eof = if len < u64::from(count) {
            true
        } else {
            let size = self.vfs.getattr(id).map_err(stat)?.size;
            offset.saturating_add(len) >= size
        };
        Ok((data, eof))
    }

    pub fn write(&self, id: fileid3, offset: u64, data: &[u8]) -> NfsResult<(u32, fattr3)> {
        let n = self.vfs.write(id, offset, data).map_err(stat)?;
        Ok((n, self.getattr(id)?))
    }

    pub fn commit(&self, id: fileid3) -> NfsResult<()> {
        self.vfs.fsync(id, false).map_err(stat)
    }

    pub fn create(
        &self,
        dir: fileid3,
        name: &[u8],
        attr: &sattr3,
        guarded: bool,
    ) -> NfsResult<(fileid3, fattr3)> {
        new_name(name)?;
        let mode = match attr.mode {
            set_mode3::mode(m) => m,
            set_mode3::Void => 0o644,
        };
        let mut changes = set_attr(attr);
        match self.vfs.create(dir, name, mode) {
            Ok(a) => {
                self.handed_out(Some(dir), &a);
                changes.mode = None;
                if changes.size == Some(0) {
                    changes.size = None;
                }
                let a = self.apply(a.ino, changes)?;
                Ok((a.ino, fattr(&a)))
            }
            Err(Error::Exists) if !guarded => {
                let existing = self.vfs.lookup(dir, name).map_err(stat)?;
                if existing.kind == FileKind::Directory {
                    self.vfs.forget(existing.ino, 1);
                    return Err(nfsstat3::NFS3ERR_ISDIR);
                }
                self.handed_out(Some(dir), &existing);
                let a = self.apply(existing.ino, changes)?;
                Ok((a.ino, fattr(&a)))
            }
            Err(e) => Err(stat(e)),
        }
    }

    pub fn create_exclusive(
        &self,
        dir: fileid3,
        name: &[u8],
        verf: createverf3,
    ) -> NfsResult<(fileid3, fattr3)> {
        new_name(name)?;
        let (atime, mtime) = verifier_times(verf);
        match self.vfs.create(dir, name, 0o600) {
            Ok(a) => {
                self.handed_out(Some(dir), &a);
                let changes = SetAttr {
                    atime: Some(SetTime::At(atime)),
                    mtime: Some(SetTime::At(mtime)),
                    ..SetAttr::default()
                };
                let a = self.vfs.setattr(a.ino, changes).map_err(stat)?;
                Ok((a.ino, fattr(&a)))
            }
            Err(Error::Exists) => {
                let a = self.vfs.lookup(dir, name).map_err(stat)?;
                if a.kind == FileKind::Regular && a.atime == atime && a.mtime == mtime {
                    self.handed_out(Some(dir), &a);
                    Ok((a.ino, fattr(&a)))
                } else {
                    self.vfs.forget(a.ino, 1);
                    Err(nfsstat3::NFS3ERR_EXIST)
                }
            }
            Err(e) => Err(stat(e)),
        }
    }

    pub fn mkdir(&self, dir: fileid3, name: &[u8], attr: &sattr3) -> NfsResult<(fileid3, fattr3)> {
        new_name(name)?;
        let mode = match attr.mode {
            set_mode3::mode(m) => m,
            set_mode3::Void => 0o755,
        };
        let a = self.vfs.mkdir(dir, name, mode).map_err(stat)?;
        self.handed_out(Some(dir), &a);
        Ok((a.ino, fattr(&a)))
    }

    pub fn symlink(
        &self,
        dir: fileid3,
        name: &[u8],
        target: &[u8],
    ) -> NfsResult<(fileid3, fattr3)> {
        new_name(name)?;
        if target.is_empty() || target.contains(&0) {
            return Err(nfsstat3::NFS3ERR_INVAL);
        }
        if target.len() > SYMLINK_TARGET_MAX {
            return Err(nfsstat3::NFS3ERR_NAMETOOLONG);
        }
        let a = self.vfs.symlink(dir, name, target).map_err(stat)?;
        self.handed_out(Some(dir), &a);
        Ok((a.ino, fattr(&a)))
    }

    pub fn link(&self, file: fileid3, dir: fileid3, name: &[u8]) -> NfsResult<fattr3> {
        new_name(name)?;
        let a = self.vfs.link(file, dir, name).map_err(stat)?;
        self.handed_out(None, &a);
        Ok(fattr(&a))
    }

    /// Removes one name and releases the inode if that was its last link.
    fn remove_one(&self, dir: Ino, name: &[u8]) -> NfsResult<()> {
        let target = self.peek(dir, name).map_err(stat)?;
        self.vfs.unlink(dir, name).map_err(stat)?;
        if target.nlink <= 1 {
            self.reap(target.ino);
        }
        Ok(())
    }

    fn sidecar(name: &[u8]) -> Option<Vec<u8>> {
        if is_appledouble(name) || APPLEDOUBLE.len() + name.len() > NAME_MAX {
            return None;
        }
        Some([APPLEDOUBLE, name].concat())
    }

    pub fn remove(&self, dir: fileid3, name: &[u8]) -> NfsResult<()> {
        check_name(name)?;
        self.remove_one(dir, name)?;
        if let (true, Some(side)) = (self.opts.hide_appledouble, Self::sidecar(name)) {
            let _ = self.remove_one(dir, &side);
        }
        Ok(())
    }

    /// Removes every entry of `dir` if all of them are AppleDouble sidecars.
    fn purge_sidecars(&self, dir: Ino) -> NfsResult<()> {
        let mut names = Vec::new();
        let mut cookie = 0;
        loop {
            let page = self.vfs.readdir(dir, cookie, READDIR_PAGE).map_err(stat)?;
            for e in &page.entries {
                if !is_appledouble(&e.name) {
                    return Err(nfsstat3::NFS3ERR_NOTEMPTY);
                }
                names.push(e.name.clone());
            }
            match page.entries.last() {
                Some(last) if !page.eof => cookie = last.cookie,
                _ => break,
            }
        }
        for name in names {
            self.remove_one(dir, &name)?;
        }
        Ok(())
    }

    pub fn rmdir(&self, dir: fileid3, name: &[u8]) -> NfsResult<()> {
        check_name(name)?;
        let target = self.peek(dir, name).map_err(stat)?;
        match self.vfs.rmdir(dir, name) {
            Err(Error::NotEmpty) if self.opts.hide_appledouble => {
                self.purge_sidecars(target.ino)?;
                self.vfs.rmdir(dir, name).map_err(stat)?;
            }
            r => r.map_err(stat)?,
        }
        self.reap(target.ino);
        Ok(())
    }

    pub fn rename(
        &self,
        from_dir: fileid3,
        from: &[u8],
        to_dir: fileid3,
        to: &[u8],
    ) -> NfsResult<()> {
        check_name(from)?;
        check_name(to)?;
        let src = self.peek(from_dir, from).map_err(stat)?;
        let replaced = self.peek(to_dir, to).ok();
        self.vfs
            .rename(from_dir, from, to_dir, to, RenameFlags::default())
            .map_err(stat)?;
        if src.kind == FileKind::Directory {
            lock(&self.parents).insert(src.ino, to_dir);
        }
        if let Some(d) = replaced {
            if d.ino != src.ino && (d.kind == FileKind::Directory || d.nlink <= 1) {
                self.reap(d.ino);
            }
        }
        if self.opts.hide_appledouble {
            if let (Some(from_side), Some(to_side)) = (Self::sidecar(from), Self::sidecar(to)) {
                let moved = self.vfs.rename(
                    from_dir,
                    &from_side,
                    to_dir,
                    &to_side,
                    RenameFlags::default(),
                );
                if moved.is_err() {
                    let _ = self.remove_one(to_dir, &to_side);
                }
            }
        }
        Ok(())
    }

    /// Up to `max` visible entries after `cookie`. Hidden sidecars are skipped but their cookies
    /// still advance the position, and entries that vanish before their attributes are read are dropped.
    pub fn readdir(
        &self,
        dir: fileid3,
        cookie: cookie3,
        max: usize,
        with_attrs: bool,
    ) -> NfsResult<ReadDirResult> {
        let max = max.max(1);
        let mut out = ReadDirResult::default();
        let mut cookie = cookie;
        loop {
            let page = self
                .vfs
                .readdir(dir, cookie, max - out.entries.len())
                .map_err(stat)?;
            let mut consumed_all = true;
            for e in &page.entries {
                if out.entries.len() == max {
                    consumed_all = false;
                    break;
                }
                cookie = e.cookie;
                if self.opts.hide_appledouble && is_appledouble(&e.name) {
                    continue;
                }
                if let Some(entry) = self.list_entry(dir, e, with_attrs) {
                    out.entries.push(entry);
                }
            }
            if consumed_all && page.eof {
                out.end = true;
                break;
            }
            if !consumed_all || out.entries.len() >= max || page.entries.is_empty() {
                break;
            }
        }
        Ok(out)
    }

    fn list_entry(&self, dir: Ino, e: &DirEntry, with_attrs: bool) -> Option<NfsDirEntry> {
        let attr = if with_attrs {
            let a = self.vfs.getattr(e.ino).ok()?;
            if a.kind == FileKind::Directory {
                lock(&self.parents).insert(a.ino, dir);
            }
            Some(fattr(&a))
        } else {
            None
        };
        Some(NfsDirEntry {
            fileid: e.ino,
            name: e.name.clone().into(),
            attr,
            cookie: e.cookie,
        })
    }

    pub fn fsstat(&self) -> NfsResult<fsstat3> {
        let s = self.vfs.statfs().map_err(stat)?;
        let bytes = |blocks: u64| blocks.saturating_mul(u64::from(s.block_size));
        Ok(fsstat3 {
            tbytes: bytes(s.blocks),
            fbytes: bytes(s.blocks_free),
            abytes: bytes(s.blocks_available),
            tfiles: s.files,
            ffiles: s.files_free,
            afiles: s.files_free,
            ..fsstat3::default()
        })
    }
}

/// The async face of `Adapter`: every `Vfs` call runs on a blocking task.
#[derive(Clone, Debug)]
pub struct CowNfs(Arc<Adapter>);

impl CowNfs {
    pub fn new(vfs: Arc<dyn Vfs>, opts: AdapterOptions) -> Self {
        Self(Arc::new(Adapter::new(vfs, opts)))
    }

    async fn run<T, F>(&self, f: F) -> NfsResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Adapter) -> NfsResult<T> + Send + 'static,
    {
        let a = self.0.clone();
        tokio::task::spawn_blocking(move || f(&a))
            .await
            .unwrap_or(Err(nfsstat3::NFS3ERR_SERVERFAULT))
    }
}

#[async_trait]
impl NFSFileSystem for CowNfs {
    fn root_dir(&self) -> fileid3 {
        ROOT_INO
    }

    fn generation(&self) -> u64 {
        self.0.generation()
    }

    async fn lookup(&self, dirid: fileid3, name: &filename3) -> NfsResult<(fileid3, fattr3)> {
        let name = name.clone();
        self.run(move |a| a.lookup(dirid, &name)).await
    }

    async fn getattr(&self, id: fileid3) -> NfsResult<fattr3> {
        self.run(move |a| a.getattr(id)).await
    }

    async fn setattr(&self, id: fileid3, s: sattr3) -> NfsResult<fattr3> {
        self.run(move |a| a.setattr(id, &s)).await
    }

    async fn readlink(&self, id: fileid3) -> NfsResult<nfspath3> {
        self.run(move |a| a.readlink(id).map(Into::into)).await
    }

    async fn read(&self, id: fileid3, offset: u64, count: u32) -> NfsResult<(Vec<u8>, bool)> {
        self.run(move |a| a.read(id, offset, count)).await
    }

    async fn write(&self, id: fileid3, offset: u64, data: Vec<u8>) -> NfsResult<(u32, fattr3)> {
        self.run(move |a| a.write(id, offset, &data)).await
    }

    async fn commit(&self, id: fileid3) -> NfsResult<()> {
        self.run(move |a| a.commit(id)).await
    }

    async fn create(
        &self,
        dirid: fileid3,
        name: &filename3,
        attr: sattr3,
        guarded: bool,
    ) -> NfsResult<(fileid3, fattr3)> {
        let name = name.clone();
        self.run(move |a| a.create(dirid, &name, &attr, guarded))
            .await
    }

    async fn create_exclusive(
        &self,
        dirid: fileid3,
        name: &filename3,
        verf: createverf3,
    ) -> NfsResult<(fileid3, fattr3)> {
        let name = name.clone();
        self.run(move |a| a.create_exclusive(dirid, &name, verf))
            .await
    }

    async fn mkdir(
        &self,
        dirid: fileid3,
        name: &filename3,
        attr: &sattr3,
    ) -> NfsResult<(fileid3, fattr3)> {
        let (name, attr) = (name.clone(), *attr);
        self.run(move |a| a.mkdir(dirid, &name, &attr)).await
    }

    async fn symlink(
        &self,
        dirid: fileid3,
        name: &filename3,
        target: &nfspath3,
        _attr: &sattr3,
    ) -> NfsResult<(fileid3, fattr3)> {
        let (name, target) = (name.clone(), target.clone());
        self.run(move |a| a.symlink(dirid, &name, &target)).await
    }

    async fn link(&self, file_id: fileid3, dir_id: fileid3, name: &filename3) -> NfsResult<fattr3> {
        let name = name.clone();
        self.run(move |a| a.link(file_id, dir_id, &name)).await
    }

    async fn remove(&self, dirid: fileid3, name: &filename3) -> NfsResult<()> {
        let name = name.clone();
        self.run(move |a| a.remove(dirid, &name)).await
    }

    async fn rmdir(&self, dirid: fileid3, name: &filename3) -> NfsResult<()> {
        let name = name.clone();
        self.run(move |a| a.rmdir(dirid, &name)).await
    }

    async fn rename(
        &self,
        from_dirid: fileid3,
        from: &filename3,
        to_dirid: fileid3,
        to: &filename3,
    ) -> NfsResult<()> {
        let (from, to) = (from.clone(), to.clone());
        self.run(move |a| a.rename(from_dirid, &from, to_dirid, &to))
            .await
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        cookie: cookie3,
        max_entries: usize,
        with_attrs: bool,
    ) -> NfsResult<ReadDirResult> {
        self.run(move |a| a.readdir(dirid, cookie, max_entries, with_attrs))
            .await
    }

    async fn fsstat(&self, _id: fileid3) -> NfsResult<fsstat3> {
        self.run(|a| a.fsstat()).await
    }
}
