use thiserror::Error;

/// Why a mount could not be set up, stopped, or ended abnormally.
#[derive(Debug, Error)]
pub enum MountError {
    /// A mount option string token was not recognised or had a bad value.
    #[error("invalid mount option: {0}")]
    InvalidOption(String),
    /// `allow_other` or `auto_unmount` was requested by a non-root user and `/etc/fuse.conf`
    /// does not enable `user_allow_other`, so `fusermount3` would refuse the mount.
    #[error(
        "allow_other and auto_unmount need `user_allow_other` in /etc/fuse.conf for non-root users"
    )]
    NeedsAllowOther,
    /// The kernel or `fusermount3` refused the mount, or the request loop failed.
    #[error("fuse: {0}")]
    Io(#[from] std::io::Error),
    /// The mount could not be unmounted, even lazily, or its request loop did not stop.
    #[error("unmount: {0}")]
    Unmount(String),
}
