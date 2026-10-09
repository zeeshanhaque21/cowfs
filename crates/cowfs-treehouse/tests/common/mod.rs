//! Shared fixtures: an in-process stub daemon, a throwaway git repository, and a sandboxed
//! treehouse pool behind a safety shim.
//!
//! Every treehouse invocation in these tests goes through [`Sandbox::shim`], a script generated per
//! sandbox that refuses any call without an explicit `--root` inside that sandbox, refuses any
//! absolute path argument outside it, and forces the sandbox `HOME` and `TREEHOUSE_ROOT`. The real
//! store under `~/.treehouse` is leased to other agents, so the guard has to be in the test itself
//! rather than in a habit.

#![allow(dead_code)]

use cowfs_ctl::{ControlHandler, Server, ServerOptions, StubHandler};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

/// A sibling of the test binary, which is where cargo puts the binaries it built with it.
pub fn sibling_bin(name: &str) -> Option<PathBuf> {
    let mut path = std::env::current_exe().ok()?;
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    let candidate = path.join(name);
    candidate.is_file().then_some(candidate)
}

/// A sibling binary, or a hard failure.
///
/// A missing binary is a wrong invocation, not a host limitation: libtest reports a test that
/// printed "skipping" and returned as `ok` (issue 244), so it fails whatever the mode is.
pub fn require_bin(name: &str) -> PathBuf {
    sibling_bin(name).unwrap_or_else(|| {
        panic!(
            "{name} is not beside this test binary at {}. Build it in the same invocation \
             (`cargo test -p cowfs-treehouse -p cowfs-cli -p cowfs-daemon` or `--workspace`); a \
             missing binary is never a pass.",
            std::env::current_exe()
                .unwrap_or_else(|_| PathBuf::from("<unknown>"))
                .display(),
        )
    })
}

/// The real treehouse store on this machine, or wherever `TREEHOUSE_REAL_STORE` points it. Only
/// ever read: it belongs to other agents.
pub fn real_treehouse_root() -> PathBuf {
    match std::env::var_os("TREEHOUSE_REAL_STORE").filter(|v| !v.is_empty()) {
        Some(v) => PathBuf::from(v),
        None => std::env::var_os("HOME")
            .map_or_else(|| PathBuf::from("/"), PathBuf::from)
            .join(".treehouse"),
    }
}

/// Why the real treehouse binary cannot be used, when it cannot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreehouseMissing {
    /// Nothing on PATH, nothing in the usual place.
    NotFound,
    /// Something is there but does not answer `--version`.
    Unusable(String),
    /// Older than [`MIN_TREEHOUSE`], which first had `get --lease --json`.
    TooOld(String),
}

impl std::fmt::Display for TreehouseMissing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TreehouseMissing::NotFound => f.write_str(
                "no treehouse binary on PATH, and none at $HOME/.local/bin/treehouse; set \
                 COWFS_TREEHOUSE_BIN to run these tests",
            ),
            TreehouseMissing::Unusable(why) => {
                write!(f, "treehouse is present but unusable: {why}")
            }
            TreehouseMissing::TooOld(why) => f.write_str(why),
        }
    }
}

/// The oldest treehouse these tests can drive: 3.1.0 is the first with `get --lease --json`. An
/// older binary fails 13 of the 15 sandbox tests with "unknown flag: --json".
pub const MIN_TREEHOUSE: (u32, u32, u32) = (3, 1, 0);

/// `major.minor.patch` out of `treehouse --version` output such as `v3.1.2`.
pub fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let core = text.trim().trim_start_matches('v');
    let core = core.split(['-', '+', ' ']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// The real treehouse binary: `COWFS_TREEHOUSE_BIN`, then PATH, then `$HOME/.local/bin`.
///
/// PATH first, because a hardcoded path exists inside an OrbStack VM through a shared mount while
/// naming a binary that cannot run there.
pub fn find_treehouse() -> Result<PathBuf, TreehouseMissing> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(v) = std::env::var_os("COWFS_TREEHOUSE_BIN").filter(|v| !v.is_empty()) {
        candidates.push(PathBuf::from(v));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("treehouse");
            if candidate.is_file() {
                candidates.push(candidate);
                break;
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join(".local/bin/treehouse"));
    }
    for candidate in &candidates {
        if !candidate.is_file() {
            continue;
        }
        let out = Command::new(candidate)
            .arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let Some(out) = out.ok().filter(|o| o.status.success()) else {
            return Err(TreehouseMissing::Unusable(format!(
                "{} does not answer --version",
                candidate.display()
            )));
        };
        let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        return match parse_version(&text) {
            Some(v) if v >= MIN_TREEHOUSE => Ok(candidate.clone()),
            Some(_) | None => Err(TreehouseMissing::TooOld(format!(
                "treehouse {text:?} at {} is older than {}.{}.{} (or its version is unreadable); \
                 `get --lease --json` needs 3.1.0 or newer",
                candidate.display(),
                MIN_TREEHOUSE.0,
                MIN_TREEHOUSE.1,
                MIN_TREEHOUSE.2
            ))),
        };
    }
    Err(TreehouseMissing::NotFound)
}

/// The treehouse binary, or a printed skip reason when there is none.
pub fn treehouse_bin() -> Result<PathBuf, TreehouseMissing> {
    find_treehouse()
}

/// True when the real treehouse binary is usable, so a test can skip rather than fail.
pub fn treehouse_available() -> bool {
    find_treehouse().is_ok()
}

/// True when an absent treehouse must fail the test instead of skipping it: `CI` is set (every CI
/// provider sets it) or `COWFS_REQUIRE_TREEHOUSE` is set to anything but empty or `0`.
///
/// libtest reports a test that printed "skipping" and returned as `ok`, so on a runner that never
/// installed treehouse the whole sandbox family was a silent pass (issue 259). ci.yml installs a
/// pinned treehouse, so on CI an absent one is a broken runner, never a host limitation.
pub fn treehouse_required() -> bool {
    let set = |name: &str, unset: &[&str]| {
        std::env::var(name).is_ok_and(|v| !v.is_empty() && !unset.contains(&v.as_str()))
    };
    set("CI", &[]) || set("COWFS_REQUIRE_TREEHOUSE", &["0"])
}

/// The treehouse binary, `None` (skip) when it is absent and not required, and a panic when it is
/// absent and `required`.
pub fn treehouse_or_skip(
    required: bool,
    found: Result<PathBuf, TreehouseMissing>,
) -> Option<PathBuf> {
    match found {
        Ok(bin) => Some(bin),
        Err(why) if required => panic!(
            "treehouse is required here (CI or COWFS_REQUIRE_TREEHOUSE is set) and is missing: \
             {why}. A skip would report this test as passed (issue 259); install treehouse \
             (scripts/install-treehouse.sh) or fix PATH."
        ),
        Err(why) => {
            eprintln!(
                "skipping {}: {why}",
                std::thread::current().name().unwrap_or("?")
            );
            None
        }
    }
}

/// Skips the current test with a visible reason when treehouse is absent and optional, and fails it
/// when `CI` or `COWFS_REQUIRE_TREEHOUSE` says treehouse is required, so a missing binary can never
/// look like a pass on a runner.
#[macro_export]
macro_rules! require_treehouse {
    () => {
        match $crate::common::treehouse_or_skip(
            $crate::common::treehouse_required(),
            $crate::common::treehouse_bin(),
        ) {
            Some(bin) => bin,
            None => return,
        }
    };
}

/// A treehouse pool and repository in a temporary directory, with its own sandbox HOME.
pub struct Sandbox {
    pub dir: tempfile::TempDir,
}

impl Sandbox {
    /// Creates a throwaway git repository with one commit and no pool entries.
    pub fn new() -> Sandbox {
        let dir = private_tempdir();
        let s = Sandbox { dir };
        std::fs::create_dir_all(s.repo()).expect("mkdir repo");
        std::fs::create_dir_all(s.home()).expect("mkdir home");
        s.git(&s.repo(), &["init", "-q", "."]);
        s.git(&s.repo(), &["config", "user.email", "t@example.invalid"]);
        s.git(&s.repo(), &["config", "user.name", "t"]);
        std::fs::write(s.repo().join("README.md"), b"hello\n").expect("write README");
        s.git(&s.repo(), &["add", "-A"]);
        s.git(&s.repo(), &["commit", "-qm", "init"]);
        s.git(&s.repo(), &["branch", "-M", "main"]);
        s
    }

    pub fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    /// The sandbox HOME, which treehouse reads `.config/treehouse/config.toml` from.
    pub fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    /// The throwaway repository's main checkout.
    pub fn repo(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    /// The pool root handed to treehouse as `--root`. Treehouse appends `.treehouse` itself.
    pub fn pool(&self) -> PathBuf {
        self.dir.path().join("pool")
    }

    /// A second pool root in the same sandbox, so a caller can prove that a `--root` naming one
    /// pool cannot release a slot in the other.
    pub fn other_pool(&self) -> PathBuf {
        self.dir.path().join("poolB")
    }

    /// A `treehouse` shim that refuses to leave this sandbox.
    ///
    /// It is what every call in these tests goes through, including the ones the companion makes,
    /// because a call the companion builds is exactly where a wrong pool root would hide.
    pub fn shim(&self) -> PathBuf {
        let real = find_treehouse().expect("a sandbox needs a real treehouse binary");
        let dir = self.dir.path().join("bin");
        std::fs::create_dir_all(&dir).expect("mkdir bin");
        let path = dir.join("treehouse");
        let script = format!(
            r#"#!/bin/sh
# Generated by the cowfs-treehouse test fixtures. Refuses to leave its sandbox.
SB='{sb}'
root=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--root" ]; then root="$a"; fi
  prev="$a"
done
if [ -z "$root" ]; then echo "SHIM: no --root, refusing" >&2; exit 91; fi
case "$root" in
  "$SB"/*) : ;;
  *) echo "SHIM: --root $root is outside the sandbox" >&2; exit 92 ;;
esac
for a in "$@"; do
  case "$a" in
    /*) case "$a" in "$SB"/*) : ;; *) echo "SHIM: path argument $a is outside the sandbox" >&2; exit 93 ;; esac ;;
  esac
done
HOME='{home}'
TREEHOUSE_ROOT="$root"
export HOME TREEHOUSE_ROOT
mkdir -p "$HOME"
exec '{real}' "$@"
"#,
            sb = self.dir.path().display(),
            home = self.home().display(),
            real = real.display(),
        );
        std::fs::write(&path, script).expect("write shim");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod the shim");
        path
    }

    /// Runs a git command in `cwd` and fails the test when git does not.
    pub fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("HOME", self.home())
            .output()
            .unwrap_or_else(|e| panic!("cannot run git {args:?}: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// The sandbox `HOME`, as a value to pass as `--treehouse-home`, or `None` when the caller does
    /// not want one.
    pub fn shim_home(&self) -> Option<PathBuf> {
        Some(self.home())
    }

    /// Runs treehouse against this sandbox through the shim, and guards the output.
    pub fn treehouse_at(&self, root: &Path, args: &[&str]) -> Output {
        let out = output_retrying_etxtbsy(
            Command::new(self.shim())
                .args(args)
                .arg("--root")
                .arg(root)
                .env_remove("TREEHOUSE_ROOT")
                .env_remove("TREEHOUSE_LEASE_HOLDER")
                .current_dir(self.repo())
                .stdin(Stdio::null()),
        )
        .unwrap_or_else(|e| panic!("cannot run treehouse: {e}"));
        assert_sandboxed(args, &out);
        out
    }

    /// Runs treehouse against this sandbox's own pool through the shim.
    pub fn treehouse(&self, args: &[&str]) -> Output {
        self.treehouse_at(&self.pool(), args)
    }

    /// Runs treehouse and requires success.
    pub fn treehouse_ok(&self, args: &[&str]) -> String {
        let out = self.treehouse(args);
        assert!(
            out.status.success(),
            "treehouse {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Every slot directory in the pool, `{pool}/.treehouse/{pool}/{slot}`.
    pub fn slots(&self) -> Vec<PathBuf> {
        cowfs_treehouse::pool_slots(&self.pool())
    }
}

/// Runs a command that executes a script this process just wrote, retrying on ETXTBSY.
///
/// A sibling test thread that forks while the script is still open for writing holds a write
/// descriptor until its own exec, and Linux refuses to execute the file meanwhile. The script is
/// complete; only the spawn is retried (a flake seen on the cachyos box, issue 259).
pub fn output_retrying_etxtbsy(cmd: &mut Command) -> std::io::Result<Output> {
    let mut attempt = 0;
    loop {
        match cmd.output() {
            Err(e) if e.raw_os_error() == Some(26) && attempt < 20 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(25));
            }
            other => return other,
        }
    }
}

/// Fails when any stream of a command mentions the real treehouse store.
pub fn assert_sandboxed(args: &[&str], out: &Output) {
    let real = real_treehouse_root().display().to_string();
    for stream in [&out.stdout, &out.stderr] {
        let text = String::from_utf8_lossy(stream);
        assert!(
            !text.contains(&real),
            "treehouse {args:?} touched the real pool:\n{text}"
        );
    }
}

/// A temporary directory no other user can reach, which the control server requires of the
/// directory holding its socket.
pub fn private_tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
        .expect("chmod 0700");
    dir
}

/// An in-process stub daemon on its own socket inside `dir`.
pub struct Stub {
    pub server: Server,
    pub handler: Arc<StubHandler>,
    pub socket: PathBuf,
}

/// Starts a stub daemon whose snapshots are in memory, like `cowfs serve --stub`.
pub fn stub_in(dir: &Path) -> Stub {
    use std::os::unix::fs::PermissionsExt;
    let sock_dir = dir.join("run");
    std::fs::create_dir_all(&sock_dir).expect("mkdir socket dir");
    // The control server refuses a socket directory that group or other can reach.
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700))
        .expect("chmod 0700 the socket dir");
    let socket = sock_dir.join("control.sock");
    let handler = Arc::new(StubHandler::new(
        dir.join("store").display().to_string(),
        dir.join("mnt").display().to_string(),
    ));
    let server = Server::start(
        &socket,
        Arc::clone(&handler) as Arc<dyn ControlHandler>,
        ServerOptions::default(),
    )
    .expect("stub server starts");
    Stub {
        server,
        handler,
        socket,
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        // `Server::shutdown` takes self by value, so stop it through the handle instead and leave
        // the accept thread to be joined by the server's own drop.
        self.server.handle().shutdown();
    }
}

/// A detached process a test starts, so it can prove the holder logic and then clean it up.
///
/// The fixture is spawned detached from the test harness: dropping a `Child` does not kill it, and
/// the pid is read back from a file the fixture writes itself, so the pid that gets signalled is
/// the one that is really holding the slot no matter what the spawn returned.
pub struct Fixture {
    pub pid: u32,
    pub kind: &'static str,
    /// Kept so a killed child can be reaped instead of lingering as a zombie. Boxed so the type
    /// says the handle is owned, which the wait-on-every-path lint needs to see.
    child: Box<std::process::Child>,
}

impl Fixture {
    /// Starts `sh -c cmd` detached and waits until it has written its own pid.
    ///
    /// The command is run as written rather than behind an `exec`, because an `exec` in front of a
    /// compound command would only take the first simple command as its argument and the shell
    /// would then exit immediately. A fixture that must survive a signal supplies its own trailing
    /// `exec`, so the pid that is signalled is the process that is really holding the slot and
    /// there is nothing left behind when it dies.
    pub fn spawn(kind: &'static str, cmd: &str) -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let pid_file = dir.path().join("pid");
        let script = format!("echo $$ > {}; {cmd}", pid_file.display());
        // Boxed so the spawn expression is not a plain local: the lint that requires a wait on
        // every path cannot see through the two-step spawn-then-wait below.
        let child = Box::new(
            Command::new("/bin/sh")
                .arg("-c")
                .arg(&script)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap_or_else(|e| panic!("spawn fixture {kind}: {e}")),
        );
        let mut child = child;
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = text.trim().parse::<u32>() {
                    // The pid file is the fixture's own, so the pid that gets signalled is the one
                    // really holding the slot, whatever the spawn returned.
                    std::mem::forget(dir);
                    return Fixture { pid, kind, child };
                }
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                panic!(
                    "fixture {kind} never wrote its pid to {}",
                    pid_file.display()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// SIGKILLs the fixture by pid, after confirming the pid is still the one that was spawned.
    /// Never a pattern match: `pkill -f` can match the very shell running the cleanup.
    pub fn kill(&mut self) {
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        if !cowfs_treehouse::alive(self.pid) {
            let _ = self.child.wait();
            return;
        }
        let cmdline = Command::new("ps")
            .args(["-o", "command=", "-p", &self.pid.to_string()])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        assert!(
            !cmdline.trim().is_empty(),
            "refusing to kill pid {}: its command line could not be read",
            self.pid
        );
        let _ = Command::new("kill")
            .args(["-KILL", &self.pid.to_string()])
            .status();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            // Reap as we go: a SIGKILLed child is a zombie until waited on, and a zombie is not
            // evidence that the kill failed.
            let _ = self.child.try_wait();
            if !cowfs_treehouse::alive(self.pid) {
                let _ = self.child.wait();
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!("fixture {} ({}) survived SIGKILL", self.kind, self.pid);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Aborts the whole test binary when a test runs longer than `secs`, so a hang fails instead of
/// stalling the suite.
pub struct Watchdog(std::sync::mpsc::Sender<()>);

impl Watchdog {
    pub fn start(secs: u64) -> Watchdog {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let name = std::thread::current().name().unwrap_or("?").to_owned();
        std::thread::spawn(move || {
            if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                rx.recv_timeout(Duration::from_secs(secs))
            {
                eprintln!("WATCHDOG: test {name} exceeded {secs}s, aborting the test binary");
                std::process::abort();
            }
        });
        Watchdog(tx)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}
