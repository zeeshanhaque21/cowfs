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
//! `._name` files. `Translate` (default) parses and synthesises them in the adapter and maps the
//! attributes to `Vfs` xattrs of the real file, so no sidecar inode ever exists. `Hide` stores
//! them and hides them from listings. `Store` treats `._` names like any other.
//!
//! # What this crate requires of every `Vfs`
//!
//! - An `Ino` is never reused for a different file within a mount's lifetime. The adapter adds a
//!   per-inode generation to file handles as a second line of defence (a handle minted before the
//!   last name of an inode went away is `STALE`), but only for removals it performed itself.
//! - `Ino` values stay below 2^63: the top bit marks a translated sidecar.
//! - `lookup`, `create`, `mkdir`, `symlink` and `link` each take one reference and the adapter
//!   gives every one back at once with `forget`. NFS handles are stateless, so nothing is pinned.
//! - `getattr`, `lookup` and `statfs` may be called on the network thread when
//!   `inline_metadata` is set, so they must not block for long.
//!
//! # Security model
//!
//! The server listens on 127.0.0.1 only, and answers MNT for one export path
//! (`localhost:/cowfs-<32 random hex digits>`) that only `mount_nfs` is told. A local process that
//! finds the port cannot guess the path, so it cannot become a client at all; that is what
//! protects the root handle, which is then given to one connection (the first MNT, one-shot,
//! `Server::rearm_mount` for a remount). Every handle carries a keyed BLAKE3 MAC (random key per
//! server instance), so a process that never obtained handles from the client cannot forge one.
//!
//! Residual risk, stated plainly: any process that can read the mounting client's memory or the
//! kernel NFS state can take a real handle; any process of the same user can use the mount point
//! itself; and the export path reaches the process table of any process that can see the
//! `mount_nfs` command line while it runs. `check_peer_uid` (an `lsof` lookup) is off by default
//! because the kernel NFS client's socket is invisible to it, so it cannot tell an attacker from
//! the client.
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
pub use handle::{random_key, HandleCodec, HANDLE_LEN};
pub use mount::{
    is_listed, is_our_export, mount_nfs_available, Mount, MountError, MountOptions, Server,
};
pub use nfsserve::take_stats;
pub use peer::same_user;
