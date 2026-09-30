use cowfs_ctl::{ControlHandler, StubHandler};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// What `cowfs serve` was asked to serve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServeConfig {
    /// Block store directory.
    pub store: PathBuf,
    /// Mount point.
    pub mount: PathBuf,
}

/// Why a backend could not be built or opened.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BackendError {
    /// The real core, store and mount adapters are not linked in yet.
    #[error("the real backend is not wired in yet; run with --stub")]
    NotWired,
    /// The backend failed to open its store or mount.
    #[error("{0}")]
    Open(String),
}

/// The seam between the CLI and the daemon's implementation: turns a config into the
/// `ControlHandler` the control server dispatches to. The real core plugs in here.
pub trait Backend {
    /// Opens the store and mount described by `config`.
    fn open(&self, config: &ServeConfig) -> Result<Arc<dyn ControlHandler>, BackendError>;
}

/// Which backend `cowfs serve` should use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    /// In-memory snapshots, pausing `work_delay` between progress events.
    Stub {
        /// Pause between progress events.
        work_delay: Duration,
    },
    /// The real core.
    Real,
}

/// The backend that keeps everything in memory.
#[derive(Clone, Copy, Debug)]
pub struct StubBackend {
    work_delay: Duration,
}

impl Backend for StubBackend {
    fn open(&self, config: &ServeConfig) -> Result<Arc<dyn ControlHandler>, BackendError> {
        Ok(Arc::new(
            StubHandler::new(
                config.store.display().to_string(),
                config.mount.display().to_string(),
            )
            .with_work(4, self.work_delay),
        ))
    }
}

/// The factory `cowfs serve` uses. The `Real` arm is where the core is wired in later.
pub fn make_backend(kind: BackendKind) -> Result<Box<dyn Backend>, BackendError> {
    match kind {
        BackendKind::Stub { work_delay } => Ok(Box::new(StubBackend { work_delay })),
        BackendKind::Real => Err(BackendError::NotWired),
    }
}
