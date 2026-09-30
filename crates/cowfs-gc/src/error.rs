//! Errors from the collector.

use std::path::PathBuf;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors from garbage collection.
///
/// A collector that hits one of these leaves the store consistent: the mark wrote nothing, a copy
/// either completed and is indexed or is an unindexed pack that the next cycle reuses, and a
/// pack is only unlinked in the last step, which is the step that reports its own errors before
/// the unlink.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The block store or the metadata database failed.
    #[error(transparent)]
    Store(#[from] cowfs_store::Error),
    /// The metadata database failed.
    #[error(transparent)]
    Meta(#[from] cowfs_meta::Error),
    /// The store or the metadata state directory could not be used.
    #[error("i/o error on {path}: {source}")]
    Io {
        /// Path the call touched.
        path: PathBuf,
        /// The underlying failure.
        source: std::io::Error,
    },
    /// The collector's own state file is unreadable in a way that is not a torn write.
    #[error("collector state at {0} is not usable")]
    BadState(PathBuf),
    /// The caller asked to collect over a store that already reported data loss.
    #[error(
        "store has {0} damaged regions in durable bytes; collect nothing over known data loss"
    )]
    CorruptStore(usize),
}
