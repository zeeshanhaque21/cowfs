//! The `Backend` seam: `cowfs serve` reaches the real core only through it.

use cowfs_cli::{
    make_backend, start_server, Backend, BackendError, BackendKind, ServeConfig, ServeError,
};
use cowfs_ctl::{Client, ControlHandler, Empty, Request, Response, StubHandler};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn config() -> ServeConfig {
    ServeConfig {
        store: PathBuf::from("/the/store"),
        mount: PathBuf::from("/the/mount"),
    }
}

fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

struct Custom;

impl Backend for Custom {
    fn open(&self, c: &ServeConfig) -> Result<Arc<dyn ControlHandler>, BackendError> {
        Ok(Arc::new(StubHandler::new(
            "custom-store",
            c.mount.display().to_string(),
        )))
    }
}

struct Broken;

impl Backend for Broken {
    fn open(&self, _: &ServeConfig) -> Result<Arc<dyn ControlHandler>, BackendError> {
        Err(BackendError::Open("store is locked".into()))
    }
}

#[test]
fn stub_kind_builds_a_working_backend() {
    let dir = private_dir();
    let socket = dir.path().join("c.sock");
    let backend = make_backend(BackendKind::Stub {
        work_delay: Duration::ZERO,
        ignore_cancel: false,
    })
    .unwrap();
    let server = start_server(backend.as_ref(), &config(), &socket).unwrap();
    let mut client = Client::connect(&socket).unwrap();
    let Response::Status(s) = client.call(Request::Status(Empty {})).unwrap() else {
        panic!("not a status")
    };
    assert_eq!(
        (s.store_path.as_str(), s.mount_path.as_str()),
        ("/the/store", "/the/mount")
    );
    server.shutdown();
}

/// The real backend is the daemon. It needs a real store directory and a real mount, so this
/// checks the wiring (it builds, and it reports a mount it cannot make) rather than a mount.
#[test]
fn the_real_backend_builds_and_reports_a_store_it_cannot_serve() {
    let backend = make_backend(BackendKind::Real).expect("the real backend builds");
    let err = backend
        .open(&config())
        .err()
        .expect("a store that does not exist cannot be served");
    assert!(
        matches!(err, BackendError::Open(_)),
        "the failure names the store, not the wiring: {err}"
    );
}

#[test]
fn a_custom_backend_plugs_in_and_serves() {
    let dir = private_dir();
    let socket = dir.path().join("c.sock");
    let server = start_server(&Custom, &config(), &socket).unwrap();
    let mut client = Client::connect(&socket).unwrap();
    let Response::Status(s) = client.call(Request::Status(Empty {})).unwrap() else {
        panic!("not a status")
    };
    assert_eq!(s.store_path, "custom-store");
    server.shutdown();
}

#[test]
fn a_backend_that_fails_to_open_binds_nothing() {
    let dir = private_dir();
    let socket = dir.path().join("c.sock");
    let err = start_server(&Broken, &config(), &socket).unwrap_err();
    assert!(matches!(err, ServeError::Backend(BackendError::Open(_))));
    assert!(!socket.exists());
}

#[test]
fn a_bind_failure_names_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let socket = dir.path().join("c.sock");
    let err = start_server(&Custom, &config(), &socket).unwrap_err();
    assert!(matches!(err, ServeError::Bind { .. }));
    assert!(err.to_string().contains("c.sock"));
}
