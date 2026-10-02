//! The daemon process: open the backend, mount it, serve the control API, and shut all of it
//! down in order.
//!
//! Order is fixed and is the reason this is a type rather than three calls in `main`. On
//! SIGTERM, SIGINT or a `shutdown` request the daemon unmounts every export, unmounts the
//! default mount, then stops the control server and closes the backend, all synchronously:
//! nothing is left for a later process to clean up.

use crate::backend::{Backend, PathBackend};
use crate::exports::Exports;
use crate::handler::Handler;
use crate::mounts::{self, Mounted};
use cowfs_ctl::{Server, ServerOptions, ShutdownHandle};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// What the daemon was asked to serve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DaemonConfig {
    /// Block store directory. With the passthrough backend it holds one directory per snapshot.
    pub store: PathBuf,
    /// Where the default mount goes.
    pub mount: PathBuf,
    /// Control socket path.
    pub socket: PathBuf,
    /// Directories a client may export a snapshot inside. Empty means `mount_snapshot` refuses
    /// every path, which is the safe default.
    pub export_roots: Vec<PathBuf>,
}

impl DaemonConfig {
    /// The default configuration for a store, a mount point and a socket.
    pub fn new(store: impl AsRef<Path>, mount: impl AsRef<Path>, socket: impl AsRef<Path>) -> Self {
        Self {
            store: store.as_ref().to_owned(),
            mount: mount.as_ref().to_owned(),
            socket: socket.as_ref().to_owned(),
            export_roots: Vec::new(),
        }
    }

    /// Adds an export root. Repeatable on the command line.
    #[must_use]
    pub fn with_export_root(mut self, root: impl AsRef<Path>) -> Self {
        self.export_roots.push(root.as_ref().to_owned());
        self
    }
}

/// Why the daemon could not start.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// The backend could not be opened.
    #[error("{0}")]
    Open(String),
    /// The control socket could not be bound.
    #[error("cannot serve on {path}: {source}")]
    Bind {
        /// The socket path.
        path: PathBuf,
        /// The failure.
        #[source]
        source: io::Error,
    },
    /// The default mount could not be established.
    #[error("cannot mount {path}: {why}")]
    Mount {
        /// The mount point.
        path: PathBuf,
        /// What the adapter said.
        why: String,
    },
}

/// A running daemon: the backend, the mount, the export registry and the control server.
///
/// Shared so a signal handler can ask it to stop. `Server::wait` consumes the server, so it
/// sits in an `Option` behind a lock and whichever of `stop` or `run` gets there first owns
/// the shutdown; the other sees `None` and returns what the first found.
#[derive(Debug)]
pub struct Daemon {
    handler: Arc<Handler>,
    socket: PathBuf,
    server: std::sync::Mutex<Option<Server>>,
    server_handle: ShutdownHandle,
    /// Set once the control server has stopped and the socket file is gone. A signal thread and
    /// the run loop both end here, and only one of them owns the join, so the other waits on
    /// this rather than returning while the socket is still in the table.
    stopped: std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl Daemon {
    /// Opens the backend, mounts the default view and binds the control socket. The mount is
    /// made before the socket, so a bind failure unmounts rather than leaving a live mount
    /// with nothing to serve it.
    pub fn start(config: &DaemonConfig) -> Result<Arc<Daemon>, DaemonError> {
        let handler = open_handler(config)?;
        let server = Server::start(
            &config.socket,
            Arc::clone(&handler) as Arc<dyn cowfs_ctl::ControlHandler>,
            ServerOptions::default(),
        )
        .map_err(|source| DaemonError::Bind {
            path: config.socket.clone(),
            source,
        })?;
        Ok(Arc::new(Daemon {
            socket: config.socket.clone(),
            server_handle: server.handle(),
            handler,
            server: std::sync::Mutex::new(Some(server)),
            stopped: std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new())),
        }))
    }

    /// The control handler, which is also how a caller reaches the export registry.
    pub fn handler(&self) -> &Arc<Handler> {
        &self.handler
    }

    /// The socket the control server is on.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Where the default mount is.
    pub fn mount_path(&self) -> &Path {
        self.handler.mount_path()
    }

    /// Unmounts everything in order, then stops the control server and waits for it to be
    /// gone. Synchronous, so the caller knows nothing is mounted and no socket is left when it
    /// returns.
    pub fn stop(&self) -> Vec<String> {
        let problems = self.handler.shutdown_mount_tree();
        self.server_handle.shutdown();
        self.wait_stopped();
        problems
    }

    /// Runs until the control server stops, then shuts down in order. A signal or a `shutdown`
    /// request both end here, because both make the server stop.
    pub fn run(&self) -> Vec<String> {
        self.wait_stopped();
        self.handler.shutdown_mount_tree()
    }

    /// Joins the control server if this call owns it, and waits for the owner otherwise. The
    /// server removes its socket as it stops, so returning before that would leave a socket
    /// file that a client sees as a live daemon.
    fn wait_stopped(&self) {
        if let Some(server) = self
            .server
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            server.wait();
            let (done, cvar) = &*self.stopped;
            *done
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
            cvar.notify_all();
            return;
        }
        let (done, cvar) = &*self.stopped;
        let mut done = done
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*done {
            let (guard, timeout) = cvar
                .wait_timeout(done, Duration::from_secs(30))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            done = guard;
            if timeout.timed_out() && !*done {
                eprintln!("cowfs-daemon: the control server did not stop within 30s");
                return;
            }
        }
    }
}

/// Opens the backend, sweeps stale mounts, installs the platform's signal cleanup and mounts
/// the default view, returning the control handler. This is the seam `cowfs serve` uses: it
/// owns the control server already, and the handler owns the mount, so the mount is released
/// when the handler is dropped or when its `shutdown` runs.
pub fn open_handler(config: &DaemonConfig) -> Result<Arc<Handler>, DaemonError> {
    prepare_platform(&config.mount);
    let backend: Arc<dyn Backend> = Arc::new(
        PathBackend::open(&config.store)
            .map_err(|e| DaemonError::Open(format!("{}: {e}", config.store.display())))?,
    );
    let store = backend.store_path().to_owned();
    std::fs::create_dir_all(&config.mount)
        .map_err(|e| DaemonError::Open(format!("{}: {e}", config.mount.display())))?;
    let mount_path = std::fs::canonicalize(&config.mount)
        .map_err(|e| DaemonError::Open(format!("{}: {e}", config.mount.display())))?;
    let exports = Exports::new(
        Arc::clone(&backend),
        config.export_roots.clone(),
        vec![store, mount_path.clone()],
    );
    let vfs = backend
        .root()
        .map_err(|e| DaemonError::Open(format!("cannot open the store: {e}")))?;
    let mount = Mounted::mount(vfs, &mount_path).map_err(|why| DaemonError::Mount {
        path: mount_path.clone(),
        why: why.to_string(),
    })?;
    Ok(Handler::new(backend, Arc::new(mount), exports))
}

/// Sweeps mounts left by a daemon that was killed, before this one mounts anything.
///
/// The platform's own `install_signal_cleanup` is deliberately not called here: it unmounts and
/// then calls `process::exit(128 + sig)` from its own signal thread, which races this daemon's
/// ordered shutdown and wins it most of the time, leaving the control socket on disk. This
/// daemon installs its own handler, unmounts every export and the default mount in order, stops
/// the control server, and only then exits. A stale socket is removed by `cowfs-ctl` at the next
/// start anyway, so the backstop is not needed for correctness.
pub fn prepare_platform(mount_prefix: &Path) {
    for p in mounts::sweep_stale(mount_prefix) {
        eprintln!("cowfs-daemon: swept a stale mount at {}", p.display());
    }
}

/// The platform's backstop: unmount this process's mounts on SIGTERM, SIGINT and SIGHUP and
/// exit. For a process that mounts and does nothing else. A daemon must not install it, because
/// it exits from its own signal thread and preempts the ordered shutdown.
pub fn install_backstop_signal_cleanup() -> io::Result<()> {
    mounts::install_signal_cleanup()
}

/// This process's uid, cached. The mount rules need it and `getuid` is a syscall.
pub fn uid() -> u32 {
    static UID: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *UID.get_or_init(cowfs_ctl::current_uid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_carries_the_three_paths_and_its_export_roots() {
        let c = DaemonConfig::new("/s", "/m", "/sock").with_export_root("/root");
        assert_eq!(
            (c.store, c.mount, c.socket),
            (
                PathBuf::from("/s"),
                PathBuf::from("/m"),
                PathBuf::from("/sock")
            )
        );
        assert_eq!(c.export_roots, [PathBuf::from("/root")]);
    }

    #[test]
    fn uid_is_this_process() {
        assert_eq!(uid(), cowfs_ctl::current_uid());
    }
}
