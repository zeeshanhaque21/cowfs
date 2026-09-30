//! The cowfs control protocol: JSON-lines types, framing, a client, a server framework and an
//! in-memory stub handler. Contract: `docs/v1-control-api.md`.

mod client;
mod error;
mod frame;
mod handler;
mod server;
mod socket;
mod stub;
mod sys;
mod types;

pub use client::{Canceller, Client, ClientError};
pub use error::{CtlError, CtlResult, ErrorCode};
pub use frame::{
    read_line, ClientFrame, FrameError, Hello, LineRead, ServerFrame, ServerHello,
    MAX_REQUEST_LINE, MAX_RESPONSE_LINE, PROTOCOL_VERSION,
};
pub use handler::{CancelToken, ControlHandler, OpContext};
pub use server::{Server, ServerOptions, ShutdownHandle};
pub use socket::default_socket_path;
pub use stub::StubHandler;
pub use sys::current_uid;
pub use types::*;
