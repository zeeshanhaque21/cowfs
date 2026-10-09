//! macOS mount adapter for cowfs: serves a [`cowfs_vfs::Vfs`] over an in-process NFSv3 loopback
//! server (a vendored, patched `nfsserve`) and mounts it with `mount_nfs`. Design: `docs/design.md`,
//! findings: `docs/spikes/2-nfs-loopback.md`.
//!
//! `Mount::new` serves a `Vfs` and mounts it (no sudo), dropping the `Mount` unmounts.
//! Call `Mount::install_signal_cleanup` once at startup so SIGTERM, SIGINT and SIGHUP unmount
//! before the process exits, and `sweep_stale_mounts` to remove mounts left by a killed host.
//!
//! # AppleDouble (`AppleDoubleMode`)
//!
//! The macOS client keeps extended attributes (`com.apple.provenance` on every new file) in
//! `._name` files. Three modes:
//!
//! - `Hide` (the default, and what the mount used before `Translate` existed): the sidecars are
//!   stored as ordinary files and hidden from listings.
//! - `Translate`: `._name` is served as a view of the extended attributes of `name`, stored in the
//!   `Vfs` as xattrs of the real file, so no sidecar inode exists and another mount of the same
//!   data sees no `._` file. A `._name` with no `name` to hold the attributes is stored as a real
//!   file instead, which is what an archive extraction and a checkout of a tree that tracks `._*`
//!   need; such a file stays a real file, and is never merged into `name`. A whole-file write at
//!   offset 0 whose bytes cannot be a sidecar is refused, so a real file under a reserved name is
//!   never accepted and then dropped. A translated sidecar has no inode, so it has a file id of
//!   its own that is never a number an inode of the `Vfs` is using.
//! - `Store`: `._` names are ordinary names.
//!
//! # Mount options and a dead server
//!
//! `locallocks` is required (rustc incremental compilation aborts without it), `rsize` and `wsize`
//! are 128 KiB, `actimeo` 120, and the mount is `hard`. `soft` is available and was measured on
//! macOS 26.6.2: with `soft,timeo=6,retrans=2` a `ls` or `touch` of an uncached path still blocked
//! for more than 20 s after the server was killed, the same as `hard`, and a soft mount can also
//! fail a write half way through. So it is off by default. What does unblock a caller is
//! [`install_signal_cleanup`] on SIGTERM and [`sweep_stale_mounts`] after a crash.
//!
//! # Known limit: fifos cannot be opened on a macOS mount
//!
//! `mkfifo` works, but `open(2)` of a fifo fails with `EACCES`: the macOS NFS client's
//! `nfs_vnop_open` refuses every vnode that is not a regular file, directory or symlink, before
//! any RPC is sent. No server change or mount option helps (issue #204, evidence in
//! `docs/verification/evidence/nfs204-fifo-open.md`; pjdfstest `open/17.t` #2).
//!
//! # What this crate requires of every `Vfs`
//!
//! - Every method may block and each is atomic, so the adapter must not rely on the order of two
//!   calls. It holds no lock across a `Vfs` call: the only locks it takes are over its own
//!   sidecar state, and they are per inode.
//! - `fsync(ROOT_INO, false)` is the whole-mount barrier. NFS COMMIT maps to `fsync(ino, false)`
//!   for every handle, the root included, and never to a data-only sync, so a file whose COMMIT
//!   returned survives a crash.
//! - The client does not send a COMMIT for every sync a caller writes: not for a directory `fsync`,
//!   and not for an `fsync` of a descriptor with no dirty pages. A rename the caller followed with
//!   `fsync(parent_dir_fd)`, which is the whole POSIX dance, would be acknowledged and still be
//!   only in memory. So the adapter does not wait to be asked: every name and attribute it changes
//!   is made durable with `sync_namespace` before the reply goes out (issue #90, measured in
//!   `docs/nfs-namespace-durability90.md`). That is a barrier on the acknowledgement of a name, not
//!   a flush per byte: `WRITE` stays unstable until the client's own COMMIT.
//! - `readdir` with `max == 0` is `InvalidArgument`, so the adapter clamps a zero budget to one
//!   entry instead of passing it on or turning it into an error the client cannot act on.
//! - `open`, `release` and `flush` are not called: NFSv3 has no procedure for them.
//! - An `Ino` is never reused for a different file within a mount's lifetime. The adapter adds a
//!   per-inode generation to file handles as a second line of defence (a handle minted before the
//!   last name of an inode went away is `STALE`), but only for removals it performed itself.
//! - Every `u64` but 0 is a legal `Ino`. The adapter spends no bit of the inode space on its own
//!   bookkeeping: a translated sidecar is not an inode, so it carries its own file id, allocated by
//!   the adapter and kept out of the way of any inode number the `Vfs` hands out (issue #60).
//! - `lookup`, `create`, `mkdir`, `symlink` and `link` each take one reference and the adapter
//!   gives every one back at once with `forget`. NFS handles are stateless, so nothing is pinned.
//! - Every call may block, and each one runs on its own blocking task, so a slow `Vfs` costs
//!   parallelism rather than the whole server. An option to answer cheap metadata calls on the
//!   network thread was removed: it stalled every request behind one slow `getattr` (measured
//!   0.8 s for an unrelated NULL call) and showed no reproducible gain in the benchmark.
//!
//! # Security model
//!
//! v1 is single-user: the server ignores AUTH_UNIX caller identity.
//! Mountpoint directory permissions are the access boundary, not per-caller NFS mode checks.
//! Owner CREATE with mode 0444 followed by WRITE on that descriptor succeeds.
//! ACCESS respects the owner mode bits, so reopening that file for writing fails with EACCES.
//! These owner behaviours match native APFS in the real-mount regression test.
//!
//! The server listens on 127.0.0.1 only, and answers MNT for one export path
//! (`localhost:/cowfs-<32 random hex digits>`) that only `mount_nfs` is told. A local process that
//! finds the port cannot guess the path, so it cannot become a client at all; that is what
//! protects the root handle, which is then given to one MNT (the first, one-shot: a second MNT is
//! refused even from the same connection, and UMNT does not reopen the gate; a deliberate remount
//! within one server's lifetime needs `Server::rearm_mount`, and a daemon restart makes a new
//! export path and gate). Every handle carries a keyed BLAKE3 MAC (random key per
//! server instance), so a process that never obtained handles from the client cannot forge one.
//!
//! Residual risk, stated plainly: any process that can read the mounting client's memory or the
//! kernel NFS state can take a real handle; any process of the same user can use the mount point
//! itself; and the export path reaches the process table of any process that can see the
//! `mount_nfs` command line while it runs (and the mount table lists it afterwards, when the gate is
//! already closed). `check_peer_uid` (an `lsof` lookup) is off by default
//! because the kernel NFS client's socket is invisible to it, so it cannot tell an attacker from
//! the client.
//!
//! # What Translate is worth, measured
//!
//! On this machine, 100 files each created, written and closed, and 100 existing files opened,
//! written and closed, counted with [`nfsserve::take_stats`]: `Translate` 13.0 and 4.0 RPCs per
//! operation, `Hide` 14.1 and 4.0, `Store` 14.1 and 4.0. So about 8% fewer RPCs on create and none
//! on an overwrite: the kernel still sends CREATE, WRITE, SETATTR and COMMIT for the sidecar, and
//! `Translate` only makes them cheap and inode-free. The real difference is the store: 200 inodes
//! for 200 files instead of 400, and no `._` file for a Linux FUSE mount of the same snapshot to
//! trip over. An earlier claim that this cut create+write from 19 RPCs to 13 was wrong; 13.4 was
//! measured for the whole change against 14.3 before it.
//!
//! Tests: `cargo test -p cowfs-nfs` runs the unit and in-process protocol tests. The mount tests
//! and the benchmark are `#[ignore]`d, see `tests/mount.rs` and `tests/bench.rs`.

mod adapter;
mod appledouble;
mod cleanup;
mod convert;
mod errors;
mod handle;
mod mount;
mod peer;
mod sidecar;

pub use adapter::{is_appledouble, Adapter, AdapterOptions, AppleDoubleMode, CowNfs};
pub use appledouble::Sidecar;
pub use cleanup::{install_signal_cleanup, sweep_stale_mounts};
pub use convert::{fattr, nfstime, set_attr, timestamp, FSID};
pub use errors::nfsstat;
pub use handle::{random_key, HandleCodec, Kind, HANDLE_LEN};
pub use mount::{
    is_listed, is_our_export, mount_nfs_available, Mount, MountError, MountOptions, Server,
};
pub use nfsserve::take_stats;
pub use peer::same_user;
