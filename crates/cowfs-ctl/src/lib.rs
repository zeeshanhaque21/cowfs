//! The cowfs control protocol: JSON-lines types, framing, a client, a server framework and an
//! in-memory stub handler. Contract: `docs/v1-control-api.md`.

mod client;
mod conformance;
mod error;
mod frame;
mod handler;
mod server;
mod socket;
mod stub;
mod sys;
mod treehash;
mod types;
mod validate;

pub use client::{Canceller, Client, ClientError, ClientOptions};
pub use conformance::{handler_conformance, AddHolder};
pub use error::{CtlError, CtlResult, ErrorCode};
pub use frame::{
    read_line, read_line_until, ClientFrame, FrameError, Hello, LineRead, ReadLimits, ServerFrame,
    ServerHello, MAX_REQUEST_LINE, MAX_RESPONSE_LINE, PROTOCOL_VERSION,
};
pub use handler::{CancelToken, ControlHandler, HolderGuard, OpContext};
pub use server::{PeerCheck, Server, ServerOptions, ShutdownHandle};
pub use socket::default_socket_path;
pub use stub::StubHandler;
pub use sys::current_uid;
pub use treehash::{hash_tree, hash_view, TreeHash, HASH_ALGORITHM};
pub use types::*;
pub use validate::{
    escape_control, name_key, validate_abs_path, validate_base_name, validate_git_ref,
    validate_mount_relative, validate_repo, validate_snapshot_name, BASE_NAME_MAX, MAX_NAME_BYTES,
    MAX_PATH_BYTES, MAX_REF_BYTES,
};
