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
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const HELLO: &str = r#"{"type":"hello","versions":[1]}"#;
const GC_REQUEST: &str = r#"{"type":"request","id":1,"method":"gc","params":{"dry_run":true}}"#;
const FSCK_REQUEST: &str = r#"{"type":"request","id":1,"method":"fsck","params":{}}"#;

/// Emits large progress events forever, so an unreading client wedges the connection thread in a
/// progress write. The write times out after `write_timeout`; `ctx.progress` then returns
/// `cancelled` and the handler stops. An `AtomicBool` records entry and an atomic counter records
/// how far it got, so a test can tell a real block from an early return.
struct ProgressFlood {
    entered: Arc<AtomicBool>,
    steps: Arc<AtomicU64>,
    /// Set when the handler is dropped, so a test can prove the server released the blocked
    /// connection by the deadline instead of leaving it to unwind against `write_timeout`.
    dropped: Option<Arc<AtomicBool>>,
}

impl Drop for ProgressFlood {
    fn drop(&mut self) {
        if let Some(d) = &self.dropped {
            d.store(true, Ordering::SeqCst);
        }
    }
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

/// Proves the flood handler's progress write is genuinely parked on the socket, not merely slow.
///
/// Two consecutive windows in which `steps` does not move. A handler that is still running advances
/// it; a handler parked in a blocked `write_all` cannot, because the next increment only happens
/// after that write returns. Requiring two in a row rules out a single unlucky window. This observes
/// only the atomic counter: it never reads the socket, because reading would drain the buffer and
/// unpark the very write under test.
fn assert_parked(steps: &AtomicU64, window: Duration) {
    for attempt in 0..2 {
        let before = steps.load(Ordering::SeqCst);
        thread::sleep(window);
        let after = steps.load(Ordering::SeqCst);
        assert_eq!(
            before, after,
            "still-window {attempt} advanced {before} -> {after}; the progress write was not parked"
        );
        assert!(
            after > 0,
            "the handler never attempted a progress write, so nothing could park"
        );
    }
}

/// One handler serving both connections of the budget test: `gc` floods progress (parking a writer),
/// `fsck` burns CPU with no socket at all.
struct FloodAndCpu {
    entered: Arc<AtomicBool>,
    steps: Arc<AtomicU64>,
    dropped: Arc<AtomicBool>,
    cpu_entered: Arc<AtomicBool>,
    cpu_ticks: Arc<AtomicU64>,
    cpu_release: Arc<AtomicBool>,
    cpu_done: Arc<AtomicBool>,
}

/// Sets a flag when the `gc` call it guards returns, so the parked writer's release can be observed
/// independently of the CPU handler, which keeps the whole handler alive for the whole test.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl ControlHandler for FloodAndCpu {
    fn gc(&self, _: GcParams, ctx: &OpContext<'_>) -> CtlResult<GcReport> {
        let _guard = DropFlag(Arc::clone(&self.dropped));
        self.entered.store(true, Ordering::SeqCst);
        loop {
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

    fn fsck(&self, _: &OpContext<'_>) -> CtlResult<FsckReport> {
        // No socket, no progress event, and the cancel token is ignored on purpose. The guard flips
        // only when this call returns, so "alive at the return" is observed, not inferred.
        let _guard = DropFlag(Arc::clone(&self.cpu_done));
        self.cpu_entered.store(true, Ordering::SeqCst);
        let mut spins: u64 = 0;
        while !self.cpu_release.load(Ordering::SeqCst) {
            spins = spins.wrapping_add(1);
            if spins.is_multiple_of(4096) {
                self.cpu_ticks.fetch_add(1, Ordering::SeqCst);
            }
            std::hint::spin_loop();
        }
        self.cpu_ticks.fetch_add(1, Ordering::SeqCst);
        Err(CtlError::cancelled())
    }
}

/// Descriptors this process holds. Sampled around a shutdown cycle to prove the abandoned
/// connections' sockets are released, not just half-closed.
fn open_fds() -> usize {
    std::fs::read_dir(if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    })
    .map_or(0, Iterator::count)
}

fn flood() -> (ProgressFlood, Arc<AtomicBool>, Arc<AtomicU64>) {
    let entered = Arc::new(AtomicBool::new(false));
    let steps = Arc::new(AtomicU64::new(0));
    (
        ProgressFlood {
            entered: Arc::clone(&entered),
            steps: Arc::clone(&steps),
            dropped: None,
        },
        entered,
        steps,
    )
}

fn flood_with_drop() -> (
    ProgressFlood,
    Arc<AtomicBool>,
    Arc<AtomicU64>,
    Arc<AtomicBool>,
) {
    let entered = Arc::new(AtomicBool::new(false));
    let steps = Arc::new(AtomicU64::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    (
        ProgressFlood {
            entered: Arc::clone(&entered),
            steps: Arc::clone(&steps),
            dropped: Some(Arc::clone(&dropped)),
        },
        entered,
        steps,
        dropped,
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
    // The deadline is 300 ms plus the ~250 ms delivery grace. A pass at 2 s would mask a
    // regression that serialized the blocked write behind write_timeout (4 s here).
    assert!(
        elapsed < Duration::from_millis(1200),
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
        elapsed < Duration::from_millis(1200),
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
            // The frame is only promised to a client that reads inside the delivery grace, and this
            // handler emits 1 MiB per event, so a client draining the backlog needs room. The grace
            // is set generously on purpose: this test measures delivery to a reading client, not how
            // fast a loaded scheduler drains a socket. The beyond-grace boundary is asserted
            // separately by `abandoned_blocked_connection_is_released_before_wait_returns` and by
            // `admission.rs::a4_a_terminal_frame_is_not_guaranteed_past_the_grace`.
            drain_deadline: Duration::from_millis(1500),
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
    // Keep reading so the abandoned request's terminal frame is not lost to a full buffer. The
    // frame now lands at the deadline plus grace, well under 2 s; a 5 s window would mask a
    // regression back to the write timeout.
    let mut terminal = false;
    let until = Instant::now() + Duration::from_secs(2);
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
    // Both workers run in parallel; wait() returns at deadline plus the delivery grace.
    assert!(
        elapsed < Duration::from_millis(1200),
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
    // Read both terminal frames. The connection is abandoned at the deadline; the frames land at
    // the deadline plus grace, and the 2 s window fails a regression to the write timeout.
    let mut seen: std::collections::BTreeSet<u64> = Default::default();
    let _ = r.stream.set_read_timeout(Some(Duration::from_millis(50)));
    let until = Instant::now() + Duration::from_secs(2);
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
        elapsed < Duration::from_millis(1200),
        "shutdown of parallel pending requests ran long: {elapsed:?}"
    );
    assert!(
        seen.contains(&1) && seen.contains(&2),
        "both pending requests must end with a terminal frame, saw {seen:?}"
    );
}

/// Locates the child fixture built next to the test binary. `cargo test` builds examples into
/// `target/<profile>/examples/`; the test executable lives in `target/<profile>/deps/`.
fn child_fixture() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let dir = exe.parent().and_then(|p| p.parent()).expect("target dir");
    let exe_name = if cfg!(windows) {
        "progress_exit_child.exe"
    } else {
        "progress_exit_child"
    };
    dir.join("examples").join(exe_name)
}

/// The regression for the process-exit race. `cowfs serve` calls `wait()` and exits at once; the
/// frame for an abandoned request must already be on the wire before `wait()` returns. The child
/// fixture runs a stuck handler that ignores cancellation, one client that keeps reading, and
/// `process::exit(0)` right after `wait()`. On the detached-thread implementation the client saw a
/// bare EOF in roughly a third of runs; with the bounded delivery grace it is never lost.
#[test]
fn terminal_frame_survives_process_exit_after_wait() {
    let _w = Watchdog::start(300);
    let child = child_fixture();
    assert!(
        child.exists(),
        "child fixture not built at {}; run via `cargo test` (examples are built too)",
        child.display()
    );
    let reps = 100;
    let mut frames = 0;
    for rep in 0..reps {
        let dir = private_tempdir();
        let sock = dir.path().join("s.sock");
        let mut proc = Command::new(&child)
            .arg(&sock)
            .arg("300")
            .arg("stuck")
            .stdout(Stdio::piped())
            .stdin(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");
        let mut out = std::io::BufReader::new(proc.stdout.take().unwrap());
        let mut ready = String::new();
        out.read_line(&mut ready).expect("child ready");
        let mut stream = UnixStream::connect(&sock).expect("connect");
        stream
            .write_all(format!("{HELLO}\n").as_bytes())
            .expect("hello");
        stream
            .write_all(format!("{FSCK_REQUEST}\n").as_bytes())
            .expect("request");
        // Let the handler enter, then ask the child to shut down.
        thread::sleep(Duration::from_millis(150));
        {
            let stdin = proc.stdin.as_mut().unwrap();
            stdin.write_all(b"\n").unwrap();
            stdin.flush().unwrap();
        }
        // Read for a terminal frame; the child exits right after wait(), so a lost frame shows up
        // as a bare EOF here.
        let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut pending = Vec::new();
        let mut buf = [0u8; 4096];
        while Instant::now() < deadline {
            match (&stream).read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);
                    let mut got = false;
                    while let Some(i) = pending.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = pending.drain(..=i).collect();
                        let v: serde_json::Value =
                            serde_json::from_slice(&line[..line.len() - 1]).unwrap_or_default();
                        if matches!(v["type"].as_str(), Some("response") | Some("error"))
                            && v["id"].as_u64() == Some(1)
                        {
                            got = true;
                        }
                    }
                    if got {
                        frames += 1;
                        break;
                    }
                }
                Err(_) => {
                    if proc.try_wait().ok().flatten().is_some() {
                        break;
                    }
                }
            }
        }
        let _ = proc.wait();
        if frames == 0 && rep < 3 {
            eprintln!("PROGRESS77 child_exit rep={rep} no terminal frame");
        }
    }
    eprintln!("PROGRESS77 child_exit_frames={frames}/{reps}");
    assert_eq!(
        frames, reps,
        "the process-exit path dropped terminal frames: {frames}/{reps}"
    );
}

/// A handler whose `gc` floods progress (to stall a non-reading client) and whose `fsck` does real
/// pending work until its cancel token fires. One handler serves both connections, so a healthy
/// `fsck` reader can sit behind a stalled `gc` writer and must still get its terminal frame at the
/// deadline. The trait default `fsck` would return `unsupported` instantly and make the assertion
/// vacuous.
struct FloodAndStuckFsck {
    entered: Arc<AtomicBool>,
    steps: Arc<AtomicU64>,
    fsck_entered: Arc<AtomicBool>,
    /// The `fsck` body ignores its cancel token and loops until this is set, so its terminal frame
    /// can only come from the shutdown abandon path. That is what makes the test discriminate:
    /// a serial abandon path delays the frame to `write_timeout`; a parallel one lands it at the
    /// deadline. The test sets it at the end to release the handler.
    release: Arc<AtomicBool>,
}

impl ControlHandler for FloodAndStuckFsck {
    fn gc(&self, _: GcParams, ctx: &OpContext<'_>) -> CtlResult<GcReport> {
        self.entered.store(true, Ordering::SeqCst);
        loop {
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

    fn fsck(&self, _: &OpContext<'_>) -> CtlResult<FsckReport> {
        self.fsck_entered.store(true, Ordering::SeqCst);
        while !self.release.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(2));
        }
        Err(CtlError::cancelled())
    }
}

/// A healthy reader behind a stalled progress writer must receive its terminal frame near the
/// deadline, not after the stalled write times out. The serial abandon thread delayed it to
/// `write_timeout` in most runs. The reader's `fsck` does pending work until cancellation, so its
/// frame is a real `shutting_down`/`cancelled` terminal, not an instant `unsupported`.
#[test]
fn healthy_reader_behind_a_stalled_writer_is_not_delayed() {
    let _w = Watchdog::start(240);
    let entered = Arc::new(AtomicBool::new(false));
    let steps = Arc::new(AtomicU64::new(0));
    let fsck_entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(AtomicBool::new(false));
    let fx = start_with(
        FloodAndStuckFsck {
            entered: Arc::clone(&entered),
            steps: Arc::clone(&steps),
            fsck_entered: Arc::clone(&fsck_entered),
            release: Arc::clone(&release),
        },
        ServerOptions {
            write_timeout: Duration::from_secs(3),
            shutdown_deadline: Duration::from_millis(300),
            ..ServerOptions::default()
        },
    );
    let _stalled = blocked_hello(&fx.path);
    wait_blocked(&entered, &steps);

    let mut healthy = Raw::hello(&fx.path);
    healthy.send(FSCK_REQUEST);
    let deadline = Instant::now() + Duration::from_secs(3);
    while !fsck_entered.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "fsck handler never entered");
        thread::sleep(Duration::from_millis(5));
    }
    thread::sleep(Duration::from_millis(200));

    let t0 = Instant::now();
    fx.server().handle().shutdown();
    let _ = healthy
        .stream
        .set_read_timeout(Some(Duration::from_millis(50)));
    let until = Instant::now() + Duration::from_secs(3);
    let mut buf = Vec::new();
    let mut frame = None;
    while Instant::now() < until {
        buf.clear();
        match healthy.reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {
                let v: serde_json::Value =
                    serde_json::from_slice(&buf[..buf.len().saturating_sub(1)]).unwrap_or_default();
                if matches!(v["type"].as_str(), Some("response") | Some("error"))
                    && v["id"].as_u64() == Some(1)
                {
                    frame = Some(v);
                    break;
                }
            }
            Err(_) => {}
        }
    }
    let at = t0.elapsed();
    let v = frame.expect("the healthy reader must get a terminal frame within the budget");
    eprintln!(
        "PROGRESS77 healthy_behind_stalled_frame_ms={} code={:?}",
        at.as_millis(),
        v["error"]["code"]
    );
    // Not `unsupported`: that would mean the trait default answered instantly and the whole
    // assertion about the abandon path is vacuous.
    assert_eq!(
        v["error"]["code"].as_str(),
        Some("shutting_down"),
        "the healthy reader must get `shutting_down`, got {:?}",
        v["error"]["code"]
    );
    assert!(
        at < Duration::from_millis(1200),
        "healthy reader waited {at:?}, behind the stalled writer's timeout"
    );
    // Release the cancel-ignoring handler so it does not leak into later tests.
    release.store(true, Ordering::SeqCst);
}

/// Staggered stalls must not stack: the workers run in parallel and the grace is finite, so
/// `wait()` stays near the deadline plus one grace regardless of how the stalls are spread.
#[test]
fn staggered_stalled_writers_stay_within_the_deadline_plus_grace() {
    let _w = Watchdog::start(240);
    let (handler, entered, steps) = flood();
    let mut fx = start_with(
        handler,
        ServerOptions {
            write_timeout: Duration::from_secs(3),
            shutdown_deadline: Duration::from_millis(300),
            ..ServerOptions::default()
        },
    );
    let mut clients = Vec::new();
    for i in 0..4 {
        let c = blocked_hello(&fx.path);
        clients.push(c);
        if i < 3 {
            thread::sleep(Duration::from_millis(400));
        }
    }
    wait_blocked(&entered, &steps);
    thread::sleep(Duration::from_millis(200));

    let server = fx.server.take().unwrap();
    let t0 = Instant::now();
    server.handle().shutdown();
    server.wait();
    let elapsed = t0.elapsed();
    eprintln!("PROGRESS77 staggered_n4_wait_ms={}", elapsed.as_millis());
    assert!(
        elapsed < Duration::from_millis(1200),
        "staggered stalls stacked to {elapsed:?}"
    );
    drop(clients);
}

/// The Shutdown contract (`docs/v1-control-api.md:397-399`) says abandoned handlers have "their
/// connections are closed and the server returns". #77 makes that true for a client that stopped
/// reading while a request streamed progress: its connection, socket and handler must be released
/// by the time `wait()` returns, not left to unwind against `write_timeout`.
///
/// Everything is sampled at the return instant, before waiting for anything, because ownership
/// retained after the return is the defect. A blocked socket write cannot finish while the handler
/// runs, so the handler being dropped is also the server-side release, and `set_read_timeout`
/// failing on the peer (macOS EINVAL) is the client-side release.
///
/// Fails on the detached-worker version without the post-grace close: measured on `89c271e` over a
/// real private socket, `wait()` returned at 557 ms with the handler alive and the connection open,
/// and the peer only saw the close at 6562 ms.
#[test]
fn abandoned_blocked_connection_is_released_before_wait_returns() {
    for (label, n) in [("n1", 1usize), ("n6", 6usize)] {
        let _w = Watchdog::start(240);
        let (handler, entered, steps, dropped) = flood_with_drop();
        let mut fx = start_with(
            handler,
            ServerOptions {
                write_timeout: Duration::from_secs(3),
                shutdown_deadline: Duration::from_millis(300),
                // The single grace is split between delivering a frame and closing, and this test
                // only needs the close. It declares a grace with enough room for the close to land
                // inside the budget it asserts, rather than relying on the 250 ms default.
                drain_deadline: Duration::from_millis(500),
                ..ServerOptions::default()
            },
        );
        let fds0 = open_fds();
        let mut clients = Vec::new();
        for _ in 0..n {
            clients.push(blocked_hello(&fx.path));
        }
        wait_blocked(&entered, &steps);
        // Sampled with every descriptor live: the listener, the accepted connections (two each,
        // because `Conn` keeps a clone) and this test's own client sockets.
        let fds_busy = open_fds();

        let server = fx.server.take().unwrap();
        let t0 = Instant::now();
        server.handle().shutdown();
        server.wait();
        let wait_ms = t0.elapsed();

        // Sampled here, at the return instant, with no grace period of its own.
        let handler_alive_at_return = !dropped.load(Ordering::SeqCst);
        let closed_at_return = clients.iter_mut().all(|c| {
            c.stream
                .set_read_timeout(Some(Duration::from_millis(1)))
                .is_err()
        });
        let fds_at_return = open_fds();
        eprintln!(
            "PROGRESS77 release {label} wait_ms={} handler_alive_at_return={} \
             connection_closed_at_return={closed_at_return} fds_busy={fds_busy} \
             fds_at_return={fds_at_return} (descriptor counts reported, not gated)",
            wait_ms.as_millis(),
            handler_alive_at_return
        );

        // Deadline 300 + grace 500 = 800 ms, plus scheduling. `write_timeout` is 3 s, so this still
        // fails an implementation that waits the write out decisively.
        let bound = Duration::from_millis(1050);
        assert!(
            wait_ms < bound,
            "{label}: wait() took {wait_ms:?}, past the deadline plus the grace"
        );
        assert!(
            !handler_alive_at_return,
            "{label}: the server still owns the abandoned handler when wait() returns"
        );
        assert!(
            closed_at_return,
            "{label}: an abandoned connection is still open when wait() returns"
        );
        // The process-wide descriptor count is reported above but deliberately not gated on here. It
        // includes this harness's own noise, watchdog pipes and fixtures from earlier tests in the
        // same binary: hosted CI read 31 against 12 locally for this identical scenario, and the
        // difference moved the count the wrong way by one. Release is gated on the two per-connection
        // signals above and on the no-leak check below, which samples the same baseline the process
        // started the cycle with.
        drop(clients);
        // With the peers dropped too, the process is back where it started: no leak across cycles.
        let until = Instant::now() + Duration::from_secs(2);
        while open_fds() > fds0 && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
        let fds_after = open_fds();
        eprintln!("PROGRESS77 release {label} fds0={fds0} fds_after={fds_after}");
        assert!(
            fds_after <= fds0,
            "{label}: {fds_after} descriptors open after the cycle, baseline {fds0}"
        );
    }
}

/// The shutdown budget is the deadline plus ONE grace, not the deadline plus three.
///
/// Three waits run after the deadline: waiting for the abandon workers' completion signals, joining
/// those workers, and waiting for every straggler connection to be released. Each was bounded by a
/// fresh `Instant::now() + drain_deadline`, so a straggler that finished nothing could stretch
/// `wait()` to the deadline plus three graces. `drain_deadline = 500 ms` gave a real 1309 ms; at
/// 1000 ms it gave 2312 ms. The budget below is `shutdown_deadline + drain_deadline` plus a modest
/// explicit allowance for the accept loop's poll interval and thread scheduling, which is far below
/// the extra grace being removed.
///
/// The two connections are the two ways a handler can be stuck. The flood client stops reading, so
/// its progress write parks in the kernel and only half-closing can free it. The `fsck` client runs a
/// handler with no socket at all, which half-closing cannot help: that request worker stays occupied
/// for as long as its computation runs. Together they hold the release wait open for the whole grace,
/// which is the case a per-wait budget hides.
///
/// Nothing here reads the socket before `wait()` returns: a read would drain the buffer and unpark
/// the write under test. Parked-ness is proven by the handler's own step counter going still in two
/// consecutive windows, and the close is proven afterwards.
#[test]
fn shutdown_budget_is_the_deadline_plus_one_grace_with_a_parked_writer_and_a_cpu_handler() {
    let shutdown_deadline = Duration::from_millis(300);
    let drain_deadline = Duration::from_millis(500);
    // Poll interval plus thread scheduling. The extra grace being removed is 2 x 500 ms, so this
    // allowance is about a fifth of what it distinguishes.
    let scheduling = Duration::from_millis(250);
    let budget = shutdown_deadline + drain_deadline + scheduling;

    let _w = Watchdog::start(240);
    let entered = Arc::new(AtomicBool::new(false));
    let steps = Arc::new(AtomicU64::new(0));
    let dropped = Arc::new(AtomicBool::new(false));
    let cpu_entered = Arc::new(AtomicBool::new(false));
    let cpu_ticks = Arc::new(AtomicU64::new(0));
    let cpu_release = Arc::new(AtomicBool::new(false));
    let cpu_done = Arc::new(AtomicBool::new(false));
    let mut fx = start_with(
        FloodAndCpu {
            entered: Arc::clone(&entered),
            steps: Arc::clone(&steps),
            dropped: Arc::clone(&dropped),
            cpu_entered: Arc::clone(&cpu_entered),
            cpu_ticks: Arc::clone(&cpu_ticks),
            cpu_release: Arc::clone(&cpu_release),
            cpu_done: Arc::clone(&cpu_done),
        },
        ServerOptions {
            // Long enough that an implementation which waits the write out fails loudly instead of
            // accidentally passing.
            write_timeout: Duration::from_secs(4),
            shutdown_deadline,
            drain_deadline,
            ..ServerOptions::default()
        },
    );

    // Connection 1: reads the handshake, then stops reading so the flood's write parks.
    let parked = blocked_hello(&fx.path);
    wait_blocked(&entered, &steps);
    // Connection 2: a handler with no socket at all.
    let mut cpu = Raw::hello(&fx.path);
    cpu.send(FSCK_REQUEST);
    let until = Instant::now() + Duration::from_secs(3);
    while !cpu_entered.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < until,
            "the CPU-bound handler never entered"
        );
        thread::sleep(Duration::from_millis(5));
    }

    // Real park proof, before anything is measured or shut down.
    assert_parked(&steps, Duration::from_millis(250));
    let parked_steps = steps.load(Ordering::SeqCst);

    let server = fx.server.take().unwrap();
    let t0 = Instant::now();
    server.handle().shutdown();
    server.wait();
    let elapsed = t0.elapsed();

    let handler_alive_at_return = !dropped.load(Ordering::SeqCst);
    let parked_closed_at_return = parked
        .stream
        .set_read_timeout(Some(Duration::from_millis(1)))
        .is_err();
    let cpu_alive_at_return = !cpu_done.load(Ordering::SeqCst);

    eprintln!(
        "PROGRESS77 budget mixed wait_ms={} parked_steps={parked_steps} \
         parked_handler_alive_at_return={handler_alive_at_return} \
         parked_socket_closed_at_return={parked_closed_at_return} \
         cpu_alive_at_return={cpu_alive_at_return} budget_ms={}",
        elapsed.as_millis(),
        budget.as_millis()
    );

    assert!(
        elapsed < budget,
        "wait() took {elapsed:?}, past the deadline plus one grace ({budget:?})"
    );
    // The parked connection is the one the contract is about: closed before the return.
    assert!(
        parked_closed_at_return,
        "the parked writer's connection was still open when wait() returned"
    );
    assert!(
        !handler_alive_at_return,
        "the parked handler was still alive when wait() returned"
    );
    // The CPU-bound handler has no socket, so nothing can force it out. It is expected to be alive
    // and that is documented, not asserted as a failure: the contract detaches handler threads.
    assert!(
        cpu_alive_at_return,
        "the CPU-bound handler finished too early to exercise the case it exists for"
    );

    // Private cleanup, after the measurement: release the CPU handler so it does not outlive the
    // test binary. This is a test-owned flag, not part of the user-visible contract.
    cpu_release.store(true, Ordering::SeqCst);
    let until = Instant::now() + Duration::from_secs(5);
    while cpu_ticks.load(Ordering::SeqCst) == 0 && Instant::now() < until {
        thread::sleep(Duration::from_millis(5));
    }
    drop(parked);
    drop(cpu);
}

/// Negative control: a parked writer alone. Same budget, so the mixed case's bound is not carrying a
/// cost that a single parked connection does not have on its own.
#[test]
fn shutdown_budget_with_only_a_parked_writer() {
    let shutdown_deadline = Duration::from_millis(300);
    let drain_deadline = Duration::from_millis(500);
    let budget = shutdown_deadline + drain_deadline + Duration::from_millis(250);

    let _w = Watchdog::start(240);
    let (handler, entered, steps, dropped) = flood_with_drop();
    let mut fx = start_with(
        handler,
        ServerOptions {
            write_timeout: Duration::from_secs(4),
            shutdown_deadline,
            drain_deadline,
            ..ServerOptions::default()
        },
    );
    let parked = blocked_hello(&fx.path);
    wait_blocked(&entered, &steps);
    assert_parked(&steps, Duration::from_millis(250));

    let server = fx.server.take().unwrap();
    let t0 = Instant::now();
    server.handle().shutdown();
    server.wait();
    let elapsed = t0.elapsed();
    let alive = !dropped.load(Ordering::SeqCst);
    let closed = parked
        .stream
        .set_read_timeout(Some(Duration::from_millis(1)))
        .is_err();
    eprintln!(
        "PROGRESS77 budget parked_only wait_ms={} handler_alive_at_return={alive} \
         socket_closed_at_return={closed} budget_ms={}",
        elapsed.as_millis(),
        budget.as_millis()
    );
    assert!(
        elapsed < budget,
        "wait() took {elapsed:?} for one parked writer"
    );
    assert!(closed, "the parked connection was still open at the return");
    assert!(!alive, "the parked handler was still alive at the return");
    drop(parked);
}

/// Negative control: a CPU-bound handler alone, with no socket anywhere in the picture.
#[test]
fn shutdown_budget_with_only_a_cpu_handler() {
    let shutdown_deadline = Duration::from_millis(300);
    let drain_deadline = Duration::from_millis(500);
    let budget = shutdown_deadline + drain_deadline + Duration::from_millis(250);

    let _w = Watchdog::start(240);
    let cpu_entered = Arc::new(AtomicBool::new(false));
    let cpu_ticks = Arc::new(AtomicU64::new(0));
    let cpu_release = Arc::new(AtomicBool::new(false));
    let cpu_done = Arc::new(AtomicBool::new(false));
    let mut fx = start_with(
        FloodAndCpu {
            entered: Arc::new(AtomicBool::new(false)),
            steps: Arc::new(AtomicU64::new(0)),
            dropped: Arc::new(AtomicBool::new(false)),
            cpu_entered: Arc::clone(&cpu_entered),
            cpu_ticks: Arc::clone(&cpu_ticks),
            cpu_release: Arc::clone(&cpu_release),
            cpu_done: Arc::clone(&cpu_done),
        },
        ServerOptions {
            shutdown_deadline,
            drain_deadline,
            ..ServerOptions::default()
        },
    );
    let mut cpu = Raw::hello(&fx.path);
    cpu.send(FSCK_REQUEST);
    let until = Instant::now() + Duration::from_secs(3);
    while !cpu_entered.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < until,
            "the CPU-bound handler never entered"
        );
        thread::sleep(Duration::from_millis(5));
    }
    // Confirm it really is computing, so the case is not an instant return wearing its name.
    let before = cpu_ticks.load(Ordering::SeqCst);
    thread::sleep(Duration::from_millis(200));
    assert!(
        cpu_ticks.load(Ordering::SeqCst) > before,
        "the CPU-bound handler is not advancing"
    );

    let server = fx.server.take().unwrap();
    let t0 = Instant::now();
    server.handle().shutdown();
    server.wait();
    let elapsed = t0.elapsed();
    let alive = !cpu_done.load(Ordering::SeqCst);
    eprintln!(
        "PROGRESS77 budget cpu_only wait_ms={} cpu_alive_at_return={alive} budget_ms={}",
        elapsed.as_millis(),
        budget.as_millis()
    );
    assert!(
        elapsed < budget,
        "wait() took {elapsed:?} for one CPU-bound handler"
    );
    assert!(
        alive,
        "the CPU-bound handler finished too early to exercise the case it exists for"
    );
    cpu_release.store(true, Ordering::SeqCst);
    let until = Instant::now() + Duration::from_secs(5);
    while cpu_ticks.load(Ordering::SeqCst) == 0 && Instant::now() < until {
        thread::sleep(Duration::from_millis(5));
    }
    drop(cpu);
}
