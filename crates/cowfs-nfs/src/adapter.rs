//! Translates NFSv3 operations to `Vfs` calls. `Adapter` is the synchronous core, `CowNfs` runs
//! it on blocking tasks behind the async `NFSFileSystem` trait. No lock is held across a `Vfs` call.
use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use cowfs_vfs::{
    validate_name, Attr, DirEntry, Error, FileKind, Ino, RenameFlags, SetAttr, SetTime, Timestamp,
    Vfs, NAME_MAX, ROOT_INO,
};
use nfsserve::nfs::{
    cookie3, count3, createverf3, fattr3, fileid3, filename3, fsstat3, nfs_fh3, nfspath3, nfsstat3,
    sattr3, set_mode3,
};
use nfsserve::vfs::{DirEntry as NfsDirEntry, NFSFileSystem, ReadDirResult};

use crate::convert::{fattr, set_attr};
use crate::errors::nfsstat;
use crate::handle::HandleCodec;
use crate::sidecar::{is_side, PerIno, SidecarBuffers};

pub(crate) type NfsResult<T> = Result<T, nfsstat3>;

const APPLEDOUBLE: &[u8] = b"._";
const SYMLINK_TARGET_MAX: usize = 1024;
const READDIR_PAGE: usize = 512;

/// What to do with the `._name` AppleDouble files the macOS client writes for extended attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppleDoubleMode {
    /// Serve `._name` as a view of the extended attributes of `name` and store nothing: the
    /// attributes live in the `Vfs` as xattrs of the real file, so no sidecar inodes exist and
    /// other mounts of the same data see no `._` files. Real files whose names start with `._`
    /// are invisible on the mount.
    Translate,
    /// Store the sidecars as ordinary files but hide them from listings. Removing a file
    /// removes its sidecar, renaming moves it, removing a directory that holds only sidecars
    /// removes them. The mounter cannot see the sidecars, other mounts see them.
    Hide,
    /// Treat `._` names like any other name.
    Store,
}

impl Default for AppleDoubleMode {
    /// `Hide` is the default: it is what the mount did before `Translate` existed and it is
    /// known to be correct for every tool. `Translate` is opt-in until its own tests pass.
    fn default() -> Self {
        AppleDoubleMode::Hide
    }
}

/// Adapter behaviour switches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdapterOptions {
    pub appledouble: AppleDoubleMode,
    /// The (uid, gid) reported for every file. `Mount` sets it to the owner of the mount point,
    /// so tools that check ownership (git) accept the tree. `None` reports what the `Vfs` says.
    pub owner: Option<(u32, u32)>,
}

/// True for AppleDouble sidecar names: `._` followed by at least one more byte.
pub fn is_appledouble(name: &[u8]) -> bool {
    name.len() > APPLEDOUBLE.len() && name.starts_with(APPLEDOUBLE)
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn stat(e: Error) -> nfsstat3 {
    nfsstat(&e)
}

/// A sidecar's file id names no directory.
fn not_side(dir: Ino) -> NfsResult<()> {
    if is_side(dir) {
        Err(nfsstat3::NFS3ERR_NOTDIR)
    } else {
        Ok(())
    }
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
    pub(crate) vfs: Arc<dyn Vfs>,
    pub(crate) opts: AdapterOptions,
    handles: HandleCodec,
    parents: Mutex<HashMap<Ino, Ino>>,
    pub(crate) sidecars: Mutex<SidecarBuffers>,
    pub(crate) sidecar_locks: Mutex<PerIno>,
}

impl std::fmt::Debug for Adapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Adapter")
            .field("opts", &self.opts)
            .field("generation", &self.handles.generation())
            .finish_non_exhaustive()
    }
}

impl Adapter {
    /// Fails only if the operating system cannot supply random bytes for the handle key.
    pub fn new(vfs: Arc<dyn Vfs>, opts: AdapterOptions) -> io::Result<Self> {
        Ok(Self {
            vfs,
            opts,
            handles: HandleCodec::new()?,
            parents: Mutex::new(HashMap::new()),
            sidecars: Mutex::new(SidecarBuffers::default()),
            sidecar_locks: Mutex::new(PerIno::default()),
        })
    }

    pub fn generation(&self) -> u64 {
        self.handles.generation()
    }

    /// The file handle for `ino`.
    pub fn handle(&self, ino: Ino) -> Vec<u8> {
        self.handles.encode(ino)
    }

    /// The inode a file handle names, or the status that refuses it.
    pub fn resolve(&self, handle: &[u8]) -> NfsResult<Ino> {
        self.handles.decode(handle)
    }

    pub(crate) fn fa(&self, a: &Attr) -> fattr3 {
        let mut f = fattr(a);
        if let Some((uid, gid)) = self.opts.owner {
            f.uid = uid;
            f.gid = gid;
        }
        f
    }

    /// Notes the parent of a directory just handed to the client and gives back the `Vfs`
    /// reference the call took. The adapter pins nothing: NFS handles are stateless, and pinned
    /// references would keep removed inodes alive (and blocked from garbage collection).
    fn handed_out(&self, parent: Option<Ino>, a: &Attr) {
        self.vfs.forget(a.ino, 1);
        if let (Some(p), FileKind::Directory) = (parent, a.kind) {
            lock(&self.parents).insert(a.ino, p);
        }
    }

    /// Looks a name up for the adapter's own use and gives the reference straight back.
    pub(crate) fn peek(&self, dir: Ino, name: &[u8]) -> Result<Attr, Error> {
        let a = self.vfs.lookup(dir, name)?;
        self.vfs.forget(a.ino, 1);
        Ok(a)
    }

    /// Forgets what the adapter knows about an inode whose last name was just removed.
    fn reap(&self, ino: Ino) {
        lock(&self.parents).remove(&ino);
        lock(&self.sidecars).remove(ino);
        self.handles.bury(ino);
    }

    /// Buries an inode that has no name left. Asked after the removal, never before: two
    /// unlinks of the last two names of one file would otherwise both see a link left and bury
    /// nothing.
    fn reap_if_last(&self, ino: Ino) {
        if !matches!(self.vfs.getattr(ino), Ok(a) if a.nlink > 0) {
            self.reap(ino);
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
        if is_side(id) {
            return self.side_getattr(id);
        }
        self.vfs.getattr(id).map(|a| self.fa(&a)).map_err(stat)
    }

    pub fn lookup(&self, dir: fileid3, name: &[u8]) -> NfsResult<(fileid3, fattr3)> {
        not_side(dir)?;
        if self.translating(dir, name) {
            return self.side_lookup(dir, name);
        }
        match name {
            b"." => {
                let a = self.vfs.getattr(dir).map_err(stat)?;
                if a.kind != FileKind::Directory {
                    return Err(nfsstat3::NFS3ERR_NOTDIR);
                }
                Ok((dir, self.fa(&a)))
            }
            b".." => {
                let p = self.parent_of(dir)?;
                Ok((p, self.getattr(p)?))
            }
            _ => {
                check_name(name)?;
                let a = self.vfs.lookup(dir, name).map_err(stat)?;
                self.handed_out(Some(dir), &a);
                Ok((a.ino, self.fa(&a)))
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
        if is_side(id) {
            return self.side_setattr(id, s);
        }
        self.apply(id, set_attr(s)).map(|a| self.fa(&a))
    }

    pub fn readlink(&self, id: fileid3) -> NfsResult<Vec<u8>> {
        if is_side(id) {
            return Err(nfsstat3::NFS3ERR_INVAL);
        }
        self.vfs.readlink(id).map_err(stat)
    }

    pub fn read(&self, id: fileid3, offset: u64, count: count3) -> NfsResult<(Vec<u8>, bool)> {
        if is_side(id) {
            return self.side_read(id, offset, count);
        }
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
        if is_side(id) {
            return self.side_write(id, offset, data);
        }
        let n = self.vfs.write(id, offset, data).map_err(stat)?;
        Ok((n, self.getattr(id)?))
    }

    pub fn commit(&self, id: fileid3) -> NfsResult<()> {
        if is_side(id) {
            return self.side_getattr(id).map(drop);
        }
        self.vfs.fsync(id, false).map_err(stat)
    }

    pub fn create(
        &self,
        dir: fileid3,
        name: &[u8],
        attr: &sattr3,
        guarded: bool,
    ) -> NfsResult<(fileid3, fattr3)> {
        not_side(dir)?;
        if self.side_of(dir, name) {
            return self.side_create(dir, name, attr, guarded);
        }
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
                Ok((a.ino, self.fa(&a)))
            }
            Err(Error::Exists) if !guarded => {
                let existing = self.vfs.lookup(dir, name).map_err(stat)?;
                if existing.kind == FileKind::Directory {
                    self.vfs.forget(existing.ino, 1);
                    return Err(nfsstat3::NFS3ERR_ISDIR);
                }
                self.handed_out(Some(dir), &existing);
                let a = self.apply(existing.ino, changes)?;
                Ok((a.ino, self.fa(&a)))
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
        not_side(dir)?;
        if self.side_of(dir, name) {
            return self.side_create_exclusive(dir, name);
        }
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
                Ok((a.ino, self.fa(&a)))
            }
            Err(Error::Exists) => {
                let a = self.vfs.lookup(dir, name).map_err(stat)?;
                if a.kind == FileKind::Regular && a.atime == atime && a.mtime == mtime {
                    self.handed_out(Some(dir), &a);
                    Ok((a.ino, self.fa(&a)))
                } else {
                    self.vfs.forget(a.ino, 1);
                    Err(nfsstat3::NFS3ERR_EXIST)
                }
            }
            Err(e) => Err(stat(e)),
        }
    }

    pub fn mkdir(&self, dir: fileid3, name: &[u8], attr: &sattr3) -> NfsResult<(fileid3, fattr3)> {
        not_side(dir)?;
        new_name(name)?;
        let mode = match attr.mode {
            set_mode3::mode(m) => m,
            set_mode3::Void => 0o755,
        };
        let a = self.vfs.mkdir(dir, name, mode).map_err(stat)?;
        self.handed_out(Some(dir), &a);
        Ok((a.ino, self.fa(&a)))
    }

    pub fn symlink(
        &self,
        dir: fileid3,
        name: &[u8],
        target: &[u8],
    ) -> NfsResult<(fileid3, fattr3)> {
        not_side(dir)?;
        new_name(name)?;
        if target.is_empty() || target.contains(&0) {
            return Err(nfsstat3::NFS3ERR_INVAL);
        }
        if target.len() > SYMLINK_TARGET_MAX {
            return Err(nfsstat3::NFS3ERR_NAMETOOLONG);
        }
        let a = self.vfs.symlink(dir, name, target).map_err(stat)?;
        self.handed_out(Some(dir), &a);
        Ok((a.ino, self.fa(&a)))
    }

    pub fn link(&self, file: fileid3, dir: fileid3, name: &[u8]) -> NfsResult<fattr3> {
        not_side(dir)?;
        if is_side(file) {
            return Err(nfsstat3::NFS3ERR_ACCES);
        }
        new_name(name)?;
        let a = self.vfs.link(file, dir, name).map_err(stat)?;
        self.handed_out(None, &a);
        Ok(self.fa(&a))
    }

    /// Removes one name and releases the inode if that was its last link.
    fn remove_one(&self, dir: Ino, name: &[u8]) -> NfsResult<()> {
        let target = self.peek(dir, name).map_err(stat)?;
        self.vfs.unlink(dir, name).map_err(stat)?;
        self.reap_if_last(target.ino);
        Ok(())
    }

    fn sidecar(name: &[u8]) -> Option<Vec<u8>> {
        if is_appledouble(name) || APPLEDOUBLE.len() + name.len() > NAME_MAX {
            return None;
        }
        Some([APPLEDOUBLE, name].concat())
    }

    pub fn remove(&self, dir: fileid3, name: &[u8]) -> NfsResult<()> {
        not_side(dir)?;
        check_name(name)?;
        if self.translating(dir, name) {
            return self.side_remove(dir, name);
        }
        self.remove_one(dir, name)?;
        if let (true, Some(side)) = (
            self.opts.appledouble == AppleDoubleMode::Hide,
            Self::sidecar(name),
        ) {
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
        not_side(dir)?;
        check_name(name)?;
        if self.translating(dir, name) {
            return Err(nfsstat3::NFS3ERR_NOTDIR);
        }
        let target = self.peek(dir, name).map_err(stat)?;
        match self.vfs.rmdir(dir, name) {
            Err(Error::NotEmpty) if self.opts.appledouble == AppleDoubleMode::Hide => {
                self.purge_sidecars(target.ino)?;
                self.vfs.rmdir(dir, name).map_err(stat)?;
            }
            r => r.map_err(stat)?,
        }
        self.reap_if_last(target.ino);
        Ok(())
    }

    pub fn rename(
        &self,
        from_dir: fileid3,
        from: &[u8],
        to_dir: fileid3,
        to: &[u8],
    ) -> NfsResult<()> {
        not_side(from_dir)?;
        not_side(to_dir)?;
        check_name(from)?;
        check_name(to)?;
        match (self.side_of(from_dir, from), self.side_of(to_dir, to)) {
            // The attributes live on the inode and moved with it, so there is nothing to do.
            (true, _) => return Ok(()),
            // A real file cannot be moved onto the view of another file.
            (false, true) if !self.translating(from_dir, from) => {
                return Err(nfsstat3::NFS3ERR_ACCES);
            }
            // A plain name moved onto a view: that view is the file's own attributes already.
            (false, true) => return Ok(()),
            // Both are ordinary names, including a `._name` with no main file to hold attributes.
            (false, false) => {}
        }
        let src = self.peek(from_dir, from).map_err(stat)?;
        let replaced = self.peek(to_dir, to).ok();
        self.vfs
            .rename(from_dir, from, to_dir, to, RenameFlags::default())
            .map_err(stat)?;
        if src.kind == FileKind::Directory {
            lock(&self.parents).insert(src.ino, to_dir);
        }
        if let Some(d) = replaced {
            if d.ino != src.ino {
                self.reap_if_last(d.ino);
            }
        }
        if self.opts.appledouble == AppleDoubleMode::Hide {
            if let (Some(from_side), Some(to_side)) = (Self::sidecar(from), Self::sidecar(to)) {
                let replaced_side = self.peek(to_dir, &to_side).ok();
                let moved = self.vfs.rename(
                    from_dir,
                    &from_side,
                    to_dir,
                    &to_side,
                    RenameFlags::default(),
                );
                if moved.is_err() {
                    let _ = self.remove_one(to_dir, &to_side);
                } else if let Some(d) = replaced_side {
                    if d.nlink <= 1 {
                        self.reap(d.ino);
                    }
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
        not_side(dir)?;
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
                if self.opts.appledouble == AppleDoubleMode::Hide && is_appledouble(&e.name) {
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
            Some(self.fa(&a))
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
    pub fn new(vfs: Arc<dyn Vfs>, opts: AdapterOptions) -> io::Result<Self> {
        Ok(Self(Arc::new(Adapter::new(vfs, opts)?)))
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

    fn id_to_fh(&self, id: fileid3) -> nfs_fh3 {
        nfs_fh3 {
            data: self.0.handle(id),
        }
    }

    fn fh_to_id(&self, fh: &nfs_fh3) -> NfsResult<fileid3> {
        self.0.resolve(&fh.data)
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

#[cfg(test)]
mod tests {
    use super::*;
    use cowfs_vfs_test::MemVfs;

    fn adapter(hide: bool) -> Adapter {
        Adapter::new(
            Arc::new(MemVfs::new()),
            AdapterOptions {
                appledouble: if hide {
                    AppleDoubleMode::Hide
                } else {
                    AppleDoubleMode::Store
                },
                ..AdapterOptions::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn new_names_are_validated() {
        assert_eq!(new_name(b"ok"), Ok(()));
        assert_eq!(new_name(b"."), Err(nfsstat3::NFS3ERR_EXIST));
        assert_eq!(new_name(b".."), Err(nfsstat3::NFS3ERR_EXIST));
        assert_eq!(new_name(b""), Err(nfsstat3::NFS3ERR_INVAL));
        assert_eq!(new_name(b"a/b"), Err(nfsstat3::NFS3ERR_INVAL));
        assert_eq!(new_name(b"a\0b"), Err(nfsstat3::NFS3ERR_INVAL));
        assert_eq!(new_name(&[b'x'; NAME_MAX]), Ok(()));
        assert_eq!(
            new_name(&[b'x'; NAME_MAX + 1]),
            Err(nfsstat3::NFS3ERR_NAMETOOLONG)
        );
    }

    #[test]
    fn appledouble_names() {
        assert!(is_appledouble(b"._x"));
        assert!(!is_appledouble(b"._"), "needs a name after the prefix");
        assert!(!is_appledouble(b".x"));
        assert!(!is_appledouble(b"x._"));
        assert_eq!(Adapter::sidecar(b"x"), Some(b"._x".to_vec()));
        assert_eq!(Adapter::sidecar(b"._x"), None);
        assert_eq!(
            Adapter::sidecar(&[b'x'; NAME_MAX]),
            None,
            "the sidecar name would be too long"
        );
    }

    #[test]
    fn exclusive_verifier_survives_a_round_trip() {
        let (a, m) = verifier_times([0, 0, 1, 2, 0xff, 0, 0, 3]);
        assert_eq!((a.secs, m.secs), (0x102, 0xff00_0003));
    }

    #[test]
    fn read_eof_flag() {
        let a = adapter(true);
        let (f, _) = a.create(ROOT_INO, b"f", &sattr3::default(), true).unwrap();
        a.write(f, 0, b"abcdef").unwrap();
        assert_eq!(a.read(f, 0, 3).unwrap(), (b"abc".to_vec(), false));
        assert_eq!(
            a.read(f, 3, 3).unwrap(),
            (b"def".to_vec(), true),
            "a read ending exactly at the end"
        );
        assert_eq!(a.read(f, 3, 10).unwrap(), (b"def".to_vec(), true));
        assert_eq!(a.read(f, 6, 3).unwrap(), (Vec::new(), true));
    }

    #[test]
    fn hidden_entries_advance_the_cookie_without_ending_the_listing() {
        let a = adapter(true);
        for n in ["a", "._1", "._2", "._3", "b", "._4", "c"] {
            a.create(ROOT_INO, n.as_bytes(), &sattr3::default(), true)
                .unwrap();
        }
        let mut seen = Vec::new();
        let mut cookie = 0;
        loop {
            let page = a.readdir(ROOT_INO, cookie, 1, false).unwrap();
            for e in &page.entries {
                seen.push(String::from_utf8(e.name.0.clone()).unwrap());
                cookie = e.cookie;
            }
            if page.end {
                break;
            }
            assert!(!page.entries.is_empty(), "an empty page must be the last");
        }
        assert_eq!(seen, ["a", "b", "c"]);
        let all = adapter(false);
        all.create(ROOT_INO, b"._1", &sattr3::default(), true)
            .unwrap();
        assert_eq!(
            all.readdir(ROOT_INO, 0, 10, false).unwrap().entries.len(),
            1
        );
    }

    #[test]
    fn readdir_of_a_missing_or_plain_file_fails() {
        let a = adapter(true);
        assert_eq!(
            a.readdir(999, 0, 10, false).err(),
            Some(nfsstat3::NFS3ERR_STALE)
        );
        let (f, _) = a.create(ROOT_INO, b"f", &sattr3::default(), true).unwrap();
        assert_eq!(
            a.readdir(f, 0, 10, false).err(),
            Some(nfsstat3::NFS3ERR_NOTDIR)
        );
    }

    #[test]
    fn symlink_targets_are_bounded() {
        let a = adapter(true);
        assert!(a.symlink(ROOT_INO, b"l", b"t").is_ok());
        assert_eq!(
            a.symlink(ROOT_INO, b"l2", b"").err(),
            Some(nfsstat3::NFS3ERR_INVAL)
        );
        assert_eq!(
            a.symlink(ROOT_INO, b"l3", b"a\0b").err(),
            Some(nfsstat3::NFS3ERR_INVAL)
        );
        let long = vec![b'x'; SYMLINK_TARGET_MAX + 1];
        assert_eq!(
            a.symlink(ROOT_INO, b"l4", &long).err(),
            Some(nfsstat3::NFS3ERR_NAMETOOLONG)
        );
    }

    #[test]
    fn fsstat_uses_block_units() {
        let f = adapter(true).fsstat().unwrap();
        assert!(f.tbytes >= f.fbytes && f.fbytes >= f.abytes.min(f.fbytes));
        assert!(f.tfiles > 0);
    }
}
