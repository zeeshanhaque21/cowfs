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
//!     xattr_names: true, // Linux needs a namespace prefix on xattr names
//!     skip: vec![("appledouble_names_are_ordinary".into(), "NFS Translate mode hides ._x".into())],
//!     ..Options::from_env()
//! };
//! let report = run_all(&factory, &opts);
//! println!("{}", report.table());
//! report.assert_ok(); // panics on a failure, prints every skip with its reason
//! assert_eq!(report.skipped_count(), 1); // a backend can pin its expected skips
//! ```
//!
//! See "Options" below for every field and environment variable.
//!
//! # Levels: what a check's expectation rests on
//!
//! Each check has a [`conformance::Level`], printed in `Report::table()` and selectable with
//! `Options::level` (default: everything). Levels are cumulative: `Portable` runs `Posix` and
//! `Portable`.
//!
//! - `Posix`: holds on ext4, btrfs and APFS, so it is safe for a native-filesystem control run.
//!   A check is `Posix` only when it was run on those three or follows directly from the POSIX
//!   text; when in doubt it is not `Posix`.
//! - `Portable`: holds on the common local filesystems but depends on something that varies
//!   (timestamp granularity, sparse files, xattr support, symlink target length) or was not
//!   verified on all three.
//! - `Cowfs`: a cowfs contract decision, or a `Vfs` concept with no native equivalent
//!   (`forget`, `Stale`, handles pinning inodes, `max` of 0, file-type bits in `mode`).
//!
//! ## Evidence
//!
//! Every row is from `cowfs-vfs-path` running this suite through `PathVfs` (findings in
//! `crates/cowfs-vfs-path/FINDINGS.md`, branch `v1/pathvfs`) on APFS, case-sensitive APFS,
//! btrfs, ext4, a FUSE mount and an NFS mount and raw NFSv3, plus the probes cited below.
//! One run per environment unless a count is given.
//!
//! - **No `Posix` check fails on APFS, btrfs or ext4** (56 of 56 on each, and 56 of 56 over
//!   raw NFSv3 with `AppleDoubleMode::Store`).
//! - Directory `nlink` 2 + subdirectories: ext4 yes, btrfs always 1, APFS 2 + all entries. All
//!   nine `nlink` checks are `Cowfs` and were seen failing natively for exactly this reason.
//! - `user.*` xattrs on a symlink: EPERM on Linux, works on APFS and through FUSE.
//!   `xattr_on_directory_and_symlink` is `Cowfs`.
//! - xattr value sizes: 60,000 bytes is ENOSPC on ext4 and btrfs, works on APFS; 4,096 bytes
//!   fails on ext4. `xattr_empty_and_large_values` is `Cowfs`.
//! - xattr names: a 255 byte name with no namespace is ENOTSUP on Linux; `user.` plus 250 bytes
//!   works; `user.` plus 251 bytes is ERANGE. `xattr_name_validation` is `Cowfs` and takes
//!   `Options::xattr_names` for the valid-name case.
//! - macOS adds `com.apple.provenance` to new files, so the two xattr listing checks ignore
//!   `com.apple.*`.
//! - APFS rejects invalid UTF-8 (EILSEQ) and is case-insensitive and normalising by default:
//!   `non_utf8_names` and `names_are_exact_bytes` are `Cowfs`, `name_max_ok_and_one_more_fails`
//!   is `Posix` (ENAMETOOLONG at 256 on all three).
//! - Symlink targets: a 4095 byte target fails on APFS (PATH_MAX 1024). Targets up to 1000 bytes
//!   work everywhere, so `symlink_size_is_target_length` and `symlink_size_multibyte` are
//!   `Portable`.
//! - Timestamps: nanosecond granularity on all three, so time checks are `Portable`. They need a
//!   clock that advances between two operations made a few milliseconds apart, so a 1 to 2
//!   second granularity filesystem (ext3, HFS+, FAT) fails them.
//! - APFS does not bump ctime for an atime-only `utimensat`, so `setattr_bumps_ctime` only
//!   requires a bump for mode, mtime and size changes (`Portable`) and the atime-only rule is
//!   the `Cowfs` check `attrs_atime_only_bumps_ctime`.
//! - btrfs reports statfs `files` 0, so `statfs_sane` only checks the inode count when the
//!   backend reports it. The macOS NFS client caches statfs for about 0.2 s, so only
//!   inequalities are ever asserted.
//! - `blocks`: 1 byte is 8 blocks and 1 MiB is 2048 on all three, so `blocks_accounting` is
//!   `Posix`.
//! - Same-size truncate: btrfs does not bump mtime, ext4 and APFS do. `truncate_to_same_size`
//!   asserts size and content only (`Posix`); the ctime rule is the `Cowfs` check
//!   `truncate_to_same_size_bumps_ctime`.
//! - Read atomicity: 8 byte reads and writes never tore in about 1M reads on ext4, btrfs, tmpfs
//!   and APFS (`small_reads_are_never_torn`, `Portable`); 512 and 4096 byte reads tore on
//!   ext4, btrfs and tmpfs (8 to 297 in about 300k reads) and never on APFS, so
//!   `concurrent_readers_and_writers_of_one_file` is `Cowfs`. POSIX says a read is atomic
//!   against a concurrent write; Linux buffered I/O does not provide that.
//! - `unlink` of a directory: EISDIR on Linux, EPERM on APFS. `mkdir_rmdir_errors` accepts both.
//! - APFS reports `nlink` 2 for a removed directory, so `rmdir_reclaimed_after_forget` is
//!   `Cowfs` and was seen failing on APFS.
//! - APFS (and the macOS NFS client) keep an unlinked open file as `.nfs.<id>`, so 9 `Posix`
//!   checks fail through an NFS mount. They pass over raw NFSv3, where the adapter is
//!   correct: an adapter test run must drive the protocol, not the macOS client.
//! - NFSv3 is stateless: a handle of a removed file is STALE (RFC 1813) and there are no xattr
//!   procedures, so the `lifecycle` checks that need a pinning handle and the `xattrs` checks
//!   cannot be judged over raw NFSv3 at all.
//! - `appledouble_names_are_ordinary` is `Cowfs`: the NFS adapter's default
//!   `AppleDoubleMode::Translate` makes `._x` names invisible on purpose, and it passes in
//!   `Store` mode. An adapter in `Translate` mode must skip it with a reason.
//! - Hardlink limits: ext4 allows 65,000 links, and creating that many over a mount took 540 s,
//!   so `hardlink_limit_reports_too_many_links` is `Cowfs` and only runs when the backend
//!   declares a small limit with `Options::link_limit`. Without it the runner reports the check
//!   as skipped instead of hanging.
//!
//! If a `Posix` check ever fails natively, reclassify it and add the evidence here.
//!
//! ## Options
//!
//! [`conformance::Options`] fields: `heavy`, `filter` (keep checks whose `category::name`
//! contains the text), `level`, `skip` (name and reason; an unknown name is a
//! `Report::config_errors` entry and makes `passed()` false), `timeout` (hang timeout per
//! check, default 60 s and 600 s for heavy checks), `xattr_names` (prefixed xattr names) and
//! `link_limit` (a small hardlink limit the backend can reach inside the timeout).
//! Environment variables: `COWFS_CONFORMANCE_HEAVY=1`, `COWFS_CONFORMANCE_FILTER=text`,
//! `COWFS_CONFORMANCE_LEVEL=posix|portable|cowfs`, `COWFS_CONFORMANCE_TIMEOUT_SECS=n`,
//! `COWFS_CONFORMANCE_XATTR_NAMES=prefixed`, `COWFS_CONFORMANCE_LINK_LIMIT=n`.
//! A check returns a [`conformance::Failure`] with a description and never panics. A check that
//! panics is a failure; a check that hangs is a failure too, its thread is leaked and marked
//! `[LEAKED THREAD]` in the table and counted by `Report::leaked_count`. Every check gets a
//! fresh filesystem from the factory, so neither can affect a later check.
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
//! length in bytes; the root is `ROOT_INO`; `Attr.blocks` is the logical allocation of non-hole
//! data in 512-byte units and 0 for an empty file; a `StatFs` field of 0 means unknown; an
//! inode with no names, handles or references is `Stale`; an inode number is never reused for a
//! different file and 0 is never an inode; a removed but still referenced directory reports
//! `nlink` 0 and refuses new entries with `NotFound`; `open` never fails because of intent and
//! `release` of an unknown handle is `InvalidArgument`; `readdir` `max` of 0 is
//! `InvalidArgument` and cookie 0 is reserved; every name-taking operation validates names;
//! hardlinks stop at a backend limit with `TooManyLinks` (`MemVfs` at `LINK_MAX`, configurable
//! with `MemVfs::with_link_max`). Each category module documents its own.
//!
//! Four contract sentences are not pinned by a check, because this layer cannot observe them:
//! `fsync` making the creating name durable (that needs a crash harness, and `cowfs-store`
//! owns it), `write` rejecting more than `u32::MAX` bytes (a 4 GiB test buffer), a synthetic
//! read-only `ROOT_INO` (`ReadOnly` and `CrossDevice`, which `MemVfs` does not have) and
//! `Error::Retry`, which no `MemVfs` operation returns.
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
