//! Teardown-safety helpers for a fixture that starts processes and mounts a real filesystem.
//!
//! Split into `reader.rs`, which holds the runtime code, and `tests.rs`, which holds its unit tests.
//! The gate includes `reader.rs` directly and must not drag the unit tests into its own binary, and
//! the `guard` test target includes both, so the platform-independent half is tested on every
//! runner rather than only on macOS.

#[path = "reader.rs"]
mod reader;

#[path = "tests.rs"]
mod tests;
