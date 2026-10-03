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
//! - The spawned `Child` is owned by an RAII guard that kills and reaps it on every exit path,
//!   including a panic. No path can abandon an unrecognised process: the guard only ever signals
//!   the pid std reserved for the handle it holds, never a process group.
//! - On timeout the parent re-checks the child's command with `ps` as a diagnostic, but a `ps`
//!   failure or mismatch never panics and never abandons the child; it records the mismatch and
//!   still kills and reaps the owned handle.
//! - The child's stdout and stderr are drained on threads with a finite total budget, so a
//!   descendant holding a pipe cannot block the parent forever. If the budget expires the reader
//!   threads are detached (they own only their cloned pipe) and the parent reports what it got.
//! - A recursion guard (`CHILD_ENV`, plus a per-run nonce) makes the spawned process run the child
//!   body instead of spawning again. The parent also requires the child's evidence line and its
//!   nonce, so a filter typo or an inherited environment cannot pass as a success.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Set in the child so the same test body runs the fixture instead of spawning another child.
pub const CHILD_ENV: &str = "COWFS_GC_CHILD_FIXTURE";
/// Set to the log path the child appends its phase log to.
pub const LOG_ENV: &str = "COWFS_GC_CHILD_LOG";
/// Set to the per-run nonce the child must echo in its evidence line.
pub const NONCE_ENV: &str = "COWFS_GC_CHILD_NONCE";
/// Set to "1" in the child to make the child park the collector forever (the hang path).
pub const PARK_ENV: &str = "COWFS_GC_CHILD_PARK";
/// Set to "1" in the child to make a writer fail early (the stop-on-error path).
pub const FAIL_ENV: &str = "COWFS_GC_CHILD_FAIL";
/// Total wall budget for draining the child's pipes after it exits or is killed.
const DRAIN_BUDGET: Duration = Duration::from_secs(10);

/// True when this process was spawned as the fixture child. Requires both the guard and a nonce, so
/// an accidentally inherited `CHILD_ENV` alone (without a spawned nonce) does not turn the parent
/// into an in-process body. `run_child_fixture` clears both before spawning.
pub fn is_child() -> bool {
    std::env::var_os(CHILD_ENV).is_some() && std::env::var_os(NONCE_ENV).is_some()
}

/// True when the guard variable is present at all, ignoring the nonce. Used by the preset-env
/// control to assert the parent is not misclassified as a child.
pub fn guard_present() -> bool {
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

/// The nonce this child was started with, if any. The parent requires the child to echo it.
pub fn child_nonce() -> String {
    std::env::var(NONCE_ENV).unwrap_or_default()
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
    use std::io::Write;
    writeln!(f, "{line}").expect("write child log");
    f.sync_all().expect("fsync child log");
}

/// The result of running one child fixture to completion.
pub struct ChildOutcome {
    /// The child's exit code, `None` if it was killed by a signal.
    pub code: Option<i32>,
    /// Everything the parent captured from the child's stdout and stderr.
    pub output: String,
    /// Wall time the parent waited.
    pub waited: Duration,
    /// True when the parent killed the child at the deadline.
    pub timed_out: bool,
    /// True when the drain budget expired and the reader threads were detached.
    pub drain_expired: bool,
    /// The `ps` diagnostic taken before the kill, if any.
    pub ps_note: Option<String>,
}

impl ChildOutcome {
    /// The child completed its fixture: exit 0, not killed, drained, and its own nonce + the
    /// evidence marker are both present. `nonce` is the value the parent generated; a child that
    /// printed the marker without the nonce (a stale or forged line) is rejected.
    pub fn succeeded(&self, evidence: &str, nonce: &str) -> bool {
        !self.timed_out
            && !self.drain_expired
            && self.code == Some(0)
            && self.output.contains(evidence)
            && self.output.contains(nonce)
    }
}

/// Owns the spawned child and guarantees it is killed and reaped on every exit path, including a
/// panic. Only signals the pid std reserved for this handle.
struct OwnedChild {
    child: Option<Child>,
    test_name: String,
}

impl OwnedChild {
    fn new(child: Child, test_name: &str) -> Self {
        Self {
            child: Some(child),
            test_name: test_name.to_owned(),
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        match self.child.as_mut() {
            Some(c) => c.try_wait(),
            None => Ok(None),
        }
    }

    fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// Kill and reap the owned child. Idempotent; safe to call from `Drop`.
    /// Returns the status if there was a child to reap.
    fn kill_and_reap(&mut self) -> Option<ExitStatus> {
        let mut c = self.child.take()?;
        let _ = c.kill();
        c.wait().ok()
    }

    /// Diagnostic only: whether the pid's command names this fixture. Never panics; returns a note
    /// describing the outcome. A `ps` failure does not stop the kill of the owned handle.
    fn verify(&self) -> Option<String> {
        let pid = self.pid()?;
        let out = Command::new("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let cmd = String::from_utf8_lossy(&o.stdout);
                if cmd.contains(&self.test_name) {
                    Some(format!("ps_ok pid={pid}"))
                } else {
                    Some(format!(
                        "ps_mismatch pid={pid} (proceeding to kill the owned handle): {cmd:?}"
                    ))
                }
            }
            Ok(o) => Some(format!(
                "ps_failed pid={pid} status={:?} (proceeding to kill the owned handle)",
                o.status.code()
            )),
            Err(e) => Some(format!(
                "ps_error pid={pid}: {e} (proceeding to kill the owned handle)"
            )),
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        // Kill and reap only if the caller did not already. Non-panicking by contract.
        if self.child.is_some() {
            let _ = self.kill_and_reap();
        }
    }
}

/// Generate a per-run nonce from the current time and this process's pid. Not a security boundary:
/// its job is to reject a stale, inherited or hand-written evidence line, not a human forger.
fn make_nonce() -> String {
    format!("{:x}-{:x}", std::process::id(), {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    })
}

/// Run `test_name` of this test binary in a child, with a fixed parent-owned deadline.
///
/// `child_env` adds or overrides environment for the child. The parent returns once the child
/// exits, exits non-zero, or the deadline passes. On the deadline the parent kills and reaps only
/// this child. Returns the outcome and the nonce the child must have echoed; the caller decides
/// PASS/FAIL and never treats a timeout as a pass or a skip.
pub fn run_child_fixture(
    test_name: &str,
    child_env: &[(&str, &str)],
    deadline: Duration,
    log_path: &Path,
) -> (ChildOutcome, String) {
    // Clear any inherited guard from the parent's own environment so a preset value cannot make the
    // spawned child think it is nested, or make the parent's dispatch misread itself as the child.
    std::env::remove_var(CHILD_ENV);
    std::env::remove_var(NONCE_ENV);
    let nonce = make_nonce();
    let exe = std::env::current_exe().expect("current_exe");
    let mut cmd = Command::new(&exe);
    cmd.arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env(LOG_ENV, log_path)
        .env(NONCE_ENV, &nonce)
        .env_remove(PARK_ENV)
        .env_remove(FAIL_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in child_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn child fixture");
    let out = child.stdout.take().expect("child stdout");
    let err = child.stderr.take().expect("child stderr");

    let owned = OwnedChild::new(child, test_name);

    // Drain both pipes on one thread each, so a verbose child cannot fill a pipe and deadlock.
    let (tx, rx) = mpsc::channel::<String>();
    let out_tx = tx.clone();
    std::thread::spawn(move || drain(out, out_tx));
    std::thread::spawn(move || drain(err, tx));

    let started = Instant::now();
    let mut timed_out = false;
    let mut ps_note = None;
    let code = {
        let mut owned = owned;
        loop {
            match owned.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {
                    if started.elapsed() >= deadline {
                        ps_note = owned.verify();
                        let status = owned.kill_and_reap();
                        timed_out = true;
                        break status.and_then(|s| s.code());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                // try_wait failed: still reap the owned handle and report a failure code.
                Err(e) => {
                    ps_note = Some(format!("try_wait_error: {e}"));
                    let status = owned.kill_and_reap();
                    break status.and_then(|s| s.code());
                }
            }
        }
    };

    // Collect the drained output with a finite total budget. If a descendant holds a pipe open the
    // reader threads block on that clone; we detach them rather than join forever.
    let drain_started = Instant::now();
    let mut output = String::new();
    let mut drain_expired = false;
    while drain_started.elapsed() < DRAIN_BUDGET {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => output.push_str(&chunk),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    if drain_started.elapsed() >= DRAIN_BUDGET && finish_pending(&mut output, &rx) {
        drain_expired = true;
    }

    (
        ChildOutcome {
            code,
            output,
            waited: started.elapsed(),
            timed_out,
            drain_expired,
            ps_note,
        },
        nonce,
    )
}

/// Try to pull any already-buffered chunks without blocking. Returns true if any arrived after the
/// budget, meaning a writer was still live when the budget expired.
fn finish_pending(output: &mut String, rx: &mpsc::Receiver<String>) -> bool {
    let mut got = false;
    while let Ok(chunk) = rx.try_recv() {
        output.push_str(&chunk);
        got = true;
    }
    got
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
