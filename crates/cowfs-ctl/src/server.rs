use crate::error::{CtlError, CtlResult, ErrorCode};
use crate::frame::{
    read_line_until, ClientFrame, LineRead, ReadLimits, ServerFrame, ServerHello, MAX_REQUEST_LINE,
    PROTOCOL_VERSION,
};
use crate::handler::{CancelToken, ControlHandler, HolderGuard, OpContext};
use crate::types::*;
use crate::validate::{validate_abs_path, validate_git_ref, validate_repo, validate_snapshot_name};
use crate::{socket, sys};
use serde_json::json;
use std::collections::HashMap;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const SUPPORTED_VERSIONS: &[u32] = &[PROTOCOL_VERSION];
const POLL: Duration = Duration::from_millis(200);
const MAX_DRAIN: u64 = 16 << 20;

type PeerFn = dyn Fn(&UnixStream) -> io::Result<u32> + Send + Sync;

/// Reads the uid of the process on the other end of a connection. The default asks the kernel;
/// tests substitute one to exercise the failure paths.
#[derive(Clone)]
pub struct PeerCheck(Arc<PeerFn>);

impl PeerCheck {
    /// Reads the peer uid from the kernel (`SO_PEERCRED` or `getpeereid`).
    pub fn system() -> Self {
        PeerCheck(Arc::new(sys::peer_uid))
    }

    /// Uses `f` to find the peer uid.
    pub fn new(f: impl Fn(&UnixStream) -> io::Result<u32> + Send + Sync + 'static) -> Self {
        PeerCheck(Arc::new(f))
    }
}

impl fmt::Debug for PeerCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PeerCheck")
    }
}

/// Tunables of the server framework. The defaults are the ones in `docs/v1-control-api.md`.
#[derive(Clone, Debug)]
pub struct ServerOptions {
    /// Reported in `hello` and `version`.
    pub server_name: String,
    /// The only uid allowed to connect, and the required owner of the socket directory.
    /// Defaults to the uid of this process.
    pub expected_uid: u32,
    /// How the peer uid is read.
    pub peer_check: PeerCheck,
    /// Total time a new connection has to complete the handshake.
    pub handshake_timeout: Duration,
    /// A connection with nothing in flight and no request for this long is closed.
    pub idle_timeout: Duration,
    /// A connection with nothing in flight and no traffic for this long may be evicted to make
    /// room for a new one when the server is at `max_connections`.
    pub evict_idle: Duration,
    /// How long a refused or drained connection is given to finish reading what the peer sent.
    pub drain_deadline: Duration,
    /// Time allowed from the first byte of a request line to its newline.
    pub line_timeout: Duration,
    /// A client that does not drain its socket for this long is disconnected.
    pub write_timeout: Duration,
    /// Requests in flight per connection.
    pub max_inflight: usize,
    /// Connections served at once. Others get `too_many_connections` and are closed.
    pub max_connections: usize,
    /// Requests in flight across all connections. Others get `busy`.
    pub max_requests: usize,
    /// After shutdown begins, how long handlers get to finish before they are abandoned.
    pub shutdown_deadline: Duration,
}

impl Default for ServerOptions {
    fn default() -> Self {
        ServerOptions {
            server_name: format!("cowfs-ctl/{}", env!("CARGO_PKG_VERSION")),
            expected_uid: sys::current_uid(),
            peer_check: PeerCheck::system(),
            handshake_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(30),
            evict_idle: Duration::from_secs(10),
            line_timeout: Duration::from_secs(10),
            write_timeout: Duration::from_secs(30),
            max_inflight: 32,
            max_connections: 64,
            max_requests: 64,
            shutdown_deadline: Duration::from_secs(5),
            drain_deadline: Duration::from_millis(250),
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug)]
struct Shared {
    epoch: Instant,
    stopping: AtomicBool,
    abandon_at: OnceLock<Instant>,
    shutdown_after: Duration,
    next_conn: AtomicU64,
    requests: AtomicUsize,
    conns: Mutex<HashMap<u64, Arc<Conn>>>,
    snapshot_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl Shared {
    fn begin_shutdown(&self) {
        let _ = self.abandon_at.set(Instant::now() + self.shutdown_after);
        self.stopping.store(true, Ordering::SeqCst);
    }

    /// The lock that serialises every change of `name`.
    fn snapshot_lock(&self, name: &str) -> Arc<Mutex<()>> {
        let mut locks = lock(&self.snapshot_locks);
        Arc::clone(
            locks
                .entry(name.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(()))),
        )
    }

    fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    fn abandoned(&self) -> bool {
        self.abandon_at.get().is_some_and(|d| Instant::now() >= *d)
    }
}

/// Asks a running server to stop. Cheap to clone and safe to use from a signal thread.
#[derive(Clone, Debug)]
pub struct ShutdownHandle(Arc<Shared>);

impl ShutdownHandle {
    /// Begins graceful shutdown: the socket is removed and no new connections are accepted,
    /// in-flight requests are cancelled, and after `ServerOptions::shutdown_deadline` handlers
    /// that still run are abandoned.
    pub fn shutdown(&self) {
        self.0.begin_shutdown();
    }

    /// True once shutdown has begun.
    pub fn is_shutting_down(&self) -> bool {
        self.0.stopping()
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
        let (listener, lock_file) = socket::bind(path, opts.expected_uid)?;
        listener.set_nonblocking(true)?;
        let shared = Arc::new(Shared {
            epoch: Instant::now(),
            stopping: AtomicBool::new(false),
            abandon_at: OnceLock::new(),
            shutdown_after: opts.shutdown_deadline,
            next_conn: AtomicU64::new(0),
            requests: AtomicUsize::new(0),
            conns: Mutex::new(HashMap::new()),
            snapshot_locks: Mutex::new(HashMap::new()),
        });
        let thread = thread::Builder::new()
            .name("cowfs-ctl-accept".into())
            .spawn({
                let shared = Arc::clone(&shared);
                let path = path.to_owned();
                move || accept_loop(listener, lock_file, &path, handler, Arc::new(opts), &shared)
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

    /// Blocks until the server has stopped (a `shutdown` request or `ShutdownHandle::shutdown`),
    /// which takes at most `shutdown_deadline` plus a moment after shutdown begins.
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
            self.shared.begin_shutdown();
            let _ = t.join();
        }
    }
}

fn accept_loop(
    listener: UnixListener,
    lock_file: File,
    path: &Path,
    handler: Arc<dyn ControlHandler>,
    opts: Arc<ServerOptions>,
    shared: &Arc<Shared>,
) {
    // Drain the backlog without sleeping between accepts: a slept-on backlog overflows under a burst.
    while !shared.stopping() {
        match listener.accept() {
            Ok((stream, _)) => admit(stream, &handler, &opts, shared),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => thread::sleep(Duration::from_millis(50)),
        }
    }
    drain_backlog(&listener, &opts, shared);
    let _ = fs::remove_file(path);
    drop(listener);
    for conn in lock(&shared.conns).values() {
        conn.cancel_all();
    }
    loop {
        if lock(&shared.conns).is_empty() {
            break;
        }
        if shared.abandoned() {
            // The deadline has passed. Deliver each straggler's `shutting_down` frame from its own
            // worker, then wait a bounded grace for those workers. A readable client's frame is
            // written before `wait()` returns, so `cowfs serve` can exit right after it without
            // dropping the frame. A worker for a client that stopped reading blocks on the write
            // lock a progress write holds for the whole `write_timeout`; the grace expires and
            // `wait()` returns without joining it, so a blocked write cannot stretch the deadline.
            // The worker count is bounded by `max_connections`.
            let stragglers: Vec<Arc<Conn>> = lock(&shared.conns).values().cloned().collect();
            let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
            let mut workers = Vec::with_capacity(stragglers.len());
            for conn in &stragglers {
                let owned = Arc::clone(conn);
                let done_tx = done_tx.clone();
                match thread::Builder::new()
                    .name("cowfs-ctl-abandon".into())
                    .spawn(move || {
                        owned.abandon_inflight();
                        owned.kill();
                        let _ = done_tx.send(());
                    }) {
                    Ok(worker) => workers.push(worker),
                    // No worker: deliver the frame best-effort without blocking on a held write
                    // lock, then close, so a readable peer still gets `shutting_down` and no
                    // connection is left open past the return. The write itself is bounded by the
                    // same grace, so a peer whose receive queue is completely full cannot park the
                    // accept thread for a whole `write_timeout` here.
                    Err(_) => {
                        conn.best_effort_abandon(opts.drain_deadline);
                        conn.kill();
                    }
                }
            }
            drop(done_tx);
            let grace = Instant::now() + opts.drain_deadline;
            for _ in 0..workers.len() {
                let left = grace.saturating_duration_since(Instant::now());
                if done_rx.recv_timeout(left).is_err() {
                    break;
                }
            }
            // A worker that has not finished is parked behind a progress write that a non-reading
            // peer is holding open. `abandon_inflight` cannot observe the cancel token until that
            // write returns, so without this the connection, socket and handler would outlive
            // `wait()` by up to `write_timeout`, which the Shutdown contract forbids ("their
            // connections are closed"). `kill` takes no lock, so it cannot block: half-closing the
            // write side aborts the parked `write_all`, so the worker returns. This truncates any
            // frame still being written to a very slow but reading peer, which is the "abandoned"
            // semantics.
            for conn in &stragglers {
                conn.kill();
            }
            // The workers own the only writes this server still makes to a control client, so join
            // them before returning: no control-client buffer writer outlives `wait()`. `kill`
            // already aborted their blocked writes, so this normally completes at once. The bound
            // keeps a worker that is descheduled past the grace from stretching the deadline;
            // such a worker only has to finish a write that can no longer reach a peer, and the
            // process exit in `cowfs serve` ends it.
            let join_until = Instant::now() + opts.drain_deadline;
            for worker in workers {
                while !worker.is_finished() && Instant::now() < join_until {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            // The connection threads own the sockets, and each request worker owns a handler. A worker parked
            // behind a blocked write cannot observe its cancel token, so both the socket and the
            // handler would outlive `wait()` by up to `write_timeout` without this wait, which the
            // Shutdown contract forbids ("their connections are closed"). The workers were already
            // killed above, so this normally completes at once. Bounded by the same grace: past it
            // the connections are half-closed and the process exit in `cowfs serve` ends the rest,
            // so a thread that is merely descheduled cannot stretch the deadline.
            let release_until = Instant::now() + opts.drain_deadline;
            while Instant::now() < release_until
                && (!lock(&shared.conns).is_empty() || stragglers.iter().any(|c| !c.released()))
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    drop(lock_file);
}

// A connection that reached the kernel queue but was never accepted gets nothing from a plain
// close: closing the listener resets it. Each one is answered with `shutting_down` and a polite
// close, bounded by count and by the drain deadline.
fn drain_backlog(listener: &UnixListener, opts: &ServerOptions, shared: &Shared) {
    let until = Instant::now() + Duration::from_secs(5);
    let mut drained = 0;
    loop {
        if drained >= 256 || Instant::now() > until {
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                drained += 1;
                let opts = opts.clone();
                let _ = thread::Builder::new()
                    .name("cowfs-ctl-drain".into())
                    .spawn(move || serve_drained(&stream, &opts));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let _ = shared.epoch;
}

// A client that was queued but never accepted still gets a hello and a terminal frame per
// request, so a `cowfs` call in flight at shutdown never sees a bare EOF.
fn serve_drained(stream: &UnixStream, opts: &ServerOptions) {
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let _ = stream.set_read_timeout(Some(POLL));
    let mut reader = BufReader::new(clone);
    let mut buf = Vec::new();
    let hard = Instant::now() + opts.drain_deadline;
    let mut frames = 0;
    while Instant::now() < hard && frames < 16 {
        frames += 1;
        buf.clear();
        let limits = ReadLimits {
            idle: &|| Some(hard),
            hard: Some(hard),
            line: None,
            abort: &|| false,
        };
        match read_line_until(&mut reader, &mut buf, MAX_REQUEST_LINE, &limits) {
            Ok(LineRead::Line) => {}
            _ => break,
        }
        match ClientFrame::decode(&buf) {
            Ok(ClientFrame::Hello(hello)) => {
                let Some(version) = hello
                    .versions
                    .iter()
                    .copied()
                    .filter(|v| SUPPORTED_VERSIONS.contains(v))
                    .max()
                else {
                    break;
                };
                let frame = ServerFrame::Hello(ServerHello {
                    version,
                    server: opts.server_name.clone(),
                    methods: METHODS.iter().map(|m| (*m).to_owned()).collect(),
                });
                if !send_on(stream, &frame) {
                    break;
                }
            }
            Ok(ClientFrame::Request { id, .. }) => {
                let frame = ServerFrame::Error {
                    id: Some(id),
                    error: CtlError::new(ErrorCode::ShuttingDown, "server is shutting down"),
                };
                if !send_on(stream, &frame) {
                    break;
                }
            }
            Ok(ClientFrame::Cancel { .. }) => {}
            Err(fe) => {
                let frame = ServerFrame::Error {
                    id: fe.id,
                    error: fe.error,
                };
                if !send_on(stream, &frame) {
                    break;
                }
            }
        }
    }
    close_after_reply(stream, None, opts.drain_deadline);
}

// Once the server is stopping, the requests already in the socket are answered with
// `shutting_down` instead of being dropped.
fn answer_pending(
    stream: &UnixStream,
    reader: &mut BufReader<UnixStream>,
    buf: &mut Vec<u8>,
    opts: &ServerOptions,
) {
    let hard = Instant::now() + opts.drain_deadline;
    let mut frames = 0;
    while Instant::now() < hard && frames < 16 {
        frames += 1;
        buf.clear();
        let limits = ReadLimits {
            idle: &|| Some(hard),
            hard: Some(hard),
            line: None,
            abort: &|| false,
        };
        if !matches!(
            read_line_until(reader, buf, MAX_REQUEST_LINE, &limits),
            Ok(LineRead::Line)
        ) {
            return;
        }
        if let Ok(ClientFrame::Request { id, .. }) = ClientFrame::decode(buf) {
            let frame = ServerFrame::Error {
                id: Some(id),
                error: CtlError::new(ErrorCode::ShuttingDown, "server is shutting down"),
            };
            if !send_on(stream, &frame) {
                return;
            }
        }
    }
}

fn send_on(stream: &UnixStream, frame: &ServerFrame) -> bool {
    let Ok(mut w) = stream.try_clone() else {
        return false;
    };
    w.write_all(&frame.encode()).is_ok()
}

// Writes the frame, half-closes, drains what the peer already sent, then closes. Closing with
// unread input in the receive queue makes the kernel send RST, which destroys the frame.
fn close_after_reply(stream: &UnixStream, frame: Option<&ServerFrame>, deadline: Duration) {
    let Ok(write) = stream.try_clone() else {
        return;
    };
    let _ = write.set_write_timeout(Some(deadline.max(Duration::from_millis(50))));
    if let Some(frame) = frame {
        let mut w = &write;
        let _ = w.write_all(&frame.encode());
        let _ = w.flush();
    }
    let _ = stream.shutdown(Shutdown::Write);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let until = Instant::now() + deadline;
    let mut sink = [0u8; 8192];
    while Instant::now() < until {
        match (&*stream).read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

fn admit(
    stream: UnixStream,
    handler: &Arc<dyn ControlHandler>,
    opts: &Arc<ServerOptions>,
    shared: &Arc<Shared>,
) {
    let Ok(conn) = Conn::new(&stream) else {
        return;
    };
    let conn = Arc::new(conn);
    let id = shared.next_conn.fetch_add(1, Ordering::SeqCst);
    {
        let mut conns = lock(&shared.conns);
        if conns.len() >= opts.max_connections {
            // Idle connections give way to a new one; anything with a request in flight stays.
            let idle = |c: &Arc<Conn>| {
                c.inflight_empty() && shared.epoch.elapsed() >= c.last_active() + opts.evict_idle
            };
            let victim = conns
                .iter()
                .filter(|(_, c)| idle(c))
                .min_by_key(|(_, c)| c.last_active())
                .map(|(k, _)| *k);
            match victim {
                Some(victim) => {
                    if let Some(old) = conns.remove(&victim) {
                        old.kill();
                    }
                }
                None => {
                    drop(conns);
                    refuse(&stream, opts.drain_deadline);
                    return;
                }
            }
        }
        conns.insert(id, Arc::clone(&conn));
    }
    let (handler, opts, shared2) = (Arc::clone(handler), Arc::clone(opts), Arc::clone(shared));
    let spawned = thread::Builder::new().name("cowfs-ctl-conn".into()).spawn({
        let conn = Arc::clone(&conn);
        move || {
            let Ok(clone) = stream.try_clone() else {
                return;
            };
            let mut reader = BufReader::new(clone);
            let _ = run_connection(&mut reader, &conn, &handler, &opts, &shared2);
            if shared2.stopping() {
                conn.abandon_inflight();
            }
            conn.kill();
            conn.drain_and_close(&mut reader, opts.drain_deadline);
            lock(&shared2.conns).remove(&id);
        }
    });
    if spawned.is_err() {
        lock(&shared.conns).remove(&id);
    }
}

fn refuse(stream: &UnixStream, deadline: Duration) {
    let frame = ServerFrame::Error {
        id: None,
        error: CtlError::new(ErrorCode::TooManyConnections, "too many connections"),
    };
    close_after_reply(stream, Some(&frame), deadline);
}

#[derive(Debug)]
struct Conn {
    stream: UnixStream,
    write_lock: Mutex<()>,
    dead: AtomicBool,
    last_active: AtomicU64,
    inflight: Mutex<HashMap<u64, CancelToken>>,
    /// Terminal frames whose write is in progress. Counted outside the inflight map so a slow
    /// write never holds the map lock, yet teardown still sees the request as unfinished.
    finishing: AtomicU64,
    /// Request worker threads this connection still owns. A worker parks in whatever the handler
    /// is doing, which a blocked progress write makes up to a whole `write_timeout`, so the
    /// connection thread dropping out of `conns` does not mean the handler is gone. Shutdown waits on
    /// this count so `wait()` returns with no handler of an abandoned connection still running.
    workers: AtomicU64,
}

impl Conn {
    fn new(stream: &UnixStream) -> io::Result<Conn> {
        stream.set_nonblocking(false)?;
        Ok(Conn {
            stream: stream.try_clone()?,
            write_lock: Mutex::new(()),
            dead: AtomicBool::new(false),
            last_active: AtomicU64::new(0),
            inflight: Mutex::new(HashMap::new()),
            finishing: AtomicU64::new(0),
            workers: AtomicU64::new(0),
        })
    }

    /// True once this connection owns no request worker, no terminal write and no live request.
    fn released(&self) -> bool {
        self.workers.load(Ordering::SeqCst) == 0 && self.inflight_empty()
    }

    fn send(&self, frame: &ServerFrame) -> bool {
        let ok = self.write_frame(frame);
        if !ok {
            self.kill();
        }
        ok
    }

    fn write_frame(&self, frame: &ServerFrame) -> bool {
        if self.dead.load(Ordering::SeqCst) {
            return false;
        }
        let _guard = lock(&self.write_lock);
        (&self.stream).write_all(&frame.encode()).is_ok()
    }

    fn send_error(&self, id: Option<u64>, error: CtlError) -> bool {
        self.send(&ServerFrame::Error { id, error })
    }

    /// Sends the terminal frame of request `id`, unless another path already did.
    ///
    /// Closing a connection is the connection thread's job and always drains what the peer sent
    /// first, so a frame that is on its way out cannot be cut by it.
    fn finish(&self, id: u64, frame: &ServerFrame) {
        {
            let mut inflight = lock(&self.inflight);
            if inflight.remove(&id).is_none() {
                return;
            }
            // Keep teardown from observing completion before the terminal write finishes, but do
            // not hold the map lock across the write: admission and shutdown take it too. The
            // counter is bumped under the lock, so any observer that sees the id gone also sees a
            // nonzero finishing count until the write below completes.
            self.finishing.fetch_add(1, Ordering::SeqCst);
        }
        let ok = self.write_frame(frame);
        // Release the completion claim only after the terminal frame is on the wire. The map lock
        // is free again, so a failed write can safely reacquire it through `kill`.
        self.finishing.fetch_sub(1, Ordering::SeqCst);
        if !ok {
            self.kill();
        }
    }

    fn cancel_all(&self) {
        for token in lock(&self.inflight).values() {
            token.cancel();
        }
    }

    fn inflight_empty(&self) -> bool {
        lock(&self.inflight).is_empty() && self.finishing.load(Ordering::SeqCst) == 0
    }

    /// Ends every request that is still running with `shutting_down`. Only sent when the server
    /// is stopping: a client that left needs no frames.
    fn abandon_inflight(&self) {
        let ids: Vec<u64> = lock(&self.inflight).keys().copied().collect();
        for id in ids {
            self.finish(
                id,
                &ServerFrame::Error {
                    id: Some(id),
                    error: CtlError::new(ErrorCode::ShuttingDown, "server is shutting down"),
                },
            );
        }
    }

    /// Same as `abandon_inflight`, but the terminal write is skipped when the write lock is held.
    /// Used on the shutdown deadline when no helper thread could be spawned, so the deadline path
    /// never blocks behind a peer that stopped reading, while a readable peer still gets its frame.
    ///
    /// `budget` bounds the write itself. A peer whose receive queue is completely full would
    /// otherwise park this `write_all` for the whole `write_timeout` on the accept thread, which is
    /// the same deadline this path exists to protect. On timeout the connection is marked dead and
    /// closed by `kill`, which is the abandoned outcome.
    fn best_effort_abandon(&self, budget: Duration) {
        let ids: Vec<u64> = lock(&self.inflight).keys().copied().collect();
        for id in ids {
            let frame = ServerFrame::Error {
                id: Some(id),
                error: CtlError::new(ErrorCode::ShuttingDown, "server is shutting down"),
            };
            if let Ok(mut inflight) = self.inflight.try_lock() {
                if inflight.remove(&id).is_none() {
                    continue;
                }
                drop(inflight);
                if let Ok(_write) = self.write_lock.try_lock() {
                    if self.dead.load(Ordering::SeqCst) {
                        continue;
                    }
                    let _ = self.stream.set_write_timeout(Some(budget));
                    if (&self.stream).write_all(&frame.encode()).is_err() {
                        self.dead.store(true, Ordering::SeqCst);
                    }
                    let _ = self.stream.set_write_timeout(None);
                }
            }
        }
    }

    /// Milliseconds since the server started at which this connection last had traffic.
    fn last_active(&self) -> Duration {
        Duration::from_millis(self.last_active.load(Ordering::SeqCst))
    }

    fn touch(&self, epoch: &Instant) {
        let ms = u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_active.fetch_max(ms, Ordering::SeqCst);
    }

    /// Marks the connection dead and half-closes it. The close itself belongs to
    /// `drain_and_close`, which owns the reader.
    fn kill(&self) {
        self.dead.store(true, Ordering::SeqCst);
        self.cancel_all();
        let _ = self.stream.shutdown(Shutdown::Write);
    }

    /// Reads away what the peer already sent, then closes for good.
    fn drain_and_close(&self, reader: &mut BufReader<UnixStream>, deadline: Duration) {
        let _ = reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(50)));
        // A connection that was already killed has had its delivery grace: the write side is closed
        // and the peer either got its terminal frame or never will. Reading away what the peer sent
        // is politeness for a peer that is still there, and it is what would otherwise hold this
        // socket, and the handler behind it, open for the whole drain deadline after `wait()`
        // returned. Close now instead; the contract only promises the close.
        let until = Instant::now() + deadline;
        let mut sink = [0u8; 8192];
        while !self.dead.load(Ordering::SeqCst) && Instant::now() < until {
            match reader.read(&mut sink) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

fn run_connection(
    reader: &mut BufReader<UnixStream>,
    conn: &Arc<Conn>,
    handler: &Arc<dyn ControlHandler>,
    opts: &Arc<ServerOptions>,
    shared: &Arc<Shared>,
) -> io::Result<()> {
    let owned = reader.get_ref().try_clone()?;
    let stream: &UnixStream = &owned;
    stream.set_write_timeout(Some(opts.write_timeout))?;
    stream.set_read_timeout(Some(POLL))?;
    match (opts.peer_check.0)(stream) {
        Ok(uid) if uid == opts.expected_uid => {}
        _ => {
            close_after_reply(
                stream,
                Some(&ServerFrame::Error {
                    id: None,
                    error: CtlError::new(
                        ErrorCode::PermissionDenied,
                        "peer uid does not match the server",
                    ),
                }),
                opts.drain_deadline,
            );
            return Ok(());
        }
    }
    let mut buf = Vec::new();
    let gone = || conn.dead.load(Ordering::SeqCst);

    let hard = Instant::now() + opts.handshake_timeout;
    let limits = ReadLimits {
        idle: &|| Some(hard),
        hard: Some(hard),
        line: None,
        abort: &gone,
    };
    match read_line_until(reader, &mut buf, MAX_REQUEST_LINE, &limits)? {
        LineRead::Line => {}
        LineRead::TooLong => {
            reject_oversized(conn, reader);
            return Ok(());
        }
        LineRead::Timeout => {
            conn.send_error(
                None,
                CtlError::new(ErrorCode::Timeout, "handshake not completed in time"),
            );
            return Ok(());
        }
        LineRead::Eof | LineRead::Aborted => return Ok(()),
    }
    if !handshake(conn, &buf, opts) {
        return Ok(());
    }

    let mut last_activity = Instant::now();
    loop {
        if shared.stopping() {
            answer_pending(stream, reader, &mut buf, opts);
            return Ok(());
        }
        buf.clear();
        let idle_at = last_activity + opts.idle_timeout;
        let idle = || conn.inflight_empty().then_some(idle_at);
        let limits = ReadLimits {
            idle: &idle,
            hard: None,
            line: Some(opts.line_timeout),
            abort: &gone,
        };
        match read_line_until(reader, &mut buf, MAX_REQUEST_LINE, &limits)? {
            LineRead::Line => {}
            LineRead::TooLong => {
                reject_oversized(conn, reader);
                return Ok(());
            }
            LineRead::Timeout => {
                conn.send_error(
                    None,
                    CtlError::new(ErrorCode::Timeout, "idle or slow client"),
                );
                return Ok(());
            }
            LineRead::Aborted | LineRead::Eof => break,
        }
        last_activity = Instant::now();
        conn.touch(&shared.epoch);
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
                start_request(conn, handler, opts, shared, id, request);
            }
        }
        if conn.dead.load(Ordering::SeqCst) {
            return Ok(());
        }
    }
    // A closed write side of the client means no more requests, not cancel: let the requests in
    // flight finish. A client that is really gone shows up as a failed write, which cancels them.
    while !conn.inflight_empty() && !conn.dead.load(Ordering::SeqCst) && !shared.abandoned() {
        thread::sleep(Duration::from_millis(10));
    }
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

fn handshake(conn: &Conn, line: &[u8], opts: &ServerOptions) -> bool {
    let hello = match ClientFrame::decode(line) {
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

struct RequestSlot(Arc<Shared>);

impl Drop for RequestSlot {
    fn drop(&mut self) {
        self.0.requests.fetch_sub(1, Ordering::SeqCst);
    }
}

fn start_request(
    conn: &Arc<Conn>,
    handler: &Arc<dyn ControlHandler>,
    opts: &Arc<ServerOptions>,
    shared: &Arc<Shared>,
    id: u64,
    request: Request,
) {
    if shared.stopping() {
        conn.send_error(
            Some(id),
            CtlError::new(ErrorCode::ShuttingDown, "server is shutting down"),
        );
        return;
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
        } else if shared.requests.fetch_add(1, Ordering::SeqCst) >= opts.max_requests {
            shared.requests.fetch_sub(1, Ordering::SeqCst);
            Some(CtlError::new(ErrorCode::Busy, "server is at capacity"))
        } else {
            inflight.insert(id, token.clone());
            None
        }
    };
    if let Some(error) = refusal {
        conn.send_error(Some(id), error);
        return;
    }
    let slot = RequestSlot(Arc::clone(shared));
    conn.workers.fetch_add(1, Ordering::SeqCst);
    let spawned = thread::Builder::new().name("cowfs-ctl-req".into()).spawn({
        let (conn, handler, opts, shared) = (
            Arc::clone(conn),
            Arc::clone(handler),
            Arc::clone(opts),
            Arc::clone(shared),
        );
        move || {
            run_request(
                &conn,
                handler.as_ref(),
                &opts,
                &shared,
                id,
                request,
                token,
                slot,
            );
            conn.workers.fetch_sub(1, Ordering::SeqCst);
        }
    });
    if spawned.is_err() {
        conn.workers.fetch_sub(1, Ordering::SeqCst);
        conn.finish(
            id,
            &ServerFrame::Error {
                id: Some(id),
                error: CtlError::new(ErrorCode::Busy, "cannot start a worker"),
            },
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn run_request(
    conn: &Conn,
    handler: &dyn ControlHandler,
    opts: &ServerOptions,
    shared: &Shared,
    id: u64,
    request: Request,
    token: CancelToken,
    _slot: RequestSlot,
) {
    let ctx = OpContext::new(token, |event| {
        conn.send(&ServerFrame::Progress { id, event })
    });
    let is_shutdown = matches!(request, Request::Shutdown(_));
    let result = catch_unwind(AssertUnwindSafe(|| {
        dispatch(handler, opts, shared, request, &ctx)
    }))
    .unwrap_or_else(|_| Err(CtlError::new(ErrorCode::Internal, "handler panicked")));
    let stop = is_shutdown && result.is_ok();
    let frame = match result {
        Ok(result) => ServerFrame::Response { id, result },
        Err(mut error) => {
            if error.code == ErrorCode::Cancelled && shared.stopping() {
                error = CtlError::new(ErrorCode::ShuttingDown, "server is shutting down");
            }
            ServerFrame::Error {
                id: Some(id),
                error,
            }
        }
    };
    conn.finish(id, &frame);
    if stop {
        shared.begin_shutdown();
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch(
    h: &dyn ControlHandler,
    opts: &ServerOptions,
    shared: &Shared,
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
            validate_snapshot_name(&p.name)?;
            if let Some(from) = &p.from {
                validate_snapshot_name(from)?;
            }
            Response::Snapshot(h.snapshot_create(p)?)
        }
        Request::SnapshotRm(p) => {
            validate_snapshot_name(&p.name)?;
            let held = shared.snapshot_lock(&p.name);
            let source = |n: &str| h.holders(n);
            let guard = HolderGuard::new(&p.name, p.expect_no_holders, held, &source);
            guard.check_holders()?;
            h.remove(&p.name, &guard)?;
            Response::Ok(Empty {})
        }
        Request::SnapshotReset(p) => {
            validate_snapshot_name(&p.name)?;
            validate_snapshot_name(&p.from)?;
            if p.name == p.from {
                return Err(CtlError::invalid("cannot reset a snapshot from itself"));
            }
            let held = shared.snapshot_lock(&p.name);
            let source = |n: &str| h.holders(n);
            let guard = HolderGuard::new(&p.name, p.expect_no_holders, held, &source);
            guard.check_holders()?;
            Response::Snapshot(h.swap(&p.name, &p.from, &guard)?)
        }
        Request::SnapshotRename(p) => {
            validate_snapshot_name(&p.from)?;
            validate_snapshot_name(&p.to)?;
            Response::Snapshot(h.snapshot_rename(&p.from, &p.to)?)
        }
        Request::SnapshotPromote(p) => {
            validate_snapshot_name(&p.name)?;
            Response::Snapshot(h.snapshot_promote(&p.name)?)
        }
        Request::Gc(p) => Response::Gc(h.gc(p, ctx)?),
        Request::Fsck(_) => Response::Fsck(h.fsck(ctx)?),
        Request::Import(p) => {
            validate_snapshot_name(&p.name)?;
            validate_abs_path("path", &p.path)?;
            Response::Import(h.import(p, ctx)?)
        }
        Request::BaseRefresh(p) => {
            validate_repo(&p.repo)?;
            validate_git_ref(&p.git_ref)?;
            if let Some(n) = &p.name {
                validate_snapshot_name(n)?;
            }
            Response::BaseRefresh(h.base_refresh(p, ctx)?)
        }
        Request::Ps(p) => {
            validate_snapshot_name(&p.snapshot)?;
            Response::Processes(ProcessList {
                processes: h.holders(&p.snapshot)?,
            })
        }
        Request::MountInfo(_) => Response::MountInfo(h.mount_info()?),
        Request::MountSnapshot(p) => {
            validate_snapshot_name(&p.name)?;
            validate_abs_path("path", &p.path)?;
            // Same lock and same guard as a removal, so the holder check and the export are one
            // critical section and a holder cannot appear between them.
            let held = shared.snapshot_lock(&p.name);
            let source = |n: &str| h.holders(n);
            let guard = HolderGuard::new(&p.name, p.expect_no_holders, held, &source);
            guard.check_holders()?;
            Response::MountInfo(h.mount_snapshot(&p, &guard)?)
        }
        Request::UnmountSnapshot(p) => {
            validate_abs_path("path", &p.path)?;
            h.unmount_snapshot(&p)?;
            Response::Ok(Empty {})
        }
        Request::Shutdown(_) => {
            h.shutdown()?;
            Response::Ok(Empty {})
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_write_remains_inflight_until_sent() {
        let (server, mut client) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let conn = Arc::new(Conn::new(&server).unwrap());
        lock(&conn.inflight).insert(1, CancelToken::new());
        let write = lock(&conn.write_lock);
        let frame = ServerFrame::Error {
            id: Some(1),
            error: CtlError::new(ErrorCode::Cancelled, "done"),
        };
        let expected = frame.encode();
        let worker = thread::spawn({
            let conn = Arc::clone(&conn);
            move || conn.finish(1, &frame)
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        // The finisher removes the id under the map lock, bumps `finishing`, then blocks on the
        // write lock held above. Seeing a nonzero count proves it is inside the terminal write.
        // Both the map and the counter must report busy in that window.
        while conn.finishing.load(Ordering::SeqCst) == 0 {
            assert!(
                Instant::now() < deadline,
                "finisher did not reach the write"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !conn.inflight_empty(),
            "teardown can observe completion before the terminal write"
        );
        drop(write);
        worker.join().unwrap();
        assert!(conn.inflight_empty());
        conn.kill();
        let mut actual = Vec::new();
        client.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn terminal_write_failure_does_not_deadlock_cancellation() {
        let (server, client) = UnixStream::pair().unwrap();
        let conn = Arc::new(Conn::new(&server).unwrap());
        let pending = CancelToken::new();
        lock(&conn.inflight).insert(1, CancelToken::new());
        lock(&conn.inflight).insert(2, pending.clone());
        drop(client);
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn({
            let conn = Arc::clone(&conn);
            move || {
                conn.finish(
                    1,
                    &ServerFrame::Error {
                        id: Some(1),
                        error: CtlError::new(ErrorCode::Cancelled, "done"),
                    },
                );
                tx.send(()).unwrap();
            }
        });
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(conn.dead.load(Ordering::SeqCst));
        assert!(pending.is_cancelled());
    }
}
