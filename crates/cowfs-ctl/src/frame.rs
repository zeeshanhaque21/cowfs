use crate::error::{CtlError, ErrorCode};
use crate::types::{ProgressEvent, Request, Response, METHODS, RESPONSE_KINDS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::io::{self, BufRead};
use std::time::{Duration, Instant};

/// The protocol major version this crate speaks.
pub const PROTOCOL_VERSION: u32 = 1;
/// Longest line a server accepts from a client.
pub const MAX_REQUEST_LINE: usize = 1 << 20;
/// Longest line a client accepts from a server.
pub const MAX_RESPONSE_LINE: usize = 64 << 20;

/// First client frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub versions: Vec<u32>,
    #[serde(default)]
    pub client: String,
}

/// Server reply to `Hello`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHello {
    pub version: u32,
    pub server: String,
    #[serde(default)]
    pub methods: Vec<String>,
}

/// A frame sent by the client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientFrame {
    Hello(Hello),
    Request { id: u64, request: Request },
    Cancel { id: u64 },
}

/// A frame sent by the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerFrame {
    Hello(ServerHello),
    Progress {
        id: u64,
        event: ProgressEvent,
    },
    Response {
        id: u64,
        result: Response,
    },
    Error {
        id: Option<u64>,
        error: CtlError,
    },
    /// A frame type this version does not know. Clients ignore it.
    Unknown(String),
}

/// A frame that could not be decoded, with the request id when one could be read.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameError {
    pub id: Option<u64>,
    pub error: CtlError,
}

impl FrameError {
    fn new(id: Option<u64>, code: ErrorCode, message: impl Into<String>) -> Self {
        FrameError {
            id,
            error: CtlError::new(code, message),
        }
    }
}

fn line(value: &Value) -> Vec<u8> {
    let mut out = serde_json::to_vec(value).unwrap_or_default();
    out.push(b'\n');
    out
}

fn typed(kind: &str, mut body: Value) -> Value {
    if let Some(map) = body.as_object_mut() {
        map.insert("type".into(), kind.into());
    }
    body
}

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

impl ClientFrame {
    /// Encodes to one wire line, including the trailing newline.
    pub fn encode(&self) -> Vec<u8> {
        let v = match self {
            ClientFrame::Hello(h) => typed("hello", to_value(h)),
            ClientFrame::Request { id, request } => {
                let mut v = typed("request", to_value(request));
                v["id"] = (*id).into();
                v
            }
            ClientFrame::Cancel { id } => json!({"type": "cancel", "id": id}),
        };
        line(&v)
    }

    /// Decodes one line (without its newline).
    pub fn decode(bytes: &[u8]) -> Result<ClientFrame, FrameError> {
        let (obj, id) = parse_object(bytes)?;
        let kind = obj.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "hello" => serde_json::from_value(Value::Object(obj))
                .map(ClientFrame::Hello)
                .map_err(|e| FrameError::new(id, ErrorCode::MalformedFrame, e.to_string())),
            "cancel" => match id {
                Some(id) => Ok(ClientFrame::Cancel { id }),
                None => Err(FrameError::new(
                    None,
                    ErrorCode::MalformedFrame,
                    "cancel needs an unsigned integer id",
                )),
            },
            "request" => decode_request(obj, id),
            "" => Err(FrameError::new(
                id,
                ErrorCode::MalformedFrame,
                "frame has no string \"type\"",
            )),
            other => Err(FrameError::new(
                id,
                ErrorCode::UnknownFrame,
                format!("unknown frame type {other:?}"),
            )),
        }
    }
}

fn decode_request(mut obj: Map<String, Value>, id: Option<u64>) -> Result<ClientFrame, FrameError> {
    let Some(id) = id else {
        return Err(FrameError::new(
            None,
            ErrorCode::MalformedFrame,
            "request needs an unsigned integer id",
        ));
    };
    let Some(method) = obj.get("method").and_then(Value::as_str).map(str::to_owned) else {
        return Err(FrameError::new(
            Some(id),
            ErrorCode::MalformedFrame,
            "request needs a string method",
        ));
    };
    if !METHODS.contains(&method.as_str()) {
        return Err(FrameError::new(
            Some(id),
            ErrorCode::UnknownMethod,
            format!("unknown method {method:?}"),
        ));
    }
    let params = match obj.remove("params") {
        None | Some(Value::Null) => json!({}),
        Some(p @ Value::Object(_)) => p,
        Some(_) => {
            return Err(FrameError::new(
                Some(id),
                ErrorCode::InvalidParams,
                "params must be an object",
            ))
        }
    };
    serde_json::from_value(json!({"method": method, "params": params}))
        .map(|request| ClientFrame::Request { id, request })
        .map_err(|e| FrameError::new(Some(id), ErrorCode::InvalidParams, e.to_string()))
}

fn parse_object(bytes: &[u8]) -> Result<(Map<String, Value>, Option<u64>), FrameError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|e| FrameError::new(None, ErrorCode::MalformedFrame, format!("bad JSON: {e}")))?;
    let Value::Object(obj) = value else {
        return Err(FrameError::new(
            None,
            ErrorCode::MalformedFrame,
            "frame must be a JSON object",
        ));
    };
    let id = obj.get("id").and_then(Value::as_u64);
    Ok((obj, id))
}

impl ServerFrame {
    /// Encodes to one wire line, including the trailing newline.
    pub fn encode(&self) -> Vec<u8> {
        let v = match self {
            ServerFrame::Hello(h) => typed("hello", to_value(h)),
            ServerFrame::Progress { id, event } => {
                json!({"type": "progress", "id": id, "event": event})
            }
            ServerFrame::Response { id, result } => {
                json!({"type": "response", "id": id, "result": result})
            }
            ServerFrame::Error { id, error } => json!({"type": "error", "id": id, "error": error}),
            ServerFrame::Unknown(kind) => json!({"type": kind}),
        };
        line(&v)
    }

    /// Decodes one line (without its newline).
    pub fn decode(bytes: &[u8]) -> Result<ServerFrame, FrameError> {
        let (mut obj, id) = parse_object(bytes)?;
        let kind = obj
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let bad = |e: serde_json::Error| {
            FrameError::new(id, ErrorCode::MalformedFrame, format!("bad {kind}: {e}"))
        };
        let need_id = || {
            id.ok_or_else(|| FrameError::new(None, ErrorCode::MalformedFrame, "frame needs an id"))
        };
        let mut field = |name: &str| obj.remove(name).unwrap_or(Value::Null);
        match kind.as_str() {
            "hello" => Ok(ServerFrame::Hello(
                serde_json::from_value(Value::Object(obj)).map_err(bad)?,
            )),
            "progress" => Ok(ServerFrame::Progress {
                id: need_id()?,
                event: serde_json::from_value(field("event")).map_err(bad)?,
            }),
            "response" => Ok(ServerFrame::Response {
                id: need_id()?,
                result: decode_response(field("result")).map_err(bad)?,
            }),
            "error" => Ok(ServerFrame::Error {
                id,
                error: serde_json::from_value(field("error")).map_err(bad)?,
            }),
            "" => Err(FrameError::new(
                id,
                ErrorCode::MalformedFrame,
                "frame has no string \"type\"",
            )),
            _ => Ok(ServerFrame::Unknown(kind)),
        }
    }
}

fn decode_response(result: Value) -> Result<Response, serde_json::Error> {
    match serde_json::from_value::<Response>(result.clone()) {
        Ok(r) => Ok(r),
        Err(e) => match result.get("kind").and_then(Value::as_str) {
            Some(kind) if !RESPONSE_KINDS.contains(&kind) => Ok(Response::Unknown {
                kind: kind.to_owned(),
                data: result.get("data").cloned().unwrap_or(Value::Null),
            }),
            _ => Err(e),
        },
    }
}

/// Outcome of `read_line`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineRead {
    /// A complete line is in the buffer, without its newline.
    Line,
    /// End of stream. The buffer may hold a truncated final line, which callers ignore.
    Eof,
    /// The line exceeded the limit.
    TooLong,
    /// A deadline in `ReadLimits` passed.
    Timeout,
    /// `ReadLimits::abort` returned true.
    Aborted,
}

/// Reads one `\n`-terminated line into `buf`, never holding more than `max` bytes.
///
/// Bytes already read stay in `buf` across `WouldBlock` and `TimedOut` errors, so the call can be
/// retried. The caller clears `buf` after a `Line`.
pub fn read_line<R: BufRead>(r: &mut R, buf: &mut Vec<u8>, max: usize) -> io::Result<LineRead> {
    loop {
        let chunk = match r.fill_buf() {
            Ok(c) => c,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            return Ok(LineRead::Eof);
        }
        let newline = chunk.iter().position(|&b| b == b'\n');
        let take = newline.unwrap_or(chunk.len());
        if buf.len() + take > max {
            return Ok(LineRead::TooLong);
        }
        buf.extend_from_slice(&chunk[..take]);
        r.consume(take + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(LineRead::Line);
        }
    }
}

/// Deadlines for `read_line_until`. They are checked whenever the underlying read wakes up, so
/// the stream needs a short read timeout (the callers use 200 ms).
pub struct ReadLimits<'a> {
    /// Deadline for the first byte of a line, re-evaluated on every wake-up.
    pub idle: &'a dyn Fn() -> Option<Instant>,
    /// Deadline for the whole call, whatever has arrived.
    pub hard: Option<Instant>,
    /// Time allowed from the first byte of a line to its newline.
    pub line: Option<Duration>,
    /// Polled on every wake-up.
    pub abort: &'a dyn Fn() -> bool,
}

/// Like `read_line`, with total deadlines instead of per-read ones, so a peer that sends one
/// byte at a time cannot hold the reader open. `buf` must be empty on entry.
pub fn read_line_until<R: BufRead>(
    r: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
    lim: &ReadLimits<'_>,
) -> io::Result<LineRead> {
    let mut line_deadline: Option<Instant> = None;
    loop {
        if (lim.abort)() {
            return Ok(LineRead::Aborted);
        }
        let now = Instant::now();
        let passed = |d: Option<Instant>| d.is_some_and(|d| now >= d);
        if passed(lim.hard)
            || passed(line_deadline)
            || (buf.is_empty() && line_deadline.is_none() && passed((lim.idle)()))
        {
            return Ok(LineRead::Timeout);
        }
        let chunk = match r.fill_buf() {
            Ok(c) => c,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            return Ok(LineRead::Eof);
        }
        if line_deadline.is_none() {
            line_deadline = lim.line.map(|l| Instant::now() + l);
        }
        let newline = chunk.iter().position(|&b| b == b'\n');
        let take = newline.unwrap_or(chunk.len());
        if buf.len() + take > max {
            return Ok(LineRead::TooLong);
        }
        buf.extend_from_slice(&chunk[..take]);
        r.consume(take + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(LineRead::Line);
        }
    }
}

impl std::fmt::Debug for ReadLimits<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadLimits")
            .field("hard", &self.hard)
            .field("line", &self.line)
            .finish_non_exhaustive()
    }
}
