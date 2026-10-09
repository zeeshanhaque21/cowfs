use crate::backend::{make_backend, Backend, BackendError, BackendKind, ServeConfig};
use crate::cli::{BaseCommand, Cli, Command, SnapshotCommand};
use crate::output::{human, Progress};
use clap::CommandFactory;
use cowfs_ctl::{
    default_socket_path, escape_control, BaseRefreshParams, Canceller, Client, ClientError,
    ClientOptions, Empty, ErrorCode, GcParams, ImportParams, NoParams, PsParams, Request, Response,
    Server, ServerOptions, SnapshotCreate, SnapshotName, SnapshotRename, SnapshotReset, SnapshotRm,
};
use serde_json::{json, Value};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::{Handle, Signals};
use std::ffi::OsString;
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
/// Exit code: usage error, including an argument the protocol cannot carry.
pub const EXIT_USAGE: i32 = 2;
/// Exit code: the daemon is not running.
pub const EXIT_NOT_RUNNING: i32 = 3;
/// Exit code: the daemon did not answer in time.
pub const EXIT_TIMEOUT: i32 = 4;
/// Exit code: interrupted by Ctrl-C.
pub const EXIT_INTERRUPTED: i32 = 130;

/// Default seconds without a reply before a client call gives up.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// How long a Ctrl-C waits for the daemon's final frame before exiting anyway.
const INTERRUPT_GRACE: Duration = Duration::from_secs(2);

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
        for _ in signals.forever() {
            if handle.is_shutting_down() {
                std::process::exit(EXIT_INTERRUPTED);
            }
            handle.shutdown();
        }
    });
    eprintln!(
        "cowfs: serving {} on {}",
        config.mount.display(),
        socket.display()
    );
    server.wait();
    Ok(())
}

/// Writes `text` and a newline to `w` and flushes. A broken pipe means the reader left on
/// purpose and counts as success; any other failure is reported and is an error.
pub fn write_bytes(w: &mut dyn Write, bytes: &[u8]) -> i32 {
    match w.write_all(bytes).and_then(|()| w.flush()) {
        Ok(()) => EXIT_OK,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => EXIT_OK,
        Err(e) => {
            let _ = writeln!(io::stderr(), "cowfs: cannot write to stdout: {e}");
            EXIT_ERROR
        }
    }
}

/// Writes one line, checking the write.
pub fn write_line(w: &mut dyn Write, text: &str) -> i32 {
    write_bytes(w, format!("{text}\n").as_bytes())
}

/// Reports a clap usage error and returns the exit code, honouring `--json`.
pub fn usage_error(json: bool, message: &str) -> i32 {
    report(json, "usage", message, None);
    EXIT_USAGE
}

fn report(json: bool, code: &str, message: &str, details: Option<&Value>) {
    if json {
        let mut error = json!({ "code": code, "message": message });
        if let (Some(d), Some(map)) = (details, error.as_object_mut()) {
            map.insert("details".into(), d.clone());
        }
        let _ = write_line(&mut io::stdout(), &json!({ "error": error }).to_string());
    } else {
        let _ = writeln!(io::stderr(), "cowfs: {}", escape_control(message));
    }
}

/// The snapshot name an import uses when the caller names none: the directory's own name. A
/// directory whose name is not a legal snapshot name (`/`, `.`, a name over 255 bytes) has to be
/// given one explicitly, and the error says so.
fn default_store_name(dir: &Path) -> Result<String, String> {
    let abs = std::path::absolute(dir).unwrap_or_else(|_| dir.to_owned());
    let name = abs
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| cowfs_ctl::validate_snapshot_name(n).is_ok())
        .ok_or_else(|| {
            format!(
                "{} has no usable directory name for a snapshot; pass --store-name",
                abs.display()
            )
        })?;
    Ok(name)
}

fn utf8_path(p: &Path) -> Result<String, String> {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_owned());
    abs.to_str().map(str::to_owned).ok_or_else(|| {
        format!(
            "{} is not valid UTF-8, and the control protocol carries UTF-8 paths only",
            abs.display()
        )
    })
}

fn request_for(command: &Command) -> Result<Option<Request>, String> {
    Ok(Some(match command {
        Command::Serve { .. } | Command::Completions { .. } => return Ok(None),
        Command::Status => Request::Status(Empty {}),
        Command::Snapshot { command } => match command {
            SnapshotCommand::List => Request::SnapshotList(Empty {}),
            SnapshotCommand::Create { name, from } => Request::SnapshotCreate(SnapshotCreate {
                name: name.clone(),
                from: from.clone(),
            }),
            SnapshotCommand::Rm { name, force } => Request::SnapshotRm(SnapshotRm {
                name: name.clone(),
                expect_no_holders: !force,
            }),
            SnapshotCommand::Reset { name, from, force } => Request::SnapshotReset(SnapshotReset {
                name: name.clone(),
                from: from.clone(),
                expect_no_holders: !force,
            }),
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
            path: utf8_path(dir)?,
            // The slot's own name is the obvious snapshot name, so deriving it means the common
            // case needs no flag at all.
            name: match name {
                Some(n) => n.clone(),
                None => default_store_name(dir)?,
            },
        }),
        Command::Base {
            command:
                BaseCommand::Refresh {
                    repo,
                    git_ref,
                    name,
                },
        } => Request::BaseRefresh(BaseRefreshParams {
            repo: utf8_path(repo)?,
            git_ref: git_ref.clone(),
            name: name.clone(),
        }),
        Command::Ps { snapshot } => Request::Ps(PsParams {
            snapshot: snapshot.clone(),
        }),
        Command::MountInfo => Request::MountInfo(Empty {}),
        Command::Shutdown => Request::Shutdown(NoParams {}),
    }))
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
                thread::spawn(|| {
                    thread::sleep(INTERRUPT_GRACE);
                    std::process::exit(EXIT_INTERRUPTED);
                });
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

fn client_command(json: bool, socket: &Path, timeout: u64, request: Request) -> i32 {
    let interrupt = match Interrupt::install() {
        Ok(i) => i,
        Err(e) => {
            report(
                json,
                "internal",
                &format!("cannot install signal handler: {e}"),
                None,
            );
            return EXIT_ERROR;
        }
    };
    let interrupted = || {
        report(json, "cancelled", "interrupted", None);
        EXIT_INTERRUPTED
    };
    let opts = ClientOptions {
        connect_timeout: Duration::from_secs(timeout.min(5)),
        handshake_timeout: Duration::from_secs(timeout.min(5)),
        idle_timeout: Duration::from_secs(timeout),
    };
    let mut client = match Client::connect_with(socket, opts) {
        Ok(c) => c,
        Err(_) if interrupt.fired() => return interrupted(),
        Err(e) => return client_failure(json, socket, &e),
    };
    interrupt.arm(client.canceller());
    if interrupt.fired() {
        return interrupted();
    }
    let mut progress = Progress::new(io::stderr().is_terminal(), json);
    let result = client.call_with_progress(request, |e| progress.update(e));
    progress.finish();
    match result {
        Ok(response) => print_response(json, &response),
        Err(_) if interrupt.fired() => interrupted(),
        Err(e) => client_failure(json, socket, &e),
    }
}

fn print_response(json: bool, response: &Response) -> i32 {
    let text = if json {
        response.data_json().to_string()
    } else {
        human(response)
    };
    let wrote = write_line(&mut io::stdout(), &text);
    if wrote == EXIT_OK {
        return response_exit(response);
    }
    wrote
}

/// The exit code a printed response implies, if the write succeeded.
///
/// A `fsck` that found problems must not look like a clean run to a script: exit 1 so a caller
/// gating on the exit code cannot mistake damage, including a missing live block, for success.
fn response_exit(response: &Response) -> i32 {
    match response {
        Response::Fsck(f) if !f.ok => EXIT_ERROR,
        _ => EXIT_OK,
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
    if err.is_timeout()
        || matches!(err, ClientError::Connect { source, .. } if source.kind() == io::ErrorKind::TimedOut)
    {
        report(json, "timeout", &err.to_string(), None);
        return EXIT_TIMEOUT;
    }
    match err {
        ClientError::Connect { source, .. } if source.kind() == io::ErrorKind::InvalidInput => {
            report(json, "usage", &err.to_string(), None);
            return EXIT_USAGE;
        }
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

fn env_nonempty(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}

/// Runs a parsed command line and returns the process exit code.
pub fn run(cli: Cli) -> i32 {
    let socket = cli
        .socket
        .clone()
        .or_else(|| env_nonempty("COWFS_SOCKET").map(PathBuf::from))
        .unwrap_or_else(default_socket_path);
    let timeout = match cli.timeout {
        Some(t) => t,
        None => match env_nonempty("COWFS_TIMEOUT") {
            None => DEFAULT_TIMEOUT_SECS,
            Some(v) => match v
                .to_str()
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|t| *t > 0)
            {
                Some(t) => t,
                None => {
                    report(
                        cli.json,
                        "usage",
                        "COWFS_TIMEOUT must be a whole number of seconds, at least 1",
                        None,
                    );
                    return EXIT_USAGE;
                }
            },
        },
    };
    match &cli.command {
        Command::Completions { shell } => {
            let mut cmd = Cli::command();
            let mut script: Vec<u8> = Vec::new();
            clap_complete::generate(*shell, &mut cmd, "cowfs", &mut script);
            write_bytes(&mut io::stdout(), &script)
        }
        Command::Serve {
            store,
            mount,
            stub,
            stub_delay_ms,
            stub_ignore_cancel,
        } => {
            if *stub {
                return EXIT_ERROR; // ci-trial: deliberately broken stub serve
            }
            let kind = if *stub {
                BackendKind::Stub {
                    work_delay: Duration::from_millis(*stub_delay_ms),
                    ignore_cancel: *stub_ignore_cancel,
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
            Ok(Some(request)) => client_command(cli.json, &socket, timeout, request),
            Ok(None) => EXIT_ERROR,
            Err(msg) => {
                report(cli.json, "usage", &msg, None);
                EXIT_USAGE
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Failing(io::ErrorKind);

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(self.0))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(self.0))
        }
    }

    fn fsck(ok: bool) -> Response {
        Response::Fsck(cowfs_ctl::FsckReport {
            ok,
            blocks_checked: 1,
            bytes_checked: 1,
            snapshots_checked: 1,
            problems: if ok {
                Vec::new()
            } else {
                vec![cowfs_ctl::FsckProblem {
                    kind: "missing_live_block".into(),
                    detail: "block deadbeef".into(),
                }]
            },
        })
    }

    #[test]
    fn a_fsck_with_problems_exits_nonzero() {
        assert_eq!(response_exit(&fsck(true)), EXIT_OK);
        assert_eq!(response_exit(&fsck(false)), EXIT_ERROR);
        assert_eq!(response_exit(&Response::Ok(cowfs_ctl::Empty {})), EXIT_OK);
    }

    #[test]
    fn write_errors_fail_the_command_but_a_closed_pipe_does_not() {
        assert_eq!(write_line(&mut Vec::new(), "x"), EXIT_OK);
        assert_eq!(
            write_line(&mut Failing(io::ErrorKind::BrokenPipe), "x"),
            EXIT_OK
        );
        assert_eq!(
            write_line(&mut Failing(io::ErrorKind::StorageFull), "x"),
            EXIT_ERROR
        );
        assert_eq!(
            write_line(&mut Failing(io::ErrorKind::PermissionDenied), "x"),
            EXIT_ERROR
        );
    }
}
