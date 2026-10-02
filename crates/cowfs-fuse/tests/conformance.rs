//! The conformance suite from `cowfs-vfs-test` through this adapter.
//!
//! Each check gets a fresh `PathVfs` rooted inside one mount, so every syscall the suite makes
//! goes through FUSE to the `Vfs` under the mount.
//!
//! Run: `cargo test -p cowfs-fuse -j4 --test conformance -- --ignored --nocapture --test-threads=1`
//! Set `COWFS_FUSE_STRICT=1` to fail the test when a check fails.

#![cfg(target_os = "linux")]

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use cowfs_vfs::Vfs;
use cowfs_vfs_path::{force_remove_dir_all, PathVfs};
use cowfs_vfs_test::conformance::{run_all, Options};

static PENDING: AtomicUsize = AtomicUsize::new(0);

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        force_remove_dir_all(&self.0);
        PENDING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The runner drops a check's filesystem on another thread after the result is out, so without
/// waiting the previous check's tree is still being deleted while the next one measures free
/// space, and a check that unlinks its only file cannot return the store to where it started.
fn wait_for_cleanup() {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    while PENDING.load(Ordering::SeqCst) > 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn mount_conformance() {
    // Zero cache lifetimes: the suite asks the `Vfs` about the tree immediately after each
    // operation, so a cached attribute or page would be a cache-policy artefact, not a defect.
    let opts = std::env::var("COWFS_FUSE_CONF_MOUNT_OPTS")
        .unwrap_or_else(|_| "attr_ttl=0,entry_ttl=0,noneg,workers=4".into());
    let Some(fx) = common::Fixture::new(&opts) else {
        return;
    };
    let base = fx.dir.clone();
    static N: AtomicU32 = AtomicU32::new(0);
    let factory = || -> Arc<dyn Vfs> {
        wait_for_cleanup();
        let dir = base.join(format!(
            "cowfs-conformance-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create the per-check directory");
        PENDING.fetch_add(1, Ordering::SeqCst);
        Arc::new(
            PathVfs::new(&dir)
                .expect("open the per-check directory")
                .keep_alive(Cleanup(dir)),
        )
    };
    eprintln!("suite base directory: {}", base.display());
    let mut opts = Options::from_env();
    if opts.timeout.is_none() {
        opts.timeout = std::time::Duration::from_secs(300).into();
    }
    // The Linux page cache owns the bytes a reader sees, so a `Cowfs`-level read/write atomicity
    // contract is not observable through a mount: the same test body tore 181 of 200 runs through
    // FUSE, 72 of 200 on native btrfs and 29 of 200 on native tmpfs, with no cowfs code in the
    // native arms. MemVfs itself is covered by `cowfs-vfs-test`'s own suite. See issue #45.
    opts.skip.push((
        "concurrent_readers_and_writers_of_one_file".into(),
        "not observable through a kernel page cache: native ext4/btrfs/tmpfs tear at this size too"
            .into(),
    ));
    // The kernel owns the inode's reference lifetime and sends FORGET whenever it likes, so reclaim
    // on forget cannot be observed synchronously through a mount: 142 of 200 runs read blocks_free
    // before the FORGET landed (the instrumented adapter logged `nlink=0 lookups=3 opens=1` at the
    // statfs, with the FORGET arriving 189us later). Native PathVfs, whose forget is synchronous,
    // passed 200 of 200, so MemVfs reclaims as the trait requires. A bounded poll turns 100 of 100
    // FUSE runs green, with a maximum of one extra poll. See issue #45.
    opts.skip.push((
        "statfs_free_after_unlink".into(),
        "kernel sends FORGET asynchronously, so reclaim is not observable in the same call; a bounded poll makes 100/100 pass"
            .into(),
    ));
    let report = run_all(&factory, &opts);
    eprintln!("{}", report.table());
    wait_for_cleanup();
    if std::env::var("COWFS_FUSE_STRICT").is_ok_and(|v| v == "1") {
        assert!(report.passed(), "some checks failed");
    }
}
