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
use crate::handle::{HandleCodec, Kind};
use crate::sidecar::{PerIno, SidecarBuffers};

pub(crate) type NfsResult<T> = Result<T, nfsstat3>;

const APPLEDOUBLE: &[u8] = b"._";
const SYMLINK_TARGET_MAX: usize = 1024;
const READDIR_PAGE: usize = 512;
/// How many file ids to step over when the `Vfs` is using the ones a sidecar would take.
const ID_PROBES: usize = 256;

/// What a file id names: an inode of the `Vfs`, or the AppleDouble sidecar of that inode.
///
/// The `Vfs` owns the whole `u64` inode space, so sidecar-ness is not a bit of the number
/// (`cowfs-core` marks a virtual inode with `1 << 63`). A sidecar has no inode at all, so the
/// adapter gives it a file id of its own and keeps the map, both ways.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Id {
    pub ino: Ino,
    pub kind: Kind,
}

impl Id {
    pub(crate) fn plain(ino: Ino) -> Self {
        Self {
            ino,
            kind: Kind::Plain,
        }
    }

    pub(crate) fn side(ino: Ino) -> Self {
        Self {
            ino,
            kind: Kind::Sidecar,
        }
    }

    fn is_side(self) -> bool {
        self.kind == Kind::Sidecar
    }
}

/// The file ids of the translated sidecars, which are not inodes and so cannot be one.
///
/// An inode of the `Vfs` keeps its own number as its file id, so an id given to a sidecar has to be
/// a number the `Vfs` is not using. Ids come from the top of the `u64` space downwards, which is
/// where a file system is least likely to hand out numbers, and one that turns out to be in use is
/// stepped over (see [`Adapter::free_id`]). A number the `Vfs` starts using afterwards is taken
/// back for the inode and the sidecar is given another, so a number the client has been given for
/// an inode never names a sidecar. A sidecar that moves is still the same sidecar to the client: it
/// sends a file handle back, never an id, and a handle names the sidecar rather than its number.
#[derive(Debug)]
struct SideIds {
    /// The id of the sidecar of each inode that has one.
    side_of: HashMap<Ino, Ino>,
    /// The inode whose sidecar has a given id.
    side_with: HashMap<Ino, Ino>,
    /// The next id to try. Never higher than the lowest one handed out.
    next: Ino,
}

impl SideIds {
    fn new() -> Self {
        Self {
            side_of: HashMap::new(),
            side_with: HashMap::new(),
            next: u64::MAX,
        }
    }

    fn of(&self, ino: Ino) -> Option<Ino> {
        self.side_of.get(&ino).copied()
    }

    /// Records `id` as the id of the sidecar of `ino`.
    fn put(&mut self, ino: Ino, id: Ino) -> Ino {
        debug_assert_ne!(id, 0, "a file id is never 0");
        self.side_of.insert(ino, id);
        self.side_with.insert(id, ino);
        if let Some(lower) = id.checked_sub(1) {
            self.next = self.next.min(lower);
        }
        id
    }

    /// Takes back the id of an inode's sidecar, which the `Vfs` wants for a real inode. The sidecar
    /// is left without an id until it is given another.
    fn take(&mut self, ino: Ino) -> Option<Ino> {
        let id = self.side_with.remove(&ino)?;
        self.side_of.remove(&id);
        Some(id)
    }

    fn forget(&mut self, ino: Ino) {
        if let Some(id) = self.side_of.remove(&ino) {
            self.side_with.remove(&id);
        }
    }
}

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
fn not_side(dir: Id) -> NfsResult<()> {
    if dir.is_side() {
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

/// One page of a listing: the entries, with their attributes when the request asked for them.
struct Page {
    entries: Vec<(DirEntry, Option<Attr>)>,
    eof: bool,
}

/// The synchronous NFS to `Vfs` translation.
pub struct Adapter {
    pub(crate) vfs: Arc<dyn Vfs>,
    pub(crate) opts: AdapterOptions,
    handles: HandleCodec,
    ids: Mutex<SideIds>,
    parents: Mutex<HashMap<Ino, Ino>>,
    pub(crate) sidecars: Mutex<SidecarBuffers>,
    pub(crate) sidecar_locks: Mutex<PerIno>,
    /// One lock per directory, so a call that reads a name and then changes it is one step against
    /// that directory. Without it the sidecar guard could pass on a name that had just become a
    /// live view, and a real directory or hard link could take the name.
    names: Mutex<PerIno>,
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
            ids: Mutex::new(SideIds::new()),
            parents: Mutex::new(HashMap::new()),
            sidecars: Mutex::new(SidecarBuffers::default()),
            sidecar_locks: Mutex::new(PerIno::default()),
            names: Mutex::new(PerIno::default()),
        })
    }

    pub fn generation(&self) -> u64 {
        self.handles.generation()
    }

    /// What a file id names.
    fn ident(&self, id: fileid3) -> Id {
        match lock(&self.ids).side_with.get(&id) {
            Some(ino) => Id::side(*ino),
            None => Id::plain(id),
        }
    }

    /// The file id of an inode or of the sidecar of one.
    fn id_of(&self, id: Id) -> fileid3 {
        match id.kind {
            Kind::Plain => self.plain_id(id.ino),
            Kind::Sidecar => self.side_id(id.ino),
        }
    }

    /// The file id of an inode: its own number. A sidecar that has it is given another first, so
    /// the number the client holds for an inode never names a sidecar.
    fn plain_id(&self, ino: Ino) -> Ino {
        let squatter = lock(&self.ids).take(ino);
        if let Some(moved) = squatter {
            self.side_id(moved);
        }
        ino
    }

    /// The file id of the sidecar of `ino`, kept for as long as the inode lives.
    fn side_id(&self, ino: Ino) -> Ino {
        if let Some(id) = lock(&self.ids).of(ino) {
            return id;
        }
        let id = self.free_id();
        let mut ids = lock(&self.ids);
        let id = ids.of(ino).unwrap_or(id);
        ids.put(ino, id)
    }

    /// A file id no inode of the `Vfs` is using. The `Vfs` may use any `u64`, so the ids start at
    /// the top of the space and step down over whatever is in use. A file system that numbers its
    /// inodes from the top could fill more than this many; then the last number tried is used and
    /// `plain_id` moves the sidecar off it as soon as the `Vfs` reports that inode.
    fn free_id(&self) -> Ino {
        let mut candidate = lock(&self.ids).next;
        for _ in 0..ID_PROBES {
            if self.vfs.getattr(candidate).is_err() {
                break;
            }
            candidate = candidate.wrapping_sub(1);
        }
        candidate
    }

    /// The file id of the mount root.
    pub fn root_id(&self) -> fileid3 {
        self.id_of(Id::plain(ROOT_INO))
    }

    /// The file handle for a file id.
    pub fn handle(&self, id: fileid3) -> Vec<u8> {
        let i = self.ident(id);
        self.handles.encode(i.ino, i.kind)
    }

    /// The file id a file handle names, or the status that refuses it.
    pub fn resolve(&self, handle: &[u8]) -> NfsResult<fileid3> {
        let (ino, kind) = self.handles.decode(handle)?;
        Ok(self.id_of(Id { ino, kind }))
    }

    pub(crate) fn fa(&self, a: &Attr, kind: Kind) -> NfsResult<fattr3> {
        let mut f = fattr(a)?;
        f.fileid = self.id_of(Id { ino: a.ino, kind });
        if let Some((uid, gid)) = self.opts.owner {
            f.uid = uid;
            f.gid = gid;
        }
        Ok(f)
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
        lock(&self.ids).forget(ino);
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
        let i = self.ident(id);
        if i.is_side() {
            return self.side_getattr(i.ino);
        }
        self.vfs
            .getattr(i.ino)
            .map_err(stat)
            .and_then(|a| self.fa(&a, Kind::Plain))
    }

    pub fn lookup(&self, dir: fileid3, name: &[u8]) -> NfsResult<(fileid3, fattr3)> {
        let d = self.ident(dir);
        not_side(d)?;
        if self.translating(d.ino, name) {
            let (id, attr) = self.side_lookup(d.ino, name)?;
            return Ok((self.id_of(id), attr));
        }
        match name {
            b"." => {
                let a = self.vfs.getattr(d.ino).map_err(stat)?;
                if a.kind != FileKind::Directory {
                    return Err(nfsstat3::NFS3ERR_NOTDIR);
                }
                Ok((self.id_of(Id::plain(a.ino)), self.fa(&a, Kind::Plain)?))
            }
            b".." => {
                let p = self.parent_of(d.ino)?;
                Ok((p, self.getattr(p)?))
            }
            _ => {
                check_name(name)?;
                let a = self.vfs.lookup(d.ino, name).map_err(stat)?;
                self.handed_out(Some(d.ino), &a);
                Ok((self.id_of(Id::plain(a.ino)), self.fa(&a, Kind::Plain)?))
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

    /// Barriers `dir`, and on failure answers with the barrier's status rather than the caller's.
    ///
    /// The status that goes out has to mean one thing only: every change this RPC made is durable.
    /// When both the change's own step and the barrier failed, the barrier's `NFS3ERR_IO` is the
    /// honest one, because the change's status would read as "that did not happen" and the caller
    /// would have no way to learn that it did.
    ///
    /// The barrier is unconditional, and deliberately so. It is never "nothing to do because this
    /// RPC changed nothing": it commits whatever the snapshot already had queued, so a refusal that
    /// arrives on top of earlier uncommitted writes still discharges them. Callers reach this only
    /// once they have passed prevalidation and may already have mutated, so the cost is one metadata
    /// sync on a refusal rather than a name left unbarriered.
    fn durable_or<T>(&self, dir: Ino, made: &NfsResult<T>) -> NfsResult<()> {
        match (self.durable(dir), made) {
            (Ok(()), Ok(_)) => Ok(()),
            (Err(e), _) => Err(e),
            (Ok(()), Err(e)) => Err(*e),
        }
    }

    pub fn setattr(&self, id: fileid3, s: &sattr3) -> NfsResult<fattr3> {
        let i = self.ident(id);
        if i.is_side() {
            return self.side_setattr(i.ino, s);
        }
        let out = self
            .apply(i.ino, set_attr(s))
            .and_then(|a| self.fa(&a, Kind::Plain));
        // Unconditional, and that is the point: the barrier commits whatever this snapshot already
        // had queued, so a refused `setattr` still discharges earlier uncommitted writes. It costs
        // one metadata sync on a refusal. A failed barrier outranks the refusal's own status.
        self.durable_or(i.ino, &out)?;
        out
    }

    pub fn readlink(&self, id: fileid3) -> NfsResult<Vec<u8>> {
        let i = self.ident(id);
        if i.is_side() {
            return Err(nfsstat3::NFS3ERR_INVAL);
        }
        self.vfs.readlink(i.ino).map_err(stat)
    }

    pub fn read(&self, id: fileid3, offset: u64, count: count3) -> NfsResult<(Vec<u8>, bool)> {
        let i = self.ident(id);
        if i.is_side() {
            return self.side_read(i.ino, offset, count);
        }
        let data = self.vfs.read(i.ino, offset, count).map_err(stat)?;
        let len = data.len() as u64;
        let eof = if len < u64::from(count) {
            true
        } else {
            let size = self.vfs.getattr(i.ino).map_err(stat)?.size;
            offset.saturating_add(len) >= size
        };
        Ok((data, eof))
    }

    pub fn write(&self, id: fileid3, offset: u64, data: &[u8]) -> NfsResult<(u32, fattr3)> {
        let i = self.ident(id);
        if i.is_side() {
            return self.side_write(i.ino, offset, data);
        }
        let n = self.vfs.write(i.ino, offset, data).map_err(stat)?;
        Ok((n, self.getattr(id)?))
    }

    pub fn commit(&self, id: fileid3) -> NfsResult<()> {
        let i = self.ident(id);
        if i.is_side() {
            return self.side_getattr(i.ino).map(drop);
        }
        self.vfs.fsync(i.ino, false).map_err(stat)
    }

    /// Makes a name or attribute change durable before it is acknowledged.
    ///
    /// macOS sends no COMMIT for a directory `fsync`, nor for an `fsync` of a descriptor with no
    /// dirty pages, so a caller that renamed and then synced the parent directory, the whole
    /// POSIX dance, was told `NFS3_OK` about a name that was still only in memory and was back at
    /// its old name after the daemon died. The reply is the one chance to fix that, and it costs
    /// file data nothing: a dirty file stays unstable until its own COMMIT.
    fn durable(&self, ino: Ino) -> NfsResult<()> {
        self.vfs.sync_namespace(ino).map_err(stat)
    }

    pub fn create(
        &self,
        dir: fileid3,
        name: &[u8],
        attr: &sattr3,
        guarded: bool,
    ) -> NfsResult<(fileid3, fattr3)> {
        let d = self.ident(dir);
        not_side(d)?;
        self.with_names(d.ino, || {
            if self.side_of(d.ino, name) {
                let (id, attr) = self.side_create(d.ino, name, attr, guarded)?;
                self.durable(d.ino)?;
                return Ok((self.id_of(id), attr));
            }
            new_name(name)?;
            let mode = match attr.mode {
                set_mode3::mode(m) => m,
                set_mode3::Void => 0o644,
            };
            let mut changes = set_attr(attr);
            // `Vfs::create` has already made the name by the time any of this runs, so the arm must not
            // return with `?`: the barrier is owed whatever the attribute step does.
            let made = match self.vfs.create(d.ino, name, mode) {
                Ok(a) => {
                    self.handed_out(Some(d.ino), &a);
                    changes.mode = None;
                    if changes.size == Some(0) {
                        changes.size = None;
                    }
                    match self.apply(a.ino, changes) {
                        Ok(a) => self
                            .fa(&a, Kind::Plain)
                            .map(|fa| (self.id_of(Id::plain(a.ino)), fa)),
                        Err(e) => Err(e),
                    }
                }
                Err(Error::Exists) if !guarded => {
                    let existing = match self.vfs.lookup(d.ino, name) {
                        Ok(a) => a,
                        Err(e) => return Err(stat(e)),
                    };
                    if existing.kind == FileKind::Directory {
                        self.vfs.forget(existing.ino, 1);
                        return Err(nfsstat3::NFS3ERR_ISDIR);
                    }
                    self.handed_out(Some(d.ino), &existing);
                    match self.apply(existing.ino, changes) {
                        Ok(a) => self
                            .fa(&a, Kind::Plain)
                            .map(|fa| (self.id_of(Id::plain(a.ino)), fa)),
                        Err(e) => Err(e),
                    }
                }
                // Nothing was created, so nothing is owed and no barrier runs.
                Err(e) => return Err(stat(e)),
            };
            self.durable_or(d.ino, &made)?;
            made
        })
    }

    pub fn create_exclusive(
        &self,
        dir: fileid3,
        name: &[u8],
        verf: createverf3,
    ) -> NfsResult<(fileid3, fattr3)> {
        let d = self.ident(dir);
        not_side(d)?;
        self.with_names(d.ino, || {
            if self.side_of(d.ino, name) {
                let (id, attr) = self.side_create_exclusive(d.ino, name)?;
                self.durable(d.ino)?;
                return Ok((self.id_of(id), attr));
            }
            new_name(name)?;
            let (atime, mtime) = verifier_times(verf);
            // The name exists once `Vfs::create` has returned, so the arm yields a value and the barrier
            // runs whatever the `setattr` and attribute conversion do. The `Exists` arm below created
            // nothing, so it owes no barrier.
            let made = match self.vfs.create(d.ino, name, 0o600) {
                Ok(a) => {
                    self.handed_out(Some(d.ino), &a);
                    let changes = SetAttr {
                        atime: Some(SetTime::At(atime)),
                        mtime: Some(SetTime::At(mtime)),
                        ..SetAttr::default()
                    };
                    match self.vfs.setattr(a.ino, changes) {
                        Ok(a) => match self.fa(&a, Kind::Plain) {
                            Ok(fa) => Ok((self.id_of(Id::plain(a.ino)), fa)),
                            Err(e) => Err(e),
                        },
                        Err(e) => Err(stat(e)),
                    }
                }
                Err(Error::Exists) => {
                    let a = match self.vfs.lookup(d.ino, name) {
                        Ok(a) => a,
                        Err(e) => return Err(stat(e)),
                    };
                    if a.kind == FileKind::Regular && a.atime == atime && a.mtime == mtime {
                        self.handed_out(Some(d.ino), &a);
                        match self.fa(&a, Kind::Plain) {
                            Ok(fa) => Ok((self.id_of(Id::plain(a.ino)), fa)),
                            Err(e) => Err(e),
                        }
                    } else {
                        self.vfs.forget(a.ino, 1);
                        return Err(nfsstat3::NFS3ERR_EXIST);
                    }
                }
                Err(e) => return Err(stat(e)),
            };
            self.durable_or(d.ino, &made)?;
            made
        })
    }

    /// A name that is a live sidecar view belongs to another file's extended attributes, so no
    /// real directory, symlink or hard link may take it: a real object under that name would
    /// shadow the view for the rest of the mount, and a hard link would hand the view the bytes
    /// of the file it names. `ACCES` is what [`Adapter::rename`] already answers when a real
    /// file is moved onto a view.
    fn not_a_view(&self, dir: Ino, name: &[u8]) -> NfsResult<()> {
        if self.side_of(dir, name) {
            Err(nfsstat3::NFS3ERR_ACCES)
        } else {
            Ok(())
        }
    }

    /// Runs `f` with `dir`'s name space locked, so a read of a name and the change that follows it
    /// are one step. The lock is per directory, so writers of different directories do not wait
    /// for each other, and it is a plain mutex because the work under it is synchronous `Vfs`
    /// calls that never re-enter the adapter.
    fn with_names<T>(&self, dir: Ino, f: impl FnOnce() -> NfsResult<T>) -> NfsResult<T> {
        let l = lock(&self.names).of(dir);
        let g = lock(&l);
        let out = f();
        drop(g);
        out
    }

    /// The same, for a call that changes two directories at once. Both locks are taken in a fixed
    /// order, so two renames cannot take them in opposite orders and stop each other forever.
    fn with_two_names<T>(&self, a: Ino, b: Ino, f: impl FnOnce() -> NfsResult<T>) -> NfsResult<T> {
        if a == b {
            return self.with_names(a, f);
        }
        let (first, second) = if a < b { (a, b) } else { (b, a) };
        let (l1, l2) = {
            let mut m = lock(&self.names);
            (m.of(first), m.of(second))
        };
        let g1 = lock(&l1);
        let g2 = lock(&l2);
        let out = f();
        drop(g2);
        drop(g1);
        out
    }

    pub fn mkdir(&self, dir: fileid3, name: &[u8], attr: &sattr3) -> NfsResult<(fileid3, fattr3)> {
        let d = self.ident(dir);
        not_side(d)?;
        new_name(name)?;
        self.with_names(d.ino, || {
            self.not_a_view(d.ino, name)?;
            let mode = match attr.mode {
                set_mode3::mode(m) => m,
                set_mode3::Void => 0o755,
            };
            let a = self.vfs.mkdir(d.ino, name, mode).map_err(stat)?;
            self.handed_out(Some(d.ino), &a);
            self.durable(d.ino)?;
            Ok((self.id_of(Id::plain(a.ino)), self.fa(&a, Kind::Plain)?))
        })
    }

    pub fn symlink(
        &self,
        dir: fileid3,
        name: &[u8],
        target: &[u8],
    ) -> NfsResult<(fileid3, fattr3)> {
        let d = self.ident(dir);
        not_side(d)?;
        new_name(name)?;
        if target.is_empty() || target.contains(&0) {
            return Err(nfsstat3::NFS3ERR_INVAL);
        }
        if target.len() > SYMLINK_TARGET_MAX {
            return Err(nfsstat3::NFS3ERR_NAMETOOLONG);
        }
        self.with_names(d.ino, || {
            self.not_a_view(d.ino, name)?;
            let a = self.vfs.symlink(d.ino, name, target).map_err(stat)?;
            self.handed_out(Some(d.ino), &a);
            self.durable(d.ino)?;
            Ok((self.id_of(Id::plain(a.ino)), self.fa(&a, Kind::Plain)?))
        })
    }

    pub fn link(&self, file: fileid3, dir: fileid3, name: &[u8]) -> NfsResult<fattr3> {
        let d = self.ident(dir);
        not_side(d)?;
        let f = self.ident(file);
        if f.is_side() {
            return Err(nfsstat3::NFS3ERR_ACCES);
        }
        new_name(name)?;
        self.with_names(d.ino, || {
            self.not_a_view(d.ino, name)?;
            let a = self.vfs.link(f.ino, d.ino, name).map_err(stat)?;
            self.handed_out(None, &a);
            self.durable(d.ino)?;
            self.fa(&a, Kind::Plain)
        })
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
        let d = self.ident(dir);
        not_side(d)?;
        check_name(name)?;
        self.with_names(d.ino, || {
            if self.translating(d.ino, name) {
                return self.side_remove(d.ino, name);
            }
            self.remove_one(d.ino, name)?;
            if let (true, Some(side)) = (
                self.opts.appledouble == AppleDoubleMode::Hide,
                Self::sidecar(name),
            ) {
                let _ = self.remove_one(d.ino, &side);
            }
            self.durable(d.ino)?;
            Ok(())
        })
    }

    /// Removes every entry of `dir` if all of them are AppleDouble sidecars.
    ///
    /// Each removal is a real name change, so a failure part way through has already mutated and the
    /// caller owes the barrier. That is why this returns a `Result` rather than acting through `?`
    /// into an early return that would skip it.
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
        // Stop at the first failure and report it. Some names may already be gone, which the
        // caller's barrier covers; this function does not pretend the directory is untouched.
        for name in names {
            self.remove_one(dir, &name)?;
        }
        Ok(())
    }

    pub fn rmdir(&self, dir: fileid3, name: &[u8]) -> NfsResult<()> {
        let d = self.ident(dir);
        not_side(d)?;
        // POSIX rmdir (pjdfstest rmdir/12.t): "." is EINVAL, but ".." names a directory that is never empty.
        if name == b".." {
            return Err(nfsstat3::NFS3ERR_NOTEMPTY);
        }
        check_name(name)?;
        self.with_names(d.ino, || {
            if self.translating(d.ino, name) {
                return Err(nfsstat3::NFS3ERR_NOTDIR);
            }
            let target = self.peek(d.ino, name).map_err(stat)?;
            // The purge removes real names, so every exit from here owes the barrier, including the ones
            // that answer with an error. The arm therefore yields a value instead of propagating with
            // `?`: a `?` after `purge_sidecars` returned before the barrier while sidecar names were
            // already removed, which is the same defect the `create` arms had.
            let made: NfsResult<()> = match self.vfs.rmdir(d.ino, name) {
                Err(Error::NotEmpty) if self.opts.appledouble == AppleDoubleMode::Hide => {
                    match self.purge_sidecars(target.ino) {
                        Ok(()) => self.vfs.rmdir(d.ino, name).map_err(stat),
                        Err(e) => Err(e),
                    }
                }
                r => r.map_err(stat),
            };
            self.durable_or(d.ino, &made)?;
            if made.is_ok() {
                self.reap_if_last(target.ino);
            }
            made
        })
    }

    pub fn rename(
        &self,
        from_dir: fileid3,
        from: &[u8],
        to_dir: fileid3,
        to: &[u8],
    ) -> NfsResult<()> {
        let (fd, td) = (self.ident(from_dir), self.ident(to_dir));
        not_side(fd)?;
        not_side(td)?;
        check_name(from)?;
        check_name(to)?;
        self.with_two_names(fd.ino, td.ino, || {
            match (self.side_of(fd.ino, from), self.side_of(td.ino, to)) {
                // The attributes live on the inode and moved with it, so there is nothing to do.
                (true, _) => return Ok(()),
                // A real file cannot be moved onto the view of another file.
                (false, true) if !self.translating(fd.ino, from) => {
                    return Err(nfsstat3::NFS3ERR_ACCES);
                }
                // A plain name moved onto a view: that view is the file's own attributes already.
                (false, true) => return Ok(()),
                // Both are ordinary names, including a `._name` with no main file to hold attributes.
                (false, false) => {}
            }
            let src = self.peek(fd.ino, from).map_err(stat)?;
            let replaced = self.peek(td.ino, to).ok();
            self.vfs
                .rename(fd.ino, from, td.ino, to, RenameFlags::default())
                .map_err(stat)?;
            if src.kind == FileKind::Directory {
                lock(&self.parents).insert(src.ino, td.ino);
            }
            if let Some(d) = replaced {
                if d.ino != src.ino {
                    self.reap_if_last(d.ino);
                }
            }
            if self.opts.appledouble == AppleDoubleMode::Hide {
                if let (Some(from_side), Some(to_side)) = (Self::sidecar(from), Self::sidecar(to)) {
                    let replaced_side = self.peek(td.ino, &to_side).ok();
                    let moved = self.vfs.rename(
                        fd.ino,
                        &from_side,
                        td.ino,
                        &to_side,
                        RenameFlags::default(),
                    );
                    if moved.is_err() {
                        let _ = self.remove_one(td.ino, &to_side);
                    } else if let Some(d) = replaced_side {
                        if d.nlink <= 1 {
                            self.reap(d.ino);
                        }
                    }
                }
            }
            self.durable(fd.ino)?;
            Ok(())
        })
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
        let d = self.ident(dir);
        not_side(d)?;
        let dir = d.ino;
        // The Vfs rejects a max of 0, and the kernel never asks for nothing, so clamp rather
        // than turn a zero into an error the client cannot act on.
        let max = max.max(1);
        let mut out = ReadDirResult::default();
        let mut cookie = cookie;
        loop {
            let want = max - out.entries.len();
            // One call for the attributes too, so a Vfs that can list them cheaply does.
            let page: Page = if with_attrs {
                let p = self.vfs.readdir_attrs(dir, cookie, want).map_err(stat)?;
                Page {
                    eof: p.eof,
                    entries: p
                        .entries
                        .into_iter()
                        .map(|e| (e.entry, Some(e.attr)))
                        .collect(),
                }
            } else {
                let p = self.vfs.readdir(dir, cookie, want).map_err(stat)?;
                Page {
                    eof: p.eof,
                    entries: p.entries.into_iter().map(|e| (e, None)).collect(),
                }
            };
            let mut consumed_all = true;
            for (e, attr) in &page.entries {
                if out.entries.len() == max {
                    consumed_all = false;
                    break;
                }
                cookie = e.cookie;
                if self.opts.appledouble == AppleDoubleMode::Hide && is_appledouble(&e.name) {
                    continue;
                }
                if let Some(entry) = self.list_entry(dir, e, attr.as_ref(), with_attrs) {
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

    fn list_entry(
        &self,
        dir: Ino,
        e: &DirEntry,
        listed: Option<&Attr>,
        with_attrs: bool,
    ) -> Option<NfsDirEntry> {
        let attr = if with_attrs {
            let a = listed?;
            if a.kind == FileKind::Directory {
                lock(&self.parents).insert(a.ino, dir);
            }
            Some(self.fa(a, Kind::Plain).ok()?)
        } else {
            None
        };
        Some(NfsDirEntry {
            fileid: self.id_of(Id::plain(e.ino)),
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
        self.0.root_id()
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

    fn adapter(mode: AppleDoubleMode) -> Adapter {
        Adapter::new(
            Arc::new(MemVfs::new()),
            AdapterOptions {
                appledouble: mode,
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

    /// A sidecar is not an inode of the `Vfs`, so it gets a file id of the adapter's own. The id is
    /// stable for as long as the inode lives, and it is never a number an inode of the `Vfs` is using.
    #[test]
    fn a_sidecar_id_is_the_adapters_own_and_never_an_inode() {
        let a = adapter(AppleDoubleMode::Translate);
        let (f, _) = a.create(ROOT_INO, b"f", &sattr3::default(), true).unwrap();
        let (s, _) = a
            .create(ROOT_INO, b"._f", &sattr3::default(), true)
            .unwrap();
        let side = a.getattr(s).unwrap();
        assert_eq!(
            side.fileid, s,
            "the attributes of a sidecar report its own id"
        );
        assert_ne!(side.fileid, f, "which is not the inode it belongs to");
        assert_eq!(
            a.vfs.getattr(side.fileid).err(),
            Some(Error::Stale),
            "and not an inode"
        );
        assert_eq!(
            a.getattr(side.fileid).unwrap().fileid,
            side.fileid,
            "and it stays put"
        );
        assert_eq!(
            a.getattr(f).unwrap().fileid,
            f,
            "the file keeps its own inode as its id"
        );

        // A directory has a sidecar too, and it is not the directory.
        let (d, _) = a.mkdir(ROOT_INO, b"d", &sattr3::default()).unwrap();
        let (sd, _) = a
            .create(ROOT_INO, b"._d", &sattr3::default(), true)
            .unwrap();
        let fattr = a.getattr(sd).unwrap();
        assert_ne!(fattr.fileid, d);
        assert_eq!(a.vfs.getattr(fattr.fileid).err(), Some(Error::Stale));
        assert_eq!(a.getattr(fattr.fileid).unwrap().size, 0, "an empty sidecar");
    }

    /// Every number a `Vfs` may use is an ordinary inode number, and the ids a sidecar takes come from
    /// the top of the `u64` space, which no inode of a file system that numbers its inodes upwards is
    /// anywhere near.
    #[test]
    fn every_inode_number_is_an_ordinary_inode() {
        let a = adapter(AppleDoubleMode::Translate);
        let (f, _) = a.create(ROOT_INO, b"f", &sattr3::default(), true).unwrap();
        let (s, _) = a
            .create(ROOT_INO, b"._f", &sattr3::default(), true)
            .unwrap();
        assert!(
            a.getattr(s).unwrap().fileid > f,
            "the sidecar id is from the top"
        );
        for ino in [
            1 << 63,
            (1 << 63) | 1,
            0x8000_0100_0000_0001,
            u64::MAX - 4096,
        ] {
            assert_eq!(
                a.getattr(ino).err(),
                Some(nfsstat3::NFS3ERR_STALE),
                "{ino:#x}"
            );
            assert_eq!(
                a.resolve(&a.handle(ino)),
                Ok(ino),
                "{ino:#x} is an ordinary inode"
            );
        }
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
        let a = adapter(AppleDoubleMode::Hide);
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
        let a = adapter(AppleDoubleMode::Hide);
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
        let all = adapter(AppleDoubleMode::Store);
        all.create(ROOT_INO, b"._1", &sattr3::default(), true)
            .unwrap();
        assert_eq!(
            all.readdir(ROOT_INO, 0, 10, false).unwrap().entries.len(),
            1
        );
    }

    #[test]
    fn a_zero_entry_budget_is_clamped_not_passed_on() {
        // The trait says the Vfs must reject a max of 0, so the adapter never sends one.
        let a = adapter(AppleDoubleMode::Store);
        a.create(ROOT_INO, b"f", &sattr3::default(), true).unwrap();
        let got = a.readdir(ROOT_INO, 0, 0, false).unwrap();
        assert_eq!((got.entries.len(), got.end), (1, true));
    }

    #[test]
    fn readdir_of_a_missing_or_plain_file_fails() {
        let a = adapter(AppleDoubleMode::Hide);
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
        let a = adapter(AppleDoubleMode::Hide);
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
        let f = adapter(AppleDoubleMode::Hide).fsstat().unwrap();
        assert!(f.tbytes >= f.fbytes && f.fbytes >= f.abytes.min(f.fbytes));
        assert!(f.tfiles > 0);
    }
}
