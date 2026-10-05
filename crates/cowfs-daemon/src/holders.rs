//! Who holds a path: a process with its working directory, an open file or a lock inside it.
//!
//! The enumeration is over the process table, not over the filesystem. `lsof` is asked for every
//! open file on the machine and the paths are filtered here, which is the only shape whose
//! completeness can be argued: lsof reads each descriptor's path from the kernel, so a directory
//! inside the mount that the scanning user cannot read does not hide the process holding a file in
//! it, and nothing walks the mount, which on a network mount is the operation that can wedge.
//!
//! Linux reads `/proc` and `/proc/locks`. Both scan only what is below the prefix and report one
//! `ProcessInfo` per pid with its holds.

use cowfs_ctl::{Hold, HoldKind, ProcessInfo};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long the whole scan may take: resolving the prefix, running `lsof`, and draining both of its
/// pipes. A deadline that only covers the child wait is not a deadline, because the resolver and
/// the readers are where a wedged mount actually blocks.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// What a scan established.
///
/// `Holders` with an empty list and `Unavailable` are different answers, and only the first one may
/// unblock a change. `lsof` can fail, can be killed, can hang on a stale mount, and can hand back
/// output that never reached end of file, and `/proc` can be unreadable in a container; every one of
/// those used to read back as "nobody is holding anything", which is how issue #20 turns into a reset
/// under a live writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scan {
    /// The enumeration completed. The list is everything it found, which may legitimately be empty.
    Holders(Vec<ProcessInfo>),
    /// The answer could not be established, so nothing is assumed in either direction.
    Unavailable(String),
}

/// Every process holding something below `prefix`, and whether that list can be trusted.
///
/// This process is never a holder: the daemon holds the store and the mount it serves by
/// definition, so counting it would make every `expect_no_holders` check `busy`. What the caller
/// wants to know is who *else* is inside.
///
/// `mount` is the mount the prefix has to stay inside, checked after the prefix is resolved so a
/// symlink out of the mount is refused rather than followed. Without one there is nothing to check
/// against, which is what the mount and export paths ask for.
pub fn scan_mounted(mount: &Path, prefix: &Path) -> Scan {
    scan_within(Some(mount), prefix)
}

/// [`scan_mounted`] for a caller that has no mount to assert containment against.
pub fn scan_checked(prefix: &Path) -> Scan {
    scan_within(None, prefix)
}

/// Best-effort scan for a caller that only reports: an unanswerable question becomes an empty list
/// and the reason goes to stderr, which is what the mount and export paths want. Never use this to
/// decide that a reset, a removal or an export may proceed; use [`scan_checked`].
pub fn scan(prefix: &Path) -> Vec<ProcessInfo> {
    match scan_checked(prefix) {
        Scan::Holders(found) => found,
        Scan::Unavailable(why) => {
            eprintln!("cowfs-daemon: holder scan of {}: {why}", prefix.display());
            Vec::new()
        }
    }
}

/// Resolves and scans under one deadline.
///
/// The work happens on its own thread so the deadline covers all of it. A thread stuck in a syscall
/// against a dead mount is still stuck when this returns: what the deadline bounds is what the caller
/// waits for, not what the kernel does. That is the same exposure commits `90c9a8f` and `265fc3f`
/// recorded, and the alternative would be never answering at all.
fn scan_within(mount: Option<&Path>, prefix: &Path) -> Scan {
    let asked_for = prefix.display().to_string();
    let (tx, rx) = mpsc::channel();
    let prefix = prefix.to_path_buf();
    let mount = mount.map(Path::to_path_buf);
    std::thread::spawn(move || {
        let _ = tx.send(resolve_and_scan(mount.as_deref(), &prefix));
    });
    match rx.recv_timeout(SCAN_TIMEOUT) {
        Ok(scan) => scan,
        // A disconnected channel means the worker panicked, which is also not an answer.
        Err(_) => Scan::Unavailable(format!(
            "the scan of {asked_for} did not finish within {}s, which is what a wedged mount looks like",
            SCAN_TIMEOUT.as_secs()
        )),
    }
}

fn resolve_and_scan(mount: Option<&Path>, prefix: &Path) -> Scan {
    let resolved = match std::fs::canonicalize(prefix) {
        Ok(resolved) => resolved,
        Err(e) => {
            return Scan::Unavailable(format!(
                "{} cannot be resolved, so nothing below it can be scanned: {e}",
                prefix.display()
            ))
        }
    };
    if let Some(mount) = mount {
        if !resolved.starts_with(mount) {
            return Scan::Unavailable(format!(
                "{} resolves to {}, which is outside the mount {}",
                prefix.display(),
                resolved.display(),
                mount.display()
            ));
        }
    }
    let me = std::process::id();
    let found: Result<Vec<ProcessInfo>, String> = {
        #[cfg(target_os = "linux")]
        {
            imp::scan_proc(&resolved)
        }
        #[cfg(not(target_os = "linux"))]
        {
            imp::scan_lsof(&resolved, SCAN_TIMEOUT)
        }
    };
    let mut out = match found {
        Ok(out) => out,
        Err(why) => return Scan::Unavailable(why),
    };
    out.retain(|p| p.pid != me);
    out.sort_by_key(|p| p.pid);
    Scan::Holders(out)
}

fn hold(prefix: &Path, path: &Path, kind: HoldKind) -> Option<Hold> {
    path.starts_with(prefix).then(|| Hold {
        kind,
        path: path.to_string_lossy().into_owned(),
    })
}

fn finish(pid: u32, command: String, holds: Vec<Hold>) -> Option<ProcessInfo> {
    (!holds.is_empty()).then_some(ProcessInfo {
        pid,
        command,
        holds,
    })
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{finish, hold, Hold, HoldKind, Path, ProcessInfo};
    use std::collections::BTreeSet;

    /// `(major, minor, inode)` triples named by `/proc/locks`, which is how a flock is seen here:
    /// `F_GETLK` needs the fd, and the whole point is a process nobody asked.
    ///
    /// The kernel writes the row as
    /// `id: CLASS ADVISORY MODE PID MAJ:MIN:INO START END`, with an optional `-> PID` after the pid
    /// for a lock that is waiting on another. So the device is the first field that looks like a
    /// device, not the fourth, and the kernel formats it `%02x:%02x:%lu`: major and minor in hex,
    /// the inode in decimal. An unreadable table is an error, not an empty set: an empty set reports
    /// every lock as absent, and a lock held on the mount is enough to block an unmount.
    fn locked() -> Result<BTreeSet<(u64, u64, u64)>, String> {
        let text = std::fs::read_to_string("/proc/locks").map_err(|e| {
            format!("/proc/locks cannot be read, so a flock would be invisible: {e}")
        })?;
        let mut out = BTreeSet::new();
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let Some((major, minor, inode)) = device_of(line) else {
                return Err(format!(
                    "/proc/locks line {} is not a row this build understands: {line:?}",
                    n + 1
                ));
            };
            out.insert((major, minor, inode));
        }
        Ok(out)
    }

    /// The `MAJ:MIN:INO` field of one `/proc/locks` row, or `None` when the row does not have one.
    ///
    /// Found by shape rather than by index, because a waiting lock inserts `-> PID` between the pid
    /// and the device and an OFD lock prints a pid of `-1`.
    pub fn device_of(line: &str) -> Option<(u64, u64, u64)> {
        let field = line
            .split_whitespace()
            .find(|f| f.matches(':').count() == 2 && !f.contains("->"))?;
        let (major, rest) = field.split_once(':')?;
        let (minor, inode) = rest.split_once(':')?;
        Some((
            u64::from_str_radix(major, 16).ok()?,
            u64::from_str_radix(minor, 16).ok()?,
            inode.parse().ok()?,
        ))
    }

    fn command(pid: u32) -> String {
        std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
            .ok()
            .map(|c| c.replace('\0', " ").trim().to_owned())
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| format!("pid {pid}"))
    }

    pub fn scan_proc(prefix: &Path) -> Result<Vec<ProcessInfo>, String> {
        use std::os::unix::fs::MetadataExt;
        let locks = locked()?;
        let mut out = Vec::new();
        let entries = std::fs::read_dir("/proc")
            .map_err(|e| format!("/proc cannot be listed, so no descriptor can be seen: {e}"))?;
        for entry in entries.flatten() {
            let Some(pid) = entry.file_name().to_string_lossy().parse::<u32>().ok() else {
                continue;
            };
            let base = entry.path();
            let mut holds: Vec<Hold> = Vec::new();
            if let Ok(cwd) = std::fs::read_link(base.join("cwd")) {
                holds.extend(hold(prefix, &cwd, HoldKind::Cwd));
            }
            if let Ok(fds) = std::fs::read_dir(base.join("fd")) {
                for fd in fds.flatten() {
                    let Ok(target) = std::fs::read_link(fd.path()) else {
                        continue;
                    };
                    let Some(h) = hold(prefix, &target, HoldKind::Fd) else {
                        continue;
                    };
                    holds.push(h);
                    // A lock lives on an open descriptor, so the target's inode is what says
                    // whether this process also holds a lock on it.
                    if std::fs::metadata(&target).is_ok_and(|m| {
                        locks.contains(&(
                            u64::from(libc::major(m.dev())),
                            u64::from(libc::minor(m.dev())),
                            m.ino(),
                        ))
                    }) {
                        holds.push(Hold {
                            kind: HoldKind::Lock,
                            path: target.to_string_lossy().into_owned(),
                        });
                    }
                }
            }
            out.extend(finish(pid, command(pid), holds));
        }
        Ok(out)
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::{finish, hold, Duration, Hold, HoldKind, Instant, Path, ProcessInfo};
    use std::collections::BTreeMap;
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;

    pub const LSOF: &str = "/usr/sbin/lsof";

    /// Whether there is an `lsof` here to run at all.
    ///
    /// Checked before spawning rather than inferred from a failed spawn, because "the command is
    /// not there" and "the command could not answer" are different operator problems and the message
    /// has to name which one it is.
    pub fn lsof_usable() -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(LSOF).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }

    /// Every open file on the machine, for the caller to filter.
    ///
    /// There is no path argument and no `+D`. `+D` is a recursive walk of the directory, and a walk
    /// that cannot enter one subdirectory makes `lsof` give up: it exits 1 with no output and no
    /// diagnostic, which is byte-identical to its own "matched nothing", and a holder behind that
    /// directory reads back as a clean slot. Asking for the process table instead makes the answer
    /// independent of directory permissions, and keeps the mount out of the scan entirely.
    pub fn scan_lsof(prefix: &Path, timeout: Duration) -> Result<Vec<ProcessInfo>, String> {
        if !lsof_usable() {
            return Err(format!(
                "no runnable {LSOF} on this machine, so open files and flocks cannot be listed here; \
                 install lsof or run the daemon where it exists rather than treating the slot as \
                 clear"
            ));
        }
        let mut cmd = Command::new(LSOF);
        // `-F pcfn` is the machine-readable form: `p` pid, `c` command, `f` descriptor, `n` name.
        // `-nP` keeps lsof off the network and out of the port database, `-w` keeps it out of the
        // kernel's blocking calls.
        cmd.args(["-nP", "-w", "-F", "pcfn"]);
        parse_lsof(&run_bounded(&mut cmd, timeout)?, prefix)
    }

    /// Parses `lsof -F pcfn`, refusing anything it does not understand.
    ///
    /// A dropped field, an unknown tag or a name with no descriptor is not something to skip past:
    /// skipping is how a partial answer becomes a clean slot.
    pub fn parse_lsof(text: &str, prefix: &Path) -> Result<Vec<ProcessInfo>, String> {
        let mut by_pid: BTreeMap<u32, (String, Vec<Hold>)> = BTreeMap::new();
        let (mut pid, mut command, mut fd) = (0u32, String::new(), String::new());
        let mut in_record = false;
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            let Some((tag, value)) = line.split_at_checked(1) else {
                return Err(format!("lsof emitted a line with no field tag: {line:?}"));
            };
            match tag {
                "p" => {
                    pid = value.trim().parse().map_err(|_| {
                        format!("lsof emitted a record with no usable pid: {line:?}")
                    })?;
                    if pid == 0 {
                        return Err(format!("lsof emitted a record with pid 0: {line:?}"));
                    }
                    command.clear();
                    fd.clear();
                    in_record = true;
                    by_pid.entry(pid).or_default();
                }
                "c" => command = value.to_owned(),
                "f" => {
                    if !in_record {
                        return Err(format!(
                            "lsof emitted a descriptor before any pid: {line:?}"
                        ));
                    }
                    fd = value.to_owned();
                }
                "n" => {
                    if !in_record || fd.is_empty() {
                        return Err(format!("lsof emitted a name with no descriptor: {line:?}"));
                    }
                    let kind = match fd.as_str() {
                        "cwd" => HoldKind::Cwd,
                        // Digits with a letter appended carry a lock: `3u` is read-write.
                        d if d.chars().any(|c| !c.is_ascii_digit()) => HoldKind::Lock,
                        _ => HoldKind::Fd,
                    };
                    if let Some(h) = hold(prefix, Path::new(value), kind) {
                        let entry = by_pid.entry(pid).or_default();
                        entry.0 = command.clone();
                        entry.1.push(h);
                    }
                }
                other => {
                    return Err(format!(
                        "lsof emitted an unknown field tag {other:?}: {line:?}"
                    ))
                }
            }
        }
        Ok(by_pid
            .into_iter()
            .filter_map(|(pid, (command, holds))| finish(pid, command, holds))
            .collect())
    }

    /// Runs `lsof` and returns its standard output, but only when the whole answer arrived.
    ///
    /// Three things have to hold before the output means anything, and each one was a way to read a
    /// truncated answer back as a clean slot: the child must have exited, both pipes must have
    /// reached end of file, and all of it must have happened inside the caller's deadline. A
    /// grandchild that inherited the pipe keeps it open after the child is gone, so waiting for end
    /// of file is how that is caught rather than timing out with whatever had arrived.
    pub fn run_bounded(cmd: &mut Command, timeout: Duration) -> Result<String, String> {
        let program = cmd.get_program().to_string_lossy().into_owned();
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot run {program}: {e}"))?;
        let deadline = Instant::now() + timeout;
        let pipes = drain_pipes(&mut child);
        collect(&mut child, pipes, &program, timeout, deadline)
    }

    /// Starts one reader thread per pipe, so neither can fill its buffer while the other is being
    /// read, and returns the channel they report into.
    pub fn drain_pipes(child: &mut std::process::Child) -> mpsc::Receiver<(usize, String)> {
        let (tx, rx) = mpsc::channel();
        let mut pipes: Vec<Box<dyn Read + Send>> = Vec::new();
        if let Some(pipe) = child.stdout.take() {
            pipes.push(Box::new(pipe));
        }
        if let Some(pipe) = child.stderr.take() {
            pipes.push(Box::new(pipe));
        }
        for (which, mut pipe) in pipes.into_iter().enumerate() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut text = String::new();
                let _ = pipe.read_to_string(&mut text);
                let _ = tx.send((which, text));
            });
        }
        drop(tx);
        rx
    }

    /// Waits for `child`, then for both pipes, then checks the exit status.
    ///
    /// Split out from [`run_bounded`] so a test can hand it a child it spawned itself, which is the
    /// only way to produce a pipe that outlives its writer.
    pub fn collect(
        child: &mut std::process::Child,
        pipes: mpsc::Receiver<(usize, String)>,
        program: &str,
        timeout: Duration,
        deadline: Instant,
    ) -> Result<String, String> {
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("cannot wait for {program}: {e}"));
                }
            }
        };
        let Some(status) = status else {
            return Err(format!(
                "{program} did not exit within {}s, which is what a wedged mount looks like; nothing \
                 is assumed about who holds what",
                timeout.as_secs()
            ));
        };
        // The child is reaped, so the pipes it held are closed. Anything still writing to them is a
        // process that inherited the descriptor, and its output is not the child's answer.
        let mut out = String::new();
        let mut err = String::new();
        let mut have = [false; 2];
        while !have[0] || !have[1] {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let Ok((which, text)) = pipes.recv_timeout(left) else {
                break;
            };
            if which == 0 {
                out = text;
            } else {
                err = text;
            }
            have[which] = true;
        }
        if !have[0] || !have[1] {
            return Err(format!(
                "{program} left a pipe open after it exited, within {}s, which is an inherited \
                 descriptor rather than a finished answer; its output is not trusted",
                timeout.as_secs()
            ));
        }
        // Exit 1 is lsof's "matched no file", which with the process-table form above is a complete
        // answer: the whole table was read and nothing was below the prefix. Any other status is a
        // real failure and is never reported as an empty list.
        match status.code() {
            Some(0) | Some(1) => Ok(out),
            Some(code) => Err(format!("{program} exited {code}: {}", tail(&err))),
            None => Err(format!("{program} was killed by a signal")),
        }
    }

    fn tail(s: &str) -> String {
        const MAX: usize = 200;
        let t = s.trim();
        if t.chars().count() <= MAX {
            return t.to_owned();
        }
        let skip = t.chars().count() - MAX;
        format!("...{}", t.chars().skip(skip).collect::<String>())
    }
}

/// A child process holding `file` open, so a test can have a holder that is not this process.
/// `scan` deliberately ignores this process, so a holder has to be somebody else. Reading it
/// waits until the child has the descriptor, so a check cannot race the child.
#[cfg(test)]
#[derive(Debug)]
pub struct Holder(std::process::Child);

#[cfg(test)]
impl Holder {
    pub fn holding(file: &Path) -> Holder {
        Holder::holding_from(&std::env::temp_dir(), file)
    }

    /// The same child, with its working directory somewhere the caller names.
    ///
    /// Point it outside the scanned prefix to get issue #20's case c: a process that has chdir'd
    /// out of the slot and still has the file open. A detector that only looked at working
    /// directories would find nothing, which is the point of the test that uses this.
    pub fn holding_from(cwd: &Path, file: &Path) -> Holder {
        let mut child = std::process::Command::new("/bin/sh")
            .arg("-c")
            // `exec` on the sleep keeps the pid and the descriptor, so killing the one pid the
            // helper knows really does release the file.
            .arg("exec 9< \"$1\"; echo ready; exec sleep 600")
            .arg("sh")
            .arg(file)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("the holder child runs");
        let mut line = String::new();
        {
            use std::io::Read;
            let mut out = child
                .stdout
                .take()
                .expect("the holder says when it is ready");
            let mut byte = [0u8; 1];
            while out.read(&mut byte).unwrap_or(0) == 1 && byte[0] != b'\n' {
                line.push(byte[0] as char);
            }
        }
        assert_eq!(line.trim(), "ready", "the holder did not open the file");
        Holder(child)
    }

    /// The pid the descriptor really belongs to, so a test asserts against the child that was
    /// spawned rather than against whatever the spawn happened to return.
    pub fn pid(&self) -> u32 {
        self.0.id()
    }
}

#[cfg(test)]
impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(target_os = "linux"))]
    use std::process::{Command, Stdio};

    #[test]
    fn a_directory_nobody_holds_has_no_holders() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scan(dir.path()).is_empty());
        assert_eq!(
            scan_checked(dir.path()),
            Scan::Holders(Vec::new()),
            "an enumeration that completed with nothing in it is an answer, not a failure"
        );
        assert!(scan(Path::new("/no/such/dir/anywhere")).is_empty());
    }

    #[test]
    fn a_prefix_that_cannot_be_resolved_is_unavailable_rather_than_clear() {
        // The fail-open this replaces: an unresolvable prefix read back as an empty list, which is
        // indistinguishable from a quiet slot to every caller that gates on it.
        match scan_checked(Path::new("/no/such/dir/anywhere")) {
            Scan::Unavailable(why) => assert!(why.contains("cannot be resolved"), "{why}"),
            other => panic!("expected Unavailable, got {other:?}"),
        }
        assert!(scan(Path::new("/no/such/dir/anywhere")).is_empty());
    }

    /// The case `treehouse return` cannot see: the holder's working directory is outside the prefix,
    /// so nothing but the open descriptor gives it away.
    #[test]
    fn a_holder_that_chdird_out_is_still_reported_and_only_by_its_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let file = real.join("held.txt");
        std::fs::write(&file, b"x").unwrap();
        let holder = Holder::holding_from(outside.path(), &file);

        let found = match scan_checked(&real) {
            Scan::Holders(found) => found,
            other => panic!("this machine must be able to scan: {other:?}"),
        };
        let mine = found
            .iter()
            .find(|p| p.pid == holder.pid())
            .unwrap_or_else(|| panic!("the holder was not reported: {found:?}"));
        let kinds: Vec<HoldKind> = mine.holds.iter().map(|h| h.kind).collect();
        assert!(
            kinds.contains(&HoldKind::Fd),
            "the open descriptor is what identifies it: {mine:?}"
        );
        assert!(
            !kinds.contains(&HoldKind::Cwd),
            "its working directory is elsewhere, which is the whole case: {mine:?}"
        );
        assert!(
            mine.holds
                .iter()
                .all(|h| Path::new(&h.path) == file.as_path()),
            "the hold is the file it still has open: {mine:?}"
        );
        drop(holder);
    }

    /// The review's blocking case. A holder behind a directory the scanning user cannot read used to
    /// be invisible, because `lsof +D` gave up on the walk and said so in a way that looks exactly
    /// like "matched nothing". Nothing walks the mount now, so the permission cannot hide it.
    #[test]
    fn a_holder_behind_an_unreadable_directory_is_still_reported() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let locked = real.join("locked");
        std::fs::create_dir(&locked).unwrap();
        let file = locked.join("held.txt");
        std::fs::write(&file, b"x").unwrap();
        let holder = Holder::holding_from(outside.path(), &file);

        // Everything readable first, so a failure below cannot be blamed on the scan.
        assert!(
            matches!(scan_checked(&real), Scan::Holders(ref h) if h.iter().any(|p| p.pid == holder.pid())),
            "the holder must be visible before the directory is closed"
        );

        use std::os::unix::fs::PermissionsExt;
        let mut mode = std::fs::metadata(&locked).unwrap().permissions().mode();
        mode &= !0o777;
        std::fs::set_permissions(&locked, PermissionsExt::from_mode(mode)).unwrap();
        if std::fs::read_dir(&locked).is_ok() {
            // Running as a user the kernel lets through, such as root: the denial is not real here,
            // so this says nothing rather than pretending to prove the fix.
            eprintln!("skipping: the closed directory is still readable by this user");
            drop(holder);
            return;
        }

        let found = scan_checked(&real);
        std::fs::set_permissions(&locked, PermissionsExt::from_mode(0o755)).unwrap();
        match found {
            Scan::Holders(found) => assert!(
                found.iter().any(|p| p.pid == holder.pid()),
                "an unreadable directory must not hide a holder: {found:?}"
            ),
            // Refusing is also a correct answer; reading it clean is not.
            Scan::Unavailable(why) => eprintln!("refused instead: {why}"),
        }
        drop(holder);
    }

    /// An enumeration that completed, found nothing, and had an unreadable directory to get past is
    /// still a real empty answer rather than a refusal, because completeness does not depend on the
    /// mount being readable.
    #[test]
    fn an_empty_answer_is_still_an_answer_with_an_unreadable_directory_present() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        let locked = real.join("locked");
        std::fs::create_dir(&locked).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut mode = std::fs::metadata(&locked).unwrap().permissions().mode();
        mode &= !0o777;
        std::fs::set_permissions(&locked, PermissionsExt::from_mode(mode)).unwrap();
        let answer = scan_checked(&real);
        std::fs::set_permissions(&locked, PermissionsExt::from_mode(0o755)).unwrap();
        match answer {
            Scan::Holders(found) => {
                // Only this process can be inside its own tempdir, and the daemon excludes itself.
                assert!(found.is_empty(), "nothing else is holding it: {found:?}")
            }
            Scan::Unavailable(why) => eprintln!("refused instead: {why}"),
        }
    }

    /// A prefix that leaves the mount is refused after resolution, so a symlink out of the mount
    /// cannot turn the scan into a scan of somewhere else.
    #[test]
    fn a_prefix_that_resolves_outside_the_mount_is_refused() {
        let mount = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = mount.path().join("escape");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        match scan_mounted(mount.path(), &link) {
            Scan::Unavailable(why) => assert!(why.contains("outside the mount"), "{why}"),
            other => panic!("expected Unavailable, got {other:?}"),
        }
        // And the same prefix with no mount to check against is a plain directory.
        assert!(matches!(scan_checked(&link), Scan::Holders(_)));
    }

    /// The first half of containment, which costs no syscall and therefore cannot be what wedges on
    /// a stale mount. A name that leaves the mount is refused before any path is touched.
    #[test]
    fn a_name_that_could_leave_the_mount_is_refused_without_a_syscall() {
        use crate::handler::lexical_child;
        let mount = std::path::Path::new("/mnt");
        assert_eq!(
            lexical_child(mount, "base/.treehouse/pool/1/repo"),
            Some(std::path::PathBuf::from("/mnt/base/.treehouse/pool/1/repo"))
        );
        assert_eq!(
            lexical_child(mount, "base/./slot"),
            Some(std::path::PathBuf::from("/mnt/base/./slot"))
        );
        for bad in [
            "",
            "/etc",
            "../elsewhere",
            "a/../../elsewhere",
            ".",
            "./slot",
            "a//b",
            "a/",
        ] {
            assert_eq!(lexical_child(mount, bad), None, "{bad:?}");
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn lsof_is_present_here_so_the_missing_command_branch_is_the_only_unreachable_one() {
        // `scan_lsof` refuses when there is no runnable lsof. That cannot be produced on a machine
        // that has one, so this asserts the premise rather than pretending to cover the branch.
        assert!(
            imp::lsof_usable(),
            "{} is expected on this platform; without it every scan here is Unavailable",
            imp::LSOF
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn lsof_output_becomes_holds() {
        let text = "p100\ncsh\nfcwd\nn/tmp/pool\nf3\nn/tmp/pool/a/b\nf5u\nn/tmp/pool/a/l\n";
        let got = imp::parse_lsof(text, Path::new("/tmp/pool")).expect("parses");
        assert_eq!(got.len(), 1);
        let p = &got[0];
        assert_eq!(p.pid, 100);
        assert_eq!(p.command, "sh");
        let kinds: Vec<_> = p.holds.iter().map(|h| (h.kind, h.path.as_str())).collect();
        assert!(kinds.contains(&(HoldKind::Cwd, "/tmp/pool")), "{kinds:?}");
        assert!(
            kinds.contains(&(HoldKind::Fd, "/tmp/pool/a/b")),
            "{kinds:?}"
        );
        assert!(
            kinds.contains(&(HoldKind::Lock, "/tmp/pool/a/l")),
            "{kinds:?}"
        );
    }

    /// A process-table listing with nothing below the prefix is the common clean case, and it must
    /// parse to an empty list rather than an error.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn a_complete_listing_with_nothing_below_the_prefix_is_empty_not_an_error() {
        let text = "p1\nclaunchd\nfcwd\nn/\nf3\nn/dev/null\np2\ncsh\nfcwd\nn/tmp\n";
        assert!(imp::parse_lsof(text, Path::new("/tmp/pool"))
            .expect("parses")
            .is_empty());
    }

    /// Every way the output can be short of an answer has to be an error, because each of them used
    /// to be an empty list.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn output_that_is_not_a_complete_answer_is_refused() {
        let prefix = Path::new("/tmp/pool");
        // Empty is fine: an enumeration that matched nothing.
        assert!(imp::parse_lsof("", prefix)
            .expect("empty parses")
            .is_empty());
        for bad in [
            "p100\ncsh\nz3\nn/tmp/pool/a\n",           // an unknown tag
            "p100\ncsh\nf3\nn/tmp/pool/a\npnotapid\n", // a record with no usable pid
            "f3\nn/tmp/pool/a\n",                      // a name before any pid
            "p0\ncsh\nf3\nn/tmp/pool/a\n",             // pid 0
            "p100\ncsh\nn/tmp/pool/a\n",               // a name with no descriptor
            "100\ncsh\nf3\nn/tmp/pool/a\n",            // a record with no field tag at all
        ] {
            assert!(imp::parse_lsof(bad, prefix).is_err(), "must refuse {bad:?}");
        }
        // A descriptor with no name is not incomplete: an anonymous mapping has one, and lsof
        // reports it as `f` alone. Refusing those would refuse every complete answer.
        assert!(imp::parse_lsof("p100\ncsh\nf3\n", prefix)
            .expect("an unnamed descriptor is complete")
            .is_empty());
    }

    /// The deadline has to cover the child's wait and both pipes, and a pipe that never reaches end
    /// of file has to be an error rather than whatever had already arrived.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn a_command_that_cannot_be_run_or_never_finishes_is_unknown_not_empty() {
        let mut missing = Command::new("/no/such/lsof");
        let why = run(&mut missing, Duration::from_secs(10)).expect_err("no binary");
        assert!(why.contains("cannot run"), "{why}");

        let started = Instant::now();
        let mut wedged = Command::new("/bin/sh");
        wedged.args(["-c", "sleep 30"]);
        let why = run(&mut wedged, Duration::from_millis(150)).expect_err("must time out");
        assert!(why.contains("did not exit within"), "{why}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the deadline fired, it did not wait for the child: {:?}",
            started.elapsed()
        );
    }

    /// A child that exits while something else still holds its stdout: the output is not that
    /// child's answer, and reading back whatever arrived is exactly the false clean being removed.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn an_inherited_pipe_is_not_a_finished_answer() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sh -c 'sleep 4' & echo started"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        // The backgrounded shell keeps the write end of stdout open after its parent exits, so the
        // parent has gone and the pipe has not.
        let timeout = Duration::from_millis(600);
        let deadline = Instant::now() + timeout;
        let pipes = imp::drain_pipes(&mut child);
        let why =
            imp::collect(&mut child, pipes, "sh", timeout, deadline).expect_err("pipe stays open");
        assert!(why.contains("left a pipe open"), "{why}");
        let _ = child.kill();
        let _ = child.wait();
    }

    /// A non-zero exit other than lsof's own "matched nothing" is a failure, and its reason is
    /// passed on rather than dropped.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn an_unexpected_exit_status_is_a_failure_with_its_reason() {
        let mut failed = Command::new("/bin/sh");
        failed.args(["-c", "echo boom >&2; exit 3"]);
        let why = run(&mut failed, Duration::from_secs(10)).expect_err("must fail");
        assert!(why.contains("exited 3"), "{why}");
        assert!(why.contains("boom"), "the reason is passed on: {why}");

        // Exit 1 with a complete, empty listing is lsof's "matched nothing" and stays an answer.
        let mut matched_nothing = Command::new("/bin/sh");
        matched_nothing.args(["-c", "exit 1"]);
        assert_eq!(
            run(&mut matched_nothing, Duration::from_secs(10)),
            Ok(String::new())
        );
    }

    #[cfg(not(target_os = "linux"))]
    fn run(cmd: &mut Command, timeout: Duration) -> Result<String, String> {
        imp::run_bounded(cmd, timeout)
    }

    /// Real `/proc/locks` rows, captured from a live `flock(1)` on a Linux 6.12 aarch64 host, with
    /// the device field where the kernel actually writes it and major and minor in hex. The first
    /// row has a hex minor that is not decimal-parseable as itself; the last has a waiting lock,
    /// which inserts `-> PID` between the pid and the device.
    #[cfg(target_os = "linux")]
    #[test]
    fn proc_locks_rows_are_read_from_the_field_the_kernel_writes() {
        let rows = [
            "27: FLOCK  ADVISORY  WRITE 1327043 00:40:228185 0 EOF",
            "31: FLOCK  ADVISORY  WRITE 4231 08:01:1234567 0 EOF  -> 4100",
            "32: -1      OFDLCK  WRITETRUNC -1 00:ff:99 0 100",
            "33: POSIX   MANDATORY READ  900 a:f:2d 0 EOF",
            "34: FLOCK  ADVISORY  WRITE 7 08:02:9 0 EOF",
        ];
        let mut got: Vec<(u64, u64, u64)> = Vec::new();
        for row in rows {
            let parsed =
                super::imp::device_of(row).unwrap_or_else(|| panic!("no device in {row:?}"));
            got.push(parsed);
        }
        assert_eq!(got[0], (0, 0x40, 228185), "hex minor, decimal inode");
        assert_eq!(
            got[1],
            (0x08, 0x01, 1234567),
            "a waiting lock moves the device field, and 08 01 are hex"
        );
        assert_eq!(got[2], (0, 0xff, 99), "an OFD lock has pid -1");
        assert_eq!(got[3], (10, 15, 45), "a and f are hex digits, not decimal");
        assert_eq!(got[4], (0x08, 0x02, 9));
        // The trap the review found: reading the device from the fourth field yields the word
        // ADVISORY and nothing else.
        assert!(rows[0].split_whitespace().nth(2) == Some("ADVISORY"));
    }

    /// A row this build does not understand must refuse the whole table rather than silently
    /// contributing no locks, because no locks reads as "nobody holds a lock".
    #[cfg(target_os = "linux")]
    #[test]
    fn a_lock_row_that_is_not_understood_is_not_silently_zero() {
        for bad in [
            "",
            "27: FLOCK  ADVISORY  WRITE 1327043",
            "27: FLOCK  ADVISORY  WRITE 1327043 zz:40:1 0 EOF",
            "27: FLOCK  ADVISORY  WRITE 1327043 00:zz:1 0 EOF",
            "27: FLOCK  ADVISORY  WRITE 1327043 00:40:xx 0 EOF",
        ] {
            assert_eq!(
                super::imp::device_of(bad),
                None,
                "must refuse {bad:?} rather than guess"
            );
        }
    }

    /// A real `flock`, taken by a real other process, reported as a lock hold on the right file.
    ///
    /// The parser tests above use captured rows; this one is the end of the chain on a real Linux,
    /// because the field index and the hex radix were both wrong for long enough that no test could
    /// have noticed: the head parser read the word `ADVISORY` as the device and produced an empty
    /// set, which is the same as reporting no locks.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_real_flock_is_reported_as_a_lock_hold() {
        use std::io::Read;
        use std::process::{Command, Stdio};
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().canonicalize().unwrap();
        let file = real.join("locked");
        std::fs::write(&file, b"x").unwrap();
        // A fifo with no writer: the holder blocks on opening it, without forking, so killing it
        // releases the only lock there is and the assertion below is about the scan and not about
        // an orphan.
        let wait = dir.path().join("wait");
        let fifo = wait.to_string_lossy().into_owned();

        let holder = Command::new("/usr/bin/flock")
            .args(["-x", &file.to_string_lossy(), "/bin/sh", "-c"])
            .arg(format!("echo locked; read line < {fifo}"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut holder) = holder else {
            eprintln!(
                "skipping: no /usr/bin/flock on this machine, so no real lock to find; the parser \
                 is covered by the captured rows above"
            );
            return;
        };
        let mut line = String::new();
        {
            let mut byte = [0u8; 1];
            let out = holder.stdout.as_mut().expect("piped");
            while out.read(&mut byte).unwrap_or(0) == 1 && byte[0] != b'\n' {
                line.push(byte[0] as char);
            }
        }
        assert_eq!(line.trim(), "locked", "the holder never took the lock");

        let found = match scan_checked(&real) {
            Scan::Holders(found) => found,
            // A refusal is a legitimate answer on a table this build cannot read, but it does not
            // prove the lock was found, so it must not pass as one.
            other => {
                let _ = holder.kill();
                let _ = holder.wait();
                panic!("the scan did not complete: {other:?}");
            }
        };
        let mine = found
            .iter()
            .flat_map(|p| &p.holds)
            .find(|h| h.kind == HoldKind::Lock && Path::new(&h.path) == file.as_path())
            .unwrap_or_else(|| {
                panic!(
                    "the real flock on {} was not reported as a lock hold: {found:?}",
                    file.display()
                )
            });
        assert!(mine.path.ends_with("locked"), "{mine:?}");
        let _ = holder.kill();
        let _ = holder.wait();
        // Once the lock is gone the answer is empty again, which is the difference between a
        // reported lock and an empty table that happens to look the same.
        match scan_checked(&real) {
            Scan::Holders(after) => assert!(
                !after
                    .iter()
                    .flat_map(|p| &p.holds)
                    .any(|h| h.kind == HoldKind::Lock && Path::new(&h.path) == file.as_path()),
                "the lock is released, so it must not still be reported: {after:?}"
            ),
            other => eprintln!("refused instead: {other:?}"),
        }
    }
}
