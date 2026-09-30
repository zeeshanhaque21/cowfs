use crate::error::CtlError;
use crate::frame::{
    read_line, ClientFrame, Hello, LineRead, ServerFrame, ServerHello, MAX_RESPONSE_LINE,
    PROTOCOL_VERSION,
};
use crate::types::{ProgressEvent, Request, Response};
use std::io::{self, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// What can go wrong talking to a server.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Connecting or reading or writing the socket failed. `NotFound` and `ConnectionRefused`
    /// on connect mean no daemon is running.
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
}

impl ClientError {
    /// True when the failure means that no daemon is listening on the socket.
    pub fn is_not_running(&self) -> bool {
        matches!(self, ClientError::Io(e)
            if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused))
    }
}

type SharedWriter = Arc<Mutex<UnixStream>>;

fn write_frame(writer: &SharedWriter, frame: &ClientFrame) -> io::Result<()> {
    writer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .write_all(&frame.encode())
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
}

impl Client {
    /// Connects and performs the version handshake.
    pub fn connect(path: &Path) -> Result<Client, ClientError> {
        let stream = UnixStream::connect(path)?;
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
        };
        write_frame(
            &client.writer,
            &ClientFrame::Hello(Hello {
                versions: vec![PROTOCOL_VERSION],
                client: format!("cowfs-ctl/{}", env!("CARGO_PKG_VERSION")),
            }),
        )?;
        client.server = match client.read_frame()? {
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
            match self.read_frame()? {
                ServerFrame::Progress { id: got, event } if got == id => on_progress(&event),
                ServerFrame::Response { id: got, result } if got == id => return Ok(result),
                ServerFrame::Error { id: got, error } if got == Some(id) || got.is_none() => {
                    return Err(error.into())
                }
                _ => {}
            }
        }
    }

    fn read_frame(&mut self) -> Result<ServerFrame, ClientError> {
        self.buf.clear();
        match read_line(&mut self.reader, &mut self.buf, MAX_RESPONSE_LINE)? {
            LineRead::Line => {
                ServerFrame::decode(&self.buf).map_err(|fe| ClientError::Protocol(fe.error.message))
            }
            LineRead::Eof => Err(ClientError::Closed),
            LineRead::TooLong => Err(ClientError::Protocol("response line too long".into())),
        }
    }
}
