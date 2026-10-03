//! A process-level watchdog for a resource-sensitive fixture (issue 83).
//!
//! The in-process `run_bounded` watchdog can only set a stop flag: a collector that parks forever
//! inside a blocking call is joined by `std::thread::scope`, so the test cannot end on its own
//! (reproduced: an externally killed 40 s alarm, `rc=142`, despite a 9 s cooperative cap). The
//! only bound that survives a permanently blocked collector is an outer process: this module runs
//! the fixture in a child of the same test binary, gives that child a fixed deadline of its own,
//! and fails the parent if the child is still alive at the deadline or exits non-zero.
//!
//! Design rules:
//! - The parent's deadline is independent of the child's own joins, cancels and progress flags.
//! - The parent polls `try_wait`, so it detects a non-zero exit promptly and a hard deadline
//!   otherwise. It never waits on the child's own bounded work to finish first.
//! - On timeout the parent kills only the child it spawned, after re-checking the child's pid and
//!   the command it ran. It never signals a process group and never touches anything else.
//! - The child's stdout and stderr are inherited as pipes and drained on a thread, so a verbose
//!   child cannot fill a pipe and deadlock. Its log is appended to a file and fsynced per phase.
//! - A recursion guard (`CHILD_ENV`) makes the spawned process run the child body instead of
//!   spawning again. The parent also asserts the child ran the intended fixture and produced its
//!   evidence, so a filter typo cannot pass as a "the child did nothing but exited 0" success.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Set in the child so the same test body runs the fixture instead of spawning another child.
pub const CHILD_ENV: &str = "COWFS_GC_CHILD_FIXTURE";
/// Set to the log path the child appends its phase log to.
pub const LOG_ENV: &str = "COWFS_GC_CHILD_LOG";
/// Set to "1" in the child to make the child park the collector forever (the hang path).
pub const PARK_ENV: &str = "COWFS_GC_CHILD_PARK";
/// Set to "1" in the child to make a writer fail early (the stop-on-error path).
pub const FAIL_ENV: &str = "COWFS_GC_CHILD_FAIL";

/// True when this process was spawned as the fixture child.
pub fn is_child() -> bool {
    std::env::var_os(CHILD_ENV).is_some()
}

/// True when the child must permanently park its collector after setup.
pub fn is_park_child() -> bool {
    std::env::var(PARK_ENV).is_ok_and(|v| v == "1")
}

/// True when the child must arm an early writer failure after setup.
pub fn is_fail_child() -> bool {
    std::env::var(FAIL_ENV).is_ok_and(|v| v == "1")
}

/// Append `line` to the child's phase log and fsync it, so the log survives a hard kill.
pub fn child_log(line: &str) {
    let Ok(path) = std::env::var(LOG_ENV) else {
        return;
    };
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("open child log");
    writeln!(f, "{line}").expect("write child log");
    f.sync_all().expect("fsync child log");
}

/// The result of running one child fixture to completion.
pub struct ChildOutcome {
    /// The child's exit code, `None` if it was killed by a signal.
    pub code: Option<i32>,
    /// Everything the child wrote to stdout and stderr.
    pub output: String,
    /// Wall time the parent waited.
    pub waited: Duration,
    /// True when the parent killed the child at the deadline.
    pub timed_out: bool,
}

impl ChildOutcome {
    /// The child completed its fixture: exit 0, not killed, and it printed the evidence line.
    pub fn succeeded(&self, evidence: &str) -> bool {
        !self.timed_out && self.code == Some(0) && self.output.contains(evidence)
    }
}

/// Run `test_name` of this test binary in a child, with a fixed parent-owned deadline.
///
/// `child_env` adds or overrides environment for the child. The parent returns once the child
/// exits, exits non-zero, or the deadline passes. On the deadline the parent kills and reaps only
/// this child. Returns the outcome; the caller decides PASS/FAIL and never treats a timeout as a
/// pass or a skip.
pub fn run_child_fixture(
    test_name: &str,
    child_env: &[(&str, &str)],
    deadline: Duration,
    log_path: &Path,
) -> ChildOutcome {
    // A clean parent: recursion guard must be absent here.
    assert!(
        std::env::var_os(CHILD_ENV).is_none(),
        "run_child_fixture must be called from the parent, not the child"
    );
    let exe = std::env::current_exe().expect("current_exe");
    let mut cmd = Command::new(&exe);
    cmd.arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env(LOG_ENV, log_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in child_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn child fixture");
    let pid = child.id();

    // Drain both pipes on one thread each, so a verbose child cannot fill a pipe and deadlock.
    let (tx, rx) = mpsc::channel::<String>();
    let out_tx = tx.clone();
    let out = child.stdout.take().expect("child stdout");
    let err = child.stderr.take().expect("child stderr");
    std::thread::spawn(move || drain(out, out_tx));
    std::thread::spawn(move || drain(err, tx));

    let started = Instant::now();
    let mut timed_out = false;
    let code = loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => break status.code(),
            None => {
                if started.elapsed() >= deadline {
                    // Re-verify this is the child we spawned before signalling it.
                    verify_child(pid, test_name);
                    // The child may have exited between try_wait and here; then kill is a no-op and
                    // wait reaps it. Either way the deadline is what ended this call.
                    let _ = child.kill();
                    let status = child.wait().expect("reap timed-out child");
                    timed_out = true;
                    break status.code();
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    };

    // The drain threads end when the pipes close; join by closing the sender side and collecting.
    drop(child);
    let mut output = String::new();
    for chunk in rx.iter() {
        output.push_str(&chunk);
    }
    ChildOutcome {
        code,
        output,
        waited: started.elapsed(),
        timed_out,
    }
}

fn drain(mut r: impl Read, tx: mpsc::Sender<String>) {
    let mut buf = [0u8; 4096];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx
                    .send(String::from_utf8_lossy(&buf[..n]).into_owned())
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

/// Confirm the pid we are about to signal is still the child we started, and that it is our
/// fixture child and not something reused. A mismatch aborts rather than killing a stranger.
/// Uses `ps` from `PATH` so it works on both macOS and Linux runners, not a hardcoded `/bin`.
fn verify_child(pid: u32, test_name: &str) {
    let out = Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let cmd = String::from_utf8_lossy(&o.stdout);
            assert!(
                cmd.contains(test_name),
                "refusing to kill pid {pid}: its command does not name this fixture ({cmd:?})"
            );
        }
        // The process is already gone, or `ps` is unavailable; kill is a no-op and wait reaps it.
        _ => {}
    }
}
