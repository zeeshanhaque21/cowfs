//! Who holds a path: a process with its working directory, an open file or a lock inside it.
//!
//! Linux reads `/proc`, which is exact and cheap. macOS has to ask `lsof`, because a process
//! can hold a descriptor the kernel NFS client opened and there is no `/proc` equivalent
//! there. Both scan only what is below the prefix and report one `ProcessInfo` per pid with
//! its holds.

use cowfs_ctl::{Hold, HoldKind, ProcessInfo};
use std::path::Path;
use std::time::Duration;

/// How long a scan may take before it answers with what it has. `busy` is recoverable and a
/// caller retries, so a bound beats a hang.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// What a scan established.
///
/// `Holders` with an empty list and `Unavailable` are different answers, and only the first one may
/// unblock a change. `lsof` can be missing, unrunnable or wedged on a stale mount, and `/proc` can
/// be unreadable in a container; every one of those used to read back as "nobody is holding
/// anything", which is how issue #20 turns into a reset under a live writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scan {
    /// The scan ran. The list is everything it found, which may legitimately be empty.
    Holders(Vec<ProcessInfo>),
    /// This platform could not be asked, so nothing is assumed in either direction.
    Unavailable(String),
}

/// Every process holding something below `prefix`, except this one, and whether that list can be
/// trusted. This is the form a holder gate must use: `Unavailable` means "not known", never "none".
///
/// This process is never a holder: the daemon holds the store and the mount it serves by
/// definition, so counting it would make every `expect_no_holders` check `busy`. What the caller
/// wants to know is who *else* is inside.
pub fn scan_checked(prefix: &Path) -> Scan {
    let canonical = std::fs::canonicalize(prefix);
    let Ok(prefix) = canonical else {
        return Scan::Unavailable(format!(
            "{} cannot be resolved, so nothing below it can be scanned",
            prefix.display()
        ));
    };
    let me = std::process::id();
    let scanned: Result<Vec<ProcessInfo>, String> = {
        #[cfg(target_os = "linux")]
        {
            imp::scan_proc(&prefix)
        }
        #[cfg(not(target_os = "linux"))]
        {
            imp::scan_lsof(&prefix, SCAN_TIMEOUT)
        }
    };
    let mut out = match scanned {
        Ok(out) => out,
        Err(why) => return Scan::Unavailable(why),
    };
    out.retain(|p| p.pid != me);
    out.sort_by_key(|p| p.pid);
    Scan::Holders(out)
}

/// Best-effort scan for a caller that only reports: an unanswered question becomes an empty list
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

    /// `(major, minor, inode)` triples named by `/proc/locks`, which is how a flock is seen
    /// here: `F_GETLK` needs the fd, and the whole point is a process nobody asked. The device
    /// is split the way the kernel writes it, so it compares with `libc::major` on a `st_dev`.
    ///
    /// A table that cannot be read is an error, not an empty set: an empty set reports every lock
    /// as absent, and a lock held on the mount is enough to block an unmount.
    fn locked() -> Result<BTreeSet<(u64, u64, u64)>, String> {
        let mut out = BTreeSet::new();
        let text = std::fs::read_to_string("/proc/locks")
            .map_err(|e| format!("/proc/locks cannot be read, so a flock would be invisible: {e}"))?;
        for line in text.lines() {
            let mut f = line.split_whitespace();
            let (Some(_kind), Some(_pid), Some(dev), Some(ino)) =
                (f.next(), f.next(), f.next(), f.next())
            else {
                continue;
            };
            let device = dev
                .split_once(':')
                .and_then(|(_maj, rest)| rest.split_once(':'))
                .and_then(|(maj, min)| Some((maj.parse().ok()?, min.parse().ok()?)));
            if let (Some((maj, min)), Some(ino)) = (device, ino.parse::<u64>().ok()) {
                out.insert((maj, min, ino));
            }
        }
        out
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
    use super::{finish, hold, Duration, Hold, HoldKind, Path, ProcessInfo};
    use std::collections::BTreeMap;

    pub const LSOF: &str = "/usr/sbin/lsof";

    /// Whether there is an `lsof` here to run at all.
    ///
    /// Checked before spawning rather than inferred from a failed spawn, because "the command is
    /// not there" and "the command could not answer" are different operator problems and the
    /// message has to name which one it is.
    pub fn lsof_usable() -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(LSOF)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }

    pub fn scan_lsof(prefix: &Path, timeout: Duration) -> Result<Vec<ProcessInfo>, String> {
        if !lsof_usable() {
            return Err(format!(
                "no runnable {LSOF} on this machine, so open files and flocks cannot be listed \
                 here; install lsof or run the daemon where it exists rather than treating the \
                 slot as clear"
            ));
        }
        let mut cmd = std::process::Command::new(LSOF);
        // `+D` recurses, so a holder of anything under the prefix is reported; a bare path
        // argument would only see the directory itself.
        cmd.args(["-nP", "-w", "-F", "pcfn", "+D"]).arg(prefix);
        Ok(parse_lsof(&run_bounded(&mut cmd, timeout)?, prefix))
    }

    /// `lsof -F pcfn` emits one tag per line: `p` pid, `c` command, `f` descriptor, `n` name.
    /// The descriptor is `cwd` for a working directory, digits for an open file, and digits
    /// with a letter appended when the descriptor also holds a lock (`3u` is read-write).
    pub fn parse_lsof(text: &str, prefix: &Path) -> Vec<ProcessInfo> {
        let mut by_pid: BTreeMap<u32, (String, Vec<Hold>)> = BTreeMap::new();
        let (mut pid, mut command, mut fd) = (0u32, String::new(), String::new());
        for line in text.lines() {
            let Some((tag, value)) = line.split_at_checked(1) else {
                continue;
            };
            match tag {
                "p" => {
                    pid = value.trim().parse().unwrap_or(0);
                    command.clear();
                    by_pid.entry(pid).or_default();
                }
                "c" => command = value.to_owned(),
                "f" => fd = value.to_owned(),
                "n" => {
                    let kind = match fd.as_str() {
                        "cwd" => HoldKind::Cwd,
                        d if d.chars().find(|c| !c.is_ascii_digit()).is_some() => HoldKind::Lock,
                        _ => HoldKind::Fd,
                    };
                    if let Some(h) = hold(prefix, Path::new(value), kind) {
                        let entry = by_pid.entry(pid).or_default();
                        entry.0 = command.clone();
                        entry.1.push(h);
                    }
                }
                _ => {}
            }
        }
        by_pid
            .into_iter()
            .filter_map(|(pid, (command, holds))| finish(pid, command, holds))
            .collect()
    }

    /// `lsof` on a dead mount can block for ever, so it gets a deadline and a kill like every
    /// other command this daemon runs.
    ///
    /// The pipes are drained on their own thread because a wedged `lsof` never closes stdout:
    /// reading it to end on this one would make the deadline unreachable and hang the daemon
    /// instead of reporting that the answer is unknown.
    pub fn run_bounded(cmd: &mut std::process::Command, timeout: Duration) -> Result<String, String> {
        use std::io::Read;
        use std::process::Stdio;
        use std::sync::mpsc;
        let program = cmd.get_program().to_string_lossy().into_owned();
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot run {program}: {e}"))?;
        // One thread per pipe, so neither can fill its buffer while the other is being read. The two
        // streams get a channel each: sharing one would make which answer arrived first decide which
        // of them was parsed as the scan.
        let (out_tx, out_rx) = mpsc::channel();
        let (err_tx, err_rx) = mpsc::channel();
        if let Some(mut pipe) = child.stdout.take() {
            std::thread::spawn(move || {
                let mut text = String::new();
                let _ = pipe.read_to_string(&mut text);
                let _ = out_tx.send(text);
            });
        }
        if let Some(mut pipe) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut text = String::new();
                let _ = pipe.read_to_string(&mut text);
                let _ = err_tx.send(text);
            });
        }
        let deadline = std::time::Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if std::time::Instant::now() >= deadline => {
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
                "{program} did not finish within {}s, which is what a wedged mount looks like; \
                 nothing is assumed about who holds {}",
                timeout.as_secs(),
                cmd.get_args()
                    .last()
                    .map_or_else(String::new, |a| a.to_string_lossy().into_owned())
            ));
        };
        // The child is reaped here so it never sits as a zombie, and its pipes are already closed
        // by a process that exited, so the drains below return without waiting.
        let text = out_rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
        let err = err_rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
        // lsof exits 1 when it matched nothing, which is an answer. Every other failure used to
        // read back as "no holders", which is the fail-open this whole path exists to remove.
        match status.code() {
            Some(0) | Some(1) => Ok(text),
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

    #[test]
    fn a_directory_nobody_holds_has_no_holders() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scan(dir.path()).is_empty());
        assert_eq!(
            scan_checked(dir.path()),
            Scan::Holders(Vec::new()),
            "a scanned directory with nobody in it is an answer, not a failure"
        );
        assert!(scan(Path::new("/no/such/dir/anywhere")).is_empty());
    }

    #[test]
    fn a_prefix_that_cannot_be_resolved_is_unavailable_rather_than_clear() {
        // The fail-open this replaces: an unresolvable prefix used to read back as an empty list,
        // which is indistinguishable from a quiet slot to every caller that gates on it.
        match scan_checked(Path::new("/no/such/dir/anywhere")) {
            Scan::Unavailable(why) => {
                assert!(why.contains("cannot be resolved"), "{why}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
        // The best-effort entry point keeps its contract for the reporting callers, and says so.
        assert!(scan(Path::new("/no/such/dir/anywhere")).is_empty());
    }

    /// The case `treehouse return` cannot see: the holder's working directory is outside the
    /// prefix, so nothing but the open descriptor gives it away.
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

    #[test]
    fn a_held_directory_reports_the_holder() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        let sub = real.join("held");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("f"), b"x").unwrap();
        let file = sub.join("f");
        let holder = Holder::holding(&file);
        let found = scan(&real);
        assert!(
            found.iter().any(|p| p.pid != std::process::id()),
            "the process holding the file was not reported: {found:?}"
        );
        assert!(
            !found.iter().any(|p| p.pid == std::process::id()),
            "this process must not be reported as a holder: {found:?}"
        );
        assert!(
            found
                .iter()
                .flat_map(|p| &p.holds)
                .any(|h| Path::new(&h.path).starts_with(&real)),
            "no hold below the prefix: {found:?}"
        );
        drop(holder);
    }

    #[cfg(test)]
    impl Holder {
        /// The pid the descriptor really belongs to, so a test asserts against the child that was
        /// spawned rather than against whatever the spawn happened to return.
        pub fn pid(&self) -> u32 {
            self.0.id()
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
    fn a_command_that_matched_nothing_is_an_answer_and_a_failure_is_not() {
        use std::process::Command;
        // lsof exits 1 when it matched no file. Treating that as a failure would block every clean
        // slot; treating a real failure as exit 1 is the fail-open being removed.
        let mut matched_nothing = Command::new("/bin/sh");
        matched_nothing.args(["-c", "exit 1"]);
        assert_eq!(
            imp::run_bounded(&mut matched_nothing, Duration::from_secs(10)),
            Ok(String::new())
        );

        let mut failed = Command::new("/bin/sh");
        failed.args(["-c", "echo boom >&2; exit 3"]);
        let why = imp::run_bounded(&mut failed, Duration::from_secs(10)).expect_err("must fail");
        assert!(why.contains("exited 3"), "{why}");
        assert!(why.contains("boom"), "the reason is passed on: {why}");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn a_command_that_cannot_be_run_or_never_finishes_is_unknown_not_empty() {
        use std::process::Command;
        let mut missing = Command::new("/no/such/lsof");
        let why = imp::run_bounded(&mut missing, Duration::from_secs(10)).expect_err("no binary");
        assert!(why.contains("cannot run"), "{why}");

        // A wedged mount is what a real `lsof +D` looks like: it never exits and never closes its
        // pipe. The deadline has to fire, which is only true because the pipes are drained off the
        // thread that waits.
        let started = std::time::Instant::now();
        let mut wedged = Command::new("/bin/sh");
        wedged.args(["-c", "sleep 30"]);
        let why = imp::run_bounded(&mut wedged, Duration::from_millis(150)).expect_err("must time out");
        assert!(why.contains("did not finish"), "{why}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the deadline fired, it did not wait for the child: {:?}",
            started.elapsed()
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn lsof_output_becomes_holds() {
        let text = "p100\ncsh\nfcwd\nn/tmp/pool\nf3\nn/tmp/pool/a/b\nf5u\nn/tmp/pool/a/l\n";
        let got = imp::parse_lsof(text, Path::new("/tmp/pool"));
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
}
