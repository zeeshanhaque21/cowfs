//! A forked child that uses a `PathVfs` whose worker thread the parent started must not hang.
//!
//! `cargo test -p cowfs-vfs-path --test fork`

#![cfg(target_os = "linux")]
#![allow(unsafe_code)]

use cowfs_vfs::{Vfs, ROOT_INO};
use cowfs_vfs_path::PathVfs;

#[test]
fn a_forked_child_can_still_mkdir_after_the_worker_started() {
    let dir = std::env::temp_dir().join(format!("cowfs-fork-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fs = PathVfs::new(&dir).unwrap();
    fs.mkdir(ROOT_INO, b"parent", 0o755).unwrap(); // starts the worker
                                                   // SAFETY: the child only calls PathVfs, `alarm` and `_exit`; it never returns into the
                                                   // test harness. `alarm` turns a hang into SIGALRM so the failure is visible, not a stall.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        unsafe { libc::alarm(10) };
        let ok = fs.mkdir(ROOT_INO, b"child", 0o755).is_ok();
        unsafe { libc::_exit(if ok { 0 } else { 1 }) };
    }
    assert!(pid > 0, "fork failed");
    let mut status = 0;
    assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "the child did not finish its mkdir (wait status {status:#x}; SIGALRM means it hung)"
    );
    assert!(dir.join("child").is_dir());
    drop(fs);
    cowfs_vfs_path::force_remove_dir_all(&dir);
}
