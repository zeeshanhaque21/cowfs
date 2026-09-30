//! The only module with `unsafe`: thin, descriptor-relative wrappers over libc.
//!
//! SAFETY, for every block below: pointers come from live `CString`s, slices or locals that
//! outlive the call, lengths are the slice lengths, and file descriptors are borrowed for the
//! duration of the call (`BorrowedFd`) or owned by the caller (`OwnedFd`).

use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("cowfs-vfs-path supports Linux and macOS only");

fn cstr(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))
}

fn cvt(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

fn cvt_size(r: isize) -> io::Result<usize> {
    usize::try_from(r).map_err(|_| io::Error::last_os_error())
}

/// The fields of `struct stat` the crate uses, widened to fixed types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stat {
    pub dev: u64,
    pub ino: u64,
    /// The full `st_mode`, file type bits included.
    pub mode: u32,
    pub nlink: u64,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub blocks: u64,
    pub atime: (i64, u32),
    pub mtime: (i64, u32),
    pub ctime: (i64, u32),
}

impl Stat {
    pub fn file_type(&self) -> u32 {
        self.mode & u32::from(libc::S_IFMT)
    }
}

#[allow(clippy::unnecessary_cast)]
fn widen(st: &libc::stat) -> Stat {
    let ts = |s: i64, n: i64| (s, n as u32);
    Stat {
        dev: st.st_dev as u64,
        ino: st.st_ino as u64,
        mode: u32::from(st.st_mode),
        nlink: u64::from(st.st_nlink),
        uid: st.st_uid,
        gid: st.st_gid,
        size: st.st_size as u64,
        blocks: st.st_blocks as u64,
        atime: ts(st.st_atime as i64, st.st_atime_nsec as i64),
        mtime: ts(st.st_mtime as i64, st.st_mtime_nsec as i64),
        ctime: ts(st.st_ctime as i64, st.st_ctime_nsec as i64),
    }
}

fn zeroed_stat() -> libc::stat {
    // SAFETY: `libc::stat` is plain old data; all zero bytes are a valid value.
    unsafe { std::mem::zeroed() }
}

/// `fstat`.
pub fn fstat(fd: BorrowedFd<'_>) -> io::Result<Stat> {
    let mut st = zeroed_stat();
    // SAFETY: see module docs.
    cvt(unsafe { libc::fstat(fd.as_raw_fd(), &mut st) })?;
    Ok(widen(&st))
}

/// `fstatat` that never follows a final symlink.
pub fn fstatat(dir: BorrowedFd<'_>, name: &[u8]) -> io::Result<Stat> {
    let name = cstr(name)?;
    let mut st = zeroed_stat();
    // SAFETY: see module docs.
    cvt(unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })?;
    Ok(widen(&st))
}

/// `openat` with `O_CLOEXEC` added.
pub fn openat(
    dir: BorrowedFd<'_>,
    name: &[u8],
    flags: libc::c_int,
    mode: u32,
) -> io::Result<OwnedFd> {
    let name = cstr(name)?;
    // SAFETY: see module docs; the mode is passed as the variadic `unsigned int` openat expects.
    let fd = cvt(unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC,
            mode as libc::c_uint,
        )
    })?;
    // SAFETY: openat returned a fresh descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Flags that open a symlink itself (not its target) for `fstat` and friends.
#[cfg(target_os = "linux")]
pub const OPEN_SYMLINK: libc::c_int = libc::O_PATH | libc::O_NOFOLLOW;
/// Flags that open a symlink itself (not its target) for `fstat` and friends.
#[cfg(target_os = "macos")]
pub const OPEN_SYMLINK: libc::c_int = libc::O_SYMLINK | libc::O_RDONLY;

pub fn mkdirat(dir: BorrowedFd<'_>, name: &[u8], mode: u32) -> io::Result<()> {
    let name = cstr(name)?;
    // SAFETY: see module docs.
    cvt(unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), mode as libc::mode_t) })?;
    Ok(())
}

pub fn unlinkat(dir: BorrowedFd<'_>, name: &[u8], remove_dir: bool) -> io::Result<()> {
    let name = cstr(name)?;
    let flags = if remove_dir { libc::AT_REMOVEDIR } else { 0 };
    // SAFETY: see module docs.
    cvt(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), flags) })?;
    Ok(())
}

pub fn symlinkat(target: &[u8], dir: BorrowedFd<'_>, name: &[u8]) -> io::Result<()> {
    let (target, name) = (cstr(target)?, cstr(name)?);
    // SAFETY: see module docs.
    cvt(unsafe { libc::symlinkat(target.as_ptr(), dir.as_raw_fd(), name.as_ptr()) })?;
    Ok(())
}

pub fn readlinkat(dir: BorrowedFd<'_>, name: &[u8]) -> io::Result<Vec<u8>> {
    let name = cstr(name)?;
    let mut buf = vec![0u8; 1024];
    loop {
        // SAFETY: see module docs.
        let n = cvt_size(unsafe {
            libc::readlinkat(
                dir.as_raw_fd(),
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        })?;
        if n < buf.len() {
            buf.truncate(n);
            return Ok(buf);
        }
        buf = vec![0u8; buf.len() * 2];
    }
}

/// `linkat` without following symlinks.
pub fn linkat(
    old_dir: BorrowedFd<'_>,
    old_name: &[u8],
    new_dir: BorrowedFd<'_>,
    new_name: &[u8],
) -> io::Result<()> {
    let (old, new) = (cstr(old_name)?, cstr(new_name)?);
    // SAFETY: see module docs.
    cvt(unsafe {
        libc::linkat(
            old_dir.as_raw_fd(),
            old.as_ptr(),
            new_dir.as_raw_fd(),
            new.as_ptr(),
            0,
        )
    })?;
    Ok(())
}

/// Atomic rename. With `no_replace` it fails with `EEXIST` if the destination exists.
pub fn renameat(
    old_dir: BorrowedFd<'_>,
    old_name: &[u8],
    new_dir: BorrowedFd<'_>,
    new_name: &[u8],
    no_replace: bool,
) -> io::Result<()> {
    let (old, new) = (cstr(old_name)?, cstr(new_name)?);
    let (od, nd) = (old_dir.as_raw_fd(), new_dir.as_raw_fd());
    // SAFETY: see module docs.
    #[cfg(target_os = "linux")]
    let r = unsafe {
        if no_replace {
            const RENAME_NOREPLACE: libc::c_uint = 1;
            libc::syscall(
                libc::SYS_renameat2,
                od,
                old.as_ptr(),
                nd,
                new.as_ptr(),
                RENAME_NOREPLACE,
            ) as libc::c_int
        } else {
            libc::renameat(od, old.as_ptr(), nd, new.as_ptr())
        }
    };
    // SAFETY: see module docs.
    #[cfg(target_os = "macos")]
    let r = unsafe {
        if no_replace {
            libc::renameatx_np(od, old.as_ptr(), nd, new.as_ptr(), libc::RENAME_EXCL)
        } else {
            libc::renameat(od, old.as_ptr(), nd, new.as_ptr())
        }
    };
    cvt(r)?;
    Ok(())
}

pub fn fchmod(fd: BorrowedFd<'_>, mode: u32) -> io::Result<()> {
    // SAFETY: see module docs.
    cvt(unsafe { libc::fchmod(fd.as_raw_fd(), mode as libc::mode_t) })?;
    Ok(())
}

/// `fchmodat`; with `nofollow` the platform decides whether a symlink's own mode can change.
pub fn fchmodat(dir: BorrowedFd<'_>, name: &[u8], mode: u32, nofollow: bool) -> io::Result<()> {
    let name = cstr(name)?;
    let flags = if nofollow {
        libc::AT_SYMLINK_NOFOLLOW
    } else {
        0
    };
    // SAFETY: see module docs.
    cvt(unsafe { libc::fchmodat(dir.as_raw_fd(), name.as_ptr(), mode as libc::mode_t, flags) })?;
    Ok(())
}

/// A time to set: leave alone, the current time, or an exact instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeSpec {
    Omit,
    Now,
    At(i64, u32),
}

fn timespec(t: TimeSpec) -> libc::timespec {
    // SAFETY: `libc::timespec` is plain old data; all zero bytes are a valid value.
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    match t {
        TimeSpec::Omit => ts.tv_nsec = libc::UTIME_OMIT,
        TimeSpec::Now => ts.tv_nsec = libc::UTIME_NOW,
        TimeSpec::At(s, n) => {
            ts.tv_sec = s as libc::time_t;
            ts.tv_nsec = libc::c_long::from(n);
        }
    }
    ts
}

/// `utimensat` that never follows a final symlink.
pub fn utimensat(
    dir: BorrowedFd<'_>,
    name: &[u8],
    atime: TimeSpec,
    mtime: TimeSpec,
) -> io::Result<()> {
    let name = cstr(name)?;
    let ts = [timespec(atime), timespec(mtime)];
    // SAFETY: see module docs; `ts` holds the two timespecs utimensat reads.
    cvt(unsafe {
        libc::utimensat(
            dir.as_raw_fd(),
            name.as_ptr(),
            ts.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })?;
    Ok(())
}

/// Sets times through a descriptor. `symlink` marks a descriptor from `OPEN_SYMLINK`.
pub fn utimens_fd(
    fd: BorrowedFd<'_>,
    atime: TimeSpec,
    mtime: TimeSpec,
    symlink: bool,
) -> io::Result<()> {
    let ts = [timespec(atime), timespec(mtime)];
    #[cfg(target_os = "linux")]
    if symlink {
        let empty = cstr(b"")?;
        // SAFETY: see module docs; an O_PATH descriptor needs AT_EMPTY_PATH.
        cvt(unsafe {
            libc::utimensat(
                fd.as_raw_fd(),
                empty.as_ptr(),
                ts.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        })?;
        return Ok(());
    }
    let _ = symlink;
    // SAFETY: see module docs.
    cvt(unsafe { libc::futimens(fd.as_raw_fd(), ts.as_ptr()) })?;
    Ok(())
}

/// Names in a directory, without `.` and `..`, in the filesystem's own order.
pub fn list_dir(fd: BorrowedFd<'_>) -> io::Result<Vec<Vec<u8>>> {
    // SAFETY: see module docs.
    let dup = cvt(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) })?;
    // SAFETY: `dup` is a directory descriptor we own; fdopendir takes it over on success.
    let dirp = unsafe { libc::fdopendir(dup) };
    if dirp.is_null() {
        let e = io::Error::last_os_error();
        // SAFETY: fdopendir failed, so `dup` is still ours to close.
        unsafe { libc::close(dup) };
        return Err(e);
    }
    // SAFETY: `dirp` is a valid open stream until closedir below.
    unsafe { libc::rewinddir(dirp) };
    let mut names = Vec::new();
    let result = loop {
        clear_errno();
        // SAFETY: `dirp` is a valid open stream.
        let ent = unsafe { libc::readdir(dirp) };
        if ent.is_null() {
            let e = io::Error::last_os_error();
            break if e.raw_os_error() == Some(0) {
                Ok(())
            } else {
                Err(e)
            };
        }
        // SAFETY: readdir returned a live dirent whose d_name is NUL terminated; we copy it out
        // before the next readdir call.
        let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            names.push(name.to_vec());
        }
    };
    // SAFETY: closes the stream and the descriptor it owns, exactly once.
    unsafe { libc::closedir(dirp) };
    result.map(|()| names)
}

fn clear_errno() {
    // SAFETY: the errno location is always valid for the calling thread.
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location() = 0;
    }
    // SAFETY: the errno location is always valid for the calling thread.
    #[cfg(target_os = "macos")]
    unsafe {
        *libc::__error() = 0;
    }
}

/// File system totals from `fstatvfs`.
#[derive(Clone, Copy, Debug)]
pub struct VfsStat {
    pub block_size: u64,
    pub blocks: u64,
    pub blocks_free: u64,
    pub blocks_available: u64,
    pub files: u64,
    pub files_free: u64,
    pub name_max: u64,
}

#[allow(clippy::unnecessary_cast)]
pub fn fstatvfs(fd: BorrowedFd<'_>) -> io::Result<VfsStat> {
    // SAFETY: `libc::statvfs` is plain old data; all zero bytes are a valid value.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: see module docs.
    cvt(unsafe { libc::fstatvfs(fd.as_raw_fd(), &mut s) })?;
    Ok(VfsStat {
        block_size: if s.f_frsize > 0 {
            s.f_frsize as u64
        } else {
            s.f_bsize as u64
        },
        blocks: s.f_blocks as u64,
        blocks_free: s.f_bfree as u64,
        blocks_available: s.f_bavail as u64,
        files: s.f_files as u64,
        files_free: s.f_ffree as u64,
        name_max: s.f_namemax as u64,
    })
}

/// Raises the soft limit on open descriptors as far as the platform allows (best effort).
pub fn raise_nofile_limit() {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `lim` is a valid rlimit for both calls.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return;
        }
        let want = lim.rlim_max.min(1 << 16);
        if lim.rlim_cur < want {
            lim.rlim_cur = want;
            let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
        }
    }
}

/// Where an extended attribute call is aimed.
#[derive(Debug)]
pub enum XTarget<'a> {
    Fd(BorrowedFd<'a>),
    /// A path resolved without following the final symlink (used for symlinks on Linux,
    /// where a symlink cannot be opened for `fgetxattr`).
    #[cfg(target_os = "linux")]
    Link(CString),
}

impl XTarget<'_> {
    /// Aims at `name` inside directory `dir` through `/proc/self/fd`.
    #[cfg(target_os = "linux")]
    pub fn link(dir: std::os::fd::RawFd, name: &[u8]) -> io::Result<XTarget<'static>> {
        let mut path = format!("/proc/self/fd/{dir}/").into_bytes();
        path.extend_from_slice(name);
        Ok(XTarget::Link(cstr(&path)?))
    }
}

const XATTR_BUF_MAX: usize = 1 << 20;

fn raw_get(t: &XTarget<'_>, name: &CStr, buf: *mut libc::c_void, len: usize) -> isize {
    // SAFETY: see module docs; `buf` is null with len 0 or points at `len` writable bytes.
    unsafe {
        match t {
            #[cfg(target_os = "linux")]
            XTarget::Fd(fd) => libc::fgetxattr(fd.as_raw_fd(), name.as_ptr(), buf, len),
            #[cfg(target_os = "macos")]
            XTarget::Fd(fd) => libc::fgetxattr(fd.as_raw_fd(), name.as_ptr(), buf, len, 0, 0),
            #[cfg(target_os = "linux")]
            XTarget::Link(p) => libc::lgetxattr(p.as_ptr(), name.as_ptr(), buf, len),
        }
    }
}

fn raw_list(t: &XTarget<'_>, buf: *mut libc::c_char, len: usize) -> isize {
    // SAFETY: see module docs; `buf` is null with len 0 or points at `len` writable bytes.
    unsafe {
        match t {
            #[cfg(target_os = "linux")]
            XTarget::Fd(fd) => libc::flistxattr(fd.as_raw_fd(), buf, len),
            #[cfg(target_os = "macos")]
            XTarget::Fd(fd) => libc::flistxattr(fd.as_raw_fd(), buf, len, 0),
            #[cfg(target_os = "linux")]
            XTarget::Link(p) => libc::llistxattr(p.as_ptr(), buf, len),
        }
    }
}

/// Reads an extended attribute in full.
pub fn getxattr(t: &XTarget<'_>, name: &[u8]) -> io::Result<Vec<u8>> {
    let name = cstr(name)?;
    loop {
        let size = cvt_size(raw_get(t, &name, std::ptr::null_mut(), 0))?;
        let mut buf = vec![0u8; size];
        let got = raw_get(t, &name, buf.as_mut_ptr().cast(), buf.len());
        match cvt_size(got) {
            Ok(n) => {
                buf.truncate(n);
                return Ok(buf);
            }
            Err(e) if e.raw_os_error() == Some(libc::ERANGE) && size < XATTR_BUF_MAX => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Sets an extended attribute; `create` and `replace` map to `XATTR_CREATE` and `XATTR_REPLACE`.
pub fn setxattr(
    t: &XTarget<'_>,
    name: &[u8],
    value: &[u8],
    create: bool,
    replace: bool,
) -> io::Result<()> {
    let name = cstr(name)?;
    let flags =
        if create { libc::XATTR_CREATE } else { 0 } | if replace { libc::XATTR_REPLACE } else { 0 };
    let flags = flags as libc::c_int;
    // SAFETY: see module docs; `value` is a valid slice (possibly empty).
    let r = unsafe {
        match t {
            #[cfg(target_os = "linux")]
            XTarget::Fd(fd) => libc::fsetxattr(
                fd.as_raw_fd(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                flags,
            ),
            #[cfg(target_os = "macos")]
            XTarget::Fd(fd) => libc::fsetxattr(
                fd.as_raw_fd(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
                flags,
            ),
            #[cfg(target_os = "linux")]
            XTarget::Link(p) => libc::lsetxattr(
                p.as_ptr(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                flags,
            ),
        }
    };
    cvt(r).map(|_| ())
}

/// Attribute names, each once, in the order the filesystem lists them.
pub fn listxattr(t: &XTarget<'_>) -> io::Result<Vec<Vec<u8>>> {
    loop {
        let size = cvt_size(raw_list(t, std::ptr::null_mut(), 0))?;
        let mut buf = vec![0u8; size];
        match cvt_size(raw_list(t, buf.as_mut_ptr().cast(), buf.len())) {
            Ok(n) => {
                buf.truncate(n);
                return Ok(buf
                    .split(|&b| b == 0)
                    .filter(|s| !s.is_empty())
                    .map(<[u8]>::to_vec)
                    .collect());
            }
            Err(e) if e.raw_os_error() == Some(libc::ERANGE) => continue,
            Err(e) => return Err(e),
        }
    }
}

pub fn removexattr(t: &XTarget<'_>, name: &[u8]) -> io::Result<()> {
    let name = cstr(name)?;
    // SAFETY: see module docs.
    let r = unsafe {
        match t {
            #[cfg(target_os = "linux")]
            XTarget::Fd(fd) => libc::fremovexattr(fd.as_raw_fd(), name.as_ptr()),
            #[cfg(target_os = "macos")]
            XTarget::Fd(fd) => libc::fremovexattr(fd.as_raw_fd(), name.as_ptr(), 0),
            #[cfg(target_os = "linux")]
            XTarget::Link(p) => libc::lremovexattr(p.as_ptr(), name.as_ptr()),
        }
    };
    cvt(r).map(|_| ())
}
