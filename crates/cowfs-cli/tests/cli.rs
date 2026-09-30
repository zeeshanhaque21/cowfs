//! Runs the built `cowfs` binary against a `cowfs serve --stub` server.

use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Aborts the test binary when a test outlives `secs`, so a hang fails CI instead of stalling it.
struct Watchdog(mpsc::Sender<()>);

fn watchdog() -> Watchdog {
    let (tx, rx) = mpsc::channel::<()>();
    let name = std::thread::current().name().unwrap_or("?").to_owned();
    std::thread::spawn(move || {
        if let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(Duration::from_secs(90)) {
            eprintln!("WATCHDOG: test {name} exceeded 90s");
            std::process::abort();
        }
    });
    Watchdog(tx)
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

/// A spawned process that is killed and reaped when dropped.
struct Kid(Child);

impl Drop for Kid {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn private_dir() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

const BIN: &str = env!("CARGO_BIN_EXE_cowfs");

struct Daemon {
    _dir: TempDir,
    socket: PathBuf,
    child: Child,
    _watchdog: Watchdog,
}

impl Daemon {
    fn start(extra: &[&str]) -> Daemon {
        let watchdog = watchdog();
        let dir = private_dir();
        let socket = dir.path().join("c.sock");
        let mut child = Command::new(BIN)
            .args([
                "serve",
                "--stub",
                "--store",
                "/stub/store",
                "--mount",
                "/stub/mount",
            ])
            .args(extra)
            .arg("--socket")
            .arg(&socket)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Event-based: wait for the daemon's own ready line, with a generous no-progress budget.
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let mut line = String::new();
            let n = stderr.read_line(&mut line).unwrap();
            if n == 0 {
                panic!("serve exited before it was ready");
            }
            if line.contains("serving") {
                break;
            }
            assert!(Instant::now() < deadline, "server did not report ready");
        }
        Daemon {
            _dir: dir,
            socket,
            child,
            _watchdog: watchdog,
        }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.arg("--socket").arg(&self.socket).args(args);
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let out = self.run(&all);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert_eq!(stdout.matches('\n').count(), 1, "one JSON line: {stdout:?}");
        serde_json::from_str(&stdout).unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn keys(v: &Value) -> Vec<&str> {
    let mut k: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    k.sort_unstable();
    k
}

#[test]
fn every_subcommand_works_with_json_output() {
    let d = Daemon::start(&[]);

    let status = d.json(&["status"]);
    assert_eq!(
        keys(&status),
        [
            "block_count",
            "logical_bytes",
            "mount_path",
            "snapshot_count",
            "store_path",
            "stored_bytes",
            "uptime_secs"
        ]
    );
    assert_eq!(status["store_path"], "/stub/store");

    let main = d.json(&["snapshot", "create", "main"]);
    assert_eq!(keys(&main), ["base", "created_unix_ms", "name", "parent"]);
    assert_eq!(
        (&main["name"], &main["parent"]),
        (&Value::from("main"), &Value::Null)
    );
    let work = d.json(&["snapshot", "create", "work", "--from", "main"]);
    assert_eq!(work["parent"], "main");

    let list = d.json(&["snapshot", "list"]);
    assert_eq!(list["snapshots"].as_array().unwrap().len(), 2);

    let promoted = d.json(&["snapshot", "promote", "main"]);
    assert!(promoted["base"].is_object());
    let renamed = d.json(&["snapshot", "rename", "main", "trunk"]);
    assert_eq!(renamed["name"], "trunk");

    let ps = d.json(&["ps", "work"]);
    assert_eq!(ps, serde_json::json!({"processes": []}));

    let dry = d.json(&["gc", "--dry-run"]);
    assert_eq!(
        keys(&dry),
        [
            "candidate_blocks",
            "candidate_bytes",
            "dry_run",
            "freed_blocks",
            "freed_bytes"
        ]
    );
    assert_eq!(
        (&dry["dry_run"], &dry["freed_blocks"]),
        (&Value::Bool(true), &Value::from(0))
    );
    let gc = d.json(&["gc"]);
    assert_eq!(gc["dry_run"], false);
    assert!(gc["freed_blocks"].as_u64().unwrap() > 0);

    let fsck = d.json(&["fsck"]);
    assert_eq!(
        keys(&fsck),
        [
            "blocks_checked",
            "bytes_checked",
            "ok",
            "problems",
            "snapshots_checked"
        ]
    );
    assert_eq!(fsck["ok"], true);

    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("a"), b"hello").unwrap();
    let import = d.json(&["import", src.path().to_str().unwrap(), "--name", "imported"]);
    assert_eq!(
        keys(&import),
        [
            "bytes",
            "files",
            "hash_algorithm",
            "imported_root_hash",
            "mismatches",
            "mismatches_truncated",
            "name",
            "source_root_hash",
            "verified"
        ]
    );
    assert_eq!(import["source_root_hash"], import["imported_root_hash"]);
    assert_eq!(
        (&import["files"], &import["bytes"], &import["verified"]),
        (&Value::from(1), &Value::from(5), &Value::Bool(true))
    );

    let base = d.json(&["base", "refresh", "--repo", "/srv/myrepo", "--ref", "main"]);
    assert_eq!(keys(&base), ["previous_commit", "snapshot"]);
    assert_eq!(base["snapshot"]["name"], "myrepo-base");
    assert_eq!(base["snapshot"]["base"]["git_ref"], "main");
    let named = d.json(&[
        "base",
        "refresh",
        "--repo",
        "/srv/myrepo",
        "--ref",
        "v2",
        "--name",
        "custom",
    ]);
    assert_eq!(named["snapshot"]["name"], "custom");

    let mount = d.json(&["mount-info"]);
    assert_eq!(keys(&mount), ["adapter", "mount_path", "mounted"]);

    let rm = d.json(&["snapshot", "rm", "work"]);
    assert_eq!(rm, serde_json::json!({}));

    let last = d.json(&["shutdown"]);
    assert_eq!(last, serde_json::json!({}));
}

#[test]
fn shutdown_stops_the_daemon_and_then_the_cli_reports_not_running() {
    let mut d = Daemon::start(&[]);
    assert_eq!(d.run(&["shutdown"]).status.code(), Some(0));
    let status = d.child.wait().unwrap();
    assert!(status.success(), "serve exits 0 after shutdown: {status}");
    assert!(!d.socket.exists());
    let out = d.run(&["status"]);
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn human_output_is_readable() {
    let d = Daemon::start(&[]);
    let out = d.run(&["status"]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains("store:") && text.contains("/stub/store"),
        "{text}"
    );
    assert_eq!(
        String::from_utf8(d.run(&["snapshot", "list"]).stdout)
            .unwrap()
            .trim(),
        "no snapshots"
    );
    d.run(&["snapshot", "create", "a"]);
    let list = String::from_utf8(d.run(&["snapshot", "list"]).stdout).unwrap();
    assert!(list.contains("  a") && list.contains('Z'), "{list}");
    let gc = String::from_utf8(d.run(&["gc", "--dry-run"]).stdout).unwrap();
    assert!(gc.starts_with("dry run:"), "{gc}");
    assert_eq!(
        String::from_utf8(d.run(&["ps", "a"]).stdout)
            .unwrap()
            .trim(),
        "no processes"
    );
}

#[test]
fn exit_code_1_and_a_json_error_for_daemon_errors() {
    let d = Daemon::start(&[]);
    let out = d.run(&["--json", "snapshot", "rm", "ghost"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        out.stderr.is_empty(),
        "no progress and no error text on stderr"
    );
    let err: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(err["error"]["code"], "not_found");
    assert!(err["error"]["message"].as_str().unwrap().contains("ghost"));

    let out = d.run(&["snapshot", "rm", "ghost"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8(out.stderr)
        .unwrap()
        .starts_with("cowfs: "));

    let out = d.run(&["--json", "snapshot", "create", "a/b"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["code"],
        "invalid_params"
    );
}

#[test]
fn exit_code_2_for_usage_errors() {
    for args in [
        &[][..],
        &["frobnicate"],
        &["snapshot"],
        &["snapshot", "create"],
        &["import", "dir"],
        &["base", "refresh", "--repo", "r"],
        &["gc", "--nope"],
    ] {
        let out = Command::new(BIN).args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(!out.stderr.is_empty());
    }
    assert_eq!(
        Command::new(BIN)
            .arg("--help")
            .output()
            .unwrap()
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        Command::new(BIN)
            .arg("--version")
            .output()
            .unwrap()
            .status
            .code(),
        Some(0)
    );
}

#[test]
fn exit_code_3_when_no_daemon_is_running() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("none.sock");
    let out = Command::new(BIN)
        .arg("--socket")
        .arg(&socket)
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8(out.stderr)
        .unwrap()
        .contains("no cowfs daemon"));

    let out = Command::new(BIN)
        .arg("--json")
        .arg("--socket")
        .arg(&socket)
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["code"],
        "not_running"
    );

    let stale = dir.path().join("stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
    let out = Command::new(BIN)
        .arg("--socket")
        .arg(&stale)
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "a socket file nobody listens on means not running"
    );
}

#[test]
fn ctrl_c_cancels_the_operation_and_exits_130() {
    let d = Daemon::start(&["--stub-delay-ms", "400"]);
    let mut child = Kid(d
        .cmd(&["gc"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap());
    let mut stderr = BufReader::new(child.0.stderr.take().unwrap());
    let mut first = String::new();
    stderr.read_line(&mut first).unwrap();
    assert!(
        first.starts_with("mark"),
        "progress reaches stderr: {first:?}"
    );
    let started = Instant::now();
    let kill = Command::new("kill")
        .args(["-INT", &child.0.id().to_string()])
        .status()
        .unwrap();
    assert!(kill.success());
    let status = child.0.wait().unwrap();
    assert_eq!(status.code(), Some(130));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "cancelled long before gc's 4 s of work"
    );
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut stderr, &mut rest).unwrap();
    assert!(rest.contains("interrupted"), "{rest:?}");

    let mut probe = d.cmd(&["status"]);
    assert_eq!(
        probe.output().unwrap().status.code(),
        Some(0),
        "the daemon survives a cancelled client"
    );
}

#[test]
fn progress_goes_to_stderr_one_line_per_phase_and_as_json_lines_with_json() {
    let d = Daemon::start(&[]);
    let out = d.run(&["gc", "--dry-run"]);
    let err = String::from_utf8(out.stderr).unwrap();
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines.len(), 2, "{err:?}");
    assert!(lines[0].starts_with("mark") && lines[1].starts_with("sweep"));

    let out = d.run(&["--json", "gc", "--dry-run"]);
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(!err.is_empty());
    for l in err.lines() {
        assert!(
            serde_json::from_str::<Value>(l).unwrap()["progress"].is_object(),
            "{l}"
        );
    }
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).matches('\n').count(),
        1
    );
    assert!(serde_json::from_slice::<Value>(&out.stdout).is_ok());
}

#[test]
fn socket_comes_from_the_environment_too() {
    let d = Daemon::start(&[]);
    let out = Command::new(BIN)
        .env("COWFS_SOCKET", &d.socket)
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let out = Command::new(BIN)
        .env("COWFS_SOCKET", "/nonexistent/x.sock")
        .arg("--socket")
        .arg(&d.socket)
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "--socket beats COWFS_SOCKET");
}

#[test]
fn serve_without_stub_says_the_real_backend_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("c.sock");
    let out = Command::new(BIN)
        .args(["serve", "--store", "/s", "--mount", "/m", "--socket"])
        .arg(&socket)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8(out.stderr).unwrap().contains("--stub"));
    assert!(!socket.exists());
}

#[test]
fn a_second_serve_on_the_same_socket_fails() {
    let d = Daemon::start(&[]);
    let out = Command::new(BIN)
        .args([
            "serve", "--stub", "--store", "/s", "--mount", "/m", "--socket",
        ])
        .arg(&d.socket)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8(out.stderr)
        .unwrap()
        .contains("cannot serve"));
    assert_eq!(
        d.run(&["status"]).status.code(),
        Some(0),
        "the first daemon is untouched"
    );
}

#[test]
fn sigterm_shuts_the_daemon_down_cleanly() {
    let mut d = Daemon::start(&[]);
    assert!(Command::new("kill")
        .args(["-TERM", &d.child.id().to_string()])
        .status()
        .unwrap()
        .success());
    assert!(d.child.wait().unwrap().success());
    assert!(!d.socket.exists());
}

#[test]
fn completions_for_every_shell() {
    for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
        let out = Command::new(BIN)
            .args(["completions", shell])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{shell}");
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(
            text.contains("cowfs") && text.contains("snapshot"),
            "{shell}"
        );
    }
    assert_eq!(
        Command::new(BIN)
            .args(["completions", "klingon"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
}

fn silent_listener() -> (TempDir, PathBuf) {
    let dir = private_dir();
    let path = dir.path().join("silent.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for s in listener.incoming().flatten() {
            held.push(s);
        }
    });
    (dir, path)
}

#[test]
fn h1_a_silent_daemon_times_out_with_exit_4() {
    let _w = watchdog();
    let (_dir, socket) = silent_listener();
    let started = Instant::now();
    let out = Command::new(BIN)
        .args(["--timeout", "1", "--json", "--socket"])
        .arg(&socket)
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(started.elapsed() < Duration::from_secs(8));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["code"], "timeout");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("did not send"));
}

#[test]
fn timeout_option_and_environment_are_validated() {
    let _w = watchdog();
    let (_dir, socket) = silent_listener();
    for bad in ["0", "x", "-1"] {
        let out = Command::new(BIN)
            .args(["--timeout", bad, "status"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "--timeout {bad}");
    }
    let out = Command::new(BIN)
        .env("COWFS_TIMEOUT", "1")
        .arg("--socket")
        .arg(&socket)
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4), "COWFS_TIMEOUT applies");
    let out = Command::new(BIN)
        .env("COWFS_TIMEOUT", "soon")
        .arg("--socket")
        .arg(&socket)
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn an_empty_cowfs_socket_means_unset() {
    let _w = watchdog();
    let dir = private_dir();
    let out = Command::new(BIN)
        .env("COWFS_SOCKET", "")
        .env("XDG_RUNTIME_DIR", dir.path())
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains(dir.path().to_str().unwrap()));
}

#[test]
fn errors_name_the_socket_path() {
    let _w = watchdog();
    let dir = private_dir();
    let file = dir.path().join("regular");
    std::fs::write(&file, b"x").unwrap();
    let out = Command::new(BIN)
        .arg("--socket")
        .arg(&file)
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains(file.to_str().unwrap()));

    let under_file = file.join("c.sock");
    let out = Command::new(BIN)
        .arg("--socket")
        .arg(&under_file)
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "a parent that is a file is an error, not no-daemon"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains(under_file.to_str().unwrap()));

    let out = Command::new(BIN)
        .arg("--socket")
        .arg(dir.path().join("absent.sock"))
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "a missing socket is no daemon");

    let long = dir.path().join("x".repeat(200)).join("c.sock");
    let out = Command::new(BIN)
        .arg("--socket")
        .arg(&long)
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "a path too long for a socket is a usage error"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("xxxxxxxx"));
}

#[test]
fn snapshot_reset_and_forced_rm_work_from_the_cli() {
    let d = Daemon::start(&[]);
    d.json(&["snapshot", "create", "base"]);
    d.json(&["snapshot", "create", "slot"]);
    let reset = d.json(&["snapshot", "reset", "slot", "--from", "base"]);
    assert_eq!(
        (&reset["name"], &reset["parent"]),
        (&Value::from("slot"), &Value::from("base"))
    );
    assert_eq!(
        d.json(&["snapshot", "list"])["snapshots"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let out = d.run(&["--json", "snapshot", "reset", "ghost", "--from", "base"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["code"],
        "not_found"
    );
    assert_eq!(
        d.json(&["snapshot", "rm", "slot", "--force"]),
        serde_json::json!({})
    );
    let out = d.run(&["--json", "snapshot", "create", "Base"]);
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"]["code"],
        "already_exists"
    );
}

#[test]
fn unsafe_names_never_print_raw_control_characters() {
    let d = Daemon::start(&[]);
    let out = d.run(&["--json", "snapshot", "create", "a\u{1b}]0;PWNED\u{7}"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&out.stdout).contains('\u{1b}'));
    let out = d.run(&["snapshot", "create", "bad\nname"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("bad\nname"));
}

#[test]
fn a_path_that_is_not_utf8_is_a_usage_error() {
    use std::os::unix::ffi::OsStrExt;
    let d = Daemon::start(&[]);
    let bad = std::ffi::OsStr::from_bytes(b"/tmp/not-utf8-\xff");
    let out = d
        .cmd(&["import"])
        .arg(bad)
        .args(["--name", "x"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("UTF-8"));
}

#[test]
fn a_failing_stdout_fails_the_command() {
    let d = Daemon::start(&[]);
    let full = std::path::Path::new("/dev/full");
    if !full.exists() {
        return;
    }
    let out = d
        .cmd(&["status"])
        .stdout(std::fs::OpenOptions::new().write(true).open(full).unwrap())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot write"));
}

#[test]
fn a_closed_pipe_reader_is_not_an_error() {
    let d = Daemon::start(&[]);
    let mut child = Kid(d
        .cmd(&["snapshot", "list"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap());
    drop(child.0.stdout.take());
    assert_eq!(child.0.wait().unwrap().code(), Some(0));
}

#[test]
fn sigint_waits_a_bounded_time_for_a_daemon_that_ignores_cancel() {
    let d = Daemon::start(&["--stub-delay-ms", "400", "--stub-ignore-cancel"]);
    let mut child = Kid(d
        .cmd(&["gc"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap());
    let mut stderr = BufReader::new(child.0.stderr.take().unwrap());
    let mut first = String::new();
    stderr.read_line(&mut first).unwrap();
    let started = Instant::now();
    assert!(Command::new("kill")
        .args(["-INT", &child.0.id().to_string()])
        .status()
        .unwrap()
        .success());
    assert_eq!(child.0.wait().unwrap().code(), Some(130));
    let took = started.elapsed();
    assert!(
        took < Duration::from_millis(3500),
        "{took:?}: gc itself runs 4 s"
    );
    assert!(
        took >= Duration::from_millis(1500),
        "{took:?}: waited for the final frame first"
    );
}

#[test]
fn a_second_sigint_exits_the_client_at_once() {
    let d = Daemon::start(&["--stub-delay-ms", "400", "--stub-ignore-cancel"]);
    let mut child = Kid(d
        .cmd(&["gc"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap());
    let mut stderr = BufReader::new(child.0.stderr.take().unwrap());
    let mut first = String::new();
    stderr.read_line(&mut first).unwrap();
    let pid = child.0.id().to_string();
    Command::new("kill").args(["-INT", &pid]).status().unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let started = Instant::now();
    Command::new("kill").args(["-INT", &pid]).status().unwrap();
    assert_eq!(child.0.wait().unwrap().code(), Some(130));
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn shutdown_during_a_streaming_gc_gives_the_client_a_terminal_error() {
    let d = Daemon::start(&["--stub-delay-ms", "300"]);
    let mut gc = Kid(d
        .cmd(&["--json", "gc"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap());
    let mut stderr = BufReader::new(gc.0.stderr.take().unwrap());
    let mut first = String::new();
    stderr.read_line(&mut first).unwrap();
    assert_eq!(d.run(&["shutdown"]).status.code(), Some(0));
    let mut stdout = String::new();
    std::io::Read::read_to_string(gc.0.stdout.as_mut().unwrap(), &mut stdout).unwrap();
    assert_eq!(gc.0.wait().unwrap().code(), Some(1));
    let v: Value = serde_json::from_str(&stdout).unwrap();
    let code = v["error"]["code"].as_str().unwrap();
    assert!(code == "shutting_down" || code == "cancelled", "{stdout}");
}

#[test]
fn a_second_signal_forces_serve_to_exit_while_a_handler_ignores_cancel() {
    let mut d = Daemon::start(&["--stub-delay-ms", "1000", "--stub-ignore-cancel"]);
    let mut gc = Kid(d
        .cmd(&["gc"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap());
    let mut stderr = BufReader::new(gc.0.stderr.take().unwrap());
    let mut first = String::new();
    stderr.read_line(&mut first).unwrap();
    let pid = d.child.id().to_string();
    Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    let t = Instant::now();
    while d.socket.exists() {
        assert!(
            t.elapsed() < Duration::from_secs(2),
            "socket not removed when shutdown began"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        d.child.try_wait().unwrap().is_none(),
        "still draining the stubborn handler"
    );
    let started = Instant::now();
    Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    let code = d.child.wait().unwrap().code();
    assert_eq!(code, Some(130));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn a6_completions_to_a_full_device_exits_1_without_panicking() {
    let _w = watchdog();
    let dir = private_dir();
    let full = std::path::Path::new("/dev/full");
    if !full.exists() {
        return;
    }
    for shell in ["bash", "zsh", "fish", "elvish", "powershell"] {
        let out = Command::new(BIN)
            .args(["completions", shell])
            .stdout(std::fs::OpenOptions::new().write(true).open(full).unwrap())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{shell}");
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("panicked"),
            "{shell} panicked: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let out = Command::new(BIN)
        .args(["completions", "bash"])
        .stdout(Stdio::piped())
        .output()
        .unwrap();
    assert!(out.stdout.starts_with(b"#"), "still a completion script");
    drop(dir);
}

#[test]
fn a7_json_mode_emits_a_json_error_for_usage_errors() {
    let _w = watchdog();
    for args in [
        vec!["--json", "snapshot", "create"],
        vec!["--json", "bogus"],
        vec!["--json", "snapshot"],
        vec!["--json", "gc", "--nope"],
        vec!["--json", "import", "dir"],
    ] {
        let out = Command::new(BIN).args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{args:?} stdout is not one JSON object: {e}: {:?}",
                String::from_utf8_lossy(&out.stdout)
            )
        });
        assert_eq!(v["error"]["code"], "usage", "{args:?}");
    }
    let out = Command::new(BIN)
        .args(["--json", "--version"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(!out.stdout.is_empty());
    let out = Command::new(BIN)
        .args(["--json", "--help"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("cowfs"));
}

#[test]
fn a8_a_connect_timeout_is_exit_4() {
    let _w = watchdog();
    // A listener that accepts nothing at all: the backlog fills and further connects time out.
    let dir = private_dir();
    let path = dir.path().join("blackhole.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let _kept = std::thread::spawn(move || {
        let mut held = Vec::new();
        for s in listener.incoming().flatten() {
            held.push(s);
            if held.len() > 4096 {
                break;
            }
        }
    });
    let started = Instant::now();
    let out = Command::new(BIN)
        .args(["--json", "--timeout", "1", "--socket"])
        .arg(&path)
        .arg("status")
        .output()
        .unwrap();
    let took = started.elapsed();
    // Either the daemon is reachable (0) or the connect times out (4); never 1.
    assert!(
        matches!(out.status.code(), Some(0) | Some(4)),
        "rc={:?} after {:?}: {}",
        out.status.code(),
        took,
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.code() == Some(4) {
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["error"]["code"], "timeout");
    }
}

#[test]
fn a10_a_held_connection_does_not_survive_a_silent_client() {
    let _w = watchdog();
    let d = Daemon::start(&[]);
    for _ in 0..40 {
        drop(UnixStream::connect(&d.socket).unwrap());
    }
    assert_eq!(d.run(&["status"]).status.code(), Some(0));
}
