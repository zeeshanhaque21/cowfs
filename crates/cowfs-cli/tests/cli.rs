//! Runs the built `cowfs` binary against a `cowfs serve --stub` server.

use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_cowfs");

struct Daemon {
    _dir: TempDir,
    socket: PathBuf,
    child: Child,
}

impl Daemon {
    fn start(extra: &[&str]) -> Daemon {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
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
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while UnixStream::connect(&socket).is_err() {
            assert!(child.try_wait().unwrap().is_none(), "serve exited early");
            assert!(Instant::now() < deadline, "server did not come up");
            std::thread::sleep(Duration::from_millis(20));
        }
        Daemon {
            _dir: dir,
            socket,
            child,
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
    assert_eq!(keys(&import), ["bytes", "files", "name", "verified"]);
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
    assert!(out.stdout.is_empty());
    let err: Value = serde_json::from_slice(&out.stderr).unwrap();
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
        serde_json::from_slice::<Value>(&out.stderr).unwrap()["error"]["code"],
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
        serde_json::from_slice::<Value>(&out.stderr).unwrap()["error"]["code"],
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
    let mut child = d
        .cmd(&["gc"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut first = String::new();
    stderr.read_line(&mut first).unwrap();
    assert!(
        first.starts_with("mark"),
        "progress reaches stderr: {first:?}"
    );
    let started = Instant::now();
    let kill = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(kill.success());
    let status = child.wait().unwrap();
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
fn progress_goes_to_stderr_one_line_per_phase() {
    let d = Daemon::start(&[]);
    let out = d.run(&["--json", "gc"]);
    let err = String::from_utf8(out.stderr).unwrap();
    let lines: Vec<&str> = err.lines().collect();
    assert_eq!(lines.len(), 2, "{err:?}");
    assert!(lines[0].starts_with("mark") && lines[1].starts_with("sweep"));
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
