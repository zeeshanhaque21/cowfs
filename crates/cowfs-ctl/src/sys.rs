//! The only unsafe code in the control crates: two libc calls with no Rust-side invariants.
#![allow(unsafe_code)]

use std::io;
use std::os::unix::net::UnixStream;

/// The real uid of this process.
pub fn current_uid() -> u32 {
    // SAFETY: getuid takes no arguments, touches no memory and cannot fail.
    unsafe { libc::getuid() }
}

/// The uid of the process on the other end of `stream`, as recorded by the kernel at connect time.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: fd is a live socket owned by `stream`; `cred` and `len` are valid for writes and
    // `len` is the size of `cred`.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(cred).cast(),
            &mut len,
        )
    };
    if rc == 0 {
        Ok(cred.uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The uid of the process on the other end of `stream`, as recorded by the kernel at connect time.
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd"
))]
pub fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    use std::os::fd::AsRawFd;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: fd is a live socket owned by `stream`; `uid` and `gid` are valid for writes.
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if rc == 0 {
        Ok(uid)
    } else {
        Err(io::Error::last_os_error())
    }
}
