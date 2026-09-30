use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::error::{Error, Result};

/// What `treehouse get --lease --json` reports. Verified against treehouse v3.1.0.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Lease {
    /// Absolute path of the leased worktree.
    pub path: PathBuf,
    /// Immutable identity of this acquisition, for `treehouse return --if-lease-id`.
    #[serde(default)]
    pub lease_id: String,
    /// Optional human-readable label recorded at acquisition.
    #[serde(default)]
    pub lease_holder: String,
    /// The base branch the slot was cut from.
    #[serde(default)]
    pub base_branch: String,
}

/// One entry of `treehouse status --json`.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
pub struct PoolEntry {
    /// Slot name, as the first column of `treehouse status` prints it.
    #[serde(default)]
    pub name: String,
    /// Absolute worktree path.
    #[serde(default)]
    pub path: String,
    /// True while a lease or owner reservation protects the slot.
    #[serde(default)]
    pub leased: bool,
    /// Lease identity, when leased.
    #[serde(default)]
    pub lease_id: String,
    /// True while the slot is being destroyed.
    #[serde(default)]
    pub destroying: bool,
    /// Why an automatic recovery quarantined the slot.
    #[serde(default)]
    pub recovery_reason: String,
}

/// The `treehouse` binary, always driven with an explicit pool root.
///
/// Every invocation carries `--root` and `TREEHOUSE_ROOT`, and optionally a sandbox `HOME`, so a
/// caller cannot reach a pool it did not name. A relative root is refused because treehouse
/// resolves it against the repository root, which would make the target depend on the cwd.
#[derive(Clone, Debug)]
pub struct Treehouse {
    bin: PathBuf,
    root: PathBuf,
    home: Option<PathBuf>,
    timeout: Duration,
}

impl Treehouse {
    /// Binds the binary to a pool root. `home` sandboxes the whole treehouse config and store
    /// lookup, which is what a test needs to keep away from a real pool.
    pub fn new(bin: PathBuf, root: &Path, home: Option<PathBuf>) -> Result<Treehouse> {
        if !root.is_absolute() {
            return Err(Error::Usage(format!(
                "treehouse --root {} must be an absolute path",
                root.display()
            )));
        }
        if root.as_os_str().is_empty() {
            return Err(Error::Usage("treehouse --root must not be empty".into()));
        }
        Ok(Treehouse {
            bin,
            root: root.to_path_buf(),
            home,
            timeout: Duration::from_secs(120),
        })
    }

    /// The pool root every call of this instance uses.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Overrides how long a single `treehouse` call may take.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args).arg("--root").arg(&self.root);
        cmd.env("TREEHOUSE_ROOT", &self.root);
        if let Some(home) = &self.home {
            cmd.env("HOME", home);
        }
        // Piped explicitly: `Command::output` does this for you, but `wait_with_output` on a
        // spawned command inherits the parent's streams, so without it every treehouse reply would
        // be printed to the companion's own terminal and read back as empty.
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd
    }

    /// Runs a `treehouse` subcommand and returns its stdout, failing on a non-zero exit.
    pub fn run(&self, args: &[&str]) -> Result<String> {
        self.run_in(None, args)
    }

    /// Runs a `treehouse` subcommand from `cwd`, which matters because the pool is resolved from
    /// the repository the caller stands in.
    pub fn run_in(&self, cwd: Option<&Path>, args: &[&str]) -> Result<String> {
        let mut cmd = self.command(args);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        let out = wait_with_timeout(cmd, self.timeout)
            .map_err(|e| Error::Treehouse(format!("cannot run {}: {e}", self.bin.display())))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if !out.status.success() {
            return Err(Error::Treehouse(format!(
                "`treehouse {}` failed: {}",
                args.join(" "),
                tail(&stderr_of(&out))
            )));
        }
        Ok(stdout)
    }

    /// `treehouse get --lease --json`, run from `repo`.
    pub fn get_lease(&self, repo: &Path, extra: &[&str]) -> Result<Lease> {
        let mut args: Vec<&str> = vec!["get", "--lease", "--json"];
        args.extend_from_slice(extra);
        let out = self.run_in(Some(repo), &args)?;
        last_json(&out).ok_or_else(|| {
            Error::Treehouse(format!(
                "`treehouse get --lease --json` printed no JSON object, only: {}",
                tail(&out)
            ))
        })
    }

    /// `treehouse status --json`, run from `repo`.
    pub fn status(&self, repo: &Path) -> Result<Vec<PoolEntry>> {
        let out = self.run_in(Some(repo), &["status", "--json"])?;
        last_json(&out).ok_or_else(|| {
            Error::Treehouse(format!(
                "`treehouse status --json` printed no JSON array, only: {}",
                tail(&out)
            ))
        })
    }

    /// `treehouse return <path> --force`, pinned to a lease identity when one is known so a slot
    /// that was re-leased since we looked is left alone.
    pub fn return_slot(&self, path: &Path, force: bool, if_lease_id: Option<&str>) -> Result<()> {
        let path = path.display().to_string();
        let mut args: Vec<&str> = vec!["return", &path];
        if force {
            args.push("--force");
        }
        if let Some(id) = if_lease_id.filter(|id| !id.is_empty()) {
            args.push("--if-lease-id");
            args.push(id);
        }
        self.run(&args).map(|_| ())
    }

    /// `treehouse destroy <path> --yes`. Destroy is a dry run without `--yes`, so this never
    /// removes anything by accident.
    pub fn destroy(&self, path: &Path) -> Result<String> {
        let path = path.display().to_string();
        self.run(&["destroy", &path, "--yes"])
    }

    /// `treehouse --version`.
    pub fn version(&self) -> Result<String> {
        self.run(&["--version"]).map(|s| s.trim().to_owned())
    }
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn tail(s: &str) -> String {
    const MAX: usize = 400;
    let t = s.trim();
    if t.chars().count() <= MAX {
        return t.to_owned();
    }
    let skip = t.chars().count() - MAX;
    format!("...{}", t.chars().skip(skip).collect::<String>())
}

/// The last line of `out` that parses as JSON, which is how the `cowfs` CLI copes with banners.
fn last_json<T: for<'de> Deserialize<'de>>(out: &str) -> Option<T> {
    out.lines()
        .rev()
        .filter(|l| l.trim_start().starts_with('{') || l.trim_start().starts_with('['))
        .find_map(|l| serde_json::from_str::<T>(l.trim()).ok())
}

fn wait_with_timeout(mut cmd: Command, timeout: Duration) -> std::io::Result<std::process::Output> {
    use std::sync::mpsc;
    let child = cmd.spawn()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("no answer within {}s", timeout.as_secs()),
        )),
    }
}

/// The `treehouse` binary name, overridable with `COWFS_TREEHOUSE_BIN`.
pub fn default_bin() -> PathBuf {
    match std::env::var_os("COWFS_TREEHOUSE_BIN") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("treehouse"),
    }
}

/// The pool root treehouse would use for `repo` when `--root` is not given: `$HOME/.treehouse`.
/// Printed by `setup` and `doctor` so an operator can see what a bare `treehouse` call would touch.
pub fn implicit_root(home: Option<&Path>) -> PathBuf {
    let home = match home {
        Some(h) => h.to_path_buf(),
        None => std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from),
    };
    home.join(".treehouse")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_captures_stdout_rather_than_inheriting_it() {
        // The bug this covers: with inherited streams, `treehouse status --json` printed its array
        // to the companion's own terminal and every caller read back an empty string.
        let dir = std::env::temp_dir();
        let th = Treehouse::new(PathBuf::from("/bin/echo"), dir.as_path(), None).expect("th");
        let out = th.run(&["hello", "world"]).expect("run");
        assert!(out.starts_with("hello world"), "{out}");
        // The explicit --root is always appended, which is what keeps a call inside its pool.
        assert!(out.contains("--root"), "{out}");
    }

    #[test]
    fn a_failing_command_reports_its_stderr() {
        let dir = std::env::temp_dir();
        let th = Treehouse::new(PathBuf::from("/bin/sh"), dir.as_path(), None).expect("th");
        let err = th.run(&["-c", "echo boom >&2; exit 3"]).expect_err("fails");
        assert!(err.to_string().contains("boom"), "{err}");
    }

    #[test]
    fn a_relative_root_is_refused() {
        let err = Treehouse::new(PathBuf::from("treehouse"), Path::new("rel"), None);
        assert!(matches!(err, Err(Error::Usage(_))), "{err:?}");
    }

    #[test]
    fn last_json_skips_banner_lines() {
        let out = "🌳 base  main\nnoise\n{\"path\":\"/p\",\"lease_id\":\"abc\"}\n";
        let lease: Lease = last_json(out).expect("parsed");
        assert_eq!(lease.path, PathBuf::from("/p"));
        assert_eq!(lease.lease_id, "abc");
    }

    #[test]
    fn last_json_of_an_empty_pool_is_an_empty_array() {
        let entries: Vec<PoolEntry> = last_json("[]").expect("parsed");
        assert!(entries.is_empty());
    }

    #[test]
    fn last_json_returns_none_when_there_is_none() {
        let entries: Option<Vec<PoolEntry>> = last_json("🌳 No worktrees in pool.");
        assert!(entries.is_none());
        let lease: Option<Lease> = last_json("not json");
        assert!(lease.is_none());
    }

    #[test]
    fn tail_is_bounded() {
        let long = "x".repeat(1000);
        let t = tail(&long);
        assert!(t.len() < 500, "{}", t.len());
        assert!(t.starts_with("..."));
    }

    #[test]
    fn implicit_root_follows_home() {
        assert_eq!(
            implicit_root(Some(Path::new("/sandbox/home"))),
            PathBuf::from("/sandbox/home/.treehouse")
        );
    }
}
