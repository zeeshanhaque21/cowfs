use crate::error::CtlError;
use crate::frame::{
    read_line_until, ClientFrame, Hello, LineRead, ReadLimits, ServerFrame, ServerHello,
    MAX_RESPONSE_LINE, PROTOCOL_VERSION,
};
use crate::types::{ProgressEvent, Request, Response};
use std::io::{self, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(200);
const REFUSED_RETRIES: u32 = 4;

/// What can go wrong talking to a server.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Connecting to the socket failed. `NotFound` and `ConnectionRefused` mean no daemon is running.
    #[error("cannot connect to {}: {source}", path.display())]
    Connect {
        /// The socket path.
        path: PathBuf,
        /// The underlying failure.
        source: io::Error,
    },
    /// Reading or writing the socket failed.
    #[error("{0}")]
    Io(#[from] io::Error),
    /// The server sent an `error` frame.
    #[error("{0}")]
    Server(#[from] CtlError),
    /// The server sent something this client cannot make sense of.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The server closed the connection.
    #[error("connection closed by the server")]
    Closed,
    /// The server did not answer in time.
    #[error("{0}")]
    Timeout(String),
}

impl ClientError {
    /// True when the failure means that no daemon is listening on the socket.
    pub fn is_not_running(&self) -> bool {
        matches!(self, ClientError::Connect { source, .. }
            if matches!(source.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused))
    }

    /// True when the server did not answer within a timeout.
    pub fn is_timeout(&self) -> bool {
        matches!(self, ClientError::Timeout(_))
    }
}

/// Timeouts of a `Client`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientOptions {
    /// Time allowed to open the socket.
    pub connect_timeout: Duration,
    /// Time allowed for the whole handshake.
    pub handshake_timeout: Duration,
    /// Longest silence tolerated while waiting for a response. Every frame, progress included,
    /// starts the clock again, so a long operation that reports progress never trips it.
    pub idle_timeout: Duration,
}

impl Default for ClientOptions {
    fn default() -> Self {
        ClientOptions {
            connect_timeout: Duration::from_secs(5),
            handshake_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(30),
        }
    }
}

type SharedWriter = Arc<Mutex<UnixStream>>;

fn write_frame(writer: &SharedWriter, frame: &ClientFrame) -> io::Result<()> {
    writer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .write_all(&frame.encode())
}

fn connect_once(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    let (tx, rx) = mpsc::channel();
    let p = path.to_owned();
    thread::spawn(move || {
        let _ = tx.send(UnixStream::connect(p));
    });
    match rx.recv_timeout(timeout) {
        Ok(r) => r,
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "connect timed out")),
    }
}

// A full listen backlog can look like a refusal, so a refusal is retried briefly before it
// is believed.
fn connect(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    let mut tries = 0;
    loop {
        match connect_once(path, timeout) {
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused && tries < REFUSED_RETRIES => {
                tries += 1;
                thread::sleep(Duration::from_millis(50));
            }
            other => return other,
        }
    }
}

/// Cancels the request a `Client` currently has in flight. Cloneable and usable from another
/// thread, for example a Ctrl-C handler.
#[derive(Clone, Debug)]
pub struct Canceller {
    writer: SharedWriter,
    active: Arc<AtomicU64>,
}

impl Canceller {
    /// Sends `cancel` for the active request. Does nothing when none is in flight.
    pub fn cancel(&self) -> io::Result<()> {
        match self.active.load(Ordering::SeqCst) {
            0 => Ok(()),
            id => write_frame(&self.writer, &ClientFrame::Cancel { id }),
        }
    }
}

/// A connection to a control server. One request at a time.
#[derive(Debug)]
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: SharedWriter,
    next_id: u64,
    active: Arc<AtomicU64>,
    server: ServerHello,
    buf: Vec<u8>,
    opts: ClientOptions,
}

impl Client {
    /// Connects with the default timeouts and performs the version handshake.
    pub fn connect(path: &Path) -> Result<Client, ClientError> {
        Client::connect_with(path, ClientOptions::default())
    }

    /// Connects and performs the version handshake within the given timeouts.
    pub fn connect_with(path: &Path, opts: ClientOptions) -> Result<Client, ClientError> {
        let stream =
            connect(path, opts.connect_timeout).map_err(|source| ClientError::Connect {
                path: path.to_owned(),
                source,
            })?;
        stream.set_read_timeout(Some(POLL))?;
        stream.set_write_timeout(Some(opts.idle_timeout.max(Duration::from_secs(1))))?;
        let writer = Arc::new(Mutex::new(stream.try_clone()?));
        let mut client = Client {
            reader: BufReader::new(stream),
            writer,
            next_id: 1,
            active: Arc::new(AtomicU64::new(0)),
            server: ServerHello {
                version: 0,
                server: String::new(),
                methods: Vec::new(),
            },
            buf: Vec::new(),
            opts,
        };
        write_frame(
            &client.writer,
            &ClientFrame::Hello(Hello {
                versions: vec![PROTOCOL_VERSION],
                client: format!("cowfs-ctl/{}", env!("CARGO_PKG_VERSION")),
            }),
        )?;
        client.server = match client.read_frame(opts.handshake_timeout, "handshake")? {
            ServerFrame::Hello(h) => h,
            ServerFrame::Error { error, .. } => return Err(error.into()),
            _ => return Err(ClientError::Protocol("expected hello".into())),
        };
        Ok(client)
    }

    /// The server's `hello`: negotiated version, name and supported methods.
    pub fn server(&self) -> &ServerHello {
        &self.server
    }

    /// A handle that cancels this client's in-flight request from another thread.
    pub fn canceller(&self) -> Canceller {
        Canceller {
            writer: Arc::clone(&self.writer),
            active: Arc::clone(&self.active),
        }
    }

    /// Sends a request and waits for the result, discarding progress.
    pub fn call(&mut self, request: Request) -> Result<Response, ClientError> {
        self.call_with_progress(request, |_| {})
    }

    /// Sends a request and waits for the result, passing each progress event to `on_progress`.
    pub fn call_with_progress(
        &mut self,
        request: Request,
        mut on_progress: impl FnMut(&ProgressEvent),
    ) -> Result<Response, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        self.active.store(id, Ordering::SeqCst);
        let result = self.exchange(id, request, &mut on_progress);
        self.active.store(0, Ordering::SeqCst);
        result
    }

    fn exchange(
        &mut self,
        id: u64,
        request: Request,
        on_progress: &mut dyn FnMut(&ProgressEvent),
    ) -> Result<Response, ClientError> {
        write_frame(&self.writer, &ClientFrame::Request { id, request })?;
        loop {
            match self.read_frame(self.opts.idle_timeout, "response")? {
                ServerFrame::Progress { id: got, event } if got == id => on_progress(&event),
                ServerFrame::Response { id: got, result } if got == id => return Ok(result),
                ServerFrame::Error { id: got, error } if got == Some(id) || got.is_none() => {
                    return Err(error.into())
                }
                _ => {}
            }
        }
    }

    fn read_frame(&mut self, within: Duration, what: &str) -> Result<ServerFrame, ClientError> {
        self.buf.clear();
        let start = Instant::now();
        let deadline = Some(start + within);
        let limits = ReadLimits {
            idle: &|| deadline,
            hard: deadline,
            line: None,
            abort: &|| false,
        };
        match read_line_until(&mut self.reader, &mut self.buf, MAX_RESPONSE_LINE, &limits)? {
            LineRead::Line => {
                ServerFrame::decode(&self.buf).map_err(|fe| ClientError::Protocol(fe.error.message))
            }
            LineRead::Eof => Err(ClientError::Closed),
            LineRead::TooLong => Err(ClientError::Protocol("response line too long".into())),
            LineRead::Timeout | LineRead::Aborted => Err(ClientError::Timeout(format!(
                "the server did not send a {what} within {}s",
                within.as_secs().max(1)
            ))),
        }
    }
}
