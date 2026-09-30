use std::fmt;

/// Errors from the metadata store.
///
/// The first group maps to POSIX errors and is safe to translate to `errno` in a mount adapter.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No such inode, entry, or snapshot content.
    #[error("not found")]
    NotFound,
    /// The name is already used.
    #[error("already exists")]
    Exists,
    /// A directory was required.
    #[error("not a directory")]
    NotDir,
    /// A non-directory was required.
    #[error("is a directory")]
    IsDir,
    /// The directory still has entries.
    #[error("directory not empty")]
    NotEmpty,
    /// The arguments are not valid (bad name, rename into own subtree, bad size).
    #[error("invalid argument: {0}")]
    Invalid(&'static str),
    /// A name or path component is longer than 255 bytes.
    #[error("name too long")]
    NameTooLong,
    /// The extended attribute does not exist.
    #[error("no such attribute")]
    NoAttr,
    /// An xattr value or symlink target is larger than the limit.
    #[error("value too large")]
    TooBig,
    /// The snapshot does not exist (or was removed).
    #[error("no such snapshot")]
    NoSuchSnapshot,
    /// A snapshot with this name already exists.
    #[error("snapshot name already exists")]
    SnapshotExists,
    /// Stored data failed validation: a hash mismatch, a malformed node, a broken invariant.
    #[error("corrupt metadata: {0}")]
    Corrupt(String),
    /// `check()` found inconsistencies.
    #[error("metadata inconsistent: {}", .0.join("; "))]
    Inconsistent(Vec<String>),
    /// The database layer failed (I/O, redb).
    #[error("storage error: {0}")]
    Storage(String),
    /// The `before_sync` hook failed, so the durable commit did not happen.
    #[error("before_sync hook failed: {0}")]
    Hook(std::io::Error),
}

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

fn storage(e: impl fmt::Display) -> Error {
    Error::Storage(e.to_string())
}

macro_rules! from_redb {
    ($($t:ty),*) => {$(
        impl From<$t> for Error {
            fn from(e: $t) -> Self {
                storage(e)
            }
        }
    )*};
}

from_redb!(
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError,
    redb::SetDurabilityError
);

/// Runs `f`, turning a panic inside the storage layer into [`Error::Corrupt`].
///
/// redb 4.3 can panic on some damaged pages instead of returning an error; a corrupt file must
/// never take the process down. Panics from caller code (batch closures, the sync hook) are
/// re-raised by their call sites and never pass through here.
pub(crate) fn guard<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        Err(Error::Corrupt(
            "storage layer panicked on damaged data".into(),
        ))
    })
}
