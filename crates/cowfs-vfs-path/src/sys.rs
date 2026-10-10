//! The only module with `unsafe`: thin, descriptor-relative wrappers over libc.
//!
//! SAFETY, for every block below: pointers come from live `CString`s, slices or locals that
//! outlive the call, lengths are the slice lengths, and file descriptors are borrowed for the
//! duration of the call (`BorrowedFd`) or owned by the caller (`OwnedFd`).

use std::ffi::{CStr, CString};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    /// The host's `st_rdev`.
    pub rdev: u64,
    pub atime: (i64, u32),
    pub mtime: (i64, u32),
    pub ctime: (i64, u32),
}

// `mode_t` is `u16` on macOS and `u32` on Linux: the casts are needed on one and useless on the other.
#[allow(clippy::unnecessary_cast)]
mod modes {
    pub const S_IFMT: u32 = libc::S_IFMT as u32;
    pub const S_IFREG: u32 = libc::S_IFREG as u32;
    pub const S_IFDIR: u32 = libc::S_IFDIR as u32;
    pub const S_IFLNK: u32 = libc::S_IFLNK as u32;
    pub const S_IFIFO: u32 = libc::S_IFIFO as u32;
    pub const S_IFSOCK: u32 = libc::S_IFSOCK as u32;
    pub const S_IFCHR: u32 = libc::S_IFCHR as u32;
    pub const S_IFBLK: u32 = libc::S_IFBLK as u32;
}
pub use modes::{S_IFBLK, S_IFCHR, S_IFDIR, S_IFIFO, S_IFLNK, S_IFREG, S_IFSOCK};

impl Stat {
    pub fn file_type(&self) -> u32 {
        self.mode & modes::S_IFMT
    }
}

/// The number a stat reports. A filesystem hands a freed number to another file, so this is
/// only the file's identity while the file is alive.
#[cfg(not(test))]
fn reported_ino(ino: u64) -> u64 {
    ino
}

#[cfg(test)]
thread_local! {
    /// The one backing number to report as another, keyed by the number the filesystem gave.
    static RENAMED_INO: std::cell::Cell<Option<(u64, u64)>> = const { std::cell::Cell::new(None) };
}

/// Reports `real` as `fake` in every stat until the guard is dropped. Keyed by the real number,
/// so only the file the test picks is affected, and a filesystem that recycles a freed inode
/// number can be stood in for on one that never does.
#[cfg(test)]
pub fn fake_inode(real: u64, fake: u64) -> FakeInode {
    RENAMED_INO.with(|c| c.set(Some((real, fake))));
    FakeInode(())
}

#[cfg(test)]
pub struct FakeInode(());

#[cfg(test)]
impl Drop for FakeInode {
    fn drop(&mut self) {
        RENAMED_INO.with(|c| c.set(None));
    }
}

#[cfg(test)]
fn reported_ino(ino: u64) -> u64 {
    RENAMED_INO.with(|c| match c.get() {
        Some((real, fake)) if real == ino => fake,
        _ => ino,
    })
}

#[allow(clippy::unnecessary_cast)]
fn widen(st: &libc::stat) -> Stat {
    let ts = |s: i64, n: i64| (s, n as u32);
    Stat {
        dev: st.st_dev as u64,
        ino: reported_ino(st.st_ino as u64),
        mode: st.st_mode as u32,
        nlink: st.st_nlink as u64,
        uid: st.st_uid,
        gid: st.st_gid,
        size: st.st_size as u64,
        blocks: st.st_blocks as u64,
        rdev: st.st_rdev as u64,
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

/// Flags that open a fifo, socket or device node itself without blocking or touching the device.
#[cfg(target_os = "linux")]
pub const OPEN_SPECIAL: libc::c_int = OPEN_SYMLINK | libc::O_NONBLOCK;

/// The cowfs device number `(major << 32) | minor` of a host `st_rdev`.
#[cfg(target_os = "linux")]
pub fn host_to_cowfs(rdev: u64) -> u64 {
    let major = ((rdev >> 8) & 0xfff) | ((rdev >> 32) & !0xfff);
    let minor = (rdev & 0xff) | ((rdev >> 12) & !0xff);
    (major << 32) | (minor & 0xffff_ffff)
}

#[cfg(target_os = "macos")]
pub fn host_to_cowfs(rdev: u64) -> u64 {
    let rdev = rdev & 0xffff_ffff;
    (((rdev >> 24) & 0xff) << 32) | (rdev & 0xff_ffff)
}

/// The host `st_rdev` of a cowfs device number; bits the host cannot carry are dropped.
#[cfg(target_os = "linux")]
pub fn cowfs_to_host(rdev: u64) -> u64 {
    let (major, minor) = (rdev >> 32, rdev & 0xffff_ffff);
    ((major & 0xfff) << 8) | ((major & !0xfff) << 32) | (minor & 0xff) | ((minor & !0xff) << 12)
}

#[cfg(target_os = "macos")]
pub fn cowfs_to_host(rdev: u64) -> u64 {
    (((rdev >> 32) & 0xff) << 24) | (rdev & 0xff_ffff)
}

/// `mknodat` of a fifo, socket or device. macOS has no `mknodat`, so it is unsupported there.
#[cfg(target_os = "linux")]
pub fn mknodat(dir: BorrowedFd<'_>, name: &[u8], mode: u32, rdev: u64) -> io::Result<()> {
    let name = cstr(name)?;
    // SAFETY: see module docs.
    cvt(unsafe {
        libc::mknodat(
            dir.as_raw_fd(),
            name.as_ptr(),
            mode as libc::mode_t,
            rdev as libc::dev_t,
        )
    })?;
    Ok(())
}

/// What is known of `unshare(CLONE_FS)` here: `true` once it worked, `false` once refused for good.
/// Unset while unknown, including after a transient failure, which the next call retries.
#[cfg(target_os = "linux")]
static PRIVATE_UMASK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Whether an `unshare` errno is a standing refusal, worth remembering (each retry costs a
/// thread): a seccomp filter answers `EPERM`, `EACCES` or `ENOSYS`, and `EINVAL` means the
/// kernel or sandbox does not support the flag. Anything else (`ENOMEM`, `EAGAIN`, ...) is
/// transient: it must not downgrade every later call to the racy chmod path.
#[cfg(target_os = "linux")]
fn refusal_is_permanent(errno: i32) -> bool {
    matches!(
        errno,
        libc::EPERM | libc::EACCES | libc::EINVAL | libc::ENOSYS
    )
}

/// Runs `f` on a short-lived thread that detaches its own umask with `unshare(CLONE_FS)` and
/// sets it to 0, so a creation call there gets exactly the mode it names (a parent's default ACL
/// still applies, as it would to a native call). The process umask is never touched. `Ok(None)`
/// means no private umask is available (a seccomp filter may refuse `unshare`, and macOS has no
/// per thread umask) and `f` did not run.
#[cfg(target_os = "linux")]
pub fn with_private_umask<R: Send>(
    f: impl FnOnce() -> io::Result<R> + Send,
) -> io::Result<Option<R>> {
    run_with_private_umask(&PRIVATE_UMASK, detach_umask, f)
}

#[cfg(target_os = "linux")]
fn detach_umask() -> Result<(), i32> {
    // SAFETY: `unshare(CLONE_FS)` gives only this thread a private copy of its cwd, root and
    // umask; it touches no memory. The thread ends right after.
    if unsafe { libc::unshare(libc::CLONE_FS) } == 0 {
        // SAFETY: `umask` cannot fail; after the unshare it affects this thread only.
        unsafe { libc::umask(0) };
        Ok(())
    } else {
        Err(io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EPERM))
    }
}

/// `with_private_umask` with the cache and the unshare-then-umask(0) step passed in, so a test can
/// inject one (a test must never call `umask`: it would change the whole test process).
#[cfg(target_os = "linux")]
fn run_with_private_umask<R: Send>(
    known: &std::sync::OnceLock<bool>,
    unshare: impl FnOnce() -> Result<(), i32> + Send,
    f: impl FnOnce() -> io::Result<R> + Send,
) -> io::Result<Option<R>> {
    if known.get() == Some(&false) {
        return Ok(None);
    }
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .name("cowfs-umask0".into())
            .spawn_scoped(s, || {
                if let Err(errno) = unshare() {
                    if refusal_is_permanent(errno) {
                        let _ = known.set(false);
                    }
                    return Ok(None);
                }
                let _ = known.set(true);
                f().map(Some)
            })?
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("the umask-0 thread panicked")))
    })
}

#[cfg(not(target_os = "linux"))]
pub fn with_private_umask<R: Send>(
    _: impl FnOnce() -> io::Result<R> + Send,
) -> io::Result<Option<R>> {
    Ok(None)
}

/// Whether a thread may detach its own umask, which `mknodat_exact` and `mkdirat_exact` need.
#[cfg(target_os = "linux")]
pub fn private_umask_available() -> bool {
    if PRIVATE_UMASK.get().is_none() {
        let _ = with_private_umask(|| Ok(()));
    }
    PRIVATE_UMASK.get() == Some(&true)
}

/// A job for the umask-0 worker: one creation call, run on the worker's thread.
#[cfg(target_os = "linux")]
type Job = Box<dyn FnOnce() -> io::Result<()> + Send>;

/// A live worker thread: it did `unshare(CLONE_FS)` and `umask(0)` once, then runs jobs sent to it
/// until its sender is dropped. One job at a time: the caller waits for the answer.
#[cfg(target_os = "linux")]
struct Worker {
    jobs: std::sync::mpsc::Sender<Job>,
    done: std::sync::mpsc::Receiver<io::Result<()>>,
    thread: std::thread::JoinHandle<()>,
    /// The process that started it. After `fork` the child holds the channels but not the thread.
    pid: u32,
}

/// The umask-0 worker of one `PathVfs`, started on the first creation call and joined on drop.
/// It replaces a thread spawned per call, which cost about 29 us each. The semantics are those
/// of `with_private_umask`: the worker's umask is thread-private (`CLONE_FS`), the process umask
/// is never touched, a refusal of `unshare` means "no private umask" (`Ok(false)`, nothing made),
/// and a panic inside a job comes back as an error without killing the worker. After a `fork`
/// only the forking thread exists in the child, so a worker started by the parent is abandoned
/// (leaked, never joined) and a fresh one is started on the next call, rather than hanging.
pub struct UmaskWorker {
    #[cfg(target_os = "linux")]
    live: Option<Worker>,
    /// What is known of `unshare(CLONE_FS)` here, and the unshare-then-umask(0) step; both
    /// injectable so a test can drive the refusal rule (a test must never call `umask`).
    #[cfg(target_os = "linux")]
    known: &'static std::sync::OnceLock<bool>,
    #[cfg(target_os = "linux")]
    unshare: fn() -> Result<(), i32>,
}

impl Default for UmaskWorker {
    fn default() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            live: None,
            #[cfg(target_os = "linux")]
            known: &PRIVATE_UMASK,
            #[cfg(target_os = "linux")]
            unshare: detach_umask,
        }
    }
}

impl std::fmt::Debug for UmaskWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UmaskWorker").finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
impl UmaskWorker {
    fn start(&self) -> io::Result<Option<Worker>> {
        let unshare = self.unshare;
        let (jobs, job_rx) = std::sync::mpsc::channel::<Job>();
        let (done_tx, done) = std::sync::mpsc::channel::<io::Result<()>>();
        let thread = std::thread::Builder::new()
            .name("cowfs-umask0".into())
            .spawn(move || {
                let started = unshare().map_err(io::Error::from_raw_os_error);
                let ok = started.is_ok();
                if done_tx.send(started).is_err() || !ok {
                    return;
                }
                for job in job_rx {
                    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job))
                        .unwrap_or_else(|_| Err(io::Error::other("the umask-0 job panicked")));
                    if done_tx.send(r).is_err() {
                        return;
                    }
                }
            })?;
        let w = Worker {
            jobs,
            done,
            thread,
            pid: std::process::id(),
        };
        match w.done.recv() {
            Ok(Ok(())) => {
                let _ = self.known.set(true);
                Ok(Some(w))
            }
            Ok(Err(e)) => {
                let _ = w.thread.join();
                if e.raw_os_error().is_some_and(refusal_is_permanent) {
                    let _ = self.known.set(false);
                }
                Ok(None)
            }
            Err(_) => Err(io::Error::other("the umask-0 thread died at start")),
        }
    }

    /// Runs `f` with umask 0. `Ok(false)`: no private umask here, `f` did not run.
    fn run(&mut self, f: impl FnOnce() -> io::Result<()> + Send + 'static) -> io::Result<bool> {
        if self.known.get() == Some(&false) {
            return Ok(false);
        }
        self.forget_stale();
        if self.live.is_none() {
            self.live = self.start()?;
        }
        let Some(w) = &self.live else {
            return Ok(false);
        };
        let answer = w
            .jobs
            .send(Box::new(f))
            .ok()
            .and_then(|()| w.done.recv().ok());
        match answer {
            Some(r) => r.map(|()| true),
            None => {
                // The worker is gone (it cannot be, short of an abort): start a fresh one next time.
                if let Some(w) = self.live.take() {
                    drop(w.jobs);
                    let _ = w.thread.join();
                }
                Err(io::Error::other("the umask-0 thread died"))
            }
        }
    }

    /// Abandons a worker that a parent process started: its thread does not exist here.
    fn forget_stale(&mut self) {
        if self
            .live
            .as_ref()
            .is_some_and(|w| w.pid != std::process::id())
        {
            std::mem::forget(self.live.take());
        }
    }

    pub fn mkdirat_exact(
        &mut self,
        dir: BorrowedFd<'_>,
        name: &[u8],
        mode: u32,
    ) -> io::Result<bool> {
        let (dir, name) = (dir.try_clone_to_owned()?, name.to_vec());
        self.run(move || mkdirat(dir.as_fd(), &name, mode))
    }

    pub fn mknodat_exact(
        &mut self,
        dir: BorrowedFd<'_>,
        name: &[u8],
        mode: u32,
        rdev: u64,
    ) -> io::Result<bool> {
        let (dir, name) = (dir.try_clone_to_owned()?, name.to_vec());
        self.run(move || mknodat(dir.as_fd(), &name, mode, rdev))
    }
}

#[cfg(target_os = "linux")]
impl Drop for UmaskWorker {
    fn drop(&mut self) {
        self.forget_stale();
        if let Some(w) = self.live.take() {
            drop(w.jobs); // ends the worker's loop
            let _ = w.thread.join();
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl UmaskWorker {
    pub fn mkdirat_exact(&mut self, _: BorrowedFd<'_>, _: &[u8], _: u32) -> io::Result<bool> {
        Ok(false)
    }

    pub fn mknodat_exact(
        &mut self,
        _: BorrowedFd<'_>,
        _: &[u8],
        _: u32,
        _: u64,
    ) -> io::Result<bool> {
        Ok(false)
    }
}

/// `fchmodat` by name, for a node that has no descriptor `fchmod` accepts (an `O_PATH` one).
/// Never follows a final symlink: one planted at the name makes this fail (Linux, `ENOTSUP`)
/// or change the link itself (macOS), not its target.
pub fn fchmodat(dir: BorrowedFd<'_>, name: &[u8], mode: u32) -> io::Result<()> {
    let name = cstr(name)?;
    // SAFETY: see module docs.
    cvt(unsafe {
        libc::fchmodat(
            dir.as_raw_fd(),
            name.as_ptr(),
            mode as libc::mode_t,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })?;
    Ok(())
}

/// `chmod` through an `O_PATH` descriptor, via its `/proc/self/fd` name.
#[cfg(target_os = "linux")]
pub fn chmod_fd(fd: BorrowedFd<'_>, mode: u32) -> io::Result<()> {
    let path = cstr(format!("/proc/self/fd/{}", fd.as_raw_fd()).as_bytes())?;
    // SAFETY: see module docs.
    cvt(unsafe { libc::chmod(path.as_ptr(), mode as libc::mode_t) })?;
    Ok(())
}

/// The effective user id.
#[cfg(target_os = "linux")]
pub fn geteuid() -> u32 {
    // SAFETY: no arguments, cannot fail.
    unsafe { libc::geteuid() }
}

/// 16 bytes from the kernel's random source (`getrandom(2)`, no descriptor, never short).
#[cfg(target_os = "linux")]
pub fn random16() -> io::Result<[u8; 16]> {
    let mut b = [0u8; 16];
    // SAFETY: a valid buffer of the stated length.
    let n = unsafe { libc::getrandom(b.as_mut_ptr().cast(), b.len(), 0) };
    if n == 16 {
        Ok(b)
    } else {
        Err(if n < 0 {
            io::Error::last_os_error()
        } else {
            io::Error::from_raw_os_error(libc::EIO)
        })
    }
}

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

/// `fallocate(2)` with the kernel's mode bits. Not `posix_fallocate`: that is mode 0 only, and
/// glibc emulates it by writing zeros where the filesystem lacks support, which is the non-atomic,
/// space-allocating behaviour `Vfs::fallocate` rules out.
#[cfg(target_os = "linux")]
pub fn fallocate(fd: BorrowedFd<'_>, mode: i32, offset: i64, len: i64) -> io::Result<()> {
    // SAFETY: see module docs.
    cvt(unsafe { libc::fallocate(fd.as_raw_fd(), mode, offset, len) })?;
    Ok(())
}

/// Other platforms have no `fallocate(2)` with these modes.
#[cfg(not(target_os = "linux"))]
pub fn fallocate(_fd: BorrowedFd<'_>, _mode: i32, _offset: i64, _len: i64) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOTSUP))
}

pub fn fchmod(fd: BorrowedFd<'_>, mode: u32) -> io::Result<()> {
    // SAFETY: see module docs.
    cvt(unsafe { libc::fchmod(fd.as_raw_fd(), mode as libc::mode_t) })?;
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

/// File system totals. `fstatvfs` counts blocks in 32 bits on macOS and wraps on a large
/// file system, so macOS uses `fstatfs`, which is 64 bit.
#[cfg(target_os = "linux")]
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

/// File system totals from `fstatfs`, see the Linux variant.
#[cfg(target_os = "macos")]
pub fn fstatvfs(fd: BorrowedFd<'_>) -> io::Result<VfsStat> {
    // SAFETY: `libc::statfs` is plain old data; all zero bytes are a valid value.
    let mut s: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: see module docs.
    cvt(unsafe { libc::fstatfs(fd.as_raw_fd(), &mut s) })?;
    // SAFETY: see module docs.
    let name_max = unsafe { libc::fpathconf(fd.as_raw_fd(), libc::_PC_NAME_MAX) };
    Ok(VfsStat {
        block_size: u64::from(s.f_bsize),
        blocks: s.f_blocks,
        blocks_free: s.f_bfree,
        blocks_available: s.f_bavail,
        files: s.f_files,
        files_free: s.f_ffree,
        name_max: u64::try_from(name_max).unwrap_or(255),
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
            Err(e) if e.raw_os_error() == Some(libc::ERANGE) && size < XATTR_BUF_MAX => continue,
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

#[cfg(test)]
mod rdev_tests {
    use super::{cowfs_to_host, host_to_cowfs};

    #[test]
    fn device_numbers_round_trip_through_the_host_encoding() {
        // (major, minor) pairs every host can carry
        for (major, minor) in [(0u64, 0u64), (1, 3), (8, 16), (4, 255), (200, 70_000)] {
            let cowfs = (major << 32) | minor;
            assert_eq!(
                host_to_cowfs(cowfs_to_host(cowfs)),
                cowfs,
                "{major}:{minor}"
            );
        }
        #[cfg(target_os = "linux")]
        assert_eq!(cowfs_to_host((8 << 32) | 16), 0x810, "glibc makedev(8, 16)");
        #[cfg(target_os = "macos")]
        assert_eq!(
            cowfs_to_host((8 << 32) | 16),
            (8 << 24) | 16,
            "macOS makedev(8, 16)"
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod umask_cache_tests {
    use super::run_with_private_umask;
    use std::sync::OnceLock;

    fn try_once(known: &OnceLock<bool>, errno: Option<i32>) -> Option<u8> {
        let unshare = move || errno.map_or(Ok(()), Err);
        run_with_private_umask(known, unshare, || Ok(7u8)).unwrap()
    }

    #[test]
    fn a_transient_unshare_failure_is_retried_not_cached() {
        for errno in [libc::ENOMEM, libc::EAGAIN] {
            let known = OnceLock::new();
            assert_eq!(
                try_once(&known, Some(errno)),
                None,
                "the failing call falls back"
            );
            assert_eq!(known.get(), None, "errno {errno} is not remembered");
            assert_eq!(
                try_once(&known, None),
                Some(7),
                "the next call gets a private umask"
            );
            assert_eq!(known.get(), Some(&true));
        }
    }

    #[test]
    fn a_permanent_refusal_is_cached_and_not_retried() {
        for errno in [libc::EPERM, libc::EACCES, libc::EINVAL, libc::ENOSYS] {
            let known = OnceLock::new();
            assert_eq!(try_once(&known, Some(errno)), None);
            assert_eq!(known.get(), Some(&false), "errno {errno} is remembered");
            // Even a call whose unshare would now succeed does not try again.
            assert_eq!(try_once(&known, None), None, "errno {errno}: no retry");
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod umask_worker_tests {
    use super::UmaskWorker;

    #[test]
    fn a_panicking_job_is_an_error_and_the_worker_lives_on() {
        let mut w = UmaskWorker::default();
        if !w.run(|| Ok(())).unwrap() {
            return; // unshare(CLONE_FS) is refused here: no worker to test
        }
        let e = w.run(|| panic!("boom")).unwrap_err();
        assert!(e.to_string().contains("panicked"), "{e}");
        assert!(w.run(|| Ok(())).unwrap(), "the next job still runs");
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s = seen.clone();
        assert!(w
            .run(move || {
                s.store(true, std::sync::atomic::Ordering::SeqCst);
                Err(std::io::Error::from_raw_os_error(libc::EEXIST))
            })
            .is_err());
        assert!(seen.load(std::sync::atomic::Ordering::SeqCst));
    }

    static ERRNO: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

    fn fake_unshare() -> Result<(), i32> {
        match ERRNO.load(std::sync::atomic::Ordering::SeqCst) {
            0 => Ok(()),
            e => Err(e),
        }
    }

    fn worker() -> UmaskWorker {
        UmaskWorker {
            live: None,
            known: Box::leak(Box::default()),
            unshare: fake_unshare,
        }
    }

    /// The refusal rule of `refusal_is_permanent`, inside the worker. One test owns `ERRNO`.
    #[test]
    fn permanent_refusals_are_cached_and_transient_ones_retried() {
        use std::sync::atomic::Ordering::SeqCst;
        for errno in [libc::EPERM, libc::EACCES, libc::ENOSYS, libc::EINVAL] {
            let mut w = worker();
            ERRNO.store(errno, SeqCst);
            assert!(!w.run(|| Ok(())).unwrap(), "errno {errno}: nothing ran");
            assert_eq!(w.known.get(), Some(&false), "errno {errno} is remembered");
            ERRNO.store(0, SeqCst);
            assert!(!w.run(|| Ok(())).unwrap(), "errno {errno}: no retry");
            assert!(w.live.is_none());
        }
        for errno in [libc::ENOMEM, libc::EAGAIN] {
            let mut w = worker();
            ERRNO.store(errno, SeqCst);
            assert!(!w.run(|| Ok(())).unwrap());
            assert_eq!(w.known.get(), None, "errno {errno} is not remembered");
            assert!(
                w.live.is_none(),
                "a worker that failed to start is not kept"
            );
            ERRNO.store(0, SeqCst);
            assert!(
                w.run(|| Ok(())).unwrap(),
                "errno {errno}: the next call retries"
            );
            assert_eq!(w.known.get(), Some(&true));
        }
        ERRNO.store(0, SeqCst);
    }

    #[test]
    fn dropping_the_worker_joins_its_thread() {
        let mut w = UmaskWorker::default();
        if !w.run(|| Ok(())).unwrap() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        w.run(move || {
            // /proc/thread-self -> <pid>/task/<tid>: the worker's own thread id
            let _ = tx.send(std::fs::read_link("/proc/thread-self"));
            Ok(())
        })
        .unwrap();
        let tid = rx.recv().unwrap().unwrap();
        let task = std::path::Path::new("/proc/self/task").join(tid.file_name().unwrap());
        assert!(task.exists());
        drop(w);
        // join returns when the thread has exited; the kernel drops its /proc entry a moment later
        let gone = (0..200).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(5));
            !task.exists()
        });
        assert!(gone, "the worker thread outlived its owner");
    }
}
