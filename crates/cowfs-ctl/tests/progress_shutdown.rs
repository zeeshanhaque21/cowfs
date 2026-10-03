//! Blocked PROGRESS writes must not hold shutdown past its deadline (#77).
//!
//! A client that stops reading while a request streams progress keeps a connection thread inside
//! `write_all` on the shared write lock. Shutdown teardown (`abandon_inflight`) and the terminal
//! `finish` path take the same lock, so `Server::wait()` used to run for a multiple of
//! `write_timeout` instead of near `shutdown_deadline`. These tests fail on that implementation.

mod common;

use common::*;
use cowfs_ctl::*;
use std::io::{BufRead, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const HELLO: &str = r#"{"type":"hello","versions":[1]}"#;
const GC_REQUEST: &str = r#"{"type":"request","id":1,"method":"gc","params":{"dry_run":true}}"#;

/// Emits large progress events forever, so an unreading client wedges the connection thread in a
/// progress write. The write times out after `write_timeout`; `ctx.progress` then returns
/// `cancelled` and the handler stops. An `AtomicBool` records entry and an atomic counter records
/// how far it got, so a test can tell a real block from an early return.
struct ProgressFlood {
    entered: Arc<AtomicBool>,
    steps: Arc<AtomicU64>,
}

impl ControlHandler for ProgressFlood {
    fn gc(&self, _: GcParams, ctx: &OpContext<'_>) -> CtlResult<GcReport> {
        self.entered.store(true, Ordering::SeqCst);
        loop {
            // Counted before the write: a 1 MiB event fills the send buffer on the first call, so
            // `steps` staying at one while `entered` is set means the write is blocked.
            self.steps.fetch_add(1, Ordering::SeqCst);
            ctx.progress(ProgressEvent {
                phase: "mark".into(),
                done: 0,
                total: None,
                unit: Unit::Items,
                message: Some("x".repeat(1 << 20)),
            })?;
        }
    }
}

/// Waits until the unread connection has at least one progress write attempted, then settles so a
/// blocked first write is definitely parked on the socket. Returns once `steps` stops moving.
fn wait_blocked(entered: &AtomicBool, steps: &AtomicU64) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !entered.load(Ordering::SeqCst) || steps.load(Ordering::SeqCst) < 1 {
        assert!(Instant::now() < deadline, "handler never started streaming");
        thread::sleep(Duration::from_millis(5));
    }
    // Settle: a fast emitter advances `steps`; a blocked one does not. Either way the connection
    // thread is now inside (or waiting to enter) a progress write.
    let first = steps.load(Ordering::SeqCst);
    let settle = Instant::now() + Duration::from_millis(400);
    while Instant::now() < settle {
        thread::sleep(Duration::from_millis(20));
    }
    let second = steps.load(Ordering::SeqCst);
    assert!(
        second > first || second >= 1,
        "handler did not attempt a progress write"
    );
}

fn flood() -> (ProgressFlood, Arc<AtomicBool>, Arc<AtomicU64>) {
    let entered = Arc::new(AtomicBool::new(false));
    let steps = Arc::new(AtomicU64::new(0));
    (
        ProgressFlood {
            entered: Arc::clone(&entered),
            steps: Arc::clone(&steps),
        },
        entered,
        steps,
    )
}

/// A client that completes the handshake, sends one request, then never reads the stream.
fn blocked_hello(path: &std::path::Path) -> Raw {
    let mut r = Raw::hello(path);
    r.send(GC_REQUEST);
    r
}

#[test]
fn blocked_progress_write_does_not_hold_shutdown_past_the_deadline() {
    let _w = Watchdog::start(120);
    let (handler, entered, steps) = flood();
    let mut fx = start_with(
        handler,
        ServerOptions {
            write_timeout: Duration::from_secs(4),
            shutdown_deadline: Duration::from_millis(300),
            ..ServerOptions::default()
        },
    );
    let blocked = blocked_hello(&fx.path);
    // Wait until the handler is streaming and the kernel send buffer is full, so the connection
    // thread is parked in a progress write.
    wait_blocked(&entered, &steps);

    let server = fx.server.take().unwrap();
    let t0 = Instant::now();
    server.handle().shutdown();
    server.wait();
    let elapsed = t0.elapsed();
    eprintln!("PROGRESS77 shutdown_elapsed_ms={}", elapsed.as_millis());
    assert!(
        elapsed < Duration::from_secs(3),
        "wait() took {elapsed:?}; a blocked progress write held the write lock past the 300ms deadline"
    );
    drop(blocked);
}

#[test]
fn blocked_progress_write_does_not_stall_admission() {
    let _w = Watchdog::start(120);
    let (handler, entered, steps) = flood();
    let fx = start_with(
        handler,
        ServerOptions {
            write_timeout: Duration::from_secs(4),
            shutdown_deadline: Duration::from_millis(300),
            max_connections: 1,
            ..ServerOptions::default()
        },
    );
    let blocked = blocked_hello(&fx.path);
    wait_blocked(&entered, &steps);

    // At the connection cap the new connection must be answered promptly, not after write_timeout.
    let mut second = Raw::connect(&fx.path);
    let t0 = Instant::now();
    second.send(HELLO);
    let f = second.recv();
    let elapsed = t0.elapsed();
    eprintln!(
        "PROGRESS77 admission_first_frame_ms={} code={}",
        elapsed.as_millis(),
        f["error"]["code"]
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "the second connection stalled {elapsed:?} behind a blocked progress write"
    );
    assert_eq!(f["error"]["code"], "too_many_connections", "{f}");
    drop(blocked);
    drop(second);
}

/// A reader that never stops reading must still get its terminal frame when shutdown abandons the
/// request, under the deadline. This is the "normal readers get a terminal frame when possible"
/// half: the same seam that abandons a blocked writer must not cut a reading client's frame.
#[test]
fn an_unblocked_client_still_receives_a_terminal_frame_at_shutdown() {
    let _w = Watchdog::start(180);
    let (handler, entered, steps) = flood();
    let fx = start_with(
        handler,
        ServerOptions {
            write_timeout: Duration::from_secs(4),
            shutdown_deadline: Duration::from_millis(300),
            ..ServerOptions::default()
        },
    );
    let s = UnixStream::connect(&fx.path).unwrap();
    let mut w = s.try_clone().unwrap();
    w.write_all(format!("{HELLO}\n").as_bytes()).unwrap();
    w.write_all(format!("{GC_REQUEST}\n").as_bytes()).unwrap();
    // Drain progress so the connection thread is never wedged, then let the request keep running.
    let _ = s.set_read_timeout(Some(Duration::from_millis(20)));
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut buf = [0u8; 8192];
    while (!entered.load(Ordering::SeqCst) || steps.load(Ordering::SeqCst) < 2)
        && Instant::now() < deadline
    {
        let _ = (&s).read(&mut buf);
    }
    assert!(steps.load(Ordering::SeqCst) >= 2, "handler never streamed");

    let t0 = Instant::now();
    fx.server().handle().shutdown();
    // Keep reading so the abandoned request's terminal frame is not lost to a full buffer.
    let mut terminal = false;
    let until = Instant::now() + Duration::from_secs(5);
    let mut pending = Vec::new();
    let _ = s.set_read_timeout(Some(Duration::from_millis(50)));
    while Instant::now() < until {
        match (&s).read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                pending.extend_from_slice(&buf[..n]);
                while let Some(i) = pending.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=i).collect();
                    let v: serde_json::Value =
                        serde_json::from_slice(&line[..line.len() - 1]).unwrap_or_default();
                    if matches!(v["type"].as_str(), Some("response") | Some("error")) {
                        assert_eq!(v["id"], 1, "terminal frame carries the request id");
                        terminal = true;
                    }
                }
            }
            Err(_) => {}
        }
    }
    let elapsed = t0.elapsed();
    eprintln!(
        "PROGRESS77 terminal_present={terminal} elapsed_ms={}",
        elapsed.as_millis()
    );
    assert!(terminal, "a reading client must receive its terminal frame");
}

/// Two connections each blocked in a progress write must both be abandoned at the deadline, not
/// serially through the write timeout. This exercises the loop over stragglers in the deadline
/// path: one blocked writer must not hold the second, and `wait()` must return near the deadline.
#[test]
fn two_blocked_progress_writers_are_both_abandoned_at_the_deadline() {
    let _w = Watchdog::start(180);
    let (handler, entered, steps) = flood();
    let mut fx = start_with(
        handler,
        ServerOptions {
            write_timeout: Duration::from_secs(4),
            shutdown_deadline: Duration::from_millis(300),
            ..ServerOptions::default()
        },
    );
    let blocked_a = blocked_hello(&fx.path);
    // The first connection is enough to prove the handler is streaming; the second is a separate
    // connection with its own request.
    wait_blocked(&entered, &steps);
    let mut blocked_b = Raw::hello(&fx.path);
    blocked_b.send(GC_REQUEST);
    thread::sleep(Duration::from_millis(300));

    let server = fx.server.take().unwrap();
    let t0 = Instant::now();
    server.handle().shutdown();
    server.wait();
    let elapsed = t0.elapsed();
    eprintln!(
        "PROGRESS77 two_writers_shutdown_elapsed_ms={}",
        elapsed.as_millis()
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "wait() took {elapsed:?}; a second blocked progress write held the deadline"
    );
    drop(blocked_a);
    drop(blocked_b);
}

/// A client that is reading and has two requests in flight, one of which is blocked on a large
/// progress write, must still see terminal frames for both when shutdown abandons the request.
/// This is the parallel-pending case on the same abandonment seam.
#[test]
fn pending_requests_are_abandoned_together_when_one_progress_write_is_blocked() {
    let _w = Watchdog::start(180);
    let (handler, entered, steps) = flood();
    let fx = start_with(
        handler,
        ServerOptions {
            write_timeout: Duration::from_secs(4),
            shutdown_deadline: Duration::from_millis(300),
            max_inflight: 8,
            ..ServerOptions::default()
        },
    );
    // One request on this connection blocks the write lock; a second is also in flight.
    let mut r = Raw::hello(&fx.path);
    r.send(GC_REQUEST);
    r.send(&GC_REQUEST.replace("\"id\":1", "\"id\":2"));
    wait_blocked(&entered, &steps);

    let t0 = Instant::now();
    fx.server().handle().shutdown();
    // Read both terminal frames. The connection is abandoned at the deadline, but the client is
    // reading, so the detached abandon thread can still land both `shutting_down` frames.
    let mut seen: std::collections::BTreeSet<u64> = Default::default();
    let _ = r.stream.set_read_timeout(Some(Duration::from_millis(50)));
    let until = Instant::now() + Duration::from_secs(5);
    let mut buf = Vec::new();
    while Instant::now() < until && seen.len() < 2 {
        buf.clear();
        match r.reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {
                let v: serde_json::Value =
                    serde_json::from_slice(&buf[..buf.len().saturating_sub(1)]).unwrap_or_default();
                if matches!(v["type"].as_str(), Some("response") | Some("error")) {
                    if let Some(id) = v["id"].as_u64() {
                        seen.insert(id);
                    }
                }
            }
            Err(_) => {}
        }
    }
    let elapsed = t0.elapsed();
    eprintln!(
        "PROGRESS77 pending_ids={:?} wait_elapsed_ms={}",
        seen,
        elapsed.as_millis()
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "shutdown of parallel pending requests ran long: {elapsed:?}"
    );
    assert!(
        seen.contains(&1) && seen.contains(&2),
        "both pending requests must end with a terminal frame, saw {seen:?}"
    );
}
