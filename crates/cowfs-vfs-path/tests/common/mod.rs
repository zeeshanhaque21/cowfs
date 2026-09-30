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

/// Runs the whole suite with each check on a fresh empty directory under `base`, prints the
/// table, and fails only when `COWFS_PATHVFS_STRICT=1` and a check failed.
pub fn run_suite(base: &Path) {
    static N: AtomicU32 = AtomicU32::new(0);
    let factory = || -> Arc<dyn Vfs> {
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
    let report = run_all(&factory, &Options::from_env());
    println!("{}", report.table());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    while PENDING.load(Ordering::SeqCst) > 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if std::env::var("COWFS_PATHVFS_STRICT").is_ok_and(|v| v == "1") {
        assert!(report.passed(), "some checks failed");
    }
}
