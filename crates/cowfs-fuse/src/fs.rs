//! The `fuser::Filesystem` implementation: one `Vfs` call per kernel request, cheap metadata on
//! the request loop thread, slow operations on keyed worker lanes.

use std::any::Any;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use cowfs_vfs::{
    validate_name, Attr, FileHandle, FileKind, Ino, SetAttr, SetTime, Vfs, MODE_MASK, ROOT_INO,
};
use fuser::consts::{FOPEN_KEEP_CACHE, FUSE_PARALLEL_DIROPS};
use fuser::{
    FileAttr, FileType, Filesystem, KernelConfig, Notifier, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyLseek, ReplyOpen, ReplyStatfs, ReplyWrite,
    ReplyXattr, Request, TimeOrNow,
};
use libc::c_int;

use crate::convert::{self, XattrReply};
use crate::cost::{append_needs_lane, lane_index, Class, Cost};
use crate::dir::{self, DirSink};
use crate::options::MountOptions;
use crate::table::Table;

type R<T> = Result<T, c_int>;
type Job = Box<dyn FnOnce() + Send>;

const BLKSIZE: u32 = 4096;

/// State shared between the request loop, the worker lanes, the `Mount` and its `Invalidator`s.
pub(crate) struct Shared {
    table: Mutex<Table>,
    pub(crate) notifier: OnceLock<Notifier>,
    pub(crate) epoch: AtomicU64,
    panics: AtomicU32,
    failed: AtomicBool,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("failed", &self.failed())
            .finish_non_exhaustive()
    }
}

impl Shared {
    pub(crate) fn new() -> Self {
        Self {
            table: Mutex::new(Table::default()),
            notifier: OnceLock::new(),
            epoch: AtomicU64::new(0),
            panics: AtomicU32::new(0),
            failed: AtomicBool::new(false),
        }
    }

    pub(crate) fn table(&self) -> MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// True once `Vfs` panics reached `max_panics`: every request then fails with `ENOTCONN`.
    pub(crate) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

/// The effective uid and gid of this process, which own every file on the mount.
#[allow(unsafe_code)]
pub(crate) fn mounter() -> (u32, u32) {
    // SAFETY: geteuid and getegid take no arguments, touch no memory and cannot fail.
    unsafe { (libc::geteuid(), libc::getegid()) }
}

struct Core {
    vfs: Arc<dyn Vfs>,
    opts: MountOptions,
    uid: u32,
    gid: u32,
    sh: Arc<Shared>,
    cost: Cost,
}

fn name(n: &OsStr) -> R<&[u8]> {
    validate_name(n.as_bytes()).map_err(|e| e.errno())?;
    Ok(n.as_bytes())
}

fn offset(o: i64) -> R<u64> {
    u64::try_from(o).map_err(|_| libc::EINVAL)
}

fn file_type(kind: FileKind) -> FileType {
    match kind {
        FileKind::Regular => FileType::RegularFile,
        FileKind::Directory => FileType::Directory,
        FileKind::Symlink => FileType::Symlink,
    }
}

fn file_attr(a: &Attr, uid: u32, gid: u32) -> FileAttr {
    FileAttr {
        ino: a.ino,
        size: a.size,
        blocks: a.blocks,
        atime: convert::to_system_time(a.atime),
        mtime: convert::to_system_time(a.mtime),
        ctime: convert::to_system_time(a.ctime),
        crtime: UNIX_EPOCH,
        kind: file_type(a.kind),
        perm: u16::try_from(a.mode & MODE_MASK).unwrap_or(0),
        nlink: a.nlink,
        uid,
        gid,
        rdev: 0,
        blksize: BLKSIZE,
        flags: 0,
    }
}

fn negative_attr() -> FileAttr {
    FileAttr {
        ino: 0,
        size: 0,
        blocks: 0,
        atime: UNIX_EPOCH,
        mtime: UNIX_EPOCH,
        ctime: UNIX_EPOCH,
        crtime: UNIX_EPOCH,
        kind: FileType::RegularFile,
        perm: 0,
        nlink: 0,
        uid: 0,
        gid: 0,
        rdev: 0,
        blksize: 0,
        flags: 0,
    }
}

fn set_time(t: TimeOrNow) -> SetTime {
    match t {
        TimeOrNow::Now => SetTime::Now,
        TimeOrNow::SpecificTime(t) => SetTime::At(convert::from_system_time(t)),
    }
}

fn empty(r: R<()>, reply: ReplyEmpty) {
    match r {
        Ok(()) => reply.ok(),
        Err(e) => reply.error(e),
    }
}

fn xattr_reply(size: u32, value: &[u8], reply: ReplyXattr) {
    match convert::xattr_reply(size, value.len()) {
        XattrReply::Size(n) => reply.size(n),
        XattrReply::Data => reply.data(value),
        XattrReply::TooSmall => reply.error(libc::ERANGE),
    }
}

struct Sink(ReplyDirectory);

impl DirSink for Sink {
    fn add(&mut self, ino: Ino, offset: i64, kind: FileKind, n: &[u8]) -> bool {
        self.0
            .add(ino, offset, file_type(kind), OsStr::from_bytes(n))
    }
}

impl Core {
    /// Runs one `Vfs` call. A panic in it becomes `EIO` for this request and is counted; after
    /// `max_panics` the mount is failed and everything answers `ENOTCONN`, so the mountpoint
    /// never turns into an empty directory that looks valid.
    fn call<T>(&self, f: impl FnOnce(&dyn Vfs) -> cowfs_vfs::Result<T>) -> R<T> {
        if self.sh.failed() {
            return Err(libc::ENOTCONN);
        }
        match catch_unwind(AssertUnwindSafe(|| f(&*self.vfs))) {
            Ok(r) => r.map_err(|e| e.errno()),
            Err(p) => {
                self.panicked(&p);
                Err(libc::EIO)
            }
        }
    }

    fn panicked(&self, p: &(dyn Any + Send)) {
        let msg = p
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let n = self.sh.panics.fetch_add(1, Ordering::AcqRel) + 1;
        if n >= self.opts.max_panics {
            self.sh.failed.store(true, Ordering::Release);
            log::error!(
                "Vfs panicked ({n} of {}): {msg}; mount failed, answering ENOTCONN",
                self.opts.max_panics
            );
        } else {
            log::error!(
                "Vfs panicked ({n} of {}): {msg}; request answered EIO",
                self.opts.max_panics
            );
        }
    }

    fn fattr(&self, a: &Attr, unlinked_ok: bool) -> FileAttr {
        let (s, changed) = convert::sanitize(a, unlinked_ok);
        if changed {
            log::warn!(
                "Vfs returned impossible attributes for inode {}: {a:?}; clamped",
                a.ino
            );
        }
        file_attr(&s, self.uid, self.gid)
    }

    /// Records the reference the kernel gets with a reply that hands out `a`, and converts it.
    fn referenced(&self, parent: Ino, n: &[u8], a: &Attr, fresh: bool) -> R<FileAttr> {
        let r = self
            .sh
            .table()
            .reference(parent, n, a.ino, a.kind, fresh, self.opts.paranoid_ino);
        if r.is_err() {
            log::error!(
                "Vfs reused inode {} while the kernel still references it",
                a.ino
            );
            let _ = self.call(|v| {
                v.forget(a.ino, 1);
                Ok(())
            });
            return Err(libc::EIO);
        }
        Ok(self.fattr(a, false))
    }

    fn lookup(&self, parent: Ino, n: &OsStr) -> R<FileAttr> {
        let n = name(n)?;
        let a = self.call(|v| v.lookup(parent, n))?;
        self.referenced(parent, n, &a, false)
    }

    fn getattr(&self, ino: Ino, open: bool) -> R<FileAttr> {
        let a = self.call(|v| v.getattr(ino))?;
        let unlinked = open || self.sh.table().is_unnamed(ino);
        Ok(self.fattr(&a, unlinked))
    }

    fn setattr(&self, ino: Ino, changes: SetAttr) -> R<FileAttr> {
        let a = self.call(|v| v.setattr(ino, changes))?;
        let unlinked = self.sh.table().is_unnamed(ino);
        Ok(self.fattr(&a, unlinked))
    }

    fn forget(&self, ino: Ino, count: u64) {
        if ino == ROOT_INO {
            return;
        }
        self.sh.table().forget(ino, count);
        let _ = self.call(|v| {
            v.forget(ino, count);
            Ok(())
        });
    }

    fn open_flags(&self) -> u32 {
        if self.opts.keep_cache() {
            FOPEN_KEEP_CACHE
        } else {
            0
        }
    }

    fn create(&self, parent: Ino, n: &OsStr, mode: u32) -> R<(FileAttr, u64)> {
        let n = name(n)?;
        let a = self.call(|v| v.create(parent, n, mode & MODE_MASK))?;
        let attr = self.referenced(parent, n, &a, true)?;
        match self.call(|v| v.open(a.ino)) {
            Ok(h) => Ok((attr, h.0)),
            Err(e) => {
                self.forget(a.ino, 1);
                Err(e)
            }
        }
    }

    /// Writes at `off`, or at the current end for an append. The kernel computes append offsets
    /// from its cached size, which is stale when the tree changed behind the mount, so the size
    /// comes from the `Vfs`. The second result is true when the kernel's size was wrong and its
    /// attributes must be refreshed.
    fn write(&self, ino: Ino, off: i64, data: &[u8], flags: i32) -> R<(u32, bool)> {
        let mut pos = offset(off)?;
        let mut stale = false;
        if flags & libc::O_APPEND != 0 {
            let size = self.call(|v| v.getattr(ino))?.size;
            stale = size != pos;
            pos = size;
        }
        let n = self.call(|v| v.write(ino, pos, data))?;
        let max = u32::try_from(data.len()).unwrap_or(u32::MAX);
        Ok((n.min(max), stale))
    }

    fn invalidate_attr(&self, ino: Ino) {
        if let Some(n) = self.sh.notifier.get() {
            if let Err(e) = n.inval_inode(ino, -1, 0) {
                log::warn!("attribute invalidation of inode {ino} failed: {e}");
            }
        }
    }
}

pub(crate) struct Fs {
    core: Arc<Core>,
    lanes: Vec<mpsc::Sender<Job>>,
}

impl Fs {
    pub(crate) fn new(vfs: Arc<dyn Vfs>, opts: MountOptions, sh: Arc<Shared>) -> Self {
        let (uid, gid) = mounter();
        let lanes = (0..opts.workers)
            .filter_map(|i| {
                let (tx, rx) = mpsc::channel::<Job>();
                std::thread::Builder::new()
                    .name(format!("cowfs-fuse-w{i}"))
                    .spawn(move || {
                        for job in rx {
                            let _ = catch_unwind(AssertUnwindSafe(job));
                        }
                    })
                    .map_err(|e| log::error!("cannot start worker {i}: {e}"))
                    .ok()
                    .map(|_| tx)
            })
            .collect();
        Self {
            core: Arc::new(Core {
                cost: Cost::new(opts.inline_below),
                vfs,
                opts,
                uid,
                gid,
                sh,
            }),
            lanes,
        }
    }

    /// Runs `f` for a request of `class` on `key`'s inode. A class that has been cheap runs right
    /// here on the loop thread, skipping the hand-off. Otherwise `f` goes to the lane that owns
    /// `key`, a FIFO, so requests for one inode keep their order while unrelated inodes run in
    /// parallel and a slow request no longer delays unrelated ones. `pinned` forces the lane.
    fn dispatch(
        &self,
        class: Class,
        key: Ino,
        pinned: bool,
        f: impl FnOnce(&Core) + Send + 'static,
    ) {
        let c = self.core.clone();
        let job = move || {
            let start = Instant::now();
            f(&c);
            c.cost.record(class, start.elapsed());
        };
        if self.lanes.is_empty() || (!pinned && self.core.cost.cheap(class)) {
            return job();
        }
        let i = lane_index(key, self.lanes.len());
        let _ = self.lanes[i].send(Box::new(job));
    }

    fn lane(&self, class: Class, key: Ino, f: impl FnOnce(&Core) + Send + 'static) {
        self.dispatch(class, key, false, f);
    }
}

impl Filesystem for Fs {
    fn init(&mut self, _req: &Request<'_>, config: &mut KernelConfig) -> Result<(), c_int> {
        if !self.lanes.is_empty() {
            let _ = config.add_capabilities(FUSE_PARALLEL_DIROPS);
        }
        Ok(())
    }

    fn lookup(&mut self, _req: &Request<'_>, parent: u64, n: &OsStr, reply: ReplyEntry) {
        let c = &self.core;
        match c.lookup(parent, n) {
            Ok(a) => reply.entry(&c.opts.entry_lifetime(), &a, 0),
            Err(libc::ENOENT) if !c.opts.negative_ttl.is_zero() => {
                reply.entry(&c.opts.negative_ttl, &negative_attr(), 0);
            }
            Err(e) => reply.error(e),
        }
    }

    fn forget(&mut self, _req: &Request<'_>, ino: u64, nlookup: u64) {
        self.core.forget(ino, nlookup);
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, fh: Option<u64>, reply: ReplyAttr) {
        let c = &self.core;
        match c.getattr(ino, fh.is_some()) {
            Ok(a) => reply.attr(&c.opts.attr_lifetime(), &a),
            Err(e) => reply.error(e),
        }
    }

    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let changes = SetAttr {
            mode: mode.map(|m| m & MODE_MASK),
            size,
            atime: atime.map(set_time),
            mtime: mtime.map(set_time),
        };
        let f = move |c: &Core| match c.setattr(ino, changes) {
            Ok(a) => reply.attr(&c.opts.attr_lifetime(), &a),
            Err(e) => reply.error(e),
        };
        if size.is_some() {
            self.lane(Class::Meta, ino, f);
        } else {
            f(&self.core);
        }
    }

    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        match self.core.call(|v| v.readlink(ino)) {
            Ok(t) => reply.data(&t),
            Err(e) => reply.error(e),
        }
    }

    fn mknod(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        n: &OsStr,
        mode: u32,
        _umask: u32,
        _rdev: u32,
        reply: ReplyEntry,
    ) {
        if !convert::mknod_is_regular(mode) {
            return reply.error(libc::ENOTSUP);
        }
        let n = n.to_owned();
        self.lane(Class::Meta, parent, move |c| {
            let r = name(&n).and_then(|nm| {
                let a = c.call(|v| v.create(parent, nm, mode & MODE_MASK))?;
                c.referenced(parent, nm, &a, true)
            });
            match r {
                Ok(a) => reply.entry(&c.opts.entry_lifetime(), &a, 0),
                Err(e) => reply.error(e),
            }
        });
    }

    fn mkdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        n: &OsStr,
        mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let n = n.to_owned();
        self.lane(Class::Meta, parent, move |c| {
            let r = name(&n).and_then(|nm| {
                let a = c.call(|v| v.mkdir(parent, nm, mode & MODE_MASK))?;
                c.referenced(parent, nm, &a, true)
            });
            match r {
                Ok(a) => reply.entry(&c.opts.entry_lifetime(), &a, 0),
                Err(e) => reply.error(e),
            }
        });
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, n: &OsStr, reply: ReplyEmpty) {
        let n = n.to_owned();
        self.lane(Class::Meta, parent, move |c| {
            let r = name(&n).and_then(|nm| {
                c.call(|v| v.unlink(parent, nm))?;
                c.sh.table().unname(parent, nm);
                Ok(())
            });
            empty(r, reply);
        });
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, n: &OsStr, reply: ReplyEmpty) {
        let n = n.to_owned();
        self.lane(Class::Meta, parent, move |c| {
            let r = name(&n).and_then(|nm| {
                c.call(|v| v.rmdir(parent, nm))?;
                c.sh.table().unname(parent, nm);
                Ok(())
            });
            empty(r, reply);
        });
    }

    fn symlink(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        link_name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        let n = link_name.to_owned();
        let target = target.as_os_str().as_bytes().to_vec();
        self.lane(Class::Meta, parent, move |c| {
            let r = name(&n).and_then(|nm| {
                let a = c.call(|v| v.symlink(parent, nm, &target))?;
                c.referenced(parent, nm, &a, true)
            });
            match r {
                Ok(a) => reply.entry(&c.opts.entry_lifetime(), &a, 0),
                Err(e) => reply.error(e),
            }
        });
    }

    fn rename(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        n: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        let flags = match convert::rename_flags(flags) {
            Ok(f) => f,
            Err(errno) => return reply.error(errno),
        };
        let (n, nn) = (n.to_owned(), newname.to_owned());
        self.lane(Class::Meta, parent, move |c| {
            let r = name(&n)
                .and_then(|n| Ok((n, name(&nn)?)))
                .and_then(|(n, nn)| {
                    c.call(|v| v.rename(parent, n, newparent, nn, flags))?;
                    c.sh.table().rename(parent, n, newparent, nn);
                    Ok(())
                });
            empty(r, reply);
        });
    }

    fn link(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        newparent: u64,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        let n = newname.to_owned();
        self.lane(Class::Meta, newparent, move |c| {
            let r = name(&n).and_then(|nm| {
                let a = c.call(|v| v.link(ino, newparent, nm))?;
                c.referenced(newparent, nm, &a, false)
            });
            match r {
                Ok(a) => reply.entry(&c.opts.entry_lifetime(), &a, 0),
                Err(e) => reply.error(e),
            }
        });
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, _flags: i32, reply: ReplyOpen) {
        self.lane(Class::Open, ino, move |c| match c.call(|v| v.open(ino)) {
            Ok(h) => reply.opened(h.0, c.open_flags()),
            Err(e) => reply.error(e),
        });
    }

    fn create(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        n: &OsStr,
        mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let n = n.to_owned();
        self.lane(Class::Meta, parent, move |c| {
            match c.create(parent, &n, mode) {
                Ok((a, fh)) => reply.created(&c.opts.entry_lifetime(), &a, 0, fh, c.open_flags()),
                Err(e) => reply.error(e),
            }
        });
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        off: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        self.lane(Class::Read, ino, move |c| {
            match offset(off).and_then(|o| c.call(|v| v.read(ino, o, size))) {
                Ok(d) => reply.data(&d[..d.len().min(size as usize)]),
                Err(e) => reply.error(e),
            }
        });
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        off: i64,
        data: &[u8],
        _write_flags: u32,
        flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        let data = data.to_vec();
        self.dispatch(
            Class::Write,
            ino,
            append_needs_lane(flags),
            move |c| match c.write(ino, off, &data, flags) {
                Ok((n, stale)) => {
                    reply.written(n);
                    if stale {
                        c.invalidate_attr(ino);
                    }
                }
                Err(e) => reply.error(e),
            },
        );
    }

    fn flush(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        _lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        self.lane(Class::Close, ino, move |c| {
            empty(c.call(|v| v.flush(ino)), reply)
        });
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.lane(Class::Close, ino, move |c| {
            empty(c.call(|v| v.release(FileHandle(fh))), reply);
        });
    }

    fn fsync(&mut self, _req: &Request<'_>, ino: u64, _fh: u64, datasync: bool, reply: ReplyEmpty) {
        self.lane(Class::Fsync, ino, move |c| {
            empty(c.call(|v| v.fsync(ino, datasync)), reply)
        });
    }

    fn fsyncdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        datasync: bool,
        reply: ReplyEmpty,
    ) {
        self.lane(Class::Fsync, ino, move |c| {
            empty(c.call(|v| v.fsync(ino, datasync)), reply)
        });
    }

    fn readdir(&mut self, _req: &Request<'_>, ino: u64, _fh: u64, off: i64, reply: ReplyDirectory) {
        self.lane(Class::Dir, ino, move |c| {
            let parent = c.sh.table().parent_of(ino);
            let mut sink = Sink(reply);
            match c.call(|v| dir::fill(v, ino, parent, off, &mut sink)) {
                Ok(()) => sink.0.ok(),
                Err(e) => sink.0.error(e),
            }
        });
    }

    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        match self.core.call(|v| v.statfs()) {
            Ok(s) => reply.statfs(
                s.blocks,
                s.blocks_free,
                s.blocks_available,
                s.files,
                s.files_free,
                s.block_size,
                s.name_max,
                s.block_size,
            ),
            Err(e) => reply.error(e),
        }
    }

    fn setxattr(
        &mut self,
        req: &Request<'_>,
        ino: u64,
        n: &OsStr,
        value: &[u8],
        flags: i32,
        position: u32,
        reply: ReplyEmpty,
    ) {
        if position != 0 {
            return reply.error(libc::EINVAL);
        }
        if let Err(e) = convert::xattr_name_ok(n.as_bytes(), req.uid() == 0) {
            return reply.error(e);
        }
        let flags = match convert::xattr_flags(flags) {
            Ok(f) => f,
            Err(errno) => return reply.error(errno),
        };
        let (n, value) = (n.to_owned(), value.to_vec());
        self.lane(Class::Meta, ino, move |c| {
            empty(
                c.call(|v| v.setxattr(ino, n.as_bytes(), &value, flags)),
                reply,
            );
        });
    }

    fn getxattr(&mut self, req: &Request<'_>, ino: u64, n: &OsStr, size: u32, reply: ReplyXattr) {
        if let Err(e) = convert::xattr_name_ok(n.as_bytes(), req.uid() == 0) {
            return reply.error(e);
        }
        match self.core.call(|v| v.getxattr(ino, n.as_bytes())) {
            Ok(v) => xattr_reply(size, &v, reply),
            Err(e) => reply.error(e),
        }
    }

    fn listxattr(&mut self, _req: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        match self.core.call(|v| v.listxattr(ino)) {
            Ok(names) => xattr_reply(size, &convert::encode_xattr_names(&names), reply),
            Err(e) => reply.error(e),
        }
    }

    fn removexattr(&mut self, req: &Request<'_>, ino: u64, n: &OsStr, reply: ReplyEmpty) {
        if let Err(e) = convert::xattr_name_ok(n.as_bytes(), req.uid() == 0) {
            return reply.error(e);
        }
        let n = n.to_owned();
        self.lane(Class::Meta, ino, move |c| {
            empty(c.call(|v| v.removexattr(ino, n.as_bytes())), reply);
        });
    }

    fn access(&mut self, req: &Request<'_>, ino: u64, mask: i32, reply: ReplyEmpty) {
        let c = &self.core;
        match c.call(|v| v.getattr(ino)) {
            Ok(a) if convert::access_allowed(a.mode, c.uid, c.gid, req.uid(), req.gid(), mask) => {
                reply.ok();
            }
            Ok(_) => reply.error(libc::EACCES),
            Err(e) => reply.error(e),
        }
    }

    fn lseek(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        off: i64,
        whence: i32,
        reply: ReplyLseek,
    ) {
        let r = self
            .core
            .call(|v| v.getattr(ino))
            .and_then(|a| convert::seek(whence, off, a.size));
        match r {
            Ok(o) => reply.offset(o),
            Err(e) => reply.error(e),
        }
    }

    fn fallocate(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _offset: i64,
        _length: i64,
        _mode: i32,
        reply: ReplyEmpty,
    ) {
        reply.error(libc::ENOTSUP);
    }

    fn copy_file_range(
        &mut self,
        _req: &Request<'_>,
        _ino_in: u64,
        _fh_in: u64,
        _offset_in: i64,
        _ino_out: u64,
        _fh_out: u64,
        _offset_out: i64,
        _len: u64,
        _flags: u32,
        reply: ReplyWrite,
    ) {
        reply.error(libc::ENOTSUP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cowfs_vfs::Timestamp;
    use std::time::Duration;

    #[test]
    fn attr_translation() {
        let t = Timestamp { secs: 10, nanos: 5 };
        let a = Attr {
            ino: 7,
            kind: FileKind::Symlink,
            mode: 0o7777,
            nlink: 3,
            uid: 0,
            gid: 0,
            size: 9,
            blocks: 1,
            atime: t,
            mtime: t,
            ctime: t,
        };
        let f = file_attr(&a, 11, 12);
        assert_eq!(
            (f.ino, f.size, f.blocks, f.nlink, f.uid, f.gid),
            (7, 9, 1, 3, 11, 12)
        );
        assert_eq!((f.kind, f.perm, f.rdev), (FileType::Symlink, 0o7777, 0));
        assert_eq!(f.mtime, UNIX_EPOCH + Duration::new(10, 5));
    }

    #[test]
    fn names_are_validated() {
        assert!(name(OsStr::new("ok")).is_ok());
        assert_eq!(name(OsStr::new("..")), Err(libc::EINVAL));
        assert_eq!(name(OsStr::new(&"x".repeat(256))), Err(libc::ENAMETOOLONG));
        assert_eq!(offset(-1), Err(libc::EINVAL));
    }

    #[test]
    fn negative_entry_has_inode_zero() {
        assert_eq!(negative_attr().ino, 0);
    }
}
