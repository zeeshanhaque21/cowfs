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
        /// Run long operations to the end even when cancelled.
        ignore_cancel: bool,
    },
    /// The real core.
    Real,
}

/// The backend that keeps everything in memory.
#[derive(Clone, Copy, Debug)]
pub struct StubBackend {
    work_delay: Duration,
    ignore_cancel: bool,
}

impl Backend for StubBackend {
    fn open(&self, config: &ServeConfig) -> Result<Arc<dyn ControlHandler>, BackendError> {
        let stub = StubHandler::new(
            config.store.display().to_string(),
            config.mount.display().to_string(),
        )
        .with_work(4, self.work_delay);
        Ok(Arc::new(if self.ignore_cancel {
            stub.ignoring_cancel()
        } else {
            stub
        }))
    }
}

/// The real backend: `cowfs-daemon` opens the store, sweeps stale mounts, installs the
/// platform's signal cleanup and mounts it. The handler owns the mount, so dropping it (or
/// its `shutdown`) unmounts, and `cowfs serve` already owns the control server and the signal
/// handling.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealBackend;

impl Backend for RealBackend {
    fn open(&self, config: &ServeConfig) -> Result<Arc<dyn ControlHandler>, BackendError> {
        let daemon = cowfs_daemon::DaemonConfig::new(&config.store, &config.mount, "");
        cowfs_daemon::open_handler(&daemon)
            .map(|h| h as Arc<dyn ControlHandler>)
            .map_err(|e| BackendError::Open(e.to_string()))
    }
}

/// The factory `cowfs serve` uses. `Real` is the daemon; `Stub` is the in-memory one the
/// tests and `cowfs serve --stub` use.
pub fn make_backend(kind: BackendKind) -> Result<Box<dyn Backend>, BackendError> {
    match kind {
        BackendKind::Stub {
            work_delay,
            ignore_cancel,
        } => Ok(Box::new(StubBackend {
            work_delay,
            ignore_cancel,
        })),
        BackendKind::Real => Ok(Box::new(RealBackend)),
    }
}
