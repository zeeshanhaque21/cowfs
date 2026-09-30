//! `MemVfs`, the in-memory reference implementation of `cowfs_vfs::Vfs`, and the conformance
//! suite every `Vfs` backend must pass. The suite is the definition of correct behaviour:
//! the core, the FUSE adapter and the NFS adapter (through a mount) are all run through it,
//! and so is a native filesystem (`cowfs-vfs-path`) as a control run.
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
//! ## Skipping a check, with a reason
//!
//! A backend that legitimately cannot pass a check says so, with the reason, in the macro:
//!
//! ```ignore
//! cowfs_vfs_test::conformance_tests!(
//!     || -> Arc<dyn Vfs> { Arc::new(MyFs::new_empty()) },
//!     skip = {
//!         dir_nlink_counts_subdirs => "btrfs reports nlink 1 for directories",
//!         xattr_empty_and_large_values => "ext4 limits xattrs to one block",
//!     }
//! );
//! ```
//!
//! A skipped check becomes an `#[ignore = "reason"]` test, which libtest prints as ignored with
//! the reason, so skips are visible in every test run and never pass silently. (Running ignored
//! tests only prints a note for a skipped check: it does not run it.) The macro also adds a test
//! `conformance_skip_names_exist` that fails if a skip names something that is not a check.
//!
//! ## Table reports, levels, timeouts
//!
//! To print a table instead of failing on the first check, call [`conformance::run_all`]:
//!
//! ```ignore
//! use cowfs_vfs_test::conformance::{run_all, Level, Options};
//! let opts = Options {
//!     level: Some(Level::Posix),
//!     skip: vec![("non_utf8_names".into(), "APFS refuses invalid UTF-8".into())],
//!     ..Options::from_env()
//! };
//! let report = run_all(&factory, &opts);
//! println!("{}", report.table());
//! report.assert_ok(); // panics on a failure, prints every skip with its reason
//! assert_eq!(report.skipped_count(), 1); // a backend can pin its expected skips
//! ```
//!
//! [`conformance::Options`] fields: `heavy`, `filter` (keep checks whose `category::name`
//! contains the text), `level` (see below), `skip` (name and reason; an unknown name is a
//! `Report::config_errors` entry and makes `passed()` false), `timeout` (hang timeout per
//! check, default 60 s and 600 s for heavy checks). A check that panics is a failure. A check
//! that hangs is a failure too, its thread is leaked and marked `[LEAKED THREAD]` in the table
//! and counted by `Report::leaked_count`. Every check gets a fresh filesystem from the factory,
//! so neither can affect a later check.
//!
//! Environment variables read by `Options::from_env` and `conformance_tests!`:
//! `COWFS_CONFORMANCE_HEAVY=1`, `COWFS_CONFORMANCE_FILTER=text`,
//! `COWFS_CONFORMANCE_LEVEL=posix|portable|cowfs`, `COWFS_CONFORMANCE_TIMEOUT_SECS=n`.
//! A check returns a [`conformance::Failure`] with a description and never panics.
//!
//! # Levels: what a check's expectation rests on
//!
//! Each check has a [`conformance::Level`], printed in `Report::table()` and selectable with
//! `Options::level` (default: everything). Levels are cumulative: `Portable` runs `Posix` and
//! `Portable`.
//!
//! - `Posix`: believed to hold on ext4, btrfs and APFS. Use it for a native-filesystem control
//!   run. A check is only `Posix` when its assertions were probed or follow directly from the
//!   POSIX text; when in doubt it is not `Posix`.
//! - `Portable`: holds on the common local filesystems but depends on something that varies
//!   (timestamp granularity, sparse files, xattr support, symlink target length) or was not
//!   verified on all three.
//! - `Cowfs`: a cowfs contract decision, or a `Vfs` concept with no native equivalent
//!   (`forget`, `Stale`, handles pinning inodes, `max` of 0, file-type bits in `mode`). These
//!   are not claims about POSIX.
//!
//! Evidence (probes run on APFS, btrfs and ext4 during the review of PR #30; "OK" means the
//! assertion held natively on all three):
//!
//! - Directory `nlink`: ext4 2 + subdirectories, btrfs always 1, APFS 2 + all entries. So
//!   every check that asserts it is `Cowfs`: `root_is_directory`, `new_file_attrs`,
//!   `dir_nlink_counts_subdirs`, `rmdir_updates_parent`, `deeply_nested_directories`,
//!   `rename_dir_over_empty_dir`, `rename_dir_cross_directory_fixes_nlink`,
//!   `rename_dir_across_parents_keeps_parent_links`, `concurrent_rename_unlink_lookup`.
//! - `create` with mode 04755 keeps setuid on btrfs and ext4 but drops it on APFS:
//!   `create_masks_mode` is `Cowfs`.
//! - xattr values: 60,000 bytes fail on btrfs (ENOSPC), 4,096 bytes fail on ext4, both work on
//!   APFS. `user.*` xattrs on a symlink fail on Linux (EPERM) and work on APFS. Both checks are
//!   `Cowfs`.
//! - Names: APFS rejects invalid UTF-8 (EILSEQ) and is case-insensitive and normalising by
//!   default, so `non_utf8_names` and `names_are_exact_bytes` are `Cowfs`. NAME_MAX 255 is
//!   enforced identically on all three (ENAMETOOLONG at 256): `name_max_ok_and_one_more_fails`
//!   is `Posix`.
//! - Timestamps: nanosecond granularity on all three (1000 of 1000 directory mtimes strictly
//!   increased after 1 ms), so time checks are `Portable`. They need a clock that advances
//!   between two operations made a few milliseconds apart: a 1 to 2 second granularity
//!   filesystem (ext3, HFS+, FAT) fails them.
//! - `blocks`: 1 byte is 8 blocks and 1 MiB is 2048 blocks on all three, so
//!   `blocks_accounting` is `Posix`.
//! - Same-size truncate: btrfs does not bump mtime, ext4 and APFS do. `truncate_to_same_size`
//!   asserts size and content only; the ctime rule is the `Cowfs` check
//!   `truncate_to_same_size_bumps_ctime`.
//! - `unlink` of a directory: EISDIR on Linux, EPERM on APFS. `mkdir_rmdir_errors` accepts both.
//! - `rename` with `no_replace` onto the very same path succeeds on APFS (Exists elsewhere).
//!   `rename_no_replace` does not test that case and is `Portable`.
//! - Sparse files and `symlink` target length: 4,095-byte targets fail on APFS (PATH_MAX 1024),
//!   so `symlink_size_is_target_length` is `Portable`.
//! - Not verified on a native filesystem by running the suite (that is the job of
//!   `cowfs-vfs-path`): everything `Posix` beyond the rows above rests on reading the POSIX
//!   text. If a `Posix` check fails natively, reclassify it and record the evidence here.
//!
//! # Error precedence: accepted alternatives
//!
//! `Vfs` has no error for EPERM: `Error::PermissionDenied` maps to EACCES, while the kernel
//! returns EPERM for a hardlink to a directory. The suite compares errnos through
//! `Error::errno()` and accepts EPERM wherever it accepts `PermissionDenied`. When several
//! errors apply at once the trait does not say which wins. `MemVfs` picks the first in the
//! table, the others are what native filesystems return and all are accepted by the model
//! oracle (`model`). Checks that hit these cases accept the listed alternatives.
//!
//! - `link(dir, name that exists)`: `MemVfs` `PermissionDenied`, Linux `Exists`, APFS EPERM.
//! - `link(dir, name in a file)`: `MemVfs` `PermissionDenied`, Linux and APFS `NotDir`.
//! - `rename(file, onto its own non-empty ancestor directory)`: `MemVfs` and APFS `IsDir`,
//!   Linux `NotEmpty` (ENOTEMPTY). Checked by `rename_file_onto_ancestor_dir_is_rejected`.
//! - `unlink(dir)`: `MemVfs` and Linux `IsDir`, APFS EPERM.
//! - `rename(dir, onto a path through a file)`: `MemVfs` `NotDir` (path errors win), a native
//!   filesystem may say `InvalidArgument` first.
//! - `symlink` with an empty target: `MemVfs` `InvalidArgument`, Linux ENOENT, APFS succeeds.
//!   Not tested.
//! - `setxattr` with an empty name: `MemVfs` `InvalidArgument`, Linux ERANGE, APFS EINVAL.
//!   `xattr_name_validation` accepts both.
//!
//! # Other decisions the suite pins (all `Cowfs` unless stated)
//!
//! Files and symlinks report the number of names as `nlink`; a symlink's size is its target
//! length in bytes; the root is `ROOT_INO`; `Attr.blocks` is in 512-byte units and 0 for an
//! empty file; an inode with no names, handles or references is `Stale`; a removed but still
//! referenced directory reports `nlink` 0 and refuses new entries with `NotFound`; `readdir`
//! `max` of 0 is `InvalidArgument`; hardlinks stop at a backend limit with `TooManyLinks`
//! (`MemVfs` at `LINK_MAX`, configurable with `MemVfs::with_link_max`). Each category module
//! documents its own.
//!
//! # Validating the suite
//!
//! - `cargo test -p cowfs-vfs-test` (well under 30 s): runs the suite on `MemVfs`, runs every
//!   `Fault` through only the checks meant to catch it, and compares `MemVfs` with the
//!   independent path-based oracle on random operation sequences (see `model`).
//! - Full mutation run, every fault against the whole suite (minutes in debug, use release):
//!   `cargo test -p cowfs-vfs-test --release --features mutation-tests --test mutations`.
//!   CI should run it nightly.
//! - Heavy checks on `MemVfs`:
//!   `COWFS_CONFORMANCE_HEAVY=1 cargo test -p cowfs-vfs-test --test memvfs -- --ignored`.

pub mod conformance;
mod memvfs;
pub mod model;
mod pages;

pub use memvfs::{Fault, MemVfs, LINK_MAX};

/// Expands to one `#[test]` per conformance check for the `Vfs` built by `$factory`,
/// a closure or function returning `Arc<dyn Vfs>`. The optional `skip = { check => "reason" }`
/// turns the named checks into ignored tests that show the reason. See the crate docs.
#[macro_export]
macro_rules! conformance_tests {
    ($factory:expr) => {
        $crate::conformance_tests!($factory, skip = {});
    };
    ($factory:expr, skip = { $($n:ident => $r:literal),* $(,)? }) => {
        $crate::__conformance_checks!(__gen_tests, ($), { $($n => $r),* }, $factory);
    };
}
