use std::path::PathBuf;

/// Success.
pub const EXIT_OK: i32 = 0;
/// The daemon returned an error, treehouse failed, or another failure.
pub const EXIT_ERROR: i32 = 1;
/// Usage error.
pub const EXIT_USAGE: i32 = 2;
/// The cowfs daemon is not running.
pub const EXIT_NOT_RUNNING: i32 = 3;
/// The cowfs daemon did not answer in time.
pub const EXIT_TIMEOUT: i32 = 4;
/// The slot is held: `busy` from the daemon, or holders survived termination.
pub const EXIT_BUSY: i32 = 5;
/// Interrupted.
pub const EXIT_INTERRUPTED: i32 = 130;

/// Anything the companion can fail with. Every variant maps to a documented exit code, so a
/// script can branch on the code rather than on message text.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Bad arguments or an unusable environment. Exit 2.
    #[error("usage: {0}")]
    Usage(String),
    /// The cowfs daemon is not listening. Exit 3.
    #[error("{0}")]
    NotRunning(String),
    /// The cowfs daemon did not answer in time. Exit 4.
    #[error("{0}")]
    Timeout(String),
    /// A slot is held. Exit 5.
    #[error("{0}")]
    Busy(String),
    /// The cowfs daemon answered with an error. Exit 1.
    #[error("cowfs: {0}")]
    Cowfs(String),
    /// A `treehouse` invocation failed. Exit 1.
    #[error("treehouse: {0}")]
    Treehouse(String),
    /// A filesystem or process operation failed. Exit 1.
    #[error("{0}")]
    Io(String),
    /// The backend does not implement what the requested mode needs. Exit 1.
    #[error("{0}")]
    Unsupported(String),
    /// Interrupted. Exit 130.
    #[error("interrupted")]
    Interrupted,
}

impl Error {
    /// The process exit code for this error.
    pub fn exit_code(&self) -> i32 {
        match self {
            Error::Usage(_) => EXIT_USAGE,
            Error::NotRunning(_) => EXIT_NOT_RUNNING,
            Error::Timeout(_) => EXIT_TIMEOUT,
            Error::Busy(_) => EXIT_BUSY,
            Error::Interrupted => EXIT_INTERRUPTED,
            Error::Cowfs(_) | Error::Treehouse(_) | Error::Io(_) | Error::Unsupported(_) => {
                EXIT_ERROR
            }
        }
    }
}

/// Result alias for the companion.
pub type Result<T> = std::result::Result<T, Error>;

/// Where the running daemon is and how to print. `cowfs-cli` builds this from the flags it has
/// already parsed, so nothing is parsed twice.
#[derive(Clone, Debug, Default)]
pub struct Env {
    /// Control socket override, like `cowfs --socket`.
    pub socket: Option<PathBuf>,
    /// Seconds without a reply before giving up, like `cowfs --timeout`.
    pub timeout: Option<u64>,
    /// Print machine-readable output on stdout.
    pub json: bool,
}
