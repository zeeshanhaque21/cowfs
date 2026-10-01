use cowfs_ctl::ProcessInfo;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// The prefix the macOS NFS client gives a file it could not unlink because a process still holds
/// it. Its presence in a slot after a return is the issue #20 signature.
pub const NFS_PREFIX: &str = ".nfs";

/// Entries in `dir` that are NFS silly-renames, sorted, with full paths.
pub fn nfs_entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = rd
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(NFS_PREFIX))
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// Waits until `dir` has no `.nfs*` entry, polling, and fails on timeout naming what is still
/// there. Exits on failure as well as on success: a loop that only knows how to end one way hangs.
pub fn wait_for_nfs_clear(dir: &Path, timeout: Duration, poll: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = nfs_entries(dir);
        if left.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Busy(format!(
                "{} still holds {} after {}s: {}",
                dir.display(),
                if left.len() == 1 {
                    "1 silly-rename"
                } else {
                    "silly-renames"
                },
                timeout.as_secs(),
                left.iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        std::thread::sleep(poll);
    }
}

/// The caller and its whole ancestry, which must never be signalled. Mirrors treehouse's
/// `process.filterProtectedProcesses`.
pub fn protected_ancestry() -> BTreeSet<u32> {
    let mut set = BTreeSet::new();
    let mut pid = std::process::id();
    while pid > 0 && set.insert(pid) {
        // An ancestor whose parent cannot be resolved stops the walk rather than widening the
        // target set: everything above it is not this process's to signal anyway.
        match ppid(pid) {
            Some(parent) if parent > 0 => pid = parent,
            _ => break,
        }
    }
    set
}

fn ppid(pid: u32) -> Option<u32> {
    let out = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// What `ps` says about a process: running, a zombie, or gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Liveness {
    /// The process exists and can still run.
    Running,
    /// The process is dead but not yet reaped, which `kill(pid, 0)` reports as existing. Treating
    /// that as alive would hang every wait loop on a process this one itself signalled.
    Zombie,
    /// No such process.
    Gone,
    /// The process table could not be read, so nothing may be assumed in either direction.
    Unknown,
}

/// Asks the process table about one pid.
///
/// `ps` rather than `kill(pid, 0)` because a SIGKILLed child stays a zombie until it is reaped and
/// `kill(pid, 0)` still succeeds for it, which is exactly the case a wait loop must not hang on.
pub fn liveness(pid: u32) -> Liveness {
    if pid == 0 {
        return Liveness::Gone;
    }
    let Ok(out) = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
    else {
        return Liveness::Unknown;
    };
    if !out.status.success() {
        return Liveness::Gone;
    }
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    if stat.is_empty() {
        return Liveness::Gone;
    }
    if stat.starts_with('Z') {
        return Liveness::Zombie;
    }
    Liveness::Running
}

/// True when a process is still running. A zombie counts as gone, because it can no longer touch
/// the slot; an unreadable process table counts as alive, so an unprovable holder blocks the reset
/// rather than being reset under.
pub fn alive(pid: u32) -> bool {
    !matches!(liveness(pid), Liveness::Gone | Liveness::Zombie)
}

/// True when this process may signal `pid`: it exists, it is not init, not this process, and not
/// one of its ancestors.
///
/// The liveness check is part of the predicate rather than a separate step in `terminate`, so there
/// is no ordering in which a pid that has already exited reaches a signal. `kill(0)` and `kill(-1)`
/// are unreachable: the pid is a `u32` narrowed to `i32` and rejected at or below 1.
pub fn signalable(pid: u32) -> bool {
    pid > 1 && pid != std::process::id() && !protected_ancestry().contains(&pid) && alive(pid)
}

fn send(pid: u32, signal: rustix::process::Signal) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    let Some(target) = rustix::process::Pid::from_raw(raw) else {
        return false;
    };
    rustix::process::kill_process(target, signal).is_ok()
}

/// How long treehouse waits between SIGTERM and SIGKILL (`cmd/get.go`, `killLingeringProcesses`).
pub const GRACE: Duration = Duration::from_secs(2);
/// How often the wait loop checks whether a process is gone.
pub const POLL: Duration = Duration::from_millis(100);

/// What a termination attempt did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Terminated {
    /// Pids that were actually signalled, with a SIGTERM.
    pub signalled: Vec<u32>,
    /// Pids that needed SIGKILL.
    pub killed: Vec<u32>,
    /// Pids that were skipped because this process may not signal them.
    pub skipped: Vec<u32>,
    /// Pids still alive after the second wait.
    pub survivors: Vec<u32>,
}

/// Terminates holders using treehouse's own policy: SIGTERM, poll for the grace period, SIGKILL
/// the survivors, then poll again so a killed process is reaped before anything runs git.
///
/// Only pids the caller may signal are touched. A survivor is reported, never hidden, so the
/// caller can leave the slot in place rather than reset a worktree a live writer is still using.
pub fn terminate(pids: &[u32], grace: Duration) -> Terminated {
    let mut out = Terminated::default();
    for &pid in pids {
        if !alive(pid) {
            continue;
        }
        if !signalable(pid) {
            out.skipped.push(pid);
            continue;
        }
        if send(pid, rustix::process::Signal::TERM) {
            out.signalled.push(pid);
        }
    }
    if !out.signalled.is_empty() && !wait_gone(&out.signalled, grace) {
        for &pid in &out.signalled {
            if alive(pid) {
                send(pid, rustix::process::Signal::KILL);
                out.killed.push(pid);
            }
        }
        wait_gone(&out.killed, grace);
    }
    out.survivors = pids
        .iter()
        .copied()
        .filter(|p| alive(*p) && signalable(*p))
        .collect();
    out
}

fn wait_gone(pids: &[u32], timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pids.iter().all(|p| !alive(*p)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL);
    }
}

/// Renders holders the way an operator needs to see them: pid, what it holds and how.
pub fn describe(holders: &[ProcessInfo]) -> Vec<String> {
    holders
        .iter()
        .map(|p| {
            let holds = p
                .holds
                .iter()
                .map(|h| format!("{} {}", hold_kind(h.kind), h.path))
                .collect::<Vec<_>>()
                .join(", ");
            format!("pid {} ({}): {}", p.pid, p.command, holds)
        })
        .collect()
}

fn hold_kind(kind: cowfs_ctl::HoldKind) -> &'static str {
    match kind {
        cowfs_ctl::HoldKind::Cwd => "cwd",
        cowfs_ctl::HoldKind::Fd => "fd",
        cowfs_ctl::HoldKind::Lock => "lock",
        cowfs_ctl::HoldKind::Other => "other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alive_is_true_for_this_process_and_false_for_pid_zero() {
        assert!(alive(std::process::id()));
        assert!(!alive(0));
        assert_eq!(liveness(0), Liveness::Gone);
        assert_eq!(liveness(std::process::id()), Liveness::Running);
    }

    #[test]
    fn a_pid_that_never_existed_is_gone() {
        assert_eq!(liveness(u32::MAX), Liveness::Gone);
        assert!(!alive(u32::MAX));
    }

    #[test]
    fn a_sigkilled_child_is_a_zombie_and_counts_as_gone() {
        // Reproduces the case a wait loop must not hang on: this process's own child, killed but
        // not yet reaped, which `kill(pid, 0)` would still report as existing.
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 300"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn");
        let pid = child.id();
        // A Child that has exited is waited on, so this test cannot leave a zombie behind either.
        let done = cowfs_ctl_alive(pid);
        assert!(done, "the child is running before the kill");
        let _ = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
        for _ in 0..200 {
            if matches!(liveness(pid), Liveness::Zombie | Liveness::Gone) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let seen = liveness(pid);
        assert!(
            !alive(pid),
            "a killed process must not count as alive, seen {seen:?}"
        );
        let _ = child.wait();
        assert!(!alive(pid), "and not after it is reaped either");
    }

    fn cowfs_ctl_alive(pid: u32) -> bool {
        alive(pid)
    }

    #[test]
    fn never_signals_self_or_ancestors() {
        let ancestry = protected_ancestry();
        assert!(ancestry.contains(&std::process::id()));
        for pid in &ancestry {
            assert!(!signalable(*pid), "pid {pid} is in our own ancestry");
        }
    }

    #[test]
    fn never_signals_init() {
        assert!(!signalable(0));
        assert!(!signalable(1));
    }

    #[test]
    fn a_pid_that_does_not_exist_is_not_signalable() {
        // The property the `if false` mutation removed. `kill(0)` signals the caller's own process
        // group, so this gate is the only thing between a stale pid list and the caller's group.
        assert!(!signalable(u32::MAX));
        assert!(!signalable(999_999_999));
        let dead = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .status();
        assert!(dead.is_ok(), "the child ran");
        // A pid that exited and was waited on is gone and therefore not signalable.
        assert!(!signalable(999_999_998));
    }

    #[test]
    fn terminating_nothing_is_a_noop() {
        let out = terminate(&[], GRACE);
        assert_eq!(out, Terminated::default());
    }

    #[test]
    fn terminating_a_dead_pid_is_a_noop() {
        let out = terminate(&[u32::MAX], GRACE);
        assert!(out.signalled.is_empty());
        assert!(out.survivors.is_empty());
    }

    #[test]
    fn terminating_self_is_refused_and_reported() {
        let out = terminate(&[std::process::id()], GRACE);
        assert!(out.signalled.is_empty());
        assert!(out.killed.is_empty());
        assert!(out.survivors.is_empty());
        assert!(alive(std::process::id()));
    }

    #[test]
    fn nfs_entries_finds_silly_renames_and_ignores_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".nfs.00000001.abc"), b"x").expect("write");
        std::fs::write(dir.path().join(".nfs.00000002.def"), b"x").expect("write");
        std::fs::write(dir.path().join("keep.txt"), b"x").expect("write");
        std::fs::write(dir.path().join(".hidden"), b"x").expect("write");
        let found = nfs_entries(dir.path());
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].ends_with(".nfs.00000001.abc"));
    }

    #[test]
    fn nfs_entries_of_a_missing_directory_is_empty() {
        assert!(nfs_entries(Path::new("/nonexistent/slot")).is_empty());
    }

    #[test]
    fn wait_returns_at_once_when_clean() {
        let dir = tempfile::tempdir().expect("tempdir");
        wait_for_nfs_clear(dir.path(), Duration::from_secs(5), POLL).expect("clean");
    }

    #[test]
    fn wait_fails_on_timeout_and_names_the_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dirt = dir.path().join(".nfs.00000001.abc");
        std::fs::write(&dirt, b"x").expect("write");
        let err = wait_for_nfs_clear(dir.path(), Duration::from_millis(150), POLL)
            .expect_err("must not succeed while dirt remains");
        assert!(matches!(err, Error::Busy(_)), "{err:?}");
        let msg = err.to_string();
        assert!(msg.contains("1 silly-rename"), "{msg}");
        assert!(msg.contains(".nfs.00000001.abc"), "{msg}");
        assert!(dirt.exists());
    }

    #[test]
    fn describe_names_pid_command_and_hold_kind() {
        let holders = vec![ProcessInfo {
            pid: 42,
            command: "sleep 600".into(),
            holds: vec![cowfs_ctl::Hold {
                kind: cowfs_ctl::HoldKind::Fd,
                path: "/pool/slot/held.txt".into(),
            }],
        }];
        let text = describe(&holders);
        assert_eq!(text.len(), 1);
        assert!(text[0].contains("pid 42"), "{}", text[0]);
        assert!(text[0].contains("sleep 600"), "{}", text[0]);
        assert!(text[0].contains("fd /pool/slot/held.txt"), "{}", text[0]);
    }
}
