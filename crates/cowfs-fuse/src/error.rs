use thiserror::Error;

/// Why a mount could not be set up or ended abnormally.
#[derive(Debug, Error)]
pub enum MountError {
    /// A mount option string token was not recognised or had a bad value.
    #[error("invalid mount option: {0}")]
    InvalidOption(String),
    /// The kernel or `fusermount3` refused the mount, or the request loop failed.
    #[error("fuse: {0}")]
    Io(#[from] std::io::Error),
}
