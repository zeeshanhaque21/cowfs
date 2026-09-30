//! `MemVfs`, the in-memory reference implementation of `cowfs_vfs::Vfs`, and the conformance
//! suite every `Vfs` backend must pass. The suite is the definition of correct behaviour:
//! the core, the FUSE adapter and the NFS adapter (through a mount) are all run through it.
//!
//! # Adding the suite to a backend crate
//!
//! Add this crate as a dev-dependency, then in one test file:
//!
//! ```ignore
//! use std::sync::Arc;
//! use cowfs_vfs::Vfs;
//!
//! // The factory builds a fresh, empty filesystem. It is called once per check.
//! cowfs_vfs_test::conformance_tests!(|| -> Arc<dyn Vfs> { Arc::new(MyFs::new_empty()) });
//! ```
//!
//! That expands to one `#[test]` per check, named after the check, so
//! `cargo test names_are_exact_bytes` runs one. Checks marked heavy (50,000 directory entries,
//! 8 MiB files) are `#[ignore]`d: run them with
//! `COWFS_CONFORMANCE_HEAVY=1 cargo test -- --ignored`.
//!
//! To print a table instead of failing on the first check, call [`conformance::run_all`]:
//!
//! ```ignore
//! let report = cowfs_vfs_test::conformance::run_all(&factory, &Options::from_env());
//! println!("{}", report.table());
//! assert!(report.passed());
//! ```
//!
//! A check returns a [`conformance::Failure`] with a description and never panics; a panic or
//! a hang inside a backend is caught by the runner and reported as a failure of that check.
//!
//! Environment: `COWFS_CONFORMANCE_HEAVY=1` includes heavy checks in `run_all`,
//! `COWFS_CONFORMANCE_FILTER=text` keeps only checks whose `category::name` contains `text`.
//!
//! # Decisions the suite pins
//!
//! Each category module documents its own; the global ones are: directories report
//! `nlink` = 2 + number of subdirectories; files and symlinks report the number of names; a
//! symlink's size is its target length; the root is `ROOT_INO`; `Attr.blocks` is in 512-byte
//! units and 0 for an empty file; an inode with no names, handles or references is `Stale`.
//!
//! # Validating the suite
//!
//! The `MemVfs` tests run the suite on the reference, run it again on 15 deliberately broken
//! copies (`Fault`) and require a failure for every one, and compare `MemVfs` against an
//! independent path-based oracle on random operation sequences (see `model`).

pub mod conformance;
mod memvfs;
pub mod model;
mod pages;

pub use memvfs::{Fault, MemVfs};

/// Expands to one `#[test]` per conformance check for the `Vfs` built by `$factory`,
/// a closure or function returning `Arc<dyn Vfs>`. See the crate docs.
#[macro_export]
macro_rules! conformance_tests {
    ($factory:expr) => {
        $crate::__conformance_checks!(__gen_tests, $factory);
    };
}
