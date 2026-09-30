use thiserror::Error;

/// Errors a `Vfs` may return. Adapters map them to errno or protocol status codes.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
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
