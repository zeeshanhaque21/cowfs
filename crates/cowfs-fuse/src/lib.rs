//! Linux FUSE mount adapter for cowfs: a thin translation from kernel requests to `cowfs_vfs::Vfs`.
//!
//! On other platforms this crate builds as an empty library apart from the pure helpers.
//!
//! # Caching and consistency
//!
//! By default ([`MountMode::Shared`]) the kernel caches names and attributes for one second
//! and file pages are dropped on every open, so a change made to the tree behind the mount,
//! by snapshot, gc or control operations, is visible within a second. Appends are placed at
//! the size the `Vfs` reports, not at the kernel's cached size, so growth behind the mount
//! cannot make an append overwrite data.
//!
//! [`MountMode::SoleWriter`] opts into long lifetimes and `keep_cache` (the spike 3 numbers).
//! It is valid only when every mutation goes through this mount or is announced through the
//! `Invalidator`; anything else is served stale for the whole lifetime. Wiring that is the
//! job of whoever owns the control plane: after every change made behind the mount, call
//! `invalidate_inode` for changed inodes and their parents, `invalidate_entry` for removed or
//! renamed names, `invalidate_children` for a directory whose contents were replaced, or
//! `invalidate_all` (also after a snapshot swap).
//!
//! "No such name" answers are cached for `negative_ttl` (default 1 s) in both modes. The
//! kernel ignores invalidation of them (verified on Linux 7.0), so a name created behind the
//! mount, such as a new snapshot directory at the mount root, appears within `negative_ttl`.
//!
//! # Threading and ordering
//!
//! Cheap metadata (lookup, getattr, access, statfs, forget, xattr reads, lseek) runs on the
//! `fuser` request loop thread. Everything that can block (read, write, readdir, open, flush,
//! fsync, release, and every operation that changes the tree) goes to one of `workers` lanes,
//! each a FIFO, or runs inline when its class has recently been cheap (see `inline_below`).
//! `O_APPEND` writes always take a lane, which makes concurrent appenders to one file atomic.
//!
//! The adapter does not promise per-inode serialization. The `Vfs` contract is that each
//! operation is atomic and that concurrent calls behave as some serial order, which is the
//! implementor's to provide. The kernel only issues a dependent request after the reply to the
//! one it depends on, so write-then-fsync, create-then-lookup and rename-then-lookup of the old
//! name stay ordered through the mount. Requests the kernel issues independently (a `getattr`
//! and a `read` of one inode from two processes) may reach the `Vfs` at the same time. What the
//! adapter adds: requests for one inode that the kernel issued one after another reach the
//! `Vfs` in that order. A rename takes only its source directory's lane, and needs no more,
//! because the kernel serialises renames per superblock (`s_vfs_rename_mutex`), so a mount never
//! has two renames in flight: 16 threads hammering 16 directories in both directions never
//! reached the `Vfs` with two at once.
//!
//! A `Vfs` must be thread safe, as its trait says, and must tolerate the loop thread and up to
//! `workers` lanes calling it at once. `workers=0` restores the single threaded loop;
//! `inline_below_us=0` sends everything to the lanes.
//!
//! # Failure containment
//!
//! A panic in a `Vfs` call is caught, logged, and answered with `EIO` for that request.
//! After `max_panics` (default 3) the mount is marked failed: every request answers
//! `ENOTCONN` (never an empty directory) until it is unmounted, `Mount::failed` is true and
//! `Mount::is_alive` is false. `is_alive()` means the session is up; a `Vfs` call that never
//! returns leaves it true, so `Mount::health` reports that instead: `Health::Wedged` names a
//! lane that has had a request in flight for longer than `lane_bound`. Attributes from the
//! `Vfs` are clamped to what the kernel accepts, and logged, so one bad reply cannot make
//! `stat` fail for the whole mount. `Error::Retry` becomes `EAGAIN`; an `Error` or `FileKind`
//! this adapter does not know is `EIO`, never a guess.
//!
//! # Lifecycle
//!
//! `Mount::unmount` returns how it ended: cleanly, lazily (a process still had a file open
//! or a working directory inside, after `unmount_timeout`), or an error. It never reports
//! success silently. `Mount::install_signal_cleanup` unmounts on SIGTERM, SIGINT and
//! SIGHUP; `sweep_stale_mounts` clears mounts left by `kill -9` at startup. `auto_unmount`
//! makes `fusermount3` unmount when the process dies, but implies `allow_other`, so a
//! non-root user needs `user_allow_other` in `/etc/fuse.conf`; without it
//! `MountError::NeedsAllowOther` is returned instead of a mount that fails obscurely.
//!
//! # Inode numbers
//!
//! The adapter always sends generation 0, so the `Vfs` must never reuse an inode number
//! during the lifetime of a mount (see `docs/v1-architecture.md`). `paranoid_ino` makes the
//! adapter detect a reuse while the kernel still references the old file and fail that
//! request with `EIO`. `.` and `..` in a listing carry the real parent inode when the adapter
//! has seen it.
//!
//! # Permissions
//!
//! Every file is owned by the mounter. With `default_permissions` (the default) the kernel
//! enforces mode bits, POSIX style: the handle returned by creating a file with mode 0444 can
//! write, but a later `open` for writing by the owner fails with `EACCES`. With
//! `nodefault_permissions` the adapter does no mode checks on `open`, only on `access(2)`, so
//! the owner can write a 0444 file. The NFS adapter is stateless and cannot see the creating
//! handle, so it behaves like `nodefault_permissions`; which policy every adapter should
//! share is a decision for the lead.
//!
//! # Not supported
//!
//! Device nodes, fifos and sockets (`mknod` of anything but a regular file is `ENOTSUP`),
//! `RENAME_EXCHANGE` (`ENOTSUP`), `fallocate` and `copy_file_range` (`ENOTSUP`, so
//! `rsync --preallocate` prints a warning per file and `cp --reflink=always` fails),
//! extended attribute namespaces other than `user.*`, `security.*` and `trusted.*` (POSIX ACLs
//! are `ENOTSUP`), holes (`SEEK_DATA` and `SEEK_HOLE` treat the whole file as data), and FUSE
//! locks: the kernel handles `flock` and POSIX locks locally. `syncfs(2)` takes the kernel
//! fallback, an `fsync` of the root, which the `Vfs` defines as the whole-mount barrier.
//!
//! pjdfstest passes on the mount except its fifo, mknod and multi-uid groups, which need the
//! three features above (`allow_other` is off by default).
//!
//! # Running the mount tests
//!
//! They need Linux with `/dev/fuse` and `fusermount3`, are `#[ignore]`d, and skip themselves
//! when FUSE is unusable:
//! `cargo test -p cowfs-fuse -j4 -- --ignored --test-threads=1 --nocapture`
//! (add `--release` for the latency floor). A Linux CI job runs exactly that.

pub mod bench;
pub mod convert;
pub mod dir;
mod error;
pub mod options;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod cost;

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod mounts;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod table;

#[cfg(target_os = "linux")]
mod fs;
#[cfg(target_os = "linux")]
mod lifecycle;
#[cfg(target_os = "linux")]
mod mount;

pub use error::MountError;
pub use options::{MountMode, MountOptions};

#[cfg(target_os = "linux")]
pub use lifecycle::{sweep_stale_mounts, Unmounted};
#[cfg(target_os = "linux")]
pub use mount::{run, Health, Invalidator, Mount};
