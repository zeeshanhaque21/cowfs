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

/// Every process holding something below `prefix`.
pub fn scan(prefix: &Path) -> Vec<ProcessInfo> {
    let Ok(prefix) = std::fs::canonicalize(prefix) else {
        return Vec::new();
    };
    #[cfg(target_os = "linux")]
    let mut out = imp::scan_proc(&prefix);
    #[cfg(target_os = "macos")]
    let mut out = imp::scan_lsof(&prefix, SCAN_TIMEOUT).unwrap_or_default();
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let mut out: Vec<ProcessInfo> = Vec::new();
    out.sort_by_key(|p| p.pid);
    out
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
    fn locked() -> BTreeSet<(u64, u64, u64)> {
        let mut out = BTreeSet::new();
        let Ok(text) = std::fs::read_to_string("/proc/locks") else {
            return out;
        };
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

    pub fn scan_proc(prefix: &Path) -> Vec<ProcessInfo> {
        use std::os::unix::fs::MetadataExt;
        let locks = locked();
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return out;
        };
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
        out
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::{finish, hold, Duration, Hold, HoldKind, Path, ProcessInfo};
    use std::collections::BTreeMap;

    pub const LSOF: &str = "/usr/sbin/lsof";

    pub fn scan_lsof(prefix: &Path, timeout: Duration) -> Option<Vec<ProcessInfo>> {
        let mut cmd = std::process::Command::new(LSOF);
        // `+D` recurses, so a holder of anything under the prefix is reported; a bare path
        // argument would only see the directory itself.
        cmd.args(["-nP", "-w", "-F", "pcfn", "+D"]).arg(prefix);
        run_bounded(&mut cmd, timeout).map(|out| parse_lsof(&out, prefix))
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
    fn run_bounded(cmd: &mut std::process::Command, timeout: Duration) -> Option<String> {
        use std::io::Read;
        use std::process::Stdio;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn().ok()?;
        let mut text = String::new();
        if let Some(mut pipe) = child.stdout.take() {
            let _ = pipe.read_to_string(&mut text);
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return Some(text),
                Ok(None) if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_nobody_holds_has_no_holders() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scan(dir.path()).is_empty());
        assert!(scan(Path::new("/no/such/dir/anywhere")).is_empty());
    }

    #[test]
    fn a_held_directory_reports_the_holder() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        let sub = real.join("held");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("f"), b"x").unwrap();
        let file = std::fs::File::open(sub.join("f")).unwrap();
        let found = scan(&real);
        assert!(
            found.iter().any(|p| p.pid == std::process::id()),
            "the process holding the file was not reported: {found:?}"
        );
        assert!(
            found
                .iter()
                .flat_map(|p| &p.holds)
                .any(|h| Path::new(&h.path).starts_with(&real)),
            "no hold below the prefix: {found:?}"
        );
        drop(file);
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
