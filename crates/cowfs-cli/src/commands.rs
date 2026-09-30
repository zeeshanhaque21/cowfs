use crate::backend::{make_backend, Backend, BackendError, BackendKind, ServeConfig};
use crate::cli::{BaseCommand, Cli, Command, SnapshotCommand};
use crate::output::{human, Progress};
use clap::CommandFactory;
use cowfs_ctl::{
    default_socket_path, BaseRefreshParams, Canceller, Client, ClientError, Empty, ErrorCode,
    GcParams, ImportParams, PsParams, Request, Response, Server, ServerOptions, SnapshotCreate,
    SnapshotName, SnapshotRename,
};
use serde_json::{json, Value};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::{Handle, Signals};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

/// Exit code: success.
pub const EXIT_OK: i32 = 0;
/// Exit code: the daemon returned an error, or another failure.
pub const EXIT_ERROR: i32 = 1;
/// Exit code: the daemon is not running.
pub const EXIT_NOT_RUNNING: i32 = 3;
/// Exit code: interrupted by Ctrl-C.
pub const EXIT_INTERRUPTED: i32 = 130;

/// Why `serve` could not start.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The backend could not be built or opened.
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// The control socket could not be bound.
    #[error("cannot serve on {}: {source}", path.display())]
    Bind {
        /// The socket path.
        path: PathBuf,
        /// The underlying failure.
        source: io::Error,
    },
    /// Installing signal handlers failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Opens the backend and binds the control socket. Does not touch signal handlers.
pub fn start_server(
    backend: &dyn Backend,
    config: &ServeConfig,
    socket: &Path,
) -> Result<Server, ServeError> {
    let handler = backend.open(config)?;
    Server::start(socket, handler, ServerOptions::default()).map_err(|source| ServeError::Bind {
        path: socket.to_owned(),
        source,
    })
}

fn serve(backend: &dyn Backend, config: &ServeConfig, socket: &Path) -> Result<(), ServeError> {
    let server = start_server(backend, config, socket)?;
    let handle = server.handle();
    let mut signals = Signals::new([SIGINT, SIGTERM])?;
    thread::spawn(move || {
        if signals.forever().next().is_some() {
            handle.shutdown();
        }
    });
    eprintln!("cowfs: serving {} on {}", config.mount.display(), socket.display());
    server.wait();
    Ok(())
}

fn out(line: &str) {
    let _ = writeln!(io::stdout(), "{line}");
}

fn report(json: bool, code: &str, message: &str, details: Option<&Value>) {
    if json {
        let mut error = json!({ "code": code, "message": message });
        if let (Some(d), Some(map)) = (details, error.as_object_mut()) {
            map.insert("details".into(), d.clone());
        }
        let _ = writeln!(io::stderr(), "{}", json!({ "error": error }));
    } else {
        let _ = writeln!(io::stderr(), "cowfs: {message}");
    }
}

fn absolute(p: &Path) -> String {
    std::path::absolute(p)
        .unwrap_or_else(|_| p.to_owned())
        .to_string_lossy()
        .into_owned()
}

fn request_for(command: &Command) -> Option<Request> {
    Some(match command {
        Command::Serve { .. } | Command::Completions { .. } => return None,
        Command::Status => Request::Status(Empty {}),
        Command::Snapshot { command } => match command {
            SnapshotCommand::List => Request::SnapshotList(Empty {}),
            SnapshotCommand::Create { name, from } => Request::SnapshotCreate(SnapshotCreate {
                name: name.clone(),
                from: from.clone(),
            }),
            SnapshotCommand::Rm { name } => Request::SnapshotRm(SnapshotName { name: name.clone() }),
            SnapshotCommand::Rename { from, to } => Request::SnapshotRename(SnapshotRename {
                from: from.clone(),
                to: to.clone(),
            }),
            SnapshotCommand::Promote { name } => {
                Request::SnapshotPromote(SnapshotName { name: name.clone() })
            }
        },
        Command::Gc { dry_run } => Request::Gc(GcParams { dry_run: *dry_run }),
        Command::Fsck => Request::Fsck(Empty {}),
        Command::Import { dir, name } => Request::Import(ImportParams {
            path: absolute(dir),
            name: name.clone(),
        }),
        Command::Base {
            command:
                BaseCommand::Refresh {
                    repo,
                    git_ref,
                    name,
                },
        } => Request::BaseRefresh(BaseRefreshParams {
            repo: absolute(repo),
            git_ref: git_ref.clone(),
            name: name.clone(),
        }),
        Command::Ps { snapshot } => Request::Ps(PsParams {
            snapshot: snapshot.clone(),
        }),
        Command::MountInfo => Request::MountInfo(Empty {}),
        Command::Shutdown => Request::Shutdown(Empty {}),
    })
}

struct Interrupt {
    fired: Arc<AtomicBool>,
    slot: Arc<Mutex<Option<Canceller>>>,
    handle: Handle,
}

impl Interrupt {
    fn install() -> io::Result<Interrupt> {
        let mut signals = Signals::new([SIGINT])?;
        let handle = signals.handle();
        let fired = Arc::new(AtomicBool::new(false));
        let slot: Arc<Mutex<Option<Canceller>>> = Arc::default();
        let (f, s) = (Arc::clone(&fired), Arc::clone(&slot));
        thread::spawn(move || {
            for _ in signals.forever() {
                if f.swap(true, Ordering::SeqCst) {
                    std::process::exit(EXIT_INTERRUPTED);
                }
                if let Some(c) = s.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
                    let _ = c.cancel();
                }
            }
        });
        Ok(Interrupt {
            fired,
            slot,
            handle,
        })
    }

    fn arm(&self, canceller: Canceller) {
        *self.slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(canceller);
    }

    fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        self.handle.close();
    }
}

fn client_command(json: bool, socket: &Path, request: Request) -> i32 {
    let interrupt = match Interrupt::install() {
        Ok(i) => i,
        Err(e) => {
            report(json, "internal", &format!("cannot install signal handler: {e}"), None);
            return EXIT_ERROR;
        }
    };
    let interrupted = || {
        report(json, "cancelled", "interrupted", None);
        EXIT_INTERRUPTED
    };
    let mut client = match Client::connect(socket) {
        Ok(c) => c,
        Err(_) if interrupt.fired() => return interrupted(),
        Err(e) => return client_failure(json, socket, &e),
    };
    interrupt.arm(client.canceller());
    if interrupt.fired() {
        return interrupted();
    }
    let mut progress = Progress::new(io::stderr().is_terminal());
    let result = client.call_with_progress(request, |e| progress.update(e));
    progress.finish();
    match result {
        Ok(response) => {
            print_response(json, &response);
            EXIT_OK
        }
        Err(_) if interrupt.fired() => interrupted(),
        Err(e) => client_failure(json, socket, &e),
    }
}

fn print_response(json: bool, response: &Response) {
    if json {
        let data = serde_json::to_value(response)
            .ok()
            .and_then(|mut v| v.get_mut("data").map(Value::take))
            .unwrap_or(Value::Null);
        out(&data.to_string());
    } else {
        out(&human(response));
    }
}

fn client_failure(json: bool, socket: &Path, err: &ClientError) -> i32 {
    if err.is_not_running() {
        let msg = format!(
            "no cowfs daemon is running on {} (start one with `cowfs serve`)",
            socket.display()
        );
        report(json, "not_running", &msg, None);
        return EXIT_NOT_RUNNING;
    }
    match err {
        ClientError::Server(e) => {
            let code = match &e.code {
                ErrorCode::Other(s) => s.as_str(),
                known => known.as_str(),
            };
            report(json, code, &e.message, e.details.as_ref());
        }
        other => report(json, "io_error", &other.to_string(), None),
    }
    EXIT_ERROR
}

/// Runs a parsed command line and returns the process exit code.
pub fn run(cli: Cli) -> i32 {
    let socket = cli.socket.clone().unwrap_or_else(default_socket_path);
    match &cli.command {
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            clap_complete::generate(*shell, &mut cmd, "cowfs", &mut io::stdout());
            EXIT_OK
        }
        Command::Serve {
            store,
            mount,
            stub,
            stub_delay_ms,
        } => {
            let kind = if *stub {
                BackendKind::Stub {
                    work_delay: Duration::from_millis(*stub_delay_ms),
                }
            } else {
                BackendKind::Real
            };
            let config = ServeConfig {
                store: store.clone(),
                mount: mount.clone(),
            };
            let result = make_backend(kind)
                .map_err(ServeError::from)
                .and_then(|backend| serve(backend.as_ref(), &config, &socket));
            match result {
                Ok(()) => EXIT_OK,
                Err(e) => {
                    report(cli.json, "serve_failed", &e.to_string(), None);
                    EXIT_ERROR
                }
            }
        }
        command => match request_for(command) {
            Some(request) => client_command(cli.json, &socket, request),
            None => EXIT_ERROR,
        },
    }
}
