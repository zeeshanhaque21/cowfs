//! The `fuser::Filesystem` implementation: one `Vfs` call per kernel request.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use cowfs_vfs::{
    validate_name, Attr, Error, FileHandle, FileKind, Ino, Result, SetAttr, SetTime, Vfs,
    MODE_MASK, ROOT_INO,
};
use fuser::consts::FOPEN_KEEP_CACHE;
use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty,
    ReplyEntry, ReplyOpen, ReplyStatfs, ReplyWrite, ReplyXattr, Request, TimeOrNow,
};

use crate::convert::{self, XattrReply};
use crate::dir::{self, DirSink};
use crate::options::MountOptions;

const BLKSIZE: u32 = 4096;

pub(crate) struct Fs {
    vfs: Arc<dyn Vfs>,
    opts: MountOptions,
    uid: u32,
    gid: u32,
}

/// The effective uid and gid of this process, which own every file on the mount.
#[allow(unsafe_code)]
fn mounter() -> (u32, u32) {
    // SAFETY: geteuid and getegid take no arguments, touch no memory and cannot fail.
    unsafe { (libc::geteuid(), libc::getegid()) }
}

impl Fs {
    pub(crate) fn new(vfs: Arc<dyn Vfs>, opts: MountOptions) -> Self {
        let (uid, gid) = mounter();
        Self {
            vfs,
            opts,
            uid,
            gid,
        }
    }

    fn file_attr(&self, a: &Attr) -> FileAttr {
        file_attr(a, self.uid, self.gid)
    }

    fn entry(&self, r: Result<Attr>, reply: ReplyEntry) {
        match r {
            Ok(a) => reply.entry(&self.opts.entry_ttl, &self.file_attr(&a), 0),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn attr(&self, r: Result<Attr>, reply: ReplyAttr) {
        match r {
            Ok(a) => reply.attr(&self.opts.attr_ttl, &self.file_attr(&a)),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn keep_cache(&self) -> u32 {
        if self.opts.keep_cache {
            FOPEN_KEEP_CACHE
        } else {
            0
        }
    }
}

fn empty(r: Result<()>, reply: ReplyEmpty) {
    match r {
        Ok(()) => reply.ok(),
        Err(e) => reply.error(e.errno()),
    }
}

fn name(n: &OsStr) -> Result<&[u8]> {
    validate_name(n.as_bytes())?;
    Ok(n.as_bytes())
}

fn offset(o: i64) -> Result<u64> {
    u64::try_from(o).map_err(|_| Error::InvalidArgument)
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

struct Sink(ReplyDirectory);

impl DirSink for Sink {
    fn add(&mut self, ino: Ino, offset: i64, kind: FileKind, n: &[u8]) -> bool {
        self.0
            .add(ino, offset, file_type(kind), OsStr::from_bytes(n))
    }
}

impl Filesystem for Fs {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, n: &OsStr, reply: ReplyEntry) {
        match name(n).and_then(|n| self.vfs.lookup(parent, n)) {
            Err(Error::NotFound) if !self.opts.negative_ttl.is_zero() => {
                reply.entry(&self.opts.negative_ttl, &negative_attr(), 0);
            }
            r => self.entry(r, reply),
        }
    }

    fn forget(&mut self, _req: &Request<'_>, ino: u64, nlookup: u64) {
        if ino != ROOT_INO {
            self.vfs.forget(ino, nlookup);
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        self.attr(self.vfs.getattr(ino), reply);
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
        _ctime: Option<std::time::SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let changes = SetAttr {
            mode: mode.map(|m| m & MODE_MASK),
            size,
            atime: atime.map(set_time),
            mtime: mtime.map(set_time),
        };
        self.attr(self.vfs.setattr(ino, changes), reply);
    }

    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        match self.vfs.readlink(ino) {
            Ok(t) => reply.data(&t),
            Err(e) => reply.error(e.errno()),
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
        self.entry(
            name(n).and_then(|n| self.vfs.create(parent, n, mode & MODE_MASK)),
            reply,
        );
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
        self.entry(
            name(n).and_then(|n| self.vfs.mkdir(parent, n, mode & MODE_MASK)),
            reply,
        );
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, n: &OsStr, reply: ReplyEmpty) {
        empty(name(n).and_then(|n| self.vfs.unlink(parent, n)), reply);
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, n: &OsStr, reply: ReplyEmpty) {
        empty(name(n).and_then(|n| self.vfs.rmdir(parent, n)), reply);
    }

    fn symlink(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        link_name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        self.entry(
            name(link_name)
                .and_then(|n| self.vfs.symlink(parent, n, target.as_os_str().as_bytes())),
            reply,
        );
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
        empty(
            name(n)
                .and_then(|n| Ok((n, name(newname)?)))
                .and_then(|(n, nn)| self.vfs.rename(parent, n, newparent, nn, flags)),
            reply,
        );
    }

    fn link(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        newparent: u64,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        self.entry(
            name(newname).and_then(|n| self.vfs.link(ino, newparent, n)),
            reply,
        );
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, _flags: i32, reply: ReplyOpen) {
        match self.vfs.open(ino) {
            Ok(h) => reply.opened(h.0, self.keep_cache()),
            Err(e) => reply.error(e.errno()),
        }
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
        let attr = match name(n).and_then(|n| self.vfs.create(parent, n, mode & MODE_MASK)) {
            Ok(a) => a,
            Err(e) => return reply.error(e.errno()),
        };
        match self.vfs.open(attr.ino) {
            Ok(h) => reply.created(&self.opts.entry_ttl, &self.file_attr(&attr), 0, h.0, 0),
            Err(e) => {
                self.vfs.forget(attr.ino, 1);
                reply.error(e.errno());
            }
        }
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
        match offset(off).and_then(|o| self.vfs.read(ino, o, size)) {
            Ok(d) => reply.data(&d),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn write(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        off: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        match offset(off).and_then(|o| self.vfs.write(ino, o, data)) {
            Ok(n) => reply.written(n),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn flush(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        _lock_owner: u64,
        reply: ReplyEmpty,
    ) {
        empty(self.vfs.flush(ino), reply);
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        empty(self.vfs.release(FileHandle(fh)), reply);
    }

    fn fsync(&mut self, _req: &Request<'_>, ino: u64, _fh: u64, datasync: bool, reply: ReplyEmpty) {
        empty(self.vfs.fsync(ino, datasync), reply);
    }

    fn fsyncdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        datasync: bool,
        reply: ReplyEmpty,
    ) {
        empty(self.vfs.fsync(ino, datasync), reply);
    }

    fn readdir(&mut self, _req: &Request<'_>, ino: u64, _fh: u64, off: i64, reply: ReplyDirectory) {
        let mut sink = Sink(reply);
        match dir::fill(&*self.vfs, ino, off, &mut sink) {
            Ok(()) => sink.0.ok(),
            Err(e) => sink.0.error(e.errno()),
        }
    }

    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        match self.vfs.statfs() {
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
            Err(e) => reply.error(e.errno()),
        }
    }

    fn setxattr(
        &mut self,
        _req: &Request<'_>,
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
        match convert::xattr_flags(flags) {
            Ok(f) => empty(self.vfs.setxattr(ino, n.as_bytes(), value, f), reply),
            Err(errno) => reply.error(errno),
        }
    }

    fn getxattr(&mut self, _req: &Request<'_>, ino: u64, n: &OsStr, size: u32, reply: ReplyXattr) {
        match self.vfs.getxattr(ino, n.as_bytes()) {
            Ok(v) => xattr_reply(size, &v, reply),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn listxattr(&mut self, _req: &Request<'_>, ino: u64, size: u32, reply: ReplyXattr) {
        match self.vfs.listxattr(ino) {
            Ok(names) => xattr_reply(size, &convert::encode_xattr_names(&names), reply),
            Err(e) => reply.error(e.errno()),
        }
    }

    fn removexattr(&mut self, _req: &Request<'_>, ino: u64, n: &OsStr, reply: ReplyEmpty) {
        empty(self.vfs.removexattr(ino, n.as_bytes()), reply);
    }

    fn access(&mut self, req: &Request<'_>, ino: u64, mask: i32, reply: ReplyEmpty) {
        match self.vfs.getattr(ino) {
            Ok(a)
                if convert::access_allowed(
                    a.mode,
                    self.uid,
                    self.gid,
                    req.uid(),
                    req.gid(),
                    mask,
                ) =>
            {
                reply.ok();
            }
            Ok(_) => reply.error(libc::EACCES),
            Err(e) => reply.error(e.errno()),
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

fn xattr_reply(size: u32, value: &[u8], reply: ReplyXattr) {
    match convert::xattr_reply(size, value.len()) {
        XattrReply::Size(n) => reply.size(n),
        XattrReply::Data => reply.data(value),
        XattrReply::TooSmall => reply.error(libc::ERANGE),
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
        assert_eq!(name(OsStr::new("..")), Err(Error::InvalidArgument));
        assert_eq!(name(OsStr::new(&"x".repeat(256))), Err(Error::NameTooLong));
        assert_eq!(offset(-1), Err(Error::InvalidArgument));
    }

    #[test]
    fn negative_entry_has_inode_zero() {
        assert_eq!(negative_attr().ino, 0);
    }
}
