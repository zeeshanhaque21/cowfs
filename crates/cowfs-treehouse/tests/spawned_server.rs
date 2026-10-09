//! The companion driven as a real process against a spawned `cowfs serve --stub`.
//!
//! `stub_server.rs` calls the library in-process, which cannot catch argument parsing, exit codes,
//! environment handling or the fact that a second process can talk to the daemon at all. This file
//! runs the built binary against a separately spawned daemon, over a real Unix socket.

mod common;

use common::{private_tempdir, require_bin, Sandbox, Watchdog};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

/// The companion binary under test.
fn companion() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cowfs-treehouse"))
}

/// A spawned `cowfs serve --stub`, stopped when the test ends.
struct Serve {
    child: std::process::Child,
    socket: PathBuf,
    /// Kept so the socket directory outlives the daemon.
    _dir: tempfile::TempDir,
}

impl Serve {
    fn start(dir: tempfile::TempDir) -> Serve {
        let path = dir.path().to_path_buf();
        let cowfs = require_bin("cowfs");
        let sock_dir = path.join("run");
        std::fs::create_dir_all(&sock_dir).expect("mkdir run");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700))
            .expect("chmod run");
        let socket = sock_dir.join("control.sock");
        let child = Command::new(cowfs)
            .args([
                "serve",
                "--store",
                &path.join("store").display().to_string(),
                "--mount",
                &path.join("mnt").display().to_string(),
                "--stub",
                "--socket",
                &socket.display().to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cowfs serve --stub");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !socket.exists() {
            if std::time::Instant::now() >= deadline {
                let mut child = child;
                let _ = child.kill();
                let _ = child.wait();
                panic!("cowfs serve --stub never bound {}", socket.display());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Serve {
            child,
            socket,
            _dir: dir,
        }
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        // Ask nicely first, so the daemon removes its socket instead of leaving a stale one.
        let _ = Command::new(companion())
            .args(["--socket", &self.socket.display().to_string(), "shutdown"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn run(args: &[&str]) -> Output {
    Command::new(companion())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run cowfs-treehouse {args:?}: {e}"))
}

fn code(args: &[&str]) -> i32 {
    run(args).status.code().unwrap_or(-1)
}

fn stdout(args: &[&str]) -> String {
    let out = run(args);
    assert!(
        out.status.success(),
        "cowfs-treehouse {args:?} failed with {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn json(args: &[&str]) -> serde_json::Value {
    let text = stdout(args);
    serde_json::from_str(text.lines().last().unwrap_or("")).unwrap_or_else(|e| {
        panic!("cowfs-treehouse {args:?} printed no JSON ({e}): {text}");
    })
}

#[test]
fn a_second_process_can_drive_the_daemon_over_a_real_socket() {
    let _w = Watchdog::start(120);
    let serve = Serve::start(private_tempdir());
    let s = Sandbox::new();
    let sock = serve.socket.display().to_string();

    // A base, created by one process and read by another: the socket, the framing and the
    // canonical path handling all have to work across process boundaries.
    let v = json(&[
        "--socket",
        &sock,
        "--json",
        "base",
        "refresh",
        "--repo",
        &s.repo().display().to_string(),
        "--ref",
        "main",
    ]);
    let snapshot = v["snapshot"].as_str().expect("snapshot name").to_owned();
    assert!(snapshot.ends_with("-base"), "{v}");

    let v = json(&[
        "--socket",
        &sock,
        "--json",
        "base",
        "status",
        "--repo",
        &s.repo().display().to_string(),
        "--ref",
        "main",
    ]);
    assert_eq!(v["snapshot"].as_str(), Some(snapshot.as_str()), "{v}");
    assert_eq!(
        v["fresh"], false,
        "the stub records a synthetic commit: {v}"
    );

    // Promote is explicit and idempotent, from a separate process.
    let v = json(&[
        "--socket",
        &sock,
        "--json",
        "base",
        "promote",
        "--snapshot",
        &snapshot,
    ]);
    assert!(v["base"].is_object(), "{v}");
    let v = json(&[
        "--socket",
        &sock,
        "--json",
        "base",
        "promote",
        "--snapshot",
        &snapshot,
    ]);
    assert!(v["base"].is_object(), "promote is idempotent: {v}");
}

#[test]
fn a_missing_daemon_is_exit_three_from_the_real_binary() {
    let _w = Watchdog::start(60);
    let dir = private_tempdir();
    let absent = dir.path().join("run/absent.sock");
    assert_eq!(
        code(&[
            "--socket",
            &absent.display().to_string(),
            "base",
            "status",
            "--repo",
            &dir.path().display().to_string(),
        ]),
        3,
        "no daemon is exit 3"
    );
}

#[test]
fn usage_errors_are_exit_two_and_name_the_problem() {
    let _w = Watchdog::start(60);
    // An unknown subcommand.
    assert_eq!(code(&["nonsense"]), 2);
    // A missing required flag.
    assert_eq!(code(&["doctor"]), 2);
    // An option value that is not a number.
    let out = run(&["--timeout", "soon", "doctor", "--mount", "/"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("timeout"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // `--root` is required for a return, and saying so beats guessing a pool.
    let out = run(&["return", "--slot", "/tmp"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--root is required"));
    // A mode that is not a or b.
    assert_eq!(
        code(&["return", "--slot", "/tmp", "--root", "/tmp", "--mode", "z"]),
        2
    );
}

#[test]
fn version_and_help_work_without_a_daemon() {
    let _w = Watchdog::start(60);
    let v = stdout(&["--version"]);
    assert!(v.contains("cowfs-treehouse"), "{v}");
    for args in [
        &["--help"][..],
        &["base", "--help"][..],
        &["hooks", "--help"][..],
    ] {
        let h = stdout(args);
        assert!(h.contains("Usage: cowfs-treehouse"), "{args:?} -> {h}");
    }
    let h = stdout(&["base", "--help"]);
    assert!(h.contains("refresh") && h.contains("status"), "{h}");
    let h = stdout(&["--help"]);
    assert!(
        h.contains("Exit codes:"),
        "the exit codes are documented: {h}"
    );
}

#[test]
fn a_pool_id_is_printed_for_a_real_repository() {
    let _w = Watchdog::start(60);
    let s = Sandbox::new();
    let id = stdout(&["pool-id", &s.repo().display().to_string()]);
    let id = id.trim();
    assert!(!id.is_empty());
    assert_eq!(
        id,
        cowfs_treehouse::pool_id(&cowfs_treehouse::main_repo_root(&s.repo()).expect("root"))
            .expect("pool id")
    );
    // And in JSON it is a string, not a bare word.
    let v = json(&["--json", "pool-id", &s.repo().display().to_string()]);
    assert_eq!(v.as_str(), Some(id), "{v}");
}

#[test]
fn provision_refuses_a_path_that_is_not_a_slot() {
    let _w = Watchdog::start(60);
    let serve = Serve::start(private_tempdir());
    let out = run(&[
        "--socket",
        &serve.socket.display().to_string(),
        "provision",
        "--slot",
        "/",
    ]);
    assert_eq!(out.status.code(), Some(2), "a usage error");
    assert!(String::from_utf8_lossy(&out.stderr).contains("treehouse slot path"));
}

/// Negative control for issue 244: a missing sibling binary must panic, never skip.
#[test]
#[should_panic(expected = "is not beside this test binary")]
fn a_missing_sibling_binary_fails_loudly() {
    let _ = require_bin("cowfs-no-such-binary-244");
}

/// The binary the serve tests depend on really is there, so they cannot be silent passes.
#[test]
fn the_cowfs_binary_is_built_beside_the_tests() {
    assert!(require_bin("cowfs").is_file());
}
