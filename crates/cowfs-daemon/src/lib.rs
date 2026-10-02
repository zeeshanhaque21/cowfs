//! The cowfs daemon: one process that serves a `Vfs` over a mount, exposes the control API of
//! `docs/v1-control-api.md`, and exports snapshots at paths a treehouse client chooses.
//!
//! # What runs where
//!
//! - [`daemon::Daemon`] is the process. It opens the backend, mounts the default view with the
//!   platform adapter (NFS loopback on macOS, FUSE on Linux), starts the control server, and
//!   shuts all three down in order on SIGTERM, SIGINT or a `shutdown` request.
//! - [`handler::Handler`] is the `ControlHandler`: the snapshot namespace, the mount, and the
//!   export registry behind the protocol. It passes `cowfs_ctl::handler_conformance`.
//! - [`exports::Exports`] is `mount_snapshot` and `unmount_snapshot`, and enforces every rule
//!   of the table in `docs/v1-treehouse.md`. One test per row.
//! - [`mounts`] is the platform adapter, including the signal backstop and
//!   `sweep_stale_mounts`.
//! - [`backend::Backend`] is the seam: `PathBackend` today, `MemBackend` for tests, and
//!   `cowfs-core` later with a few lines. Nothing above the trait changes when it lands.
//!
//! # What this backend is not
//!
//! `PathBackend` is a passthrough: snapshots are directories under the store, a clone copies,
//! and there is no block store, so `gc` and `fsck` answer `unsupported` rather than guessing.
//! Every byte is written once and stored once only because the store is a normal filesystem;
//! the dedup, the O(1) snapshots and the content addressing arrive with `cowfs-core`.
//!
//! # Running it
//!
//! ```text
//! cowfs serve --store DIR --mount PATH [--socket PATH] [--export-root DIR]
//! ```
//!
//! `--export-root` is what makes `mount_snapshot` usable: without one, every path is refused.

#![deny(unsafe_code)]

pub mod backend;
pub mod daemon;
pub mod exports;
mod handler;
pub mod holders;
pub mod import;
pub mod mounts;

pub use backend::{Backend, MemBackend, PathBackend, Snapshots};
pub use daemon::{
    install_backstop_signal_cleanup, open_handler, prepare_platform, Daemon, DaemonConfig,
    DaemonError,
};
pub use exports::{Exports, MountSnapshot, UnmountSnapshot};
pub use handler::Handler;
