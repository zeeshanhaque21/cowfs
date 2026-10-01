//! Mounts a `PathVfs` over the macOS NFS loopback, so `bench/gates.py` can be pointed at a
//! cowfs mount instead of a plain directory.
//!
//! ```text
//! mount-nfs BACKING_DIR MOUNTPOINT
//! ```
//!
//! Prints `mounted <mountpoint> pid <pid>`, then serves until SIGTERM, SIGINT or SIGHUP, which
//! unmount before exiting. No daemon mode: the caller owns the process.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cowfs_nfs::{
    install_signal_cleanup, mount_nfs_available, sweep_stale_mounts, Mount, MountOptions,
};
use cowfs_vfs_path::PathVfs;

const USAGE: &str = "usage: mount-nfs BACKING_DIR MOUNTPOINT";

fn main() {
    let mut args = std::env::args_os().skip(1);
    let backing = PathBuf::from(args.next().expect(USAGE));
    let mountpoint = PathBuf::from(args.next().expect(USAGE));
    assert!(args.next().is_none(), "{USAGE}");
    assert!(mount_nfs_available(), "mount_nfs is not usable here");

    install_signal_cleanup().expect("install signal cleanup");
    let sweep_root = std::env::var("COWFS_BENCH_SWEEP_PREFIX").unwrap_or_else(|_| {
        mountpoint
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    });
    match sweep_stale_mounts(Path::new(&sweep_root)) {
        Ok(stale) if !stale.is_empty() => eprintln!("swept {stale:?}"),
        Ok(_) => {}
        Err(e) => eprintln!("sweep failed: {e}"),
    }

    let opts = MountOptions {
        command_timeout: Duration::from_secs(60),
        ..Default::default()
    };
    let vfs = Arc::new(PathVfs::new(&backing).expect("open backing dir"));
    let mount = Mount::new(vfs, &mountpoint, opts).expect("mount");

    println!(
        "mounted {} pid {}",
        mount.mountpoint().display(),
        std::process::id()
    );
    use std::io::Write;
    std::io::stdout().flush().ok();

    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
