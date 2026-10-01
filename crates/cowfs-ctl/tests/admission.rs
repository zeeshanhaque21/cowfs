//! Admission, shutdown and close-discipline regressions (A1, A2, A3), plus the coverage gaps of
//! round 2 (A4). Each test fails on the code before its fix.

mod common;

use common::*;
use cowfs_ctl::*;
use serde_json::json;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const HELLO: &str = r#"{"type":"hello","versions":[1]}"#;
const REQUEST: &str = r#"{"type":"request","id":1,"method":"gc","params":{"dry_run":true}}"#;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Ending {
    NoHello,
    NoFrame,
    Response,
    TwoTerminals,
    ReadFailed(std::io::ErrorKind),
}

struct Streamer {
    steps: u64,
    padding: usize,
}

impl ControlHandler for Streamer {
    fn gc(&self, _: GcParams, ctx: &OpContext<'_>) -> CtlResult<GcReport> {
        for done in 0..=self.steps {
            ctx.progress(ProgressEvent {
                phase: "mark".into(),
                done,
                total: Some(self.steps),
                unit: Unit::Items,
                message: (self.padding > 0).then(|| "x".repeat(self.padding)),
            })?;
        }
        Ok(GcReport {
            dry_run: true,
            candidate_blocks: 0,
            candidate_bytes: 0,
            freed_blocks: 0,
            freed_bytes: 0,
        })
    }
}

struct Never {
    entered: Arc<AtomicBool>,
}

impl ControlHandler for Never {
    fn gc(&self, _: GcParams, ctx: &OpContext<'_>) -> CtlResult<GcReport> {
        self.entered.store(true, Ordering::SeqCst);
        let _ = ctx.progress(ProgressEvent {
            phase: "never".into(),
            done: 0,
            total: None,
            unit: Unit::Items,
            message: None,
        });
        while !ctx.is_cancelled() {
            thread::sleep(Duration::from_millis(20));
        }
        thread::sleep(Duration::from_secs(3600));
        #[allow(unreachable_code)]
        Err(CtlError::cancelled())
    }
}

/// Feeds a socket into an ending, reading until the deadline or EOF.
fn classify(s: &UnixStream, read_budget: Duration) -> Ending {
    let mut pending: Vec<u8> = Vec::new();
    let mut hello = false;
    let mut terminals = 0;
    let until = Instant::now() + read_budget;
    let _ = s.set_read_timeout(Some(Duration::from_millis(50)));
    while Instant::now() < until && terminals < 2 {
        let mut buf = [0u8; 4096];
        match (&*s).read(&mut buf) {
            Ok(0) => {
                return if !hello {
                    Ending::NoHello
                } else if terminals == 0 {
                    Ending::NoFrame
                } else {
                    ending(terminals, true)
                };
            }
            Ok(n) => {
                pending.extend_from_slice(&buf[..n]);
                while let Some(i) = pending.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=i).collect();
                    let v: serde_json::Value = serde_json::from_slice(&line[..line.len() - 1])
                        .unwrap_or(serde_json::Value::Null);
                    match v["type"].as_str() {
                        Some("hello") => hello = true,
                        Some("progress") => {}
                        Some("response") | Some("error") => {
                            terminals += 1;
                            if terminals == 2 {
                                return Ending::TwoTerminals;
                            }
                        }
                        _ => return Ending::NoFrame,
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Ending::ReadFailed(e.kind()),
        }
    }
    if !hello {
        return Ending::NoHello;
    }
    ending(terminals, false)
}

fn ending(terminals: usize, eof: bool) -> Ending {
    let _ = eof;
    match terminals {
        1 => Ending::Response,
        _ => Ending::NoFrame,
    }
}

fn count(tally: &[(&'static str, u32)]) -> String {
    tally
        .iter()
        .map(|(e, n)| format!("{e}:{n}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn a1_a_backlogged_connection_at_shutdown_is_answered() {
    let _w = Watchdog::start(300);
    let mut tally: std::collections::BTreeMap<&'static str, u32> = Default::default();
    for _ in 0..150 {
        let fx = start(Streamer {
            steps: 3,
            padding: 0,
        });
        let s = UnixStream::connect(&fx.path).unwrap();
        let mut w = s.try_clone().unwrap();
        w.write_all(HELLO.as_bytes()).unwrap();
        w.write_all(b"\n").unwrap();
        w.write_all(REQUEST.as_bytes()).unwrap();
        w.write_all(b"\n").unwrap();
        fx.server().handle().shutdown();
        let e = classify(&s, Duration::from_millis(200));
        let key: &'static str = match e {
            Ending::NoHello => "NoHello",
            Ending::NoFrame => "NoFrame",
            Ending::Response => "Response",
            Ending::TwoTerminals => "TwoTerminals",
            Ending::ReadFailed(_) => "ReadFailed",
        };
        *tally.entry(key).or_default() += 1;
    }
    eprintln!(
        "A1 TALLY {}",
        count(&[
            ("NoHello", tally.get("NoHello").copied().unwrap_or(0)),
            ("NoFrame", tally.get("NoFrame").copied().unwrap_or(0)),
            ("Response", tally.get("Response").copied().unwrap_or(0)),
            ("ErrorFrame", tally.get("ErrorFrame").copied().unwrap_or(0)),
            (
                "TwoTerminals",
                tally.get("TwoTerminals").copied().unwrap_or(0)
            ),
            ("ReadFailed", tally.get("ReadFailed").copied().unwrap_or(0)),
        ])
    );
    assert_eq!(
        tally.get("NoHello").copied().unwrap_or(0),
        0,
        "every queued client must be answered"
    );
    assert_eq!(
        tally.get("NoFrame").copied().unwrap_or(0),
        0,
        "every request must end with a frame"
    );
    assert_eq!(tally.get("TwoTerminals").copied().unwrap_or(0), 0);
}

#[test]
fn a2_a_legit_client_is_served_while_the_cap_is_full_of_idle_connections() {
    let _w = Watchdog::start(300);
    let opts = ServerOptions {
        max_connections: 8,
        evict_idle: Duration::from_millis(200),
        ..ServerOptions::default()
    };
    let fx = start_with(stub(), opts);
    let mut held: Vec<UnixStream> = Vec::new();
    for _ in 0..8 {
        let s = UnixStream::connect(&fx.path).unwrap();
        let mut w = s.try_clone().unwrap();
        // A handshake keeps the server from timing the connection out while it is held.
        w.write_all(format!("{HELLO}\n").as_bytes()).unwrap();
        let mut line = String::new();
        let mut r = std::io::BufReader::new(s.try_clone().unwrap());
        std::io::BufRead::read_line(&mut r, &mut line).unwrap();
        assert!(
            line.contains("hello"),
            "held connection did not complete a handshake"
        );
        held.push(s);
    }
    thread::sleep(Duration::from_millis(300));
    for round in 0..20 {
        let mut c = bounded_connect(&fx.path);
        assert!(
            c.call(Request::Ping(Empty {})).is_ok(),
            "round {round}: legit client refused"
        );
        thread::sleep(Duration::from_millis(250));
    }
}

#[test]
fn a2_a_connection_with_a_request_in_flight_is_never_evicted() {
    let _w = Watchdog::start(120);
    let opts = ServerOptions {
        max_connections: 2,
        evict_idle: Duration::from_secs(5),
        ..ServerOptions::default()
    };
    let fx = start_with(stub().with_work(400, Duration::from_millis(10)), opts);
    let mut busy = Raw::hello(&fx.path);
    busy.send(REQUEST);
    assert_eq!(busy.recv()["type"], "progress");
    let mut refused = 0;
    let mut kept: Vec<Raw> = Vec::new();
    for _ in 0..8 {
        let mut extra = Raw::connect(&fx.path);
        extra
            .stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        extra.send(r#"{"type":"hello","versions":[1]}"#);
        let f = extra.recv();
        assert!(
            f["type"] == "hello" || f["error"]["code"] == "too_many_connections",
            "{f}"
        );
        if f["error"]["code"] == "too_many_connections" {
            refused += 1;
        } else {
            kept.push(extra);
        }
    }
    assert!(
        kept.len() <= 1,
        "{} idle connections were admitted",
        kept.len()
    );
    assert!(
        refused > 0,
        "the busy connection filled a slot and nothing gave way"
    );
    assert_eq!(
        busy.recv_final()["type"],
        "response",
        "the busy request finished"
    );
}

#[test]
fn a3_a_denied_peer_always_receives_its_error_frame() {
    let _w = Watchdog::start(300);
    let opts = ServerOptions {
        peer_check: PeerCheck::new(|_| Ok(current_uid().wrapping_add(1))),
        ..ServerOptions::default()
    };
    let fx = start_with(stub(), opts);
    let mut lost = 0;
    let mut other = 0;
    for _ in 0..600 {
        let mut c = UnixStream::connect(&fx.path).unwrap();
        let _ = c.set_read_timeout(Some(Duration::from_millis(500)));
        let wrote = c.write_all(format!("{HELLO}\n").as_bytes()).is_ok();
        let mut got = false;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match c.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.contains(&b'\n') {
                        got = true;
                        break;
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break
                }
                Err(_) => break,
            }
        }
        if !got {
            lost += 1;
            if !wrote {
                other += 1;
            }
        }
    }
    eprintln!("A3 lost={lost} write_failed_too={other}");
    assert_eq!(lost, 0, "a denied peer must always read its error frame");
}

#[test]
fn a4_shutdown_abandons_a_handler_that_never_returns() {
    let _w = Watchdog::start(120);
    let entered = Arc::new(AtomicBool::new(false));
    let mut fx = start_with(
        Never {
            entered: Arc::clone(&entered),
        },
        ServerOptions {
            shutdown_deadline: Duration::from_millis(300),
            ..ServerOptions::default()
        },
    );
    let mut busy = Raw::hello(&fx.path);
    busy.send(REQUEST);
    assert_eq!(busy.recv()["type"], "progress");
    assert!(entered.load(Ordering::SeqCst));
    let t0 = Instant::now();
    let server = fx.server.take().unwrap();
    let handle = server.handle();
    thread::scope(|s| {
        s.spawn(move || {
            handle.shutdown();
            server.wait();
        });
        thread::sleep(Duration::from_millis(900));
    });
    let f = busy.recv_final();
    assert_eq!(f["type"], "error", "{f}");
    assert!(
        matches!(
            f["error"]["code"].as_str(),
            Some("shutting_down") | Some("cancelled")
        ),
        "{f}"
    );
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "wait() must not block on the handler"
    );
}

#[test]
fn a4_a_terminal_frame_being_written_is_not_cut_by_shutdown() {
    let _w = Watchdog::start(180);
    // The client stops reading, so the terminal frame write is still in flight when the server
    // gives up on the connection.
    let fx = start_with(
        Streamer {
            steps: 20_000,
            padding: 2000,
        },
        ServerOptions {
            shutdown_deadline: Duration::from_millis(200),
            write_timeout: Duration::from_secs(5),
            ..ServerOptions::default()
        },
    );
    let s = UnixStream::connect(&fx.path).unwrap();
    let mut w = s.try_clone().unwrap();
    w.write_all(format!("{HELLO}\n").as_bytes()).unwrap();
    w.write_all(format!("{REQUEST}\n").as_bytes()).unwrap();
    thread::sleep(Duration::from_millis(300));
    fx.server().handle().shutdown();
    thread::sleep(Duration::from_millis(600));
    drop(w);
    let e = classify(&s, Duration::from_millis(2500));
    eprintln!("A4 finishing ending={e:?}");
    assert!(
        !matches!(e, Ending::NoFrame | Ending::NoHello | Ending::TwoTerminals),
        "the terminal frame must be whole: {e:?}"
    );
}

#[test]
fn a4_documented_default_limits_are_the_defaults() {
    let s = ServerOptions::default();
    assert_eq!(s.handshake_timeout, Duration::from_secs(10));
    assert_eq!(s.line_timeout, Duration::from_secs(10));
    assert_eq!(s.idle_timeout, Duration::from_secs(30));
    assert_eq!(s.write_timeout, Duration::from_secs(30));
    assert_eq!(s.max_connections, 64);
    assert_eq!(s.max_inflight, 32);
    assert_eq!(s.max_requests, 64);
    assert_eq!(s.shutdown_deadline, Duration::from_secs(5));
    assert_eq!(s.evict_idle, Duration::from_secs(10));
    let c = ClientOptions::default();
    assert_eq!(c.connect_timeout, Duration::from_secs(5));
    assert_eq!(c.handshake_timeout, Duration::from_secs(5));
    assert_eq!(c.idle_timeout, Duration::from_secs(30));
}

struct HolderRacer {
    seen: Arc<AtomicU64>,
    slept: Arc<AtomicBool>,
}

impl ControlHandler for HolderRacer {
    fn holders(&self, _name: &str) -> CtlResult<Vec<ProcessInfo>> {
        Ok(if self.slept.load(Ordering::SeqCst) {
            vec![ProcessInfo {
                pid: 1,
                command: "x".into(),
                holds: vec![],
            }]
        } else {
            Vec::new()
        })
    }

    fn swap(&self, name: &str, from: &str, guard: &HolderGuard) -> CtlResult<SnapshotInfo> {
        guard.check_holders()?;
        self.seen.fetch_add(1, Ordering::SeqCst);
        self.slept.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(200));
        guard.check_holders()?;
        Ok(SnapshotInfo {
            name: name.into(),
            parent: Some(from.into()),
            base: None,
            created_unix_ms: 0,
        })
    }
}

#[test]
fn a5_the_framework_rejects_a_reset_whose_holders_appeared() {
    let _w = Watchdog::start(120);
    let racer = Arc::new(HolderRacer {
        seen: Arc::default(),
        slept: Arc::default(),
    });
    let fx = start_arc(racer.clone(), ServerOptions::default());
    let mut c = Client::connect(&fx.path).unwrap();
    let r = c.call(Request::SnapshotReset(SnapshotReset {
        name: "slot".into(),
        from: "base".into(),
        expect_no_holders: true,
    }));
    assert_eq!(
        code(r.unwrap_err()),
        ErrorCode::Busy,
        "the holder appeared during the swap"
    );
    assert!(racer.seen.load(Ordering::SeqCst) > 0);
}

struct NeverCheck {
    holders: Vec<String>,
}

impl ControlHandler for NeverCheck {
    fn swap(&self, name: &str, from: &str, _: &HolderGuard<'_>) -> CtlResult<SnapshotInfo> {
        Ok(SnapshotInfo {
            name: name.into(),
            parent: Some(from.into()),
            base: None,
            created_unix_ms: 0,
        })
    }

    fn remove(&self, _: &str, _: &HolderGuard<'_>) -> CtlResult<()> {
        Ok(())
    }

    fn holders(&self, name: &str) -> CtlResult<Vec<ProcessInfo>> {
        Ok(self
            .holders
            .iter()
            .filter(|h| *h == name)
            .map(|h| ProcessInfo {
                pid: 3,
                command: h.clone(),
                holds: vec![],
            })
            .collect())
    }
}

#[test]
fn a5_the_framework_alone_refuses_a_handler_that_never_checks() {
    let _w = Watchdog::start(60);
    // A handler that ignores the guard is non-conformant, but the framework still refuses a
    // snapshot that has a holder before the handler ever runs.
    let fx = start_arc(
        Arc::new(NeverCheck {
            holders: vec!["held".into()],
        }),
        ServerOptions::default(),
    );
    let mut c = Client::connect(&fx.path).unwrap();
    let err = c
        .call(Request::SnapshotReset(SnapshotReset {
            name: "held".into(),
            from: "other".into(),
            expect_no_holders: true,
        }))
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::Busy, "reset");
    let err = c
        .call(Request::SnapshotRm(SnapshotRm {
            name: "held".into(),
            expect_no_holders: true,
        }))
        .unwrap_err();
    assert_eq!(code(err), ErrorCode::Busy, "rm");
    assert!(
        c.call(Request::SnapshotReset(SnapshotReset {
            name: "free".into(),
            from: "other".into(),
            expect_no_holders: true,
        }))
        .is_ok(),
        "a snapshot with no holder still reaches the handler"
    );
}

#[test]
fn a4_reset_rejects_unknown_params_and_rm_validates_the_name() {
    let _w = Watchdog::start(60);
    let fx = start(stub());
    let mut r = Raw::hello(&fx.path);
    r.send(r#"{"type":"request","id":1,"method":"snapshot_reset","params":{"name":"a","from":"b","fast":1}}"#);
    assert_eq!(code_of(&r.recv_final()), "invalid_params");
    r.send(r#"{"type":"request","id":2,"method":"snapshot_rm","params":{"name":".nfs1"}}"#);
    assert_eq!(code_of(&r.recv_final()), "invalid_params");
    r.send(r#"{"type":"request","id":3,"method":"snapshot_rm","params":{"name":"a\nb"}}"#);
    assert_eq!(code_of(&r.recv_final()), "invalid_params");
}

#[test]
fn a5_a_holder_is_reported_before_the_snapshot_is_looked_up() {
    let _w = Watchdog::start(60);
    let holder = ProcessInfo {
        pid: 7,
        command: "x".into(),
        holds: vec![],
    };
    let fx = start(stub().with_process("held", holder));
    let mut r = Raw::hello(&fx.path);
    assert_eq!(
        req(&mut r, 1, "snapshot_create", json!({"name": "base"}))["type"],
        "response"
    );
    assert_eq!(
        code_of(&req(
            &mut r,
            2,
            "snapshot_reset",
            json!({"name": "held", "from": "ghost"})
        )),
        "busy",
        "the framework holder check runs before the backend looks the snapshots up"
    );
    assert_eq!(
        code_of(&req(&mut r, 3, "snapshot_rm", json!({"name": "held"}))),
        "busy"
    );
}

fn bounded_connect(path: &std::path::Path) -> Client {
    let p = path.to_owned();
    bounded(20, move || {
        for _ in 0..100 {
            if let Ok(c) = Client::connect(&p) {
                return c;
            }
            thread::sleep(Duration::from_millis(100));
        }
        panic!("never served");
    })
}
