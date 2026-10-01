//! The conformance suite on the machine's own filesystem, as a control.
//!
//! `cargo test -p cowfs-vfs-path --test native -j4 -- --ignored --nocapture`
//!
//! `COWFS_PATHVFS_NATIVE_DIR` picks the directory (default: the system temp dir).
//! `COWFS_CONFORMANCE_HEAVY=1` adds the heavy checks. `COWFS_PATHVFS_STRICT=1` makes any
//! failing check fail the test.

mod common;

use std::path::PathBuf;

fn base() -> PathBuf {
    std::env::var_os("COWFS_PATHVFS_NATIVE_DIR").map_or_else(std::env::temp_dir, PathBuf::from)
}

#[test]
#[ignore = "runs the whole suite on the native filesystem; see the file docs"]
#[cfg(target_os = "macos")]
fn native_apfs_conformance() {
    common::run_suite(&base());
}

#[test]
#[ignore = "runs the whole suite on the native filesystem; see the file docs"]
#[cfg(target_os = "linux")]
fn native_linux_conformance() {
    common::run_suite(&base());
}
