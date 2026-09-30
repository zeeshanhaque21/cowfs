//! Wire-level only: compiles against both the pre-fix and post-fix server.
mod common;

use common::*;
use cowfs_ctl::*;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::thread;
use std::time::{Duration, Instant};

const HELLO: &str = r#"{"type":"hello","versions":[1]}"#;
const REQUEST: &str = r#"{"type":"request","id":1,"method":"gc","params":{"dry_run":true}}"#;

struct Streamer;
impl ControlHandler for Streamer {
    fn gc(&self, _: GcParams, ctx: &OpContext<'_>) -> CtlResult<GcReport> {
        for done in 0..=3 {
            ctx.progress(ProgressEvent {
                phase: "mark".into(),
                done,
                total: Some(3),
                unit: Unit::Items,
                message: None,
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

#[derive(Default)]
struct Tally {
    no_hello: u32,
    no_frame: u32,
    ok: u32,
    other: u32,
}

fn classify(s: &UnixStream, budget: Duration, t: &mut Tally) {
    let mut pending: Vec<u8> = Vec::new();
    let mut hello = false;
    let mut terminals = 0;
    let until = Instant::now() + budget;
    let _ = s.set_read_timeout(Some(Duration::from_millis(50)));
    while Instant::now() < until && terminals < 2 {
        let mut buf = [0u8; 4096];
        match (&*s).read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                pending.extend_from_slice(&buf[..n]);
                while let Some(i) = pending.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=i).collect();
                    let v: serde_json::Value = serde_json::from_slice(&line[..line.len() - 1])
                        .unwrap_or(serde_json::Value::Null);
                    match v["type"].as_str() {
                        Some("hello") => hello = true,
                        Some("progress") => {}
                        Some("response") | Some("error") => terminals += 1,
                        _ => {}
                    }
                }
            }
            Err(_) => {}
        }
    }
    if !hello {
        t.no_hello += 1
    } else if terminals == 0 {
        t.no_frame += 1
    } else {
        t.ok += 1
    }
}

#[test]
fn before_a1_a_backlogged_connection_at_shutdown_is_answered() {
    let _w = Watchdog::start(300);
    let mut t = Tally::default();
    for _ in 0..300 {
        let fx = start(Streamer);
        let s = UnixStream::connect(&fx.path).unwrap();
        let mut w = s.try_clone().unwrap();
        w.write_all(HELLO.as_bytes()).unwrap();
        w.write_all(b"\n").unwrap();
        w.write_all(REQUEST.as_bytes()).unwrap();
        w.write_all(b"\n").unwrap();
        fx.server().handle().shutdown();
        classify(&s, Duration::from_millis(200), &mut t);
    }
    eprintln!(
        "A1 TALLY no_hello={} no_frame={} ok={} other={}",
        t.no_hello, t.no_frame, t.ok, t.other
    );
    assert_eq!(t.no_hello, 0, "every queued client must be answered");
    assert_eq!(t.no_frame, 0, "every request must end with a frame");
}

#[test]
fn before_a2_a_legit_client_is_served_while_the_cap_is_full() {
    let _w = Watchdog::start(200);
    let opts = ServerOptions {
        max_connections: 8,
        ..ServerOptions::default()
    };
    let fx = start_with(Streamer, opts);
    let mut held: Vec<UnixStream> = Vec::new();
    for _ in 0..8 {
        let s = UnixStream::connect(&fx.path).unwrap();
        let mut w = s.try_clone().unwrap();
        w.write_all(HELLO.as_bytes()).unwrap();
        w.write_all(b"\n").unwrap();
        thread::sleep(Duration::from_millis(30));
        held.push(s);
    }
    let (mut served, mut refused) = (0, 0);
    let until = Instant::now() + Duration::from_secs(30);
    while Instant::now() < until {
        match Client::connect(&fx.path) {
            Ok(mut c) => {
                if c.call(Request::Ping(Empty {})).is_ok() {
                    served += 1;
                }
            }
            Err(_) => refused += 1,
        }
        thread::sleep(Duration::from_millis(250));
    }
    eprintln!("A2 served {served} refused {refused} while 8 idle connections were held");
    assert!(
        served > 0,
        "a legit client must be served within 30s (refused {refused})"
    );
}

#[test]
fn before_a3_a_denied_peer_always_receives_its_error_frame() {
    let _w = Watchdog::start(300);
    let opts = ServerOptions {
        peer_check: PeerCheck::new(|_| Ok(current_uid().wrapping_add(1))),
        ..ServerOptions::default()
    };
    let fx = start_with(Streamer, opts);
    let mut lost = 0;
    let mut write_failed = 0;
    for _ in 0..2000 {
        let mut c = UnixStream::connect(&fx.path).unwrap();
        let _ = c.set_read_timeout(Some(Duration::from_millis(500)));
        let wrote = c.write_all(format!("{HELLO}\n").as_bytes()).is_ok();
        if !wrote {
            write_failed += 1;
        }
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
                Err(_) => break,
            }
        }
        if !got {
            lost += 1
        }
    }
    eprintln!("A3 lost={lost} write_failed={write_failed} of 2000");
    assert_eq!(lost, 0, "a denied peer must always read its error frame");
}
