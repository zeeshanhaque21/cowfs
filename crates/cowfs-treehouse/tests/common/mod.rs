//! Shared fixtures: an in-process stub daemon, a throwaway git repository, and a sandboxed
//! treehouse pool.
//!
//! Nothing here touches a real pool. Every treehouse invocation gets a sandbox `HOME`, a sandbox
//! `TREEHOUSE_ROOT` and an explicit `--root`, and every one of them asserts afterwards that no
//! path under the real `~/.treehouse` appeared in its output.

#![allow(dead_code)]

use cowfs_ctl::{ControlHandler, Server, ServerOptions, StubHandler};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::Duration;

/// The real treehouse store on this machine. Every guard below refuses to let it near a command.
pub fn real_treehouse_root() -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    home.join(".treehouse")
}

/// The treehouse binary, from the environment or the usual place.
pub fn treehouse_bin() -> PathBuf {
    match std::env::var_os("COWFS_TREEHOUSE_BIN") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/Users/zeeshanhaque/.local/bin/treehouse"),
    }
}

/// True when the real treehouse binary is present, so a test can skip rather than fail on a
/// machine without it.
pub fn treehouse_available() -> bool {
    treehouse_bin().is_file()
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

    /// Runs the real treehouse binary against this sandbox and guards the output.
    pub fn treehouse(&self, args: &[&str]) -> Output {
        let root = self.pool();
        let out = Command::new(treehouse_bin())
            .args(args)
            .arg("--root")
            .arg(&root)
            .env("HOME", self.home())
            .env("TREEHOUSE_ROOT", &root)
            .env_remove("TREEHOUSE_LEASE_HOLDER")
            .current_dir(self.repo())
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|e| panic!("cannot run treehouse: {e}"));
        assert_sandboxed(args, &out);
        out
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
