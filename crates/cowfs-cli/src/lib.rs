//! The `cowfs` command line. Contract: `docs/v1-control-api.md`.

mod backend;
mod cli;
mod commands;
mod output;

pub use backend::{make_backend, Backend, BackendError, BackendKind, ServeConfig, StubBackend};
pub use cli::{BaseCommand, Cli, Command, SnapshotCommand};
pub use commands::{
    run, start_server, usage_error, write_bytes, ServeError, EXIT_ERROR, EXIT_INTERRUPTED,
    EXIT_NOT_RUNNING, EXIT_OK,
};
pub use output::{human, progress_bar, progress_line, utc, Progress};
