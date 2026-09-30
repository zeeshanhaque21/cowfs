//! macOS mount adapter for cowfs: serves a [`cowfs_vfs::Vfs`] over an in-process NFSv3 loopback
//! server (a vendored, patched `nfsserve`) and mounts it with `mount_nfs`. Design: `docs/design.md`,
//! findings: `docs/spikes/2-nfs-loopback.md`.

mod adapter;
mod convert;
mod errors;
mod mount;

pub use adapter::{is_appledouble, Adapter, AdapterOptions, CowNfs};
pub use convert::{fattr, nfstime, set_attr, timestamp, FSID};
pub use errors::nfsstat;
pub use mount::{is_listed, mount_nfs_available, Mount, MountError, MountOptions, Server};
pub use nfsserve::take_stats;
