//! macOS mount adapter for cowfs: serves a [`cowfs_vfs::Vfs`] over an in-process NFSv3 loopback
//! server (a vendored, patched `nfsserve`) and mounts it with `mount_nfs`. Design: `docs/design.md`,
//! findings: `docs/spikes/2-nfs-loopback.md`.
//!
//! `Mount::new` serves a `Vfs` and mounts it (no sudo), dropping the `Mount` unmounts.
//! `MountOptions::hide_appledouble` (default on) hides `._*` sidecars from listings: the macOS
//! client creates them for extended attributes, they stay in the store and can still be looked up,
//! but the mounter never sees them listed. Turn it off to see and copy them as ordinary files.
//!
//! Tests: `cargo test -p cowfs-nfs` runs the unit and in-process protocol tests. The mount tests
//! and the benchmark are `#[ignore]`d, see `tests/mount.rs` and `tests/bench.rs`.

mod adapter;
mod appledouble;
mod convert;
mod errors;
mod handle;
mod mount;
mod peer;
mod sidecar;

pub use adapter::{is_appledouble, Adapter, AdapterOptions, AppleDoubleMode, CowNfs};
pub use appledouble::Sidecar;
pub use convert::{fattr, nfstime, set_attr, timestamp, FSID};
pub use errors::nfsstat;
pub use handle::{random_key, HandleCodec, HANDLE_LEN};
pub use mount::{is_listed, mount_nfs_available, Mount, MountError, MountOptions, Server};
pub use nfsserve::take_stats;
pub use peer::same_user;
