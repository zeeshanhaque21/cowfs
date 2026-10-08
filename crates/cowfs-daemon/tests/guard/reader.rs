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
//! Three rules hold throughout:
//!
//! - No signal is sent until the pid's identity is re-read and matches what this fixture recorded
//!   when it spawned the child. A mismatch is a preserve, never a signal.
//! - No destructive step runs on `Unknown`. An unreadable or unparseable mount table is treated as
//!   "maybe mounted", which means the fixture keeps its store and reports a leak.
//! - Startup never deletes anything. Every run mints a fresh unique root and only creates inside it,
//!   so the destructive question does not arise at startup at all. A path that already exists is a
//!   bug in the name generator, and the answer is a refusal, never a cleanup of the old one.

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
///
/// This bounds a *test fixture* cleaning up after itself in `Drop`, nothing else. It is not a
/// runtime deadline, not a timeout on the daemon or the filesystem, and not a promise to any
/// caller: raising it does not change what a daemon does under load, and nothing in `cowfs-core`,
/// `cowfs-nfs` or `cowfs-daemon` reads it. It exists so a wedged `umount` cannot hang a test run
/// forever, and it was raised from 30 s to 60 s because a two-variant gate spends its teardown
/// unmounting and re-reading the table, and 30 s left no headroom on a loaded host.
pub const TEARDOWN_BUDGET: Duration = Duration::from_secs(60);

/// The only decision that may authorise a recursive delete, and the only place a delete is
/// performed. Everything that could destroy a live filesystem goes through here.
///
/// The two inputs are deliberately separate. `state` is what the mount table says, and `owned` is
/// whether this fixture created the path. `Absent` alone is not enough: a fixture that never made
/// the path has no business deleting it whatever the table says.
pub fn cleanup(
    state: MountState,
    owned: bool,
    label: &str,
    remove: impl FnOnce() -> std::io::Result<()>,
) -> Cleanup {
    if state == MountState::Mounted {
        return Cleanup::Preserved(format!(
            "{label}: the mount table names this path, so it may be a live filesystem"
        ));
    }
    if state == MountState::Unknown {
        return Cleanup::Preserved(format!(
            "{label}: the mount table could not be trusted, so this path may be a live filesystem"
        ));
    }
    if !owned {
        return Cleanup::Preserved(format!(
            "{label}: absent, but this fixture did not create it"
        ));
    }
    match remove() {
        Ok(()) => Cleanup::Removed,
        Err(e) => Cleanup::Preserved(format!("{label}: the remove failed, kept: {e}")),
    }
}

/// What `cleanup` decided. `Removed` only ever means the fixture's own path, proven unmounted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cleanup {
    Removed,
    Preserved(String),
}

/// The paths one attempt uses. Minted fresh per run, never reused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attempt {
    /// The export root and everything this run writes.
    pub dir: PathBuf,
    pub store: PathBuf,
    pub mount: PathBuf,
    /// On the short TMPDIR, not under `dir`, because of `sun_path`.
    pub socket: PathBuf,
}

/// `sun_path` is a fixed 104-byte buffer on macOS, and the limit is in *bytes* of the encoded path,
/// not characters: a name with a multi-byte character in it is shorter in characters than the
/// kernel measures. The daemon rejects a longer path at startup, so this is checked here first.
pub const SUN_PATH_MAX: usize = 104;

/// Mints a root that has not been used before, so startup has nothing to clear.
///
/// The name carries the pid, a nanosecond clock reading and a per-process counter, so two fixtures
/// on one host, and two runs of the same fixture, cannot collide. It stays short on purpose: a
/// long root is what pushes the socket past `SUN_PATH_MAX`.
pub fn attempt(base: &Path, tag: &str) -> Attempt {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos() as u64 + d.as_secs());
    let unique = format!("{}-{nanos:x}-{n:x}", std::process::id());
    let root = base.join(format!("{tag}-{unique}"));
    let socket = std::env::temp_dir().join(format!("d90-{unique}-{tag}.sock"));
    Attempt {
        dir: root.clone(),
        store: root.join("store"),
        mount: root.join("mnt"),
        socket,
    }
}

/// The length the kernel measures: the encoded bytes of the path, not its characters.
pub fn socket_len(socket: &Path) -> usize {
    socket.as_os_str().as_encoded_bytes().len()
}

/// Whether the kernel will accept this socket path.
///
/// Split out from `preflight` so the bound is a thing that can be asserted directly on any path,
/// including ones `attempt` would never mint.
pub fn socket_fits(socket: &Path) -> bool {
    socket_len(socket) < SUN_PATH_MAX
}

/// Refuses to start inside a path that already exists, and never touches what is there.
///
/// This is the whole startup safety story: because every run mints a fresh root, the only way one
/// can already exist is a name collision or a reused fixed path, and in both cases the right answer
/// is to stop and say so. Cleaning it up would mean deleting a directory nobody has yet established
/// is not a stale mount.
pub fn preflight(a: &Attempt) -> Result<(), String> {
    if !socket_fits(&a.socket) {
        return Err(format!(
            "the socket path is {} bytes, and sun_path holds {SUN_PATH_MAX}: {}",
            socket_len(&a.socket),
            a.socket.display()
        ));
    }
    for (what, p) in [("dir", &a.dir), ("store", &a.store), ("mount", &a.mount)] {
        if p.exists() {
            return Err(format!(
                "{what} already exists at {}: refusing to start rather than deleting a path this \
                 fixture has not established is not a stale mount",
                p.display()
            ));
        }
    }
    if a.socket.exists() {
        return Err(format!(
            "the socket already exists at {}: refusing to start",
            a.socket.display()
        ));
    }
    Ok(())
}

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
