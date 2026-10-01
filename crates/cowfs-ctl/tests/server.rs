//! The server framework and client against a real Unix socket.

mod common;

use common::*;
use cowfs_ctl::*;
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn end_to_end_every_method_with_the_real_client() {
    let handler = stub().with_process(
        "work",
        ProcessInfo {
            pid: 42,
            command: "sleep".into(),
            holds: vec![Hold {
                kind: HoldKind::Fd,
                path: "/mount/work/x".into(),
            }],
        },
    );
    let fx = start(handler);
    let mut c = Client::connect(&fx.path).unwrap();
    assert_eq!(c.server().version, 1);
    assert_eq!(c.server().methods, METHODS);

    assert!(matches!(
        c.call(Request::Ping(Empty {})).unwrap(),
        Response::Pong(_)
    ));
    let Response::Version(v) = c.call(Request::Version(Empty {})).unwrap() else {
        panic!()
    };
    assert_eq!(v.protocol, 1);

    let create = |name: &str, from: Option<&str>| {
        Request::SnapshotCreate(SnapshotCreate {
            name: name.into(),
            from: from.map(Into::into),
        })
    };
    let Response::Snapshot(s) = c.call(create("main", None)).unwrap() else {
        panic!()
    };
    assert_eq!((s.name.as_str(), s.parent), ("main", None));
    let Response::Snapshot(s) = c.call(create("work", Some("main"))).unwrap() else {
        panic!()
    };
    assert_eq!(s.parent.as_deref(), Some("main"));
    assert_eq!(
        code(c.call(create("work", None)).unwrap_err()),
        ErrorCode::AlreadyExists
    );
    assert_eq!(
        code(c.call(create("x", Some("nope"))).unwrap_err()),
        ErrorCode::NotFound
    );

    let Response::SnapshotList(l) = c.call(Request::SnapshotList(Empty {})).unwrap() else {
        panic!()
    };
    assert_eq!(l.snapshots.len(), 2);

    let Response::Snapshot(s) = c
        .call(Request::SnapshotPromote(SnapshotName {
            name: "main".into(),
        }))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(s.base, Some(BaseMeta::default()));

    let Response::Snapshot(s) = c
        .call(Request::SnapshotRename(SnapshotRename {
            from: "main".into(),
            to: "trunk".into(),
        }))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(s.name, "trunk");
    let Response::SnapshotList(l) = c.call(Request::SnapshotList(Empty {})).unwrap() else {
        panic!()
    };
    let work = l.snapshots.iter().find(|s| s.name == "work").unwrap();
    assert_eq!(
        work.parent.as_deref(),
        Some("trunk"),
        "rename updates children"
    );

    let Response::Processes(p) = c
        .call(Request::Ps(PsParams {
            snapshot: "work".into(),
        }))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(p.processes[0].pid, 42);
    assert_eq!(
        code(
            c.call(Request::SnapshotRm(SnapshotRm {
                name: "work".into(),
                expect_no_holders: true
            }))
            .unwrap_err()
        ),
        ErrorCode::Busy
    );
    assert!(matches!(
        c.call(Request::SnapshotRm(SnapshotRm {
            name: "trunk".into(),
            expect_no_holders: true
        }))
        .unwrap(),
        Response::Ok(_)
    ));
    assert_eq!(
        code(
            c.call(Request::SnapshotRm(SnapshotRm {
                name: "trunk".into(),
                expect_no_holders: true
            }))
            .unwrap_err()
        ),
        ErrorCode::NotFound
    );

    let Response::Status(st) = c.call(Request::Status(Empty {})).unwrap() else {
        panic!()
    };
    assert_eq!(
        (
            st.store_path.as_str(),
            st.mount_path.as_str(),
            st.snapshot_count
        ),
        ("/store", "/mount", 1)
    );
    let Response::MountInfo(m) = c.call(Request::MountInfo(Empty {})).unwrap() else {
        panic!()
    };
    assert_eq!(m.mount_path, "/mount");

    let Response::Gc(g) = c.call(Request::Gc(GcParams { dry_run: true })).unwrap() else {
        panic!()
    };
    assert_eq!((g.freed_blocks, g.candidate_blocks > 0), (0, true));
    let Response::Gc(g) = c.call(Request::Gc(GcParams { dry_run: false })).unwrap() else {
        panic!()
    };
    assert_eq!(g.freed_blocks, g.candidate_blocks);
    let Response::Fsck(f) = c.call(Request::Fsck(Empty {})).unwrap() else {
        panic!()
    };
    assert!(f.ok && f.problems.is_empty());

    let src = private_tempdir();
    std::fs::write(src.path().join("a"), b"12345").unwrap();
    std::fs::create_dir(src.path().join("d")).unwrap();
    std::fs::write(src.path().join("d/b"), b"123").unwrap();
    let Response::Import(i) = c
        .call(Request::Import(ImportParams {
            path: src.path().to_string_lossy().into_owned(),
            name: "slot".into(),
        }))
        .unwrap()
    else {
        panic!()
    };
    assert_eq!((i.files, i.bytes, i.verified), (2, 8, true));
    assert_eq!(
        code(
            c.call(Request::Import(ImportParams {
                path: "/no/such/dir".into(),
                name: "s2".into()
            }))
            .unwrap_err()
        ),
        ErrorCode::NotFound
    );

    let refresh = |c: &mut Client| {
        let Response::BaseRefresh(r) = c
            .call(Request::BaseRefresh(BaseRefreshParams {
                repo: "/srv/myrepo".into(),
                git_ref: "main".into(),
                name: None,
            }))
            .unwrap()
        else {
            panic!()
        };
        r
    };
    let first = refresh(&mut c);
    assert_eq!(first.snapshot.name, "myrepo-base");
    assert_eq!(first.previous_commit, None);
    assert_eq!(
        first.snapshot.base.unwrap().repo.as_deref(),
        Some("/srv/myrepo")
    );
    assert_eq!(
        refresh(&mut c).previous_commit.as_deref(),
        Some("stub-main")
    );

    assert!(matches!(
        c.call(Request::Shutdown(NoParams {})).unwrap(),
        Response::Ok(_)
    ));
    fx.server.unwrap().wait();
}

#[test]
fn long_operations_stream_progress() {
    let fx = start(stub());
    let mut c = Client::connect(&fx.path).unwrap();
    let mut events = Vec::new();
    c.call_with_progress(Request::Gc(GcParams { dry_run: false }), |e| {
        events.push(e.clone())
    })
    .unwrap();
    assert_eq!(events.len(), 10);
    assert_eq!(events[0].phase, "mark");
    assert_eq!(events[9].phase, "sweep");
    assert_eq!(events[4].done, 4);
    assert_eq!(events[4].total, Some(4));
}

#[test]
fn version_negotiation() {
    let fx = start(stub());
    let mut r = Raw::connect(&fx.path);
    r.send(r#"{"type":"hello","versions":[9,1,3]}"#);
    let h = r.recv();
    assert_eq!(
        (h["type"].as_str(), h["version"].as_u64()),
        (Some("hello"), Some(1))
    );
    assert_eq!(h["methods"].as_array().unwrap().len(), METHODS.len());

    let mut r = Raw::connect(&fx.path);
    r.send(r#"{"type":"hello","versions":[7,8]}"#);
    let e = r.recv();
    assert_eq!(e["type"], "error");
    assert_eq!(e["error"]["code"], "unsupported_version");
    assert_eq!(e["error"]["details"], json!({"supported": [1]}));
    r.assert_eof();

    let mut r = Raw::connect(&fx.path);
    r.send(r#"{"type":"hello","versions":[]}"#);
    assert_eq!(r.recv()["error"]["code"], "unsupported_version");
}

#[test]
fn handshake_is_required_and_times_out() {
    let opts = ServerOptions {
        handshake_timeout: Duration::from_millis(300),
        ..ServerOptions::default()
    };
    let fx = start_with(stub(), opts);

    let mut r = Raw::connect(&fx.path);
    r.send(r#"{"type":"request","id":1,"method":"ping"}"#);
    assert_eq!(r.recv()["error"]["code"], "handshake_required");
    r.assert_eof();

    let mut r = Raw::connect(&fx.path);
    r.send("garbage");
    assert_eq!(r.recv()["error"]["code"], "malformed_frame");
    r.assert_eof();

    let mut silent = Raw::connect(&fx.path);
    assert_eq!(silent.recv()["error"]["code"], "timeout");
    silent.assert_eof();
    assert!(Client::connect(&fx.path).is_ok());
}

#[test]
fn malformed_and_unknown_input_gets_structured_errors_and_the_connection_survives() {
    let fx = start(stub());
    let mut r = Raw::hello(&fx.path);
    let cases: &[(&str, &str, Value)] = &[
        ("not json", "malformed_frame", Value::Null),
        ("[1,2]", "malformed_frame", Value::Null),
        (r#"{"type":"request"}"#, "malformed_frame", Value::Null),
        (r#"{"type":"request","id":1}"#, "malformed_frame", json!(1)),
        (
            r#"{"type":"request","id":2,"method":"nope"}"#,
            "unknown_method",
            json!(2),
        ),
        (r#"{"type":"zzz","id":3}"#, "unknown_frame", json!(3)),
        (
            r#"{"type":"request","id":4,"method":"snapshot_rm","params":{"name":5}}"#,
            "invalid_params",
            json!(4),
        ),
        (
            r#"{"type":"hello","versions":[1]}"#,
            "malformed_frame",
            Value::Null,
        ),
        (
            r#"{"type":"request","id":5,"method":"snapshot_create","params":{"name":"a/b"}}"#,
            "invalid_params",
            json!(5),
        ),
        (
            r#"{"type":"request","id":6,"method":"snapshot_rename","params":{"from":"ok","to":".."}}"#,
            "invalid_params",
            json!(6),
        ),
    ];
    for (line, want_code, want_id) in cases {
        r.send(line);
        let e = r.recv();
        assert_eq!(e["type"], "error", "{line}");
        assert_eq!(e["error"]["code"], *want_code, "{line}");
        assert_eq!(e["id"], *want_id, "{line}");
    }
    r.send("");
    r.send("   ");
    r.send(r#"{"type":"request","id":9,"method":"ping"}"#);
    let p = r.recv();
    assert_eq!(
        (p["type"].as_str(), p["id"].as_u64()),
        (Some("response"), Some(9))
    );
}

#[test]
fn truncated_and_oversized_lines_never_hurt_the_server() {
    let fx = start(stub());

    let mut r = Raw::hello(&fx.path);
    r.stream
        .write_all(br#"{"type":"request","id":1,"method":"pi"#)
        .unwrap();
    drop(r);

    let mut r = Raw::hello(&fx.path);
    let junk = vec![b'x'; MAX_REQUEST_LINE + 10];
    r.stream.write_all(&junk).unwrap();
    let e = r.recv();
    assert_eq!(e["error"]["code"], "line_too_long");
    r.assert_eof();

    let mut r = Raw::connect(&fx.path);
    r.stream.write_all(&junk).unwrap();
    assert_eq!(r.recv()["error"]["code"], "line_too_long");
    r.assert_eof();

    let mut r = Raw::hello(&fx.path);
    r.stream.write_all(&[0xff, 0xfe, 0x00, b'\n']).unwrap();
    assert_eq!(r.recv()["error"]["code"], "malformed_frame");

    assert!(Client::connect(&fx.path)
        .unwrap()
        .call(Request::Ping(Empty {}))
        .is_ok());
}

#[test]
fn server_survives_clients_that_connect_and_close() {
    let fx = start(stub());
    for _ in 0..25 {
        drop(UnixStream::connect(&fx.path).unwrap());
    }
    for _ in 0..5 {
        let mut r = Raw::connect(&fx.path);
        r.send(r#"{"type":"hello","versions":[1]}"#);
        drop(r);
    }
    assert!(Client::connect(&fx.path)
        .unwrap()
        .call(Request::Ping(Empty {}))
        .is_ok());
}

#[test]
fn concurrent_clients() {
    let fx = start(stub());
    let path = &fx.path;
    thread::scope(|s| {
        for t in 0..8 {
            s.spawn(move || {
                let mut c = Client::connect(path).unwrap();
                for i in 0..25 {
                    assert!(matches!(
                        c.call(Request::Ping(Empty {})).unwrap(),
                        Response::Pong(_)
                    ));
                    if i % 5 == 0 {
                        let name = format!("s{t}-{i}");
                        c.call(Request::SnapshotCreate(SnapshotCreate { name, from: None }))
                            .unwrap();
                    }
                    c.call(Request::Gc(GcParams { dry_run: true })).unwrap();
                }
            });
        }
    });
    let mut c = Client::connect(path).unwrap();
    let Response::SnapshotList(l) = c.call(Request::SnapshotList(Empty {})).unwrap() else {
        panic!()
    };
    assert_eq!(l.snapshots.len(), 8 * 5);
}

#[test]
fn responses_to_concurrent_requests_are_matched_by_id() {
    let fx = start(stub().with_work(50, Duration::from_millis(20)));
    let mut r = Raw::hello(&fx.path);
    r.send(r#"{"type":"request","id":1,"method":"gc","params":{"dry_run":true}}"#);
    r.send(r#"{"type":"request","id":2,"method":"ping"}"#);
    let first_final = loop {
        let f = r.recv();
        if f["type"] != "progress" {
            break f;
        }
    };
    assert_eq!(
        (first_final["type"].as_str(), first_final["id"].as_u64()),
        (Some("response"), Some(2))
    );
    r.send(r#"{"type":"cancel","id":1}"#);
    loop {
        let f = r.recv();
        if f["type"] != "progress" {
            assert_eq!(
                (f["id"].as_u64(), f["error"]["code"].as_str()),
                (Some(1), Some("cancelled"))
            );
            break;
        }
    }
}

#[test]
fn cancel_request_stops_the_operation_and_keeps_the_connection() {
    let fx = start(stub().with_work(1000, Duration::from_millis(10)));
    let mut r = Raw::hello(&fx.path);
    r.send(r#"{"type":"request","id":1,"method":"fsck","params":{}}"#);
    assert_eq!(r.recv()["type"], "progress");
    r.send(r#"{"type":"cancel","id":1}"#);
    r.send(r#"{"type":"cancel","id":999}"#);
    let end = loop {
        let f = r.recv();
        if f["type"] != "progress" {
            break f;
        }
    };
    assert_eq!(end["type"], "error");
    assert_eq!(end["error"]["code"], "cancelled");
    assert_eq!(end["id"], 1);
    r.send(r#"{"type":"request","id":1,"method":"ping"}"#);
    assert_eq!(
        r.recv()["type"],
        "response",
        "the id is reusable once finished"
    );
}

#[test]
fn client_canceller_cancels_from_another_thread() {
    let fx = start(stub().with_work(1000, Duration::from_millis(10)));
    let mut c = Client::connect(&fx.path).unwrap();
    let canceller = c.canceller();
    let mut seen = 0;
    let started = Instant::now();
    let result = thread::scope(|s| {
        s.spawn(|| {
            thread::sleep(Duration::from_millis(100));
            canceller.cancel().unwrap();
        });
        c.call_with_progress(Request::Gc(GcParams { dry_run: false }), |_| seen += 1)
    });
    assert_eq!(code(result.unwrap_err()), ErrorCode::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the cancel was not acted on"
    );
    eprintln!("client_canceller: {seen} progress frames before the cancel");
    assert!(c.call(Request::Ping(Empty {})).is_ok());
}

struct Probe {
    saw_cancel: Arc<AtomicBool>,
    padding: usize,
}

impl ControlHandler for Probe {
    fn gc(&self, _: GcParams, ctx: &OpContext<'_>) -> CtlResult<GcReport> {
        for _ in 0..200_000 {
            let event = ProgressEvent {
                phase: "spin".into(),
                done: 0,
                total: None,
                unit: Unit::Items,
                message: (self.padding > 0).then(|| "x".repeat(self.padding)),
            };
            if ctx.progress(event).is_err() {
                self.saw_cancel.store(true, Ordering::SeqCst);
                return Err(CtlError::cancelled());
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("probe was never cancelled");
    }

    fn fsck(&self, _: &OpContext<'_>) -> CtlResult<FsckReport> {
        panic!("boom")
    }
}

#[test]
fn disconnect_mid_stream_cancels_the_operation() {
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let fx = start(Probe {
        saw_cancel: Arc::clone(&saw_cancel),
        padding: 0,
    });
    let mut r = Raw::hello(&fx.path);
    r.send(r#"{"type":"request","id":1,"method":"gc","params":{"dry_run":true}}"#);
    assert_eq!(r.recv()["type"], "progress");
    drop(r);
    wait_for("cancellation after disconnect", || {
        saw_cancel.load(Ordering::SeqCst)
    });
}

#[test]
fn slow_client_is_disconnected_and_its_operation_cancelled() {
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let opts = ServerOptions {
        write_timeout: Duration::from_millis(200),
        ..ServerOptions::default()
    };
    let fx = start_with(
        Probe {
            saw_cancel: Arc::clone(&saw_cancel),
            padding: 64 * 1024,
        },
        opts,
    );
    let mut r = Raw::hello(&fx.path);
    r.send(r#"{"type":"request","id":1,"method":"gc","params":{"dry_run":true}}"#);
    wait_for("cancellation of a client that stopped reading", || {
        saw_cancel.load(Ordering::SeqCst)
    });
    drop(r);
    assert!(Client::connect(&fx.path).is_ok());
}

#[test]
fn handler_panic_becomes_an_internal_error() {
    let fx = start(Probe {
        saw_cancel: Arc::default(),
        padding: 0,
    });
    let mut c = Client::connect(&fx.path).unwrap();
    assert_eq!(
        code(c.call(Request::Fsck(Empty {})).unwrap_err()),
        ErrorCode::Internal
    );
    assert!(c.call(Request::Ping(Empty {})).is_ok());
}

#[test]
fn unimplemented_methods_report_unsupported() {
    let fx = start(Probe {
        saw_cancel: Arc::default(),
        padding: 0,
    });
    let mut c = Client::connect(&fx.path).unwrap();
    assert_eq!(
        code(c.call(Request::Status(Empty {})).unwrap_err()),
        ErrorCode::Unsupported
    );
    assert_eq!(
        code(c.call(Request::SnapshotList(Empty {})).unwrap_err()),
        ErrorCode::Unsupported
    );
}

#[test]
fn duplicate_ids_and_inflight_limit() {
    let opts = ServerOptions {
        max_inflight: 2,
        ..ServerOptions::default()
    };
    let fx = start_with(stub().with_work(1000, Duration::from_millis(10)), opts);
    let mut r = Raw::hello(&fx.path);
    r.send(r#"{"type":"request","id":1,"method":"gc","params":{"dry_run":true}}"#);
    r.send(r#"{"type":"request","id":1,"method":"gc","params":{"dry_run":true}}"#);
    r.send(r#"{"type":"request","id":2,"method":"gc","params":{"dry_run":true}}"#);
    r.send(r#"{"type":"request","id":3,"method":"gc","params":{"dry_run":true}}"#);
    let mut errors = Vec::new();
    while errors.len() < 2 {
        let f = r.recv();
        if f["type"] == "error" {
            errors.push((
                f["id"].as_u64().unwrap(),
                f["error"]["code"].as_str().unwrap().to_owned(),
            ));
        }
    }
    errors.sort();
    assert_eq!(
        errors,
        vec![(1, "duplicate_id".to_owned()), (3, "busy".to_owned())]
    );
}

#[test]
fn shutdown_request_stops_the_server_and_removes_the_socket() {
    let mut fx = start(stub());
    let mut a = Client::connect(&fx.path).unwrap();
    let mut b = Client::connect(&fx.path).unwrap();
    assert!(matches!(
        a.call(Request::Shutdown(NoParams {})).unwrap(),
        Response::Ok(_)
    ));
    fx.server.take().unwrap().wait();
    assert!(!fx.path.exists());
    assert!(matches!(
        b.call(Request::Ping(Empty {})),
        Err(ClientError::Closed | ClientError::Io(_))
    ));
    assert!(Client::connect(&fx.path).unwrap_err().is_not_running());
}

#[test]
fn shutdown_cancels_running_operations() {
    let saw_cancel = Arc::new(AtomicBool::new(false));
    let mut fx = start(Probe {
        saw_cancel: Arc::clone(&saw_cancel),
        padding: 0,
    });
    let mut r = Raw::hello(&fx.path);
    r.send(r#"{"type":"request","id":1,"method":"gc","params":{"dry_run":true}}"#);
    assert_eq!(r.recv()["type"], "progress");
    fx.server.take().unwrap().shutdown();
    assert!(saw_cancel.load(Ordering::SeqCst));
    assert!(!fx.path.exists());
}

#[test]
fn shutdown_handle_stops_the_server() {
    let mut fx = start(stub());
    let handle = fx.server().handle();
    handle.shutdown();
    fx.server.take().unwrap().wait();
    assert!(!fx.path.exists());
}

#[test]
fn stale_socket_is_replaced() {
    let dir = private_tempdir();
    let path = dir.path().join("c.sock");
    drop(UnixListener::bind(&path).unwrap());
    assert!(path.exists());
    let server = Server::start(&path, Arc::new(stub()), ServerOptions::default()).unwrap();
    assert!(Client::connect(&path)
        .unwrap()
        .call(Request::Ping(Empty {}))
        .is_ok());
    server.shutdown();
}

#[test]
fn a_live_socket_is_never_stolen() {
    let dir = private_tempdir();
    let path = dir.path().join("c.sock");
    let live = UnixListener::bind(&path).unwrap();
    let err = Server::start(&path, Arc::new(stub()), ServerOptions::default()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    assert!(path.exists());
    live.set_nonblocking(true).unwrap();
    drop(UnixStream::connect(&path).unwrap());
    wait_for("the original listener to still receive connections", || {
        live.accept().is_ok()
    });
}

#[test]
fn a_second_server_on_the_same_socket_is_refused() {
    let fx = start(stub());
    let err = Server::start(&fx.path, Arc::new(stub()), ServerOptions::default()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    assert!(Client::connect(&fx.path)
        .unwrap()
        .call(Request::Ping(Empty {}))
        .is_ok());
}

#[test]
fn a_non_socket_at_the_path_is_left_alone() {
    let dir = private_tempdir();
    let path = dir.path().join("c.sock");
    std::fs::write(&path, b"precious").unwrap();
    let err = Server::start(&path, Arc::new(stub()), ServerOptions::default()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&path).unwrap(), b"precious");
}

#[test]
fn socket_and_directory_permissions() {
    let base = private_tempdir();
    let dir = base.path().join("made");
    let path = dir.join("c.sock");
    let server = Server::start(&path, Arc::new(stub()), ServerOptions::default()).unwrap();
    assert_eq!(std::fs::metadata(&dir).unwrap().mode() & 0o777, 0o700);
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert_eq!(std::fs::metadata(&path).unwrap().uid(), current_uid());
    server.shutdown();

    let open = base.path().join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
    let err = Server::start(
        &open.join("c.sock"),
        Arc::new(stub()),
        ServerOptions::default(),
    )
    .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(!open.join("c.sock").exists());
}

#[test]
fn peer_uid_mismatch_is_refused() {
    let opts = ServerOptions {
        peer_check: PeerCheck::new(|_| Ok(current_uid().wrapping_add(1))),
        ..ServerOptions::default()
    };
    let fx = start_with(stub(), opts);
    let mut r = Raw::connect(&fx.path);
    let e = r.recv();
    assert_eq!(e["error"]["code"], "permission_denied");
    r.assert_eof();
    assert_eq!(
        code(Client::connect(&fx.path).unwrap_err()),
        ErrorCode::PermissionDenied
    );
}

#[test]
fn a_failing_peer_credential_lookup_fails_closed() {
    let opts = ServerOptions {
        peer_check: PeerCheck::new(|_| Err(std::io::Error::other("no credentials"))),
        ..ServerOptions::default()
    };
    let fx = start_with(stub(), opts);
    let mut r = Raw::connect(&fx.path);
    assert_eq!(r.recv()["error"]["code"], "permission_denied");
    r.assert_eof();
}

#[test]
fn a_socket_directory_owned_by_someone_else_is_refused() {
    let dir = private_tempdir();
    let opts = ServerOptions {
        expected_uid: current_uid().wrapping_add(1),
        ..ServerOptions::default()
    };
    let err = Server::start(&dir.path().join("c.sock"), Arc::new(stub()), opts).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(!dir.path().join("c.sock").exists());
}

#[test]
fn the_single_instance_lock_is_taken() {
    let dir = private_tempdir();
    let path = dir.path().join("c.sock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.path().join("c.sock.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let err = Server::start(&path, Arc::new(stub()), ServerOptions::default()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    assert!(err.to_string().contains("lock"), "{err}");
    assert!(!path.exists(), "nothing was bound");
}

#[test]
fn matching_peer_uid_is_accepted() {
    let opts = ServerOptions {
        expected_uid: current_uid(),
        ..ServerOptions::default()
    };
    let fx = start_with(stub(), opts);
    assert!(Client::connect(&fx.path).is_ok());
}

#[test]
fn default_socket_path_is_per_user() {
    let p = default_socket_path();
    assert!(p.is_absolute());
    assert_eq!(p.file_name().unwrap(), "control.sock");
}

#[test]
fn connecting_to_nothing_is_not_running() {
    let dir = private_tempdir();
    let err = Client::connect(&dir.path().join("none.sock")).unwrap_err();
    assert!(err.is_not_running());
}
