use fuser::{
    consts::*, FileAttr, FileType, Filesystem, KernelConfig, MountOption, ReplyAttr, ReplyCreate,
    ReplyData, ReplyDirectory, ReplyDirectoryPlus, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyStatfs,
    ReplyWrite, Request, TimeOrNow,
};
use libc::c_int;
use std::collections::HashMap;
use std::ffi::{CString, OsStr, OsString};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirEntryExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type R<T> = Result<T, c_int>;
type Job = Box<dyn FnOnce() + Send>;
type Ents = Arc<Vec<(u64, FileType, OsString)>>;

fn cvt(r: c_int) -> R<c_int> {
    if r < 0 {
        Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO))
    } else {
        Ok(r)
    }
}
fn cvtl(r: isize) -> R<usize> {
    cvt(if r < 0 { -1 } else { 0 }).map(|_| r as usize)
}
fn cstr(s: &OsStr) -> R<CString> {
    CString::new(s.as_bytes()).map_err(|_| libc::EINVAL)
}
fn procpath(fd: RawFd) -> CString {
    CString::new(format!("/proc/self/fd/{fd}")).unwrap()
}
fn ts(s: i64, n: i64) -> SystemTime {
    UNIX_EPOCH + Duration::new(s.max(0) as u64, n as u32)
}
fn kind(mode: u32) -> FileType {
    match mode & libc::S_IFMT {
        libc::S_IFDIR => FileType::Directory,
        libc::S_IFLNK => FileType::Symlink,
        libc::S_IFBLK => FileType::BlockDevice,
        libc::S_IFCHR => FileType::CharDevice,
        libc::S_IFIFO => FileType::NamedPipe,
        libc::S_IFSOCK => FileType::Socket,
        _ => FileType::RegularFile,
    }
}
fn neg_attr() -> FileAttr {
    FileAttr {
        ino: 0, size: 0, blocks: 0, atime: UNIX_EPOCH, mtime: UNIX_EPOCH, ctime: UNIX_EPOCH,
        crtime: UNIX_EPOCH, kind: FileType::RegularFile, perm: 0, nlink: 0, uid: 0, gid: 0,
        rdev: 0, blksize: 0, flags: 0,
    }
}

struct Cfg {
    entry_ttl: Duration,
    attr_ttl: Duration,
    neg: bool,
    null_lookup: bool,
    writeback: bool,
    rdplus: bool,
    keep_cache: bool,
    direct_io: bool,
    cache_dir: bool,
    cache_symlinks: bool,
    max_write: Option<u32>,
    max_ra: Option<u32>,
    max_bg: Option<u16>,
    cong: Option<u16>,
}

struct Inner {
    cfg: Cfg,
    root_ino: u64,
    inodes: Mutex<HashMap<u64, (Arc<OwnedFd>, u64)>>,
    dirs: Mutex<HashMap<u64, Ents>>,
    next_dh: AtomicU64,
}

impl Inner {
    fn fino(&self, st_ino: u64) -> u64 {
        if st_ino == self.root_ino { 1 } else { st_ino }
    }
    fn fd(&self, ino: u64) -> R<Arc<OwnedFd>> {
        self.inodes.lock().unwrap().get(&ino).map(|e| e.0.clone()).ok_or(libc::ENOENT)
    }
    fn stat(&self, fd: RawFd, name: &std::ffi::CStr, flags: c_int) -> R<libc::stat> {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        cvt(unsafe { libc::fstatat(fd, name.as_ptr(), &mut st, flags) })?;
        Ok(st)
    }
    fn attr(&self, st: &libc::stat) -> FileAttr {
        FileAttr {
            ino: self.fino(st.st_ino as u64),
            size: st.st_size as u64,
            blocks: st.st_blocks as u64,
            atime: ts(st.st_atime as i64, st.st_atime_nsec as i64),
            mtime: ts(st.st_mtime as i64, st.st_mtime_nsec as i64),
            ctime: ts(st.st_ctime as i64, st.st_ctime_nsec as i64),
            crtime: UNIX_EPOCH,
            kind: kind(st.st_mode as u32),
            perm: (st.st_mode as u32 & 0o7777) as u16,
            nlink: st.st_nlink as u32,
            uid: st.st_uid,
            gid: st.st_gid,
            rdev: st.st_rdev as u32,
            blksize: st.st_blksize as u32,
            flags: 0,
        }
    }
    fn stat_ino(&self, ino: u64) -> R<FileAttr> {
        let fd = self.fd(ino)?;
        let st = self.stat(fd.as_raw_fd(), c"", libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW)?;
        Ok(self.attr(&st))
    }
    fn lookup(&self, parent: u64, name: &OsStr) -> R<FileAttr> {
        let pfd = self.fd(parent)?;
        let cname = cstr(name)?;
        let st = self.stat(pfd.as_raw_fd(), &cname, libc::AT_SYMLINK_NOFOLLOW)?;
        let attr = self.attr(&st);
        let mut tab = self.inodes.lock().unwrap();
        if let Some(e) = tab.get_mut(&attr.ino) {
            e.1 += 1;
            return Ok(attr);
        }
        drop(tab);
        let fd = cvt(unsafe {
            libc::openat(pfd.as_raw_fd(), cname.as_ptr(), libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        })?;
        let fd = Arc::new(unsafe { OwnedFd::from_raw_fd(fd) });
        self.inodes.lock().unwrap().entry(attr.ino).and_modify(|e| e.1 += 1).or_insert((fd, 1));
        Ok(attr)
    }
    fn forget(&self, ino: u64, n: u64) {
        let mut tab = self.inodes.lock().unwrap();
        if let Some(e) = tab.get_mut(&ino) {
            e.1 = e.1.saturating_sub(n);
            if e.1 == 0 && ino != 1 {
                tab.remove(&ino);
            }
        }
    }
    fn setattr(
        &self, ino: u64, mode: Option<u32>, uid: Option<u32>, gid: Option<u32>, size: Option<u64>,
        atime: Option<TimeOrNow>, mtime: Option<TimeOrNow>,
    ) -> R<FileAttr> {
        let fd = self.fd(ino)?;
        let f = fd.as_raw_fd();
        if let Some(m) = mode {
            cvt(unsafe { libc::chmod(procpath(f).as_ptr(), m & 0o7777) })?;
        }
        if uid.is_some() || gid.is_some() {
            cvt(unsafe {
                libc::fchownat(f, c"".as_ptr(), uid.unwrap_or(!0), gid.unwrap_or(!0),
                    libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW)
            })?;
        }
        if let Some(s) = size {
            cvt(unsafe { libc::truncate(procpath(f).as_ptr(), s as i64) })?;
        }
        if atime.is_some() || mtime.is_some() {
            let t = |x: Option<TimeOrNow>| match x {
                None => libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_OMIT },
                Some(TimeOrNow::Now) => libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_NOW },
                Some(TimeOrNow::SpecificTime(s)) => {
                    let d = s.duration_since(UNIX_EPOCH).unwrap_or_default();
                    libc::timespec { tv_sec: d.as_secs() as i64, tv_nsec: d.subsec_nanos() as i64 }
                }
            };
            let times = [t(atime), t(mtime)];
            cvt(unsafe { libc::utimensat(f, c"".as_ptr(), times.as_ptr(), libc::AT_EMPTY_PATH) })?;
        }
        self.stat_ino(ino)
    }
    fn open_flags(&self, flags: i32) -> i32 {
        let mut f = flags & !libc::O_NOFOLLOW;
        if self.cfg.writeback {
            f &= !libc::O_APPEND;
            if f & libc::O_ACCMODE == libc::O_WRONLY {
                f = (f & !libc::O_ACCMODE) | libc::O_RDWR;
            }
        }
        f
    }
    fn fopen(&self) -> u32 {
        (if self.cfg.keep_cache { FOPEN_KEEP_CACHE } else { 0 }) | (if self.cfg.direct_io { FOPEN_DIRECT_IO } else { 0 })
    }
    fn snapshot(&self, ino: u64) -> R<Vec<(u64, FileType, OsString)>> {
        let fd = self.fd(ino)?;
        let mut v = vec![(ino, FileType::Directory, ".".into()), (ino, FileType::Directory, "..".into())];
        let io = |e: std::io::Error| e.raw_os_error().unwrap_or(libc::EIO);
        for e in std::fs::read_dir(format!("/proc/self/fd/{}", fd.as_raw_fd())).map_err(io)? {
            let e = e.map_err(io)?;
            let k = match e.file_type().map_err(io)? {
                t if t.is_dir() => FileType::Directory,
                t if t.is_symlink() => FileType::Symlink,
                _ => FileType::RegularFile,
            };
            v.push((self.fino(e.ino()), k, e.file_name()));
        }
        Ok(v)
    }
    fn dirlist(&self, ino: u64, fh: u64, offset: i64) -> R<Ents> {
        if offset == 0 {
            let e = Arc::new(self.snapshot(ino)?);
            self.dirs.lock().unwrap().insert(fh, e.clone());
            return Ok(e);
        }
        self.dirs.lock().unwrap().get(&fh).cloned().ok_or(libc::EIO)
    }
    fn entry_reply(&self, r: R<FileAttr>, reply: ReplyEntry) {
        match r {
            Ok(a) => reply.entry(&self.cfg.entry_ttl, &a, 0),
            Err(libc::ENOENT) if self.cfg.neg => reply.entry(&self.cfg.entry_ttl, &neg_attr(), 0),
            Err(e) => reply.error(e),
        }
    }
    fn make(&self, parent: u64, name: &OsStr, f: impl FnOnce(RawFd, &CString) -> c_int) -> R<FileAttr> {
        let pfd = self.fd(parent)?;
        let c = cstr(name)?;
        cvt(f(pfd.as_raw_fd(), &c))?;
        self.lookup(parent, name)
    }
}

struct Fs {
    i: Arc<Inner>,
    tx: Option<mpsc::Sender<Job>>,
}

impl Fs {
    fn go(&self, f: impl FnOnce(&Inner) + Send + 'static) {
        let i = self.i.clone();
        match &self.tx {
            None => f(&i),
            Some(tx) => tx.send(Box::new(move || f(&i))).unwrap(),
        }
    }
}

impl Filesystem for Fs {
    fn init(&mut self, _req: &Request<'_>, c: &mut KernelConfig) -> Result<(), c_int> {
        let cfg = &self.i.cfg;
        let mut caps = FUSE_PARALLEL_DIROPS;
        if cfg.writeback { caps |= FUSE_WRITEBACK_CACHE; }
        if cfg.rdplus { caps |= FUSE_DO_READDIRPLUS | FUSE_READDIRPLUS_AUTO; }
        if cfg.cache_symlinks { caps |= FUSE_CACHE_SYMLINKS; }
        if let Err(missing) = c.add_capabilities(caps) {
            eprintln!("kernel lacks capabilities {missing:#x}");
        }
        if let Some(v) = cfg.max_write { eprintln!("max_write {:?}", c.set_max_write(v)); }
        if let Some(v) = cfg.max_ra { eprintln!("max_readahead {:?}", c.set_max_readahead(v)); }
        if let Some(v) = cfg.max_bg { eprintln!("max_background {:?}", c.set_max_background(v)); }
        if let Some(v) = cfg.cong { eprintln!("congestion {:?}", c.set_congestion_threshold(v)); }
        eprintln!("{c:?}");
        Ok(())
    }

    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        if self.i.cfg.null_lookup && (name.as_bytes().starts_with(b"m") || name.as_bytes().starts_with(b"nofile")) {
            return self.i.entry_reply(Err(libc::ENOENT), reply);
        }
        let name = name.to_owned();
        self.go(move |i| i.entry_reply(i.lookup(parent, &name), reply));
    }

    fn forget(&mut self, _req: &Request<'_>, ino: u64, n: u64) {
        self.i.forget(ino, n);
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        self.go(move |i| match i.stat_ino(ino) {
            Ok(a) => reply.attr(&i.cfg.attr_ttl, &a),
            Err(e) => reply.error(e),
        });
    }

    fn setattr(
        &mut self, _req: &Request<'_>, ino: u64, mode: Option<u32>, uid: Option<u32>, gid: Option<u32>,
        size: Option<u64>, atime: Option<TimeOrNow>, mtime: Option<TimeOrNow>, _ctime: Option<SystemTime>,
        _fh: Option<u64>, _crtime: Option<SystemTime>, _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>, _flags: Option<u32>, reply: ReplyAttr,
    ) {
        self.go(move |i| match i.setattr(ino, mode, uid, gid, size, atime, mtime) {
            Ok(a) => reply.attr(&i.cfg.attr_ttl, &a),
            Err(e) => reply.error(e),
        });
    }

    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        self.go(move |i| {
            let r = i.fd(ino).and_then(|fd| {
                let mut buf = vec![0u8; libc::PATH_MAX as usize];
                let n = cvtl(unsafe {
                    libc::readlinkat(fd.as_raw_fd(), c"".as_ptr(), buf.as_mut_ptr() as *mut _, buf.len())
                })?;
                buf.truncate(n);
                Ok(buf)
            });
            match r {
                Ok(b) => reply.data(&b),
                Err(e) => reply.error(e),
            }
        });
    }

    fn mkdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, mode: u32, _umask: u32, reply: ReplyEntry) {
        let name = name.to_owned();
        self.go(move |i| {
            let r = i.make(parent, &name, |p, c| unsafe { libc::mkdirat(p, c.as_ptr(), mode) });
            i.entry_reply(r, reply)
        });
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let name = name.to_owned();
        self.go(move |i| {
            let r = i.fd(parent).and_then(|p| cvt(unsafe { libc::unlinkat(p.as_raw_fd(), cstr(&name)?.as_ptr(), 0) }));
            match r { Ok(_) => reply.ok(), Err(e) => reply.error(e) }
        });
    }

    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let name = name.to_owned();
        self.go(move |i| {
            let r = i.fd(parent).and_then(|p| {
                cvt(unsafe { libc::unlinkat(p.as_raw_fd(), cstr(&name)?.as_ptr(), libc::AT_REMOVEDIR) })
            });
            match r { Ok(_) => reply.ok(), Err(e) => reply.error(e) }
        });
    }

    fn symlink(&mut self, _req: &Request<'_>, parent: u64, link_name: &OsStr, target: &Path, reply: ReplyEntry) {
        let name = link_name.to_owned();
        let target = target.to_owned();
        self.go(move |i| {
            let r = cstr(target.as_os_str()).and_then(|t| {
                i.make(parent, &name, |p, c| unsafe { libc::symlinkat(t.as_ptr(), p, c.as_ptr()) })
            });
            i.entry_reply(r, reply)
        });
    }

    fn rename(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, newparent: u64, newname: &OsStr, flags: u32, reply: ReplyEmpty) {
        let (name, newname) = (name.to_owned(), newname.to_owned());
        self.go(move |i| {
            let r = (|| {
                let (p, np) = (i.fd(parent)?, i.fd(newparent)?);
                let (c, nc) = (cstr(&name)?, cstr(&newname)?);
                cvt(unsafe {
                    libc::syscall(libc::SYS_renameat2, p.as_raw_fd(), c.as_ptr(), np.as_raw_fd(), nc.as_ptr(), flags) as c_int
                })
            })();
            match r { Ok(_) => reply.ok(), Err(e) => reply.error(e) }
        });
    }

    fn link(&mut self, _req: &Request<'_>, ino: u64, newparent: u64, newname: &OsStr, reply: ReplyEntry) {
        let name = newname.to_owned();
        self.go(move |i| {
            let r = i.fd(ino).and_then(|src| {
                let sp = procpath(src.as_raw_fd());
                i.make(newparent, &name, |p, c| unsafe {
                    libc::linkat(libc::AT_FDCWD, sp.as_ptr(), p, c.as_ptr(), libc::AT_SYMLINK_FOLLOW)
                })
            });
            i.entry_reply(r, reply)
        });
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        self.go(move |i| {
            let r = i.fd(ino).and_then(|fd| {
                cvt(unsafe { libc::open(procpath(fd.as_raw_fd()).as_ptr(), i.open_flags(flags) | libc::O_CLOEXEC) })
            });
            match r { Ok(fh) => reply.opened(fh as u64, i.fopen()), Err(e) => reply.error(e) }
        });
    }

    fn create(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, mode: u32, _umask: u32, flags: i32, reply: ReplyCreate) {
        let name = name.to_owned();
        self.go(move |i| {
            let r = (|| {
                let p = i.fd(parent)?;
                let fh = cvt(unsafe {
                    libc::openat(p.as_raw_fd(), cstr(&name)?.as_ptr(),
                        i.open_flags(flags) | libc::O_CREAT | libc::O_NOFOLLOW | libc::O_CLOEXEC, mode)
                })?;
                match i.lookup(parent, &name) {
                    Ok(a) => Ok((a, fh)),
                    Err(e) => { unsafe { libc::close(fh) }; Err(e) }
                }
            })();
            match r {
                Ok((a, fh)) => reply.created(&i.cfg.entry_ttl, &a, 0, fh as u64, i.fopen()),
                Err(e) => reply.error(e),
            }
        });
    }

    fn read(&mut self, _req: &Request<'_>, _ino: u64, fh: u64, offset: i64, size: u32, _flags: i32, _lo: Option<u64>, reply: ReplyData) {
        self.go(move |_| {
            let mut buf: Vec<u8> = Vec::with_capacity(size as usize);
            match cvtl(unsafe { libc::pread(fh as c_int, buf.as_mut_ptr() as *mut _, size as usize, offset) }) {
                Ok(n) => {
                    unsafe { buf.set_len(n) };
                    reply.data(&buf)
                }
                Err(e) => reply.error(e),
            }
        });
    }

    fn write(&mut self, _req: &Request<'_>, _ino: u64, fh: u64, offset: i64, data: &[u8], _wf: u32, _flags: i32, _lo: Option<u64>, reply: ReplyWrite) {
        match cvtl(unsafe { libc::pwrite(fh as c_int, data.as_ptr() as *const _, data.len(), offset) }) {
            Ok(n) => reply.written(n as u32),
            Err(e) => reply.error(e),
        }
    }

    fn flush(&mut self, _req: &Request<'_>, _ino: u64, fh: u64, _lo: u64, reply: ReplyEmpty) {
        let r = cvt(unsafe { libc::dup(fh as c_int) }).and_then(|d| cvt(unsafe { libc::close(d) }));
        match r { Ok(_) => reply.ok(), Err(e) => reply.error(e) }
    }

    fn release(&mut self, _req: &Request<'_>, _ino: u64, fh: u64, _flags: i32, _lo: Option<u64>, _flush: bool, reply: ReplyEmpty) {
        drop(unsafe { OwnedFd::from_raw_fd(fh as c_int) });
        reply.ok();
    }

    fn fsync(&mut self, _req: &Request<'_>, _ino: u64, fh: u64, datasync: bool, reply: ReplyEmpty) {
        let fd = fh as c_int;
        let r = cvt(unsafe { if datasync { libc::fdatasync(fd) } else { libc::fsync(fd) } });
        match r { Ok(_) => reply.ok(), Err(e) => reply.error(e) }
    }

    fn opendir(&mut self, _req: &Request<'_>, _ino: u64, _flags: i32, reply: ReplyOpen) {
        let f = if self.i.cfg.cache_dir { FOPEN_CACHE_DIR | FOPEN_KEEP_CACHE } else { 0 };
        reply.opened(self.i.next_dh.fetch_add(1, Relaxed), f);
    }

    fn readdir(&mut self, _req: &Request<'_>, ino: u64, fh: u64, offset: i64, mut reply: ReplyDirectory) {
        self.go(move |i| match i.dirlist(ino, fh, offset) {
            Ok(ents) => {
                for (idx, (eino, k, name)) in ents.iter().enumerate().skip(offset as usize) {
                    if reply.add(*eino, idx as i64 + 1, *k, name) { break; }
                }
                reply.ok()
            }
            Err(e) => reply.error(e),
        });
    }

    fn readdirplus(&mut self, _req: &Request<'_>, ino: u64, fh: u64, offset: i64, mut reply: ReplyDirectoryPlus) {
        self.go(move |i| match i.dirlist(ino, fh, offset) {
            Ok(ents) => {
                for (idx, (_, _, name)) in ents.iter().enumerate().skip(offset as usize) {
                    let dot = name == "." || name == "..";
                    let a = if dot {
                        i.fd(ino).and_then(|fd| {
                            let c = cstr(name)?;
                            i.stat(fd.as_raw_fd(), &c, libc::AT_SYMLINK_NOFOLLOW).map(|st| i.attr(&st))
                        })
                    } else {
                        i.lookup(ino, name)
                    };
                    let Ok(a) = a else { continue };
                    if reply.add(a.ino, idx as i64 + 1, name, &i.cfg.entry_ttl, &a, 0) {
                        if !dot { i.forget(a.ino, 1); }
                        break;
                    }
                }
                reply.ok()
            }
            Err(e) => reply.error(e),
        });
    }

    fn releasedir(&mut self, _req: &Request<'_>, _ino: u64, fh: u64, _flags: i32, reply: ReplyEmpty) {
        self.i.dirs.lock().unwrap().remove(&fh);
        reply.ok();
    }

    fn statfs(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyStatfs) {
        self.go(move |i| {
            let r = i.fd(ino).and_then(|fd| {
                let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
                cvt(unsafe { libc::fstatvfs(fd.as_raw_fd(), &mut s) }).map(|_| s)
            });
            match r {
                Ok(s) => reply.statfs(s.f_blocks, s.f_bfree, s.f_bavail, s.f_files, s.f_ffree, s.f_bsize as u32, s.f_namemax as u32, s.f_frsize as u32),
                Err(e) => reply.error(e),
            }
        });
    }
}

fn main() {
    let mut a = std::env::args().skip(1);
    let (backing, mnt) = (a.next().expect("backing"), a.next().expect("mnt"));
    let mut cfg = Cfg {
        entry_ttl: Duration::from_secs(1), attr_ttl: Duration::from_secs(1), neg: false, null_lookup: false, writeback: false,
        rdplus: false, keep_cache: false, direct_io: false, cache_dir: false, cache_symlinks: false,
        max_write: None, max_ra: None, max_bg: None, cong: None,
    };
    let mut threads = 0usize;
    let mut opts = vec![MountOption::FSName("fusepass".into()), MountOption::Subtype("fusepass".into())];
    while let Some(f) = a.next() {
        let mut v = || a.next().expect("value");
        let secs = |s: String| Duration::from_secs_f64(s.parse().unwrap());
        match f.as_str() {
            "--ttl" => { let d = secs(v()); cfg.entry_ttl = d; cfg.attr_ttl = d; }
            "--entry-ttl" => cfg.entry_ttl = secs(v()),
            "--attr-ttl" => cfg.attr_ttl = secs(v()),
            "--neg" => cfg.neg = true,
            "--null-lookup" => cfg.null_lookup = true,
            "--writeback" => cfg.writeback = true,
            "--rdplus" => cfg.rdplus = true,
            "--keep-cache" => cfg.keep_cache = true,
            "--direct-io" => cfg.direct_io = true,
            "--cache-dir" => cfg.cache_dir = true,
            "--cache-symlinks" => cfg.cache_symlinks = true,
            "--threads" => threads = v().parse().unwrap(),
            "--max-write" => cfg.max_write = Some(v().parse().unwrap()),
            "--max-ra" => cfg.max_ra = Some(v().parse().unwrap()),
            "--max-bg" => cfg.max_bg = Some(v().parse().unwrap()),
            "--cong" => cfg.cong = Some(v().parse().unwrap()),
            "--max-read" => opts.push(MountOption::CUSTOM(format!("max_read={}", v()))),
            "--opt" => opts.push(MountOption::CUSTOM(v())),
            x => panic!("unknown flag {x}"),
        }
    }
    let mut rl = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    unsafe {
        libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl);
        rl.rlim_cur = rl.rlim_max;
        libc::setrlimit(libc::RLIMIT_NOFILE, &rl);
    }
    let b = CString::new(backing).unwrap();
    let root = cvt(unsafe { libc::open(b.as_ptr(), libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC) }).expect("open backing");
    let root = unsafe { OwnedFd::from_raw_fd(root) };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    cvt(unsafe { libc::fstatat(root.as_raw_fd(), c"".as_ptr(), &mut st, libc::AT_EMPTY_PATH) }).unwrap();
    let inner = Arc::new(Inner {
        cfg, root_ino: st.st_ino as u64,
        inodes: Mutex::new(HashMap::from([(1, (Arc::new(root), 1))])),
        dirs: Mutex::new(HashMap::new()),
        next_dh: AtomicU64::new(1),
    });
    let tx = (threads > 0).then(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..threads {
            let rx = rx.clone();
            std::thread::spawn(move || loop {
                let j = rx.lock().unwrap().recv();
                match j { Ok(j) => j(), Err(_) => break }
            });
        }
        tx
    });
    eprintln!("mounting {mnt} threads={threads}");
    fuser::mount2(Fs { i: inner, tx }, &mnt, &opts).unwrap();
}
