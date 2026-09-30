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
    /// The compare-and-swap version of a file's content did not match.
    #[error("content version conflict")]
    Conflict,
    /// The requested size is inside a chunk. The caller must re-chunk the tail, `put` the new
    /// tail block, then replace the tail with `splice_content` or `set_content`.
    #[error("size is not on a chunk boundary: re-chunk the tail")]
    NeedsRechunk,
    /// A fixed limit was reached (inode numbers, snapshot ids).
    #[error("limit reached: {0}")]
    LimitExceeded(&'static str),
    /// A hook or other callback called back into the store from inside a commit.
    #[error("re-entered the metadata store from a before_sync hook")]
    Reentrant,
    /// The store was closed.
    #[error("metadata store is closed")]
    Closed,
    /// The file is not a cowfs-meta database, or has an unsupported format version.
    #[error("not a usable cowfs-meta database: {0}")]
    Format(String),
}

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

fn storage(e: impl fmt::Display) -> Error {
    Error::Storage(e.to_string())
}

impl From<redb::StorageError> for Error {
    fn from(e: redb::StorageError) -> Self {
        match e {
            redb::StorageError::Corrupted(m) => Error::Corrupt(m),
            other => storage(other),
        }
    }
}

impl From<redb::DatabaseError> for Error {
    fn from(e: redb::DatabaseError) -> Self {
        match e {
            redb::DatabaseError::Storage(s) => s.into(),
            other => storage(other),
        }
    }
}

impl From<redb::TransactionError> for Error {
    fn from(e: redb::TransactionError) -> Self {
        match e {
            redb::TransactionError::Storage(s) => s.into(),
            other => storage(other),
        }
    }
}

impl From<redb::TableError> for Error {
    fn from(e: redb::TableError) -> Self {
        match e {
            redb::TableError::Storage(s) => s.into(),
            other => storage(other),
        }
    }
}

impl From<redb::CommitError> for Error {
    fn from(e: redb::CommitError) -> Self {
        match e {
            redb::CommitError::Storage(s) => s.into(),
            other => storage(other),
        }
    }
}

impl From<redb::SetDurabilityError> for Error {
    fn from(e: redb::SetDurabilityError) -> Self {
        storage(e)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_turns_a_panic_into_corrupt() {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let r: Result<()> = guard(|| panic!("redb went wrong"));
        std::panic::set_hook(prev);
        assert!(matches!(r, Err(Error::Corrupt(_))));
        assert_eq!(guard(|| Ok(7)).unwrap(), 7);
    }

    #[test]
    fn redb_corruption_maps_to_corrupt_and_io_to_storage() {
        let c: Error = redb::StorageError::Corrupted("x".into()).into();
        assert!(matches!(c, Error::Corrupt(_)));
        let d: Error =
            redb::DatabaseError::Storage(redb::StorageError::Corrupted("x".into())).into();
        assert!(matches!(d, Error::Corrupt(_)));
        let io: Error = redb::StorageError::Io(std::io::Error::other("disk")).into();
        assert!(matches!(io, Error::Storage(_)));
    }
}
