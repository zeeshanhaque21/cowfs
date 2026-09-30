//! Regression tests for the critic findings on PR #33. Each fails on the code before its fix.

mod common;

use common::*;
use cowfs_ctl::*;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

fn req(r: &mut Raw, id: u64, method: &str, params: Value) -> Value {
    r.send(&json!({"type": "request", "id": id, "method": method, "params": params}).to_string());
    r.recv_final()
}

fn err_code(f: &Value) -> &str {
    assert_eq!(f["type"], "error", "{f}");
    f["error"]["code"].as_str().unwrap()
}

struct Stubborn {
    started: Arc<AtomicBool>,
}

impl ControlHandler for Stubborn {
    fn gc(&self, _: GcParams, _: &OpContext<'_>) -> CtlResult<GcReport> {
        self.started.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_secs(9));
        Err(CtlError::cancelled())
    }
}

#[test]
fn h1_client_gives_up_on_a_listener_that_never_replies() {
    let dir = private_tempdir();
    let path = dir.path().join("silent.sock");
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        let mut held = Vec::new();
        for s in listener.incoming().flatten() {
            held.push(s);
        }
    });
    let started = Instant::now();
    let p = path.clone();
    let result = bounded(15, move || Client::connect(&p).map(|_| ()));
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(14));
}

#[test]
fn h2_shutdown_is_bounded_removes_the_socket_first_and_sends_a_terminal_frame() {
    let started = Arc::new(AtomicBool::new(false));
    let mut fx = start_with(
        Stubborn {
            started: Arc::clone(&started),
        },
        ServerOptions {
            shutdown_deadline: Duration::from_secs(2),
            ..ServerOptions::default()
        },
    );
    let mut busy = Raw::hello(&fx.path);
    busy.send(r#"{"type":"request","id":1,"method":"gc","params":{"dry_run":true}}"#);
    wait_for("the handler to start", || started.load(Ordering::SeqCst));

    let mut c = Client::connect(&fx.path).unwrap();
    assert!(matches!(
        c.call(Request::Shutdown(NoParams {})).unwrap(),
        Response::Ok(_)
    ));
    let t0 = Instant::now();
    wait_for("the socket file to be removed when shutdown begins", || {
        !fx.path.exists()
    });
    assert!(t0.elapsed() < Duration::from_secs(1));
    assert!(UnixStream::connect(&fx.path).is_err());

    let f = busy.recv_final();
    let c = err_code(&f).to_owned();
    assert!(c == "shutting_down" || c == "cancelled", "{f}");
    assert_eq!(f["id"], 1);

    let (tx, rx) = mpsc::channel();
    let server = fx.server.take().unwrap();
    thread::spawn(move || {
        server.wait();
        let _ = tx.send(t0.elapsed());
    });
    let waited = rx
        .recv_timeout(Duration::from_secs(8))
        .expect("server.wait() never returned");
    assert!(waited < Duration::from_secs(5), "{waited:?}");
}

fn fast_limits() -> ServerOptions {
    ServerOptions {
        handshake_timeout: Duration::from_secs(3),
        line_timeout: Duration::from_secs(2),
        ..ServerOptions::default()
    }
}

#[test]
fn h3_drip_fed_handshake_is_dropped_at_a_total_deadline() {
    let fx = start_with(stub(), fast_limits());
    let mut r = Raw::connect(&fx.path);
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut closed = false;
    r.stream
        .set_read_timeout(Some(Duration::from_millis(5)))
        .unwrap();
    while Instant::now() < deadline {
        if r.stream.write_all(b" ").is_err() {
            closed = true;
            break;
        }
        thread::sleep(Duration::from_secs(1));
        let mut b = [0u8; 256];
        if let Ok(0) = r.stream.read(&mut b) {
            closed = true;
            break;
        }
    }
    assert!(
        closed,
        "a one byte per second drip held the handshake open past 12 s"
    );
}

#[test]
fn h3_partial_line_after_the_handshake_is_dropped() {
    let fx = start_with(stub(), fast_limits());
    let mut r = Raw::hello(&fx.path);
    r.stream
        .write_all(br#"{"type":"request","id":1,"met"#)
        .unwrap();
    r.stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut all = Vec::new();
    let res = r.stream.read_to_end(&mut all);
    assert!(res.is_ok(), "still open after 10 s: {res:?}");
}

#[test]
fn h3_drip_fed_request_line_is_dropped_at_the_line_deadline() {
    let fx = start_with(stub(), fast_limits());
    let mut r = Raw::hello(&fx.path);
    r.stream
        .set_read_timeout(Some(Duration::from_millis(5)))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut closed = false;
    while Instant::now() < deadline {
        if r.stream.write_all(b"{").is_err() {
            closed = true;
            break;
        }
        thread::sleep(Duration::from_millis(500));
        let mut b = [0u8; 256];
        if let Ok(0) = r.stream.read(&mut b) {
            closed = true;
            break;
        }
    }
    assert!(closed, "a drip-fed request line was never dropped");
}

#[test]
fn h3_idle_connection_is_closed_but_a_busy_one_is_not() {
    let opts = ServerOptions {
        idle_timeout: Duration::from_millis(600),
        ..ServerOptions::default()
    };
    let fx = start_with(stub().with_work(30, Duration::from_millis(50)), opts);
    let mut idle = Raw::hello(&fx.path);
    let mut busy = Raw::hello(&fx.path);
    busy.send(r#"{"type":"request","id":1,"method":"gc","params":{"dry_run":true}}"#);
    let f = idle.recv();
    assert_eq!(err_code(&f), "timeout");
    idle.assert_eof();
    let f = busy.recv_final();
    assert_eq!(
        f["type"], "response",
        "a long request outlives the idle timeout: {f}"
    );
}

#[test]
fn h3_connections_over_the_cap_get_a_structured_error() {
    let opts = ServerOptions {
        max_connections: 2,
        ..ServerOptions::default()
    };
    let fx = start_with(stub(), opts);
    let a = Raw::hello(&fx.path);
    let b = Raw::hello(&fx.path);
    let mut c = Raw::connect(&fx.path);
    assert_eq!(err_code(&c.recv()), "too_many_connections");
    c.assert_eof();
    drop(a);
    wait_for("a slot to free up", || {
        let mut r = Raw::connect(&fx.path);
        r.send(r#"{"type":"hello","versions":[1]}"#);
        r.recv()["type"] == "hello"
    });
    drop(b);
}

#[test]
fn h3_global_request_cap_is_enforced() {
    let opts = ServerOptions {
        max_requests: 2,
        ..ServerOptions::default()
    };
    let fx = start_with(stub().with_work(1000, Duration::from_millis(10)), opts);
    let mut a = Raw::hello(&fx.path);
    let mut b = Raw::hello(&fx.path);
    a.send(r#"{"type":"request","id":1,"method":"gc","params":{"dry_run":true}}"#);
    b.send(r#"{"type":"request","id":1,"method":"gc","params":{"dry_run":true}}"#);
    assert_eq!(a.recv()["type"], "progress");
    assert_eq!(b.recv()["type"], "progress");
    let mut c = Raw::hello(&fx.path);
    let f = req(&mut c, 1, "gc", json!({"dry_run": true}));
    assert_eq!(err_code(&f), "busy");
}

#[test]
fn h1_client_timeouts_are_configurable_and_typed() {
    let dir = private_tempdir();
    let path = dir.path().join("silent.sock");
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        let mut held = Vec::new();
        for s in listener.incoming().flatten() {
            held.push(s);
        }
    });
    let opts = ClientOptions {
        handshake_timeout: Duration::from_millis(500),
        ..ClientOptions::default()
    };
    let t = Instant::now();
    let err = Client::connect_with(&path, opts).unwrap_err();
    assert!(err.is_timeout(), "{err:?}");
    assert!(t.elapsed() < Duration::from_secs(3));
}

#[test]
fn h1_request_idle_timeout_resets_on_progress() {
    let fx = start(stub().with_work(6, Duration::from_millis(300)));
    let opts = ClientOptions {
        idle_timeout: Duration::from_secs(1),
        ..ClientOptions::default()
    };
    let mut c = Client::connect_with(&fx.path, opts).unwrap();
    let t = Instant::now();
    assert!(c.call(Request::Gc(GcParams { dry_run: true })).is_ok());
    assert!(
        t.elapsed() > Duration::from_secs(2),
        "the call outlived the idle timeout"
    );

    let fx = start(Stubborn {
        started: Arc::default(),
    });
    let mut c = Client::connect_with(&fx.path, opts).unwrap();
    let t = Instant::now();
    let err = c.call(Request::Gc(GcParams { dry_run: true })).unwrap_err();
    assert!(err.is_timeout(), "{err:?}");
    assert!(t.elapsed() < Duration::from_secs(4));
}

#[test]
fn m1_half_close_means_no_more_requests_not_cancel() {
    let fx = start(stub().with_work(20, Duration::from_millis(20)));
    let mut r = Raw::hello(&fx.path);
    r.send(r#"{"type":"request","id":1,"method":"gc","params":{"dry_run":true}}"#);
    r.stream.shutdown(std::net::Shutdown::Write).unwrap();
    let f = r.recv_final();
    assert_eq!(f["type"], "response", "{f}");
    r.assert_eof();
}

#[test]
fn m3_destructive_params_are_strict() {
    let fx = start(stub());
    let mut r = Raw::hello(&fx.path);
    r.send(r#"{"type":"request","id":1,"method":"gc","params":{"dryrun":true}}"#);
    assert_eq!(err_code(&r.recv_final()), "invalid_params");
    r.send(r#"{"type":"request","id":2,"method":"gc","params":{}}"#);
    assert_eq!(
        err_code(&r.recv_final()),
        "invalid_params",
        "gc must not default to a real run"
    );
    assert_eq!(
        err_code(&req(
            &mut r,
            20,
            "gc",
            json!({"dry_run": true, "dryrun": false})
        )),
        "invalid_params",
        "an unknown field next to a valid dry_run is still rejected"
    );
    assert_eq!(
        err_code(&req(
            &mut r,
            3,
            "snapshot_rm",
            json!({"name": "a", "force": true})
        )),
        "invalid_params"
    );
    assert_eq!(
        err_code(&req(&mut r, 4, "shutdown", json!({"now": true}))),
        "invalid_params"
    );
    assert_eq!(
        err_code(&req(
            &mut r,
            5,
            "import",
            json!({"path": "/x", "name": "a", "verify": false})
        )),
        "invalid_params"
    );
    assert_eq!(
        err_code(&req(
            &mut r,
            6,
            "base_refresh",
            json!({"repo": "/r", "git_ref": "main", "fast": 1})
        )),
        "invalid_params"
    );
}

#[test]
fn m6_rm_and_reset_are_atomic_against_a_holder_appearing() {
    for round in 0..40 {
        let h = Arc::new(stub());
        h.snapshot_create(SnapshotCreate {
            name: "base".into(),
            from: None,
        })
        .unwrap();
        h.snapshot_create(SnapshotCreate {
            name: "slot".into(),
            from: None,
        })
        .unwrap();
        let holder = ProcessInfo {
            pid: 1,
            command: "x".into(),
            holds: vec![],
        };
        let h2 = Arc::clone(&h);
        let adder = thread::spawn(move || h2.add_process("slot", holder));
        let outcome = if round % 2 == 0 {
            h.snapshot_rm("slot", true)
        } else {
            h.snapshot_reset("slot", "base", true).map(|_| ())
        };
        adder.join().unwrap();
        let present = h.snapshot_list().unwrap().iter().any(|s| s.name == "slot");
        match outcome {
            Ok(()) if round % 2 == 0 => assert!(!present, "rm succeeded but the snapshot is there"),
            Ok(()) => assert!(present, "reset left no snapshot under the name"),
            Err(e) => {
                assert_eq!(e.code, ErrorCode::Busy);
                assert!(present && !h.ps("slot").unwrap().is_empty());
            }
        }
    }
}

#[test]
fn l5_params_must_be_an_object() {
    let fx = start(stub());
    let mut r = Raw::hello(&fx.path);
    assert_eq!(
        err_code(&req(&mut r, 1, "snapshot_create", json!(["x"]))),
        "invalid_params"
    );
    assert_eq!(
        err_code(&req(&mut r, 2, "ping", json!([]))),
        "invalid_params"
    );
    assert_eq!(
        err_code(&req(&mut r, 3, "ping", json!("x"))),
        "invalid_params"
    );
}

#[test]
fn m4_argument_injection_is_rejected_before_the_handler() {
    let fx = start(stub());
    let mut r = Raw::hello(&fx.path);
    let refresh = |r: &mut Raw, id, repo: &str, git_ref: &str| {
        err_code(&req(
            r,
            id,
            "base_refresh",
            json!({"repo": repo, "git_ref": git_ref}),
        ))
        .to_owned()
    };
    assert_eq!(
        refresh(&mut r, 1, "/srv/repo", "--upload-pack=evil"),
        "invalid_params"
    );
    assert_eq!(refresh(&mut r, 2, "-x", "main"), "invalid_params");
    assert_eq!(refresh(&mut r, 3, "rel/repo", "main"), "invalid_params");
    assert_eq!(refresh(&mut r, 4, "/srv/repo", "main\nx"), "invalid_params");
    assert_eq!(refresh(&mut r, 5, "/srv/repo", "a b"), "invalid_params");
    assert_eq!(refresh(&mut r, 6, "/srv/repo", "a..b"), "invalid_params");
    assert_eq!(
        refresh(&mut r, 7, "/srv/re\u{0}po", "main"),
        "invalid_params"
    );
    assert_eq!(
        refresh(&mut r, 8, "/srv/repo", &"x".repeat(300)),
        "invalid_params"
    );
    assert_eq!(
        err_code(&req(
            &mut r,
            9,
            "import",
            json!({"path": "relative/dir", "name": "a"})
        )),
        "invalid_params"
    );
    assert_eq!(
        err_code(&req(
            &mut r,
            10,
            "import",
            json!({"path": "/tmp/a\nb", "name": "a"})
        )),
        "invalid_params"
    );
    let ok = req(
        &mut r,
        11,
        "base_refresh",
        json!({"repo": "/srv/repo", "git_ref": "refs/heads/main"}),
    );
    assert_eq!(ok["type"], "response", "{ok}");
}

#[test]
fn m5_unsafe_and_colliding_snapshot_names_are_rejected() {
    let fx = start(stub());
    let mut r = Raw::hello(&fx.path);
    let mut id = 0;
    let mut create = |r: &mut Raw, name: &str| {
        id += 1;
        req(r, id, "snapshot_create", json!({"name": name}))
    };
    for bad in [
        ".nfs123",
        "._x",
        ".hidden",
        "a\nb",
        "a\u{1b}]0;X\u{7}",
        "\u{85}x",
        "tab\there",
    ] {
        assert_eq!(err_code(&create(&mut r, bad)), "invalid_params", "{bad:?}");
    }
    assert_eq!(create(&mut r, "Foo")["type"], "response");
    assert_eq!(err_code(&create(&mut r, "foo")), "already_exists");
    assert_eq!(err_code(&create(&mut r, "FOO")), "already_exists");
    assert_eq!(create(&mut r, "caf\u{e9}")["type"], "response");
    assert_eq!(err_code(&create(&mut r, "cafe\u{301}")), "already_exists");
    assert_eq!(create(&mut r, "other")["type"], "response");
    let f = req(
        &mut r,
        90,
        "snapshot_rename",
        json!({"from": "other", "to": "FOO"}),
    );
    assert_eq!(err_code(&f), "already_exists");
    let f = req(
        &mut r,
        91,
        "snapshot_rename",
        json!({"from": "Foo", "to": "foo"}),
    );
    assert_eq!(
        f["type"], "response",
        "a case-only rename of the same snapshot is allowed: {f}"
    );
}

#[test]
fn m6_snapshot_reset_is_atomic_and_refuses_when_busy() {
    let holder = ProcessInfo {
        pid: 9,
        command: "sleep".into(),
        holds: vec![Hold {
            kind: HoldKind::Cwd,
            path: "/mount/slot".into(),
        }],
    };
    let fx = start(stub().with_process("slot", holder));
    let mut r = Raw::hello(&fx.path);
    assert_eq!(
        req(&mut r, 1, "snapshot_create", json!({"name": "base"}))["type"],
        "response"
    );
    assert_eq!(
        req(&mut r, 2, "snapshot_create", json!({"name": "slot"}))["type"],
        "response"
    );
    let f = req(
        &mut r,
        3,
        "snapshot_reset",
        json!({"name": "slot", "from": "base"}),
    );
    assert_eq!(err_code(&f), "busy", "{f}");
    let f = req(
        &mut r,
        4,
        "snapshot_reset",
        json!({"name": "slot", "from": "base", "expect_no_holders": false}),
    );
    assert_eq!(f["type"], "response", "{f}");
    assert_eq!(f["result"]["data"]["parent"], "base");
    let f = req(&mut r, 5, "snapshot_list", json!({}));
    assert_eq!(
        f["result"]["data"]["snapshots"].as_array().unwrap().len(),
        2,
        "no window where the name is missing"
    );
    assert_eq!(
        err_code(&req(
            &mut r,
            6,
            "snapshot_reset",
            json!({"name": "ghost", "from": "base"})
        )),
        "not_found"
    );
    assert_eq!(
        err_code(&req(
            &mut r,
            7,
            "snapshot_reset",
            json!({"name": "slot", "from": "ghost"})
        )),
        "not_found"
    );
    assert_eq!(
        err_code(&req(
            &mut r,
            8,
            "snapshot_reset",
            json!({"name": "slot", "from": "slot"})
        )),
        "invalid_params"
    );
}

#[test]
fn m7_import_report_is_independently_checkable() {
    let fx = start(stub());
    let mut r = Raw::hello(&fx.path);
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("a"), b"hello").unwrap();
    let f = req(
        &mut r,
        1,
        "import",
        json!({"path": src.path().to_str().unwrap(), "name": "imp"}),
    );
    assert_eq!(f["type"], "response", "{f}");
    let data = f["result"]["data"].as_object().unwrap();
    for key in [
        "hash_algorithm",
        "source_root_hash",
        "imported_root_hash",
        "mismatches",
        "mismatches_truncated",
        "verified",
        "files",
        "bytes",
    ] {
        assert!(data.contains_key(key), "missing {key}: {data:?}");
    }
    assert_eq!(data["hash_algorithm"], "blake3");
    assert_eq!(data["source_root_hash"], data["imported_root_hash"]);
    assert_eq!(data["source_root_hash"].as_str().unwrap().len(), 64);
}

#[test]
fn l6_unknown_response_kind_does_not_break_an_old_client() {
    let f = ServerFrame::decode(
        br#"{"type":"response","id":1,"result":{"kind":"from_the_future","data":{"x":1}}}"#,
    );
    assert!(f.is_ok(), "{f:?}");
}
