use crate::error::{CtlError, CtlResult, ErrorCode};
use crate::frame::{
    read_line, ClientFrame, LineRead, ServerFrame, ServerHello, MAX_REQUEST_LINE, PROTOCOL_VERSION,
};
use crate::handler::{CancelToken, ControlHandler, OpContext};
use crate::types::*;
use crate::{socket, sys};
use serde_json::json;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const MAX_DRAIN: u64 = 16 << 20;
const SUPPORTED_VERSIONS: &[u32] = &[PROTOCOL_VERSION];

/// Tunables of the server framework. The defaults are the ones in `docs/v1-control-api.md`.
#[derive(Clone, Debug)]
pub struct ServerOptions {
    /// Reported in `hello` and `version`.
    pub server_name: String,
    /// Peers with any other uid are refused. Defaults to the uid of this process.
    pub expected_uid: u32,
    /// How long a new connection may take to send `hello`.
    pub handshake_timeout: Duration,
    /// A client that does not drain its socket for this long is disconnected.
    pub write_timeout: Duration,
    /// Requests in flight per connection.
    pub max_inflight: usize,
}

impl Default for ServerOptions {
    fn default() -> Self {
        ServerOptions {
            server_name: format!("cowfs-ctl/{}", env!("CARGO_PKG_VERSION")),
            expected_uid: sys::current_uid(),
            handshake_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(30),
            max_inflight: 32,
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Default)]
struct Shared {
    stopping: AtomicBool,
    next_conn: AtomicU64,
    conns: Mutex<HashMap<u64, UnixStream>>,
}

/// Asks a running server to stop. Cheap to clone and safe to use from a signal thread.
#[derive(Clone, Debug)]
pub struct ShutdownHandle(Arc<Shared>);

impl ShutdownHandle {
    /// Begins graceful shutdown: no new connections, in-flight requests are cancelled.
    pub fn shutdown(&self) {
        self.0.stopping.store(true, Ordering::SeqCst);
    }
}

/// A running control server.
#[derive(Debug)]
pub struct Server {
    path: PathBuf,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Binds `path` (see `docs/v1-control-api.md` for the directory, lock and stale socket rules)
    /// and starts accepting connections on a background thread.
    pub fn start(
        path: &Path,
        handler: Arc<dyn ControlHandler>,
        opts: ServerOptions,
    ) -> io::Result<Server> {
        let (listener, lock_file) = socket::bind(path)?;
        listener.set_nonblocking(true)?;
        let shared = Arc::new(Shared::default());
        let thread = thread::Builder::new()
            .name("cowfs-ctl-accept".into())
            .spawn({
                let shared = Arc::clone(&shared);
                let path = path.to_owned();
                move || accept_loop(listener, lock_file, &path, handler, &opts, &shared)
            })?;
        Ok(Server {
            path: path.to_owned(),
            shared,
            thread: Some(thread),
        })
    }

    /// The socket path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A handle that can stop the server from another thread.
    pub fn handle(&self) -> ShutdownHandle {
        ShutdownHandle(Arc::clone(&self.shared))
    }

    /// Blocks until the server has stopped (a `shutdown` request or `ShutdownHandle::shutdown`)
    /// and every connection is closed.
    pub fn wait(mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// Stops the server and waits for it.
    pub fn shutdown(self) {
        self.handle().shutdown();
        self.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            self.shared.stopping.store(true, Ordering::SeqCst);
            let _ = t.join();
        }
    }
}

fn accept_loop(
    listener: UnixListener,
    lock_file: File,
    path: &Path,
    handler: Arc<dyn ControlHandler>,
    opts: &ServerOptions,
    shared: &Arc<Shared>,
) {
    let opts = Arc::new(opts.clone());
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    while !shared.stopping.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                workers.retain(|w| !w.is_finished());
                let (handler, opts, shared) =
                    (Arc::clone(&handler), Arc::clone(&opts), Arc::clone(shared));
                if let Ok(w) = thread::Builder::new()
                    .name("cowfs-ctl-conn".into())
                    .spawn(move || serve_connection(stream, handler.as_ref(), &opts, &shared))
                {
                    workers.push(w);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
    for stream in lock(&shared.conns).values() {
        let _ = stream.shutdown(Shutdown::Both);
    }
    for w in workers {
        let _ = w.join();
    }
    let _ = fs::remove_file(path);
    drop(lock_file);
}

struct Conn {
    stream: UnixStream,
    write_lock: Mutex<()>,
    dead: AtomicBool,
    inflight: Mutex<HashMap<u64, CancelToken>>,
}

impl Conn {
    fn send(&self, frame: &ServerFrame) -> bool {
        if self.dead.load(Ordering::SeqCst) {
            return false;
        }
        let ok = {
            let _guard = lock(&self.write_lock);
            (&self.stream).write_all(&frame.encode()).is_ok()
        };
        if !ok {
            self.kill();
        }
        ok
    }

    fn send_error(&self, id: Option<u64>, error: CtlError) -> bool {
        self.send(&ServerFrame::Error { id, error })
    }

    fn cancel_all(&self) {
        for token in lock(&self.inflight).values() {
            token.cancel();
        }
    }

    fn kill(&self) {
        self.dead.store(true, Ordering::SeqCst);
        self.cancel_all();
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

fn serve_connection(
    stream: UnixStream,
    handler: &dyn ControlHandler,
    opts: &ServerOptions,
    shared: &Shared,
) {
    let conn_id = shared.next_conn.fetch_add(1, Ordering::SeqCst);
    if let Ok(clone) = stream.try_clone() {
        lock(&shared.conns).insert(conn_id, clone);
    }
    if !shared.stopping.load(Ordering::SeqCst) {
        let _ = run_connection(stream, handler, opts, shared);
    }
    lock(&shared.conns).remove(&conn_id);
}

fn run_connection(
    stream: UnixStream,
    handler: &dyn ControlHandler,
    opts: &ServerOptions,
    shared: &Shared,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(opts.write_timeout))?;
    stream.set_read_timeout(Some(opts.handshake_timeout))?;
    let conn = Conn {
        stream: stream.try_clone()?,
        write_lock: Mutex::new(()),
        dead: AtomicBool::new(false),
        inflight: Mutex::new(HashMap::new()),
    };
    let result = run_frames(&conn, stream, handler, opts, shared);
    conn.kill();
    result
}

fn run_frames(
    conn: &Conn,
    stream: UnixStream,
    handler: &dyn ControlHandler,
    opts: &ServerOptions,
    shared: &Shared,
) -> io::Result<()> {
    match sys::peer_uid(&stream) {
        Ok(uid) if uid == opts.expected_uid => {}
        _ => {
            conn.send_error(
                None,
                CtlError::new(
                    ErrorCode::PermissionDenied,
                    "peer uid does not match the server",
                ),
            );
            return Ok(());
        }
    }
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    if !handshake(conn, &mut reader, &mut buf, opts) {
        return Ok(());
    }
    reader.get_ref().set_read_timeout(None)?;
    thread::scope(|scope| {
        loop {
            buf.clear();
            match read_line(&mut reader, &mut buf, MAX_REQUEST_LINE) {
                Ok(LineRead::Line) => {}
                Ok(LineRead::TooLong) => {
                    reject_oversized(conn, &mut reader);
                    break;
                }
                Ok(LineRead::Eof) | Err(_) => break,
            }
            if buf.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match ClientFrame::decode(&buf) {
                Err(fe) => {
                    conn.send_error(fe.id, fe.error);
                }
                Ok(ClientFrame::Hello(_)) => {
                    conn.send_error(
                        None,
                        CtlError::new(ErrorCode::MalformedFrame, "hello was already sent"),
                    );
                }
                Ok(ClientFrame::Cancel { id }) => {
                    if let Some(token) = lock(&conn.inflight).get(&id) {
                        token.cancel();
                    }
                }
                Ok(ClientFrame::Request { id, request }) => {
                    if shared.stopping.load(Ordering::SeqCst) {
                        conn.send_error(
                            Some(id),
                            CtlError::new(ErrorCode::ShuttingDown, "server is shutting down"),
                        );
                        continue;
                    }
                    let token = CancelToken::new();
                    let refusal = {
                        let mut inflight = lock(&conn.inflight);
                        if inflight.contains_key(&id) {
                            Some(CtlError::new(
                                ErrorCode::DuplicateId,
                                "request id already in flight",
                            ))
                        } else if inflight.len() >= opts.max_inflight {
                            Some(CtlError::new(
                                ErrorCode::Busy,
                                "too many requests in flight",
                            ))
                        } else {
                            inflight.insert(id, token.clone());
                            None
                        }
                    };
                    if let Some(error) = refusal {
                        conn.send_error(Some(id), error);
                        continue;
                    }
                    scope.spawn(move || {
                        run_request(conn, handler, opts, shared, id, request, token)
                    });
                }
            }
            if conn.dead.load(Ordering::SeqCst) {
                break;
            }
        }
        conn.cancel_all();
    });
    Ok(())
}

/// Sends `line_too_long`, then drains what the client is still sending so that closing does not
/// reset the connection and destroy the error frame.
fn reject_oversized(conn: &Conn, reader: &mut BufReader<UnixStream>) {
    conn.send_error(
        None,
        CtlError::new(ErrorCode::LineTooLong, "request line too long"),
    );
    let _ = conn.stream.shutdown(Shutdown::Write);
    let _ = reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(1)));
    let _ = io::copy(&mut reader.by_ref().take(MAX_DRAIN), &mut io::sink());
}

fn handshake(
    conn: &Conn,
    reader: &mut BufReader<UnixStream>,
    buf: &mut Vec<u8>,
    opts: &ServerOptions,
) -> bool {
    match read_line(reader, buf, MAX_REQUEST_LINE) {
        Ok(LineRead::Line) => {}
        Ok(LineRead::TooLong) => {
            reject_oversized(conn, reader);
            return false;
        }
        Ok(LineRead::Eof) | Err(_) => return false,
    }
    let hello = match ClientFrame::decode(buf) {
        Ok(ClientFrame::Hello(h)) => h,
        Err(fe) if fe.error.code == ErrorCode::MalformedFrame => {
            conn.send_error(None, fe.error);
            return false;
        }
        _ => {
            conn.send_error(
                None,
                CtlError::new(ErrorCode::HandshakeRequired, "send hello first"),
            );
            return false;
        }
    };
    let Some(version) = hello
        .versions
        .iter()
        .copied()
        .filter(|v| SUPPORTED_VERSIONS.contains(v))
        .max()
    else {
        conn.send_error(
            None,
            CtlError::new(ErrorCode::UnsupportedVersion, "no common protocol version")
                .with_details(json!({ "supported": SUPPORTED_VERSIONS })),
        );
        return false;
    };
    conn.send(&ServerFrame::Hello(ServerHello {
        version,
        server: opts.server_name.clone(),
        methods: METHODS.iter().map(|m| (*m).to_owned()).collect(),
    }))
}

fn run_request(
    conn: &Conn,
    handler: &dyn ControlHandler,
    opts: &ServerOptions,
    shared: &Shared,
    id: u64,
    request: Request,
    token: CancelToken,
) {
    let ctx = OpContext::new(token, |event| {
        conn.send(&ServerFrame::Progress { id, event })
    });
    let is_shutdown = matches!(request, Request::Shutdown(_));
    let result = catch_unwind(AssertUnwindSafe(|| dispatch(handler, opts, request, &ctx)))
        .unwrap_or_else(|_| Err(CtlError::new(ErrorCode::Internal, "handler panicked")));
    lock(&conn.inflight).remove(&id);
    let stop = is_shutdown && result.is_ok();
    match result {
        Ok(result) => conn.send(&ServerFrame::Response { id, result }),
        Err(error) => conn.send_error(Some(id), error),
    };
    if stop {
        shared.stopping.store(true, Ordering::SeqCst);
    }
}

fn name(s: &str) -> CtlResult<()> {
    cowfs_vfs::validate_name(s.as_bytes())
        .map_err(|e| CtlError::invalid(format!("invalid snapshot name {s:?}: {e}")))
}

fn dispatch(
    h: &dyn ControlHandler,
    opts: &ServerOptions,
    request: Request,
    ctx: &OpContext<'_>,
) -> CtlResult<Response> {
    Ok(match request {
        Request::Ping(_) => Response::Pong(Empty {}),
        Request::Version(_) => Response::Version(VersionInfo {
            protocol: PROTOCOL_VERSION,
            server: opts.server_name.clone(),
            ctl: env!("CARGO_PKG_VERSION").to_owned(),
        }),
        Request::Status(_) => Response::Status(h.status()?),
        Request::SnapshotList(_) => Response::SnapshotList(SnapshotList {
            snapshots: h.snapshot_list()?,
        }),
        Request::SnapshotCreate(p) => {
            name(&p.name)?;
            if let Some(from) = &p.from {
                name(from)?;
            }
            Response::Snapshot(h.snapshot_create(p)?)
        }
        Request::SnapshotRm(p) => {
            name(&p.name)?;
            h.snapshot_rm(&p.name)?;
            Response::Ok(Empty {})
        }
        Request::SnapshotRename(p) => {
            name(&p.from)?;
            name(&p.to)?;
            Response::Snapshot(h.snapshot_rename(&p.from, &p.to)?)
        }
        Request::SnapshotPromote(p) => {
            name(&p.name)?;
            Response::Snapshot(h.snapshot_promote(&p.name)?)
        }
        Request::Gc(p) => Response::Gc(h.gc(p, ctx)?),
        Request::Fsck(_) => Response::Fsck(h.fsck(ctx)?),
        Request::Import(p) => {
            name(&p.name)?;
            Response::Import(h.import(p, ctx)?)
        }
        Request::BaseRefresh(p) => {
            if let Some(n) = &p.name {
                name(n)?;
            }
            Response::BaseRefresh(h.base_refresh(p, ctx)?)
        }
        Request::Ps(p) => {
            name(&p.snapshot)?;
            Response::Processes(ProcessList {
                processes: h.ps(&p.snapshot)?,
            })
        }
        Request::MountInfo(_) => Response::MountInfo(h.mount_info()?),
        Request::Shutdown(_) => {
            h.shutdown()?;
            Response::Ok(Empty {})
        }
    })
}
