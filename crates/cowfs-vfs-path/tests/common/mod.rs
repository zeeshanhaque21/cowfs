use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use cowfs_vfs::Vfs;
use cowfs_vfs_path::{force_remove_dir_all, PathVfs};
use cowfs_vfs_test::conformance::{run_all, Options};

static PENDING: AtomicUsize = AtomicUsize::new(0);

struct Cleanup(PathBuf);

impl Cleanup {
    fn new(dir: PathBuf) -> Self {
        PENDING.fetch_add(1, Ordering::SeqCst);
        Self(dir)
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        force_remove_dir_all(&self.0);
        PENDING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The runner drops a check's filesystem on its own thread after the result is out, so without
/// waiting here the previous check's directory tree is still being deleted while the next check
/// measures free space.
fn wait_for_cleanup() {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    while PENDING.load(Ordering::SeqCst) > 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// Runs the whole suite with each check on a fresh empty directory under `base`, prints the
/// table, and fails only when `COWFS_PATHVFS_STRICT=1` and a check failed.
pub fn run_suite(base: &Path) {
    static N: AtomicU32 = AtomicU32::new(0);
    let factory = || -> Arc<dyn Vfs> {
        wait_for_cleanup();
        let dir = base.join(format!(
            "cowfs-pathvfs-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create the per-check directory");
        let fs = PathVfs::new(&dir)
            .expect("open the per-check directory")
            .keep_alive(Cleanup::new(dir));
        Arc::new(fs)
    };
    println!("suite base directory: {}", base.display());
    let mut opts = Options::from_env();
    // A real kernel: a device node needs root, so non-root runs check the fifo and the socket.
    opts.devices_need_privilege = true;
    if opts.timeout.is_none() {
        // One Posix check creates 8,000 hardlink pairs in one directory and lists them six times
        // at page sizes from 1 up. On this machine a single `linkat` costs about 0.9 ms, so the
        // check needs minutes, and the suite's 60 s default would report a hang that says
        // nothing about the filesystem.
        opts.timeout = std::time::Duration::from_secs(300).into();
    }
    // macOS has no `mknodat`, so PathVfs::mknod is Linux only.
    #[cfg(not(target_os = "linux"))]
    for c in cowfs_vfs_test::conformance::all_checks()
        .iter()
        .filter(|c| c.category == "special")
    {
        opts.skip.push((
            c.name.into(),
            "PathVfs::mknod needs mknodat, which only Linux has".into(),
        ));
    }
    let report = run_all(&factory, &opts);
    println!("{}", report.table());
    wait_for_cleanup();
    if std::env::var("COWFS_PATHVFS_STRICT").is_ok_and(|v| v == "1") {
        assert!(report.passed(), "some checks failed");
    }
}
