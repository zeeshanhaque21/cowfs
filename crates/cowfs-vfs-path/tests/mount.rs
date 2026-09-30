//! The conformance suite through a mounted cowfs adapter: `PathVfs` rooted inside the mount.
//!
//! Mount an adapter over some `Vfs` first (see the crate docs), then:
//!
//! `COWFS_PATHVFS_MOUNT=/path/to/mount cargo test -p cowfs-vfs-path --test mount -j4 -- --ignored --nocapture`

mod common;

use std::path::PathBuf;

#[test]
#[ignore = "needs COWFS_PATHVFS_MOUNT pointing at a mounted adapter"]
fn mount_conformance() {
    let mount = std::env::var_os("COWFS_PATHVFS_MOUNT")
        .map(PathBuf::from)
        .expect("set COWFS_PATHVFS_MOUNT to the mounted directory");
    common::run_suite(&mount);
}
