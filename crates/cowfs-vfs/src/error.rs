use thiserror::Error;

/// Errors a `Vfs` may return. Adapters map them to errno or protocol status codes.
///
/// New variants may be added, so match with a wildcard arm. `Io` always maps to `EIO` and
/// drops the source errno, which is why `NoSpace` is its own variant. `PermissionDenied`
/// maps to `EACCES`, but linking a directory is `EPERM` on Linux and macOS, so adapters
/// map that case themselves.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    #[error("no such file or directory")]
    NotFound,
    #[error("file exists")]
    Exists,
    #[error("not a directory")]
    NotDir,
    #[error("is a directory")]
    IsDir,
    #[error("directory not empty")]
    NotEmpty,
    #[error("invalid argument")]
    InvalidArgument,
    #[error("name too long")]
    NameTooLong,
    #[error("no space left on device")]
    NoSpace,
    #[error("permission denied")]
    PermissionDenied,
    #[error("too many links")]
    TooManyLinks,
    #[error("operation not supported")]
    NotSupported,
    #[error("stale inode")]
    Stale,
    #[error("extended attribute not found")]
    NoAttr,
    #[error("result too large")]
    Range,
    #[error("read-only file system")]
    ReadOnly,
    #[error("cross-device link")]
    CrossDevice,
    #[error("data corruption detected: {0}")]
    Corrupt(String),
    #[error("i/o error: {0}")]
    Io(String),
    #[error("temporarily unavailable, retry")]
    Retry,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// The errno for this error on the current platform.
    pub fn errno(&self) -> i32 {
        match self {
            Error::NotFound => libc::ENOENT,
            Error::Exists => libc::EEXIST,
            Error::NotDir => libc::ENOTDIR,
            Error::IsDir => libc::EISDIR,
            Error::NotEmpty => libc::ENOTEMPTY,
            Error::InvalidArgument => libc::EINVAL,
            Error::NameTooLong => libc::ENAMETOOLONG,
            Error::NoSpace => libc::ENOSPC,
            Error::PermissionDenied => libc::EACCES,
            Error::TooManyLinks => libc::EMLINK,
            Error::NotSupported => libc::ENOTSUP,
            Error::Stale => libc::ESTALE,
            Error::NoAttr => no_attr_errno(),
            Error::Range => libc::ERANGE,
            Error::ReadOnly => libc::EROFS,
            Error::CrossDevice => libc::EXDEV,
            Error::Corrupt(_) | Error::Io(_) => libc::EIO,
            Error::Retry => libc::EAGAIN,
        }
    }
}

#[cfg(target_os = "macos")]
fn no_attr_errno() -> i32 {
    libc::ENOATTR
}

#[cfg(not(target_os = "macos"))]
fn no_attr_errno() -> i32 {
    libc::ENODATA
}
