//! Linux FUSE mount adapter for cowfs: a thin translation from kernel requests to `cowfs_vfs::Vfs`.
//!
//! On other platforms this crate builds as an empty library apart from the pure helpers.
//!
//! # Caching and consistency
//!
//! The defaults (see [`MountOptions`]) let the kernel cache names, attributes and file pages
//! for an hour. That is only correct while every change to the tree goes through this mount.
//! A change made any other way must be announced with [`Mount::invalidate_inode`] and
//! [`Mount::invalidate_entry`].
//!
//! # Threading
//!
//! The `fuser` 0.15 request loop is single threaded and this adapter runs every `Vfs` call
//! on that thread. In the spike 3 measurements, handing requests to a worker pool made
//! lookups slower (15 to 34 microseconds against 6) and no-op builds twice as slow, so
//! there is none. The `Vfs` must tolerate calls from this thread while other threads call it
//! too, and a slow `Vfs` call stalls the whole mount.
//!
//! # Not supported
//!
//! Device nodes, fifos and sockets (`mknod` of anything but a regular file is `ENOTSUP`),
//! `RENAME_EXCHANGE` (`ENOTSUP`), `fallocate` and `copy_file_range` (`ENOTSUP`), and FUSE
//! locks: the kernel handles `flock` and POSIX locks locally.

pub mod bench;
pub mod convert;
pub mod dir;
mod error;
pub mod options;

#[cfg(target_os = "linux")]
mod fs;
#[cfg(target_os = "linux")]
mod mount;

pub use error::MountError;
pub use options::MountOptions;

#[cfg(target_os = "linux")]
pub use mount::{run, Mount};

#[cfg(all(test, target_os = "linux"))]
mod mount_tests;
#[cfg(test)]
mod stub;
