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
//! - Verify-before-kill is taken from the owned handle alone: the reserved pid plus the `try_wait`
//!   state seen immediately before `Child::kill`. No external probe runs on the deadline path, so a
//!   hung or absent `ps` cannot extend the deadline. The pid is never reused before the reap.
//! - The child's stdout and stderr are drained on threads with a finite total budget, so a
//!   descendant holding a pipe cannot block the parent forever. If the budget elapses without the
//!   channel disconnecting, the reader threads are detached (they own only their cloned pipe),
//!   `drain_expired` is reported, and the parent fails. A silent descendant that holds the pipe
//!   open is a failure, not a clean drain.
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
/// Set to a seconds count in the child to spawn a bounded descendant that holds the inherited
/// stdout/stderr pipe open for that many seconds (the drain-expiry path).
pub const DESC_ENV: &str = "COWFS_GC_CHILD_DESC";
/// Total wall budget for draining the child's pipes after it exits or is killed.
const DRAIN_BUDGET: Duration = Duration::from_secs(10);

/// True when this process was spawned as the fixture child. Requires both the guard and a nonce, so
/// an accidentally inherited `CHILD_ENV` alone (without a spawned nonce) does not turn the parent
/// into an in-process body. The parent sets both on the child `Command` only.
pub fn is_child() -> bool {
    std::env::var_os(CHILD_ENV).is_some() && std::env::var_os(NONCE_ENV).is_some()
}

/// The child predicate as a pure function of its two inputs, so a control can assert the
/// guard-without-nonce case without mutating the process environment.
pub fn is_child_given(guard_and_nonce: (bool, bool)) -> bool {
    guard_and_nonce.0 && guard_and_nonce.1
}

/// True when the child must permanently park its collector after setup.
pub fn is_park_child() -> bool {
    std::env::var(PARK_ENV).is_ok_and(|v| v == "1")
}

/// True when the child must arm an early writer failure after setup.
pub fn is_fail_child() -> bool {
    std::env::var(FAIL_ENV).is_ok_and(|v| v == "1")
}

/// The bounded number of seconds a descendant should hold the inherited pipe for, if requested.
pub fn descendant_secs() -> Option<u64> {
    std::env::var(DESC_ENV).ok().and_then(|v| v.parse().ok())
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
    /// True when the drain budget elapsed without the channel disconnecting: a descendant still
    /// held a pipe open, which is a failure, not a clean drain.
    pub drain_expired: bool,
    /// The verify-before-kill state of the owned handle, taken immediately before the kill.
    pub prekill_note: Option<String>,
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
}

impl OwnedChild {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
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

    /// Standing verify-before-kill evidence, taken from the owned handle alone.
    ///
    /// No external probe runs here: an earlier version shelled out to `ps` before the kill, and a
    /// hung `ps` (a `PATH` shim that slept 35 s) extended a 20 s deadline to 55 s. The owned
    /// `std::process::Child` already reserves the pid until it is reaped, so the safe statement is
    /// the pid reserved for this handle plus the `try_wait` state observed immediately before the
    /// kill. It never names an unknown pid and never touches a process group.
    fn state_before_kill(&self) -> String {
        match self.pid() {
            Some(pid) => format!("owned pid={pid} alive_until_kill"),
            None => "owned pid=<already reaped>".to_string(),
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
    run_child_impl(test_name, child_env, deadline, log_path, true)
}

/// Run `test_name` with the guard set but deliberately *without* a spawned nonce. Used only by the
/// preset-inherited-guard control to prove that an inherited guard alone does not dispatch a
/// process into the child body. Never use this to run a fixture that needs the child path.
pub fn run_guard_only_child(test_name: &str, deadline: Duration, log_path: &Path) -> ChildOutcome {
    run_child_impl(test_name, &[], deadline, log_path, false).0
}

fn run_child_impl(
    test_name: &str,
    child_env: &[(&str, &str)],
    deadline: Duration,
    log_path: &Path,
    with_nonce: bool,
) -> (ChildOutcome, String) {
    // The spawned child gets the guard explicitly on its `Command`; no in-process environment
    // mutation happens here, so concurrent tests in this binary do not race on the process
    // environment. An inherited `CHILD_ENV` alone cannot make this parent misread itself as the
    // child, because `is_child()` requires the guard and a nonce together.
    let nonce = make_nonce();
    let exe = std::env::current_exe().expect("current_exe");
    let mut cmd = Command::new(&exe);
    cmd.arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env(LOG_ENV, log_path)
        .env_remove(PARK_ENV)
        .env_remove(FAIL_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if with_nonce {
        cmd.env(NONCE_ENV, &nonce);
    } else {
        cmd.env_remove(NONCE_ENV);
    }
    for (k, v) in child_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn child fixture");
    let out = child.stdout.take().expect("child stdout");
    let err = child.stderr.take().expect("child stderr");

    let owned = OwnedChild::new(child);

    // Drain both pipes on one thread each, so a verbose child cannot fill a pipe and deadlock.
    let (tx, rx) = mpsc::channel::<String>();
    let out_tx = tx.clone();
    std::thread::spawn(move || drain(out, out_tx));
    std::thread::spawn(move || drain(err, tx));

    let started = Instant::now();
    let mut timed_out = false;
    let mut prekill_note = None;
    let code = {
        let mut owned = owned;
        loop {
            match owned.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {
                    if started.elapsed() >= deadline {
                        prekill_note = Some(owned.state_before_kill());
                        let status = owned.kill_and_reap();
                        timed_out = true;
                        break status.and_then(|s| s.code());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                // try_wait failed: still reap the owned handle and report a failure code.
                Err(e) => {
                    prekill_note = Some(format!(
                        "try_wait_error: {e} ({})",
                        owned.state_before_kill()
                    ));
                    let status = owned.kill_and_reap();
                    break status.and_then(|s| s.code());
                }
            }
        }
    };

    // Collect the drained output with a finite total budget. If a descendant holds a pipe open the
    // reader threads block on that clone; we detach them rather than join forever. The budget
    // elapsing without a `Disconnected` is reported as `drain_expired`, because it means a child of
    // the child still held the pipe: a silent holder must fail the parent, not read as a clean drain.
    let mut output = String::new();
    let disconnected = drain_until(&rx, &mut output, DRAIN_BUDGET);
    let drain_expired = !disconnected;

    (
        ChildOutcome {
            code,
            output,
            waited: started.elapsed(),
            timed_out,
            drain_expired,
            prekill_note,
        },
        nonce,
    )
}

/// Drain until the sender disconnects or `budget` elapses. Returns `true` if the channel
/// disconnected (both reader threads closed their pipe and dropped their senders), `false` if the
/// budget elapsed first, which means a descendant still held a pipe open. Any chunks already
/// buffered when the budget elapses are pulled non-blockingly first, so the captured output is
/// preserved either way.
fn drain_until(rx: &mpsc::Receiver<String>, output: &mut String, budget: Duration) -> bool {
    let started = Instant::now();
    loop {
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            while let Ok(chunk) = rx.try_recv() {
                output.push_str(&chunk);
            }
            return false;
        }
        match rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(chunk) => output.push_str(&chunk),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return true,
        }
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
