//! Teardown safety for a fixture that starts processes and mounts a real filesystem.
//!
//! The gate in `namespace_durability_gate.rs` kills daemons and unmounts NFS mounts in `Drop`, which
//! is the last line of defence and whose failure mode is destructive: signalling a pid that is not
//! ours, or walking a directory that is still mounted, can take out something on the machine that
//! has nothing to do with the test.
//!
//! Everything here is deliberately local to this test tree.
//! `cowfs_daemon::mounts` and `cowfs_nfs::is_listed` belong to other owners, and `is_listed` is a
//! prefix match on unescaped output, which is exactly the property that must not guard a recursive
//! delete. Reusing it here would inherit that. What follows is a fail-closed reader instead: it
//! answers `Mounted`, `Absent` or `Unknown`, and only `Absent`, proven from a table that parsed and
//! contained no entry for this exact path, permits a delete.
//!
//! Two rules hold throughout:
//!
//! - No signal is sent until the pid's identity is re-read and matches what this fixture recorded
//!   when it spawned the child. A mismatch is a preserve, never a signal.
//! - No destructive step runs on `Unknown`. An unreadable or unparseable mount table is treated as
//!   "maybe mounted", which means the fixture keeps its store and reports a leak.

#![allow(dead_code)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A mount table that could not be trusted. Deleting anything is forbidden while this is possible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountState {
    /// The table parsed and names this path.
    Mounted,
    /// The table parsed and does not name this path. Only this state permits a delete.
    Absent,
    /// The table could not be run, or could not be parsed. Treat as mounted.
    Unknown,
}

/// What this fixture recorded about a child it spawned, read back before any signal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildIdentity {
    pub pid: u32,
    /// Seconds since the epoch, from the kernel, so a recycled pid cannot match.
    pub start: u64,
    /// The executable path as spawned.
    pub exe: PathBuf,
    /// The full argv as spawned, joined with spaces.
    pub argv: String,
    /// The `--store` this fixture owns.
    pub store: PathBuf,
    /// The `--socket` this fixture owns.
    pub socket: PathBuf,
}

/// How long any single child process this fixture spawns may take, including `/sbin/mount`,
/// `/sbin/umount` and `/bin/kill`.
///
/// The bound is absolute and starts before the spawn, so a slow `umount` cannot extend it. A
/// `Command::status()` call has no timeout of its own and can block forever against a wedged
/// kernel, which is why every external command here is spawned and polled instead.
pub const CHILD_BUDGET: Duration = Duration::from_secs(20);

/// How long the whole teardown may take. Shared and absolute: it is computed once and never
/// recreated per step, so a slow step cannot buy later steps a fresh budget.
pub const TEARDOWN_BUDGET: Duration = Duration::from_secs(60);

/// One attempt at running an external command, bounded, with its identity kept for a signal that
/// may still be needed.
#[derive(Debug)]
pub struct Bounded {
    child: Option<Child>,
    /// What this fixture recorded about the child it just spawned.
    pub identity: ChildIdentity,
    /// True when the command finished within its budget.
    pub finished: bool,
}

impl Bounded {
    /// Takes the child out, for a caller that needs `wait_with_output` or a handle it can stop.
    pub fn take(&mut self) -> Option<Child> {
        self.child.take()
    }

    /// Stops the child through its own handle, which needs no identity check because this process
    /// still owns it. Reaps it, so its pid is free afterwards.
    pub fn stop_owned(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Bounded {
    /// True when the child was reaped, so its pid is free and must never be signalled again.
    pub fn reaped(&mut self) -> bool {
        match self.child.as_mut() {
            None => true,
            Some(c) => matches!(c.try_wait(), Ok(Some(_)) | Err(_)),
        }
    }
}

/// The identity of a child this fixture is about to spawn, captured from what it will be run as.
///
/// `exe` and `argv` come from the caller's own arguments rather than from a re-read of the process,
/// because at this point the process may not have started far enough to answer. The fields that
/// decide a match, the kernel start time and the live command line, are read separately.
pub fn identity_for(
    pid: u32,
    exe: &Path,
    args: &[&OsStr],
    store: &Path,
    socket: &Path,
) -> ChildIdentity {
    let mut argv = vec![exe.display().to_string()];
    argv.extend(args.iter().map(|a| a.to_string_lossy().into_owned()));
    identity_of(pid, &argv, store, socket)
}

pub fn identity_of(pid: u32, argv: &[String], store: &Path, socket: &Path) -> ChildIdentity {
    ChildIdentity {
        pid,
        start: process_start(pid).unwrap_or(0),
        exe: PathBuf::from(argv.first().map_or("", |s| s.as_str())),
        argv: argv.join(" "),
        store: store.to_path_buf(),
        socket: socket.to_path_buf(),
    }
}

/// Spawns `program` with `args`, records its identity, and waits at most `budget` for it to finish.
///
/// The identity is captured before the wait, so a fixture that has to signal a still-running child
/// afterwards has something to check it against.
pub fn spawn_bounded(
    program: &Path,
    args: &[&OsStr],
    store: &Path,
    socket: &Path,
    budget: Duration,
) -> std::io::Result<Bounded> {
    let mut argv = vec![program.display().to_string()];
    argv.extend(args.iter().map(|a| a.to_string_lossy().into_owned()));
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let identity = identity_of(child.id(), &argv, store, socket);
    // `wait_timeout` is not available without a new dependency, so poll `try_wait` against an
    // absolute deadline. The deadline is created before the spawn so a slow spawn cannot extend it.
    let deadline = Instant::now() + budget;
    let mut finished = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                finished = true;
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(Bounded {
        child: Some(child),
        identity,
        finished,
    })
}

/// The kernel's start time for `pid`, in seconds since the epoch, or `None` if it cannot be read.
pub fn process_start(pid: u32) -> Option<u64> {
    let out = Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // `ps -o lstart=` prints e.g. "Sun Oct  4 15:37:05 2026". Converted to epoch seconds so the
    // comparison cannot be fooled by formatting.
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let secs = run_date_to_epoch(&text)?;
    Some(secs)
}

/// Turns a `ps -o lstart=` string into epoch seconds. `None` when it cannot be parsed, which the
/// callers treat as an identity that cannot be confirmed.
fn run_date_to_epoch(text: &str) -> Option<u64> {
    // "Sun Oct  4 15:37:05 2026"
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.len() < 5 {
        return None;
    }
    let month = *parts.get(1)?;
    let day: u32 = parts.get(2)?.parse().ok()?;
    let time = parts.get(3)?;
    let year: i64 = parts.get(4)?.parse().ok()?;
    let mut hms = time.split(':');
    let h: u32 = hms.next()?.parse().ok()?;
    let m: u32 = hms.next()?.parse().ok()?;
    let s: u32 = hms.next()?.parse().ok()?;
    let mi = match month {
        "Jan" => 0,
        "Feb" => 1,
        "Mar" => 2,
        "Apr" => 3,
        "May" => 4,
        "Jun" => 5,
        "Jul" => 6,
        "Aug" => 7,
        "Sep" => 8,
        "Oct" => 9,
        "Nov" => 10,
        "Dec" => 11,
        _ => return None,
    };
    // Days from the epoch to the start of `year`, by the civil calendar. Good enough to seconds for
    // a comparison against a pid's own recorded start, and it needs no timezone database: the only
    // use is equality against another reading of the same field.
    let mut days: i64 = 0;
    for y in 1970..year {
        days += if leap(y) { 366 } else { 365 };
    }
    const CUMULATIVE: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    days += CUMULATIVE[mi as usize];
    if mi > 1 && leap(year) {
        days += 1;
    }
    days += i64::from(day) - 1;
    Some(((days * 86_400) + (i64::from(h) * 3600) + (i64::from(m) * 60) + i64::from(s)) as u64)
}

fn leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Reads the live identity of `pid` and reports whether it is the child this fixture spawned.
///
/// Every field is compared, not just the pid: a pid recycled between the child's death and this
/// check would carry a different start time and a different command line.
pub fn identity_matches(want: &ChildIdentity) -> bool {
    if want.pid == 0 || want.start == 0 || want.exe.as_os_str().is_empty() {
        return false;
    }
    let Some(start) = process_start(want.pid) else {
        return false;
    };
    if start != want.start {
        return false;
    }
    let out = Command::new("/bin/ps")
        .args(["-o", "command=", "-p", &want.pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let Ok(out) = out else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let command = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // The argv a fixture records is what it asked for; `ps` prints what is running. Both must name
    // the fixture's own store and socket, which no other process on this machine uses.
    command.contains(&want.store.display().to_string())
        && command.contains(&want.socket.display().to_string())
}

/// Signals `identity` only if it still names the child this fixture spawned.
///
/// A mismatch, or an unreadable identity, is reported and nothing is sent. There is no process
/// group and no `pkill`: one verified pid or nothing.
pub fn signal_if_ours(identity: &ChildIdentity, sig: &str) -> SignalOutcome {
    if identity_matches(identity) {
        match Command::new("/bin/kill")
            .arg(format!("-{sig}"))
            .arg(identity.pid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(s) if s.success() => SignalOutcome::Signalled,
            _ => SignalOutcome::Refused("kill reported failure".into()),
        }
    } else {
        SignalOutcome::Refused("identity no longer matches the child this fixture spawned".into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignalOutcome {
    Signalled,
    Refused(String),
}

/// Decodes the escapes `mount` uses in a path, in one pass, with no second scan.
///
/// `mount` prints a space as `\040`, a tab as `\011`, a newline as `\012` and a backslash as
/// `\134`. Decoding by repeatedly replacing a pattern would mis-handle a literal `\040` in a name,
/// so this walks the bytes once.
pub fn unescape_mount_field(field: &str) -> String {
    let b = field.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1].is_ascii_digit() {
            let d = (b[i + 1], b[i + 2], b[i + 3]);
            let v = match (d.0, d.1, d.2) {
                (b'0', b'4', b'0') => Some(0x20u8),
                (b'0', b'1', b'1') => Some(0x09),
                (b'0', b'1', b'2') => Some(0x0a),
                (b'1', b'3', b'4') => Some(0x5c),
                _ => None,
            };
            if let Some(c) = v {
                out.push(c);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The paths `mount_output` lists, each decoded once.
///
/// `mount` prints `source on <path> (<fstype>[, options])` on macOS. A line that does not parse as
/// that shape makes the whole table `Unknown`, because a partial parse cannot support "absent".
pub fn listed_paths(mount_output: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for line in mount_output.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        // "src on path (fstype, ...)"
        let on = line.find(" on ")?;
        let rest = &line[on + 4..];
        let open = rest.rfind(" (")?;
        let path = &rest[..open];
        if path.is_empty() {
            return None;
        }
        out.push(unescape_mount_field(path));
    }
    Some(out)
}

/// Classifies `path` against a mount table that has already been read.
///
/// `table` is `None` when `/sbin/mount` could not be run or did not exit successfully. That is
/// `Unknown`, never `Absent`.
pub fn classify(table: Option<&str>, path: &Path) -> MountState {
    let Some(text) = table else {
        return MountState::Unknown;
    };
    let Some(paths) = listed_paths(text) else {
        return MountState::Unknown;
    };
    let want = path.to_string_lossy();
    if paths.iter().any(|p| p == want.as_ref()) {
        MountState::Mounted
    } else {
        MountState::Absent
    }
}

/// Reads the mount table and classifies `mount_path` against it.
///
/// Returns `Unknown` whenever the table could not be run, did not exit zero, did not finish inside
/// `budget`, or could not be parsed. Only a table that parsed cleanly and does not name `mount_path`
/// yields `Absent`, and only `Absent` permits a delete.
pub fn read_mount_table(
    mount_path: &Path,
    store: &Path,
    socket: &Path,
    budget: Duration,
) -> (MountState, Option<Bounded>) {
    let run = spawn_bounded(Path::new("/sbin/mount"), &[], store, socket, budget);
    let Ok(mut b) = run else {
        return (MountState::Unknown, None);
    };
    if !b.finished {
        // Still running: it is kept, with its identity, so the caller can decide.
        return (MountState::Unknown, Some(b));
    }
    // `mount` with no arguments lists the table and exits zero. `try_wait` above already reaped
    // it, so the pid is free and `wait_with_output` returns immediately with what it printed.
    let out = match b.take().map(|c| c.wait_with_output()) {
        Some(Ok(o)) => o,
        _ => return (MountState::Unknown, None),
    };
    if !out.status.success() {
        return (MountState::Unknown, Some(b));
    }
    (
        classify(Some(&String::from_utf8_lossy(&out.stdout)), mount_path),
        Some(b),
    )
}
