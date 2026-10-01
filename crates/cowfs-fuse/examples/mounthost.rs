//! Mounts an empty `MemVfs` at the given directory, cleans up on signals, and waits. Used by the
//! signal tests. Linux only.

#[cfg(target_os = "linux")]
fn main() {
    use std::sync::Arc;

    let dir = std::env::args()
        .nth(1)
        .expect("usage: mounthost <dir> [options]");
    let opts = std::env::args().nth(2).unwrap_or_default();
    Mount::install_signal_cleanup().expect("signal handlers");
    let vfs = Arc::new(cowfs_vfs_test::MemVfs::new());
    let _mount = Mount::new(vfs, dir, opts.parse().expect("options")).expect("mount");
    println!("READY");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

#[cfg(target_os = "linux")]
use cowfs_fuse::Mount;

#[cfg(not(target_os = "linux"))]
fn main() {}
