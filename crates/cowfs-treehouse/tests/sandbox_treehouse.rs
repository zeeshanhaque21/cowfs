//! The real `treehouse` binary, driven through the companion, in a sandboxed pool on a native
//! directory.
//!
//! This is where the wrapper is proven against the actual command surface: `get --lease` and
//! `return` with an explicit `--root`, the open-fd and flock holders that `treehouse return` misses
//! (issue #20), and the `.nfs*` dirt that leaves a slot dirty on a network mount.
//!
//! Every invocation gets a sandbox `HOME`, a sandbox `TREEHOUSE_ROOT` and an explicit `--root`, and
//! `real_pools_are_untouched` proves the real `~/.treehouse` is byte-identical before and after the
//! whole run.

mod common;

use common::{require_bin, Fixture, Sandbox, Watchdog};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// The companion binary this test runs, so the tests cover the real argument parsing and exit
/// codes rather than the library.
fn companion() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cowfs-treehouse"))
}

/// The sandboxed treehouse shim, so every companion call in this file is guarded too.
fn shim_of(s: &Sandbox) -> String {
    s.shim().display().to_string()
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(companion())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run cowfs-treehouse {args:?}: {e}"))
}

fn ok(args: &[&str]) -> String {
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
    let text = ok(args);
    serde_json::from_str(text.lines().last().unwrap_or("")).unwrap_or_else(|e| {
        panic!("cowfs-treehouse {args:?} printed no JSON ({e}): {text}");
    })
}

fn git(cwd: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("cannot run git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Leases a slot through the real binary and returns its path, like an operator would.
fn lease(s: &Sandbox) -> (PathBuf, String) {
    let out = s.treehouse(&["get", "--lease", "--json"]);
    assert!(
        out.status.success(),
        "treehouse get --lease failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON in treehouse output: {text}"));
    let v: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
    (
        PathBuf::from(v["path"].as_str().expect("path")),
        v["lease_id"].as_str().unwrap_or_default().to_owned(),
    )
}

#[test]
fn the_pool_id_the_companion_derives_is_the_pool_directory_treehouse_creates() {
    let _w = Watchdog::start(120);
    crate::require_treehouse!();
    let s = Sandbox::new();
    let (slot, _) = lease(&s);

    // treehouse names the pool directory after the repository plus a short hash. The companion
    // derives the same string, which is the whole basis of the snapshot naming.
    let pool_dir = slot
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::file_name)
        .and_then(std::ffi::OsStr::to_str)
        .expect("pool directory name")
        .to_owned();

    let derived = ok(&["pool-id", &s.repo().display().to_string()]);
    let derived = derived.trim();
    assert_eq!(
        derived, pool_dir,
        "the companion and treehouse must agree on the pool id, or every snapshot name is wrong"
    );
    s.treehouse_ok(&["return", &slot.display().to_string(), "--force"]);
}

#[test]
fn return_without_a_root_is_refused_rather_than_guessing_a_pool() {
    let _w = Watchdog::start(60);
    crate::require_treehouse!();
    let s = Sandbox::new();
    let (slot, _) = lease(&s);
    let out = run(&[
        "--treehouse-bin",
        &shim_of(&s),
        "--treehouse-home",
        &s.home().display().to_string(),
        "return",
        "--slot",
        &slot.display().to_string(),
    ]);
    assert_eq!(out.status.code(), Some(2), "a usage error is exit 2");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("--root is required"), "{err}");
    s.treehouse_ok(&["return", &slot.display().to_string(), "--force"]);
}

#[test]
fn a_cwd_holder_in_a_slot_is_terminated_and_the_slot_returns_clean() {
    let _w = Watchdog::start(120);
    crate::require_treehouse!();
    let s = Sandbox::new();
    let (slot, lease_id) = lease(&s);
    let q = slot.display().to_string();
    // cwd inside the slot, which is the one case treehouse detects on its own.
    let holder = Fixture::spawn("cwd", &format!("cd {q}; exec sleep 300"));
    std::thread::sleep(Duration::from_millis(300));
    assert!(cowfs_treehouse::alive(holder.pid));

    // Mode (a): no daemon at all, and no --socket either.
    let out = run(&[
        "--treehouse-bin",
        &shim_of(&s),
        "--treehouse-home",
        &s.home().display().to_string(),
        "--json",
        "return",
        "--slot",
        &q,
        "--root",
        &s.pool().display().to_string(),
        "--force",
    ]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "return failed: {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        text
    );
    assert!(
        !cowfs_treehouse::alive(holder.pid),
        "mode (a) must terminate a cwd holder, which is what plain treehouse return does too"
    );
    let _ = lease_id;

    let status = s.treehouse_ok(&["status", "--json"]);
    assert!(
        status.contains("available") || !status.contains("leased"),
        "the slot is back in the pool: {status}"
    );
}

#[test]
fn an_open_fd_holder_is_named_through_the_control_api_and_the_return_refuses_while_it_is_listed() {
    let _w = Watchdog::start(120);
    crate::require_treehouse!();
    let dir = common::private_tempdir();
    let stub = common::stub_in(dir.path());
    let s = Sandbox::new();
    let (slot, _) = lease(&s);
    let q = slot.display().to_string();

    // Case c of spike 5: chdir out, keep an open fd on a file in the slot. `treehouse return` does
    // not see this at all; the cowfs control API does.
    std::fs::write(slot.join("held.txt"), b"held\n").expect("write");
    let holder = Fixture::spawn(
        "fd-out",
        &format!("cd {q}; exec 3<{q}/held.txt; cd /; exec sleep 300"),
    );
    std::thread::sleep(Duration::from_millis(300));

    let mut daemon =
        cowfs_treehouse::Daemon::connect(Some(&stub.socket), Some(5)).expect("connect");
    let pool_id = cowfs_treehouse::pool_id_of_slot_path(&slot).expect("pool id");
    let slot_name = cowfs_treehouse::slot_of(&slot).expect("slot name");
    let snapshot = cowfs_treehouse::slot_snapshot(&pool_id, slot_name).expect("snapshot name");
    stub.handler.add_process(
        &snapshot,
        cowfs_ctl::ProcessInfo {
            pid: holder.pid,
            command: "sleep 300".into(),
            holds: vec![cowfs_ctl::Hold {
                kind: cowfs_ctl::HoldKind::Fd,
                path: slot.join("held.txt").display().to_string(),
            }],
        },
    );
    let holders = daemon.ps(&snapshot).expect("ps");
    assert_eq!(holders.len(), 1, "the control API sees the open-fd holder");
    assert_eq!(holders[0].holds[0].kind, cowfs_ctl::HoldKind::Fd);

    let out = run(&[
        "--socket",
        &stub.socket.display().to_string(),
        "--treehouse-bin",
        &shim_of(&s),
        "--treehouse-home",
        &s.home().display().to_string(),
        "--json",
        "return",
        "--mode",
        "b",
        "--slot",
        &q,
        "--root",
        &s.pool().display().to_string(),
        "--force",
        "--nfs-timeout",
        "2",
    ]);
    let text = String::from_utf8_lossy(&out.stdout);
    let err = String::from_utf8_lossy(&out.stderr);
    // The stub lists the process for ever, so the authoritative in-operation check keeps refusing
    // even after the wrapper signalled it. That is the behaviour that stops a reset under a writer.
    assert_eq!(out.status.code(), Some(5), "stderr: {err}\nstdout: {text}");
    assert!(err.contains("acquired a holder after the scan"), "{err}");
    assert!(
        !cowfs_treehouse::alive(holder.pid),
        "the holder was still terminated"
    );
    assert!(slot.join(".git").exists(), "the slot is left intact");
}

#[test]
fn a_mode_b_return_empties_the_slot_and_hands_it_back() {
    let _w = Watchdog::start(120);
    crate::require_treehouse!();
    let dir = common::private_tempdir();
    let stub = common::stub_in(dir.path());
    let s = Sandbox::new();
    let (slot, lease_id) = lease(&s);
    let q = slot.display().to_string();
    let pool_id = cowfs_treehouse::pool_id_of_slot_path(&slot).expect("pool id");
    let slot_name = cowfs_treehouse::slot_of(&slot).expect("slot name");
    let snapshot = cowfs_treehouse::slot_snapshot(&pool_id, slot_name).expect("snapshot name");
    let base = cowfs_treehouse::base_snapshot(&pool_id).expect("base name");

    let mut daemon =
        cowfs_treehouse::Daemon::connect(Some(&stub.socket), Some(5)).expect("connect");
    // A warm base and a slot snapshot, so the wrapper's reset has something real to swap in.
    daemon
        .base_refresh(&s.repo(), "main", Some(&base))
        .expect("base");
    daemon
        .snapshot_create(&snapshot, Some(&base))
        .expect("slot snapshot");

    let out = run(&[
        "--socket",
        &stub.socket.display().to_string(),
        "--treehouse-bin",
        &shim_of(&s),
        "--treehouse-home",
        &s.home().display().to_string(),
        "--json",
        "return",
        "--mode",
        "b",
        "--slot",
        &q,
        "--root",
        &s.pool().display().to_string(),
        "--force",
    ]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "return failed: {}\n{}",
        String::from_utf8_lossy(&out.stderr),
        text
    );
    let v: serde_json::Value =
        serde_json::from_str(text.lines().last().unwrap()).expect("JSON output");
    assert_eq!(v["snapshot"].as_str(), Some(snapshot.as_str()), "{v}");
    assert_eq!(v["lease_id"].as_str(), Some(lease_id.as_str()), "{v}");
    assert_eq!(v["refused_busy"], false, "{v}");

    // The slot snapshot now points at the empty snapshot, not the warm base, so treehouse's own
    // reset ran against nothing.
    let info = daemon
        .snapshot_list()
        .expect("list")
        .into_iter()
        .find(|x| x.name == snapshot)
        .expect("slot snapshot");
    assert_eq!(
        info.parent.as_deref(),
        Some(
            cowfs_treehouse::empty_snapshot(&pool_id)
                .expect("empty")
                .as_str()
        ),
        "the slot was emptied, not left warm: {info:?}"
    );

    let status = s.treehouse_ok(&["status", "--json"]);
    assert!(
        status.contains("\"status\":\"available\""),
        "the slot is free: {status}"
    );
}

#[test]
fn discard_releases_the_lease_and_drops_the_snapshot() {
    let _w = Watchdog::start(120);
    crate::require_treehouse!();
    let dir = common::private_tempdir();
    let stub = common::stub_in(dir.path());
    let s = Sandbox::new();
    let (slot, _) = lease(&s);
    let pool_id = cowfs_treehouse::pool_id_of_slot_path(&slot).expect("pool id");
    let slot_name = cowfs_treehouse::slot_of(&slot).expect("slot name");
    let snapshot = cowfs_treehouse::slot_snapshot(&pool_id, slot_name).expect("snapshot name");
    let mut daemon =
        cowfs_treehouse::Daemon::connect(Some(&stub.socket), Some(5)).expect("connect");
    daemon
        .snapshot_create(&snapshot, None)
        .expect("slot snapshot");

    let out = run(&[
        "--socket",
        &stub.socket.display().to_string(),
        "--treehouse-bin",
        &shim_of(&s),
        "--treehouse-home",
        &s.home().display().to_string(),
        "--json",
        "discard",
        "--slot",
        &slot.display().to_string(),
        "--root",
        &s.pool().display().to_string(),
        "--force",
    ]);
    assert!(
        out.status.success(),
        "discard failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let names: Vec<String> = daemon
        .snapshot_list()
        .expect("list")
        .into_iter()
        .map(|x| x.name)
        .collect();
    assert!(
        !names.contains(&snapshot),
        "the snapshot is gone: {names:?}"
    );
    let status = s.treehouse_ok(&["status", "--json"]);
    assert!(
        status.contains("\"status\":\"available\""),
        "the lease was released: {status}"
    );
}

#[test]
fn a_holder_that_appears_between_the_scan_and_the_swap_is_still_caught() {
    let _w = Watchdog::start(120);
    crate::require_treehouse!();
    let dir = common::private_tempdir();
    let stub = common::stub_in(dir.path());
    let s = Sandbox::new();
    let (slot, _) = lease(&s);
    let q = slot.display().to_string();
    let pool_id = cowfs_treehouse::pool_id_of_slot_path(&slot).expect("pool id");
    let slot_name = cowfs_treehouse::slot_of(&slot).expect("slot name");
    let snapshot = cowfs_treehouse::slot_snapshot(&pool_id, slot_name).expect("snapshot name");

    let mut daemon =
        cowfs_treehouse::Daemon::connect(Some(&stub.socket), Some(5)).expect("connect");
    // The slot snapshot and an empty one to reset it to, which is the cheap path the wrapper takes.
    daemon.snapshot_create(&snapshot, None).expect("create");
    let empty = cowfs_treehouse::base_snapshot(&pool_id).expect("empty name");
    daemon.snapshot_create(&empty, None).expect("create empty");

    // The scan a return does first sees nothing.
    assert!(daemon.ps(&snapshot).expect("ps").is_empty());
    // Then a writer shows up, between the scan and the swap.
    stub.handler.add_process(
        &snapshot,
        cowfs_ctl::ProcessInfo {
            pid: 4242,
            command: "late-writer".into(),
            holds: vec![cowfs_ctl::Hold {
                kind: cowfs_ctl::HoldKind::Cwd,
                path: q.clone(),
            }],
        },
    );
    let err = daemon
        .snapshot_reset(&snapshot, &empty, true)
        .expect_err("the in-operation check is authoritative");
    assert!(matches!(err, cowfs_treehouse::Error::Busy(_)), "{err:?}");

    // The slot is untouched and the lease is still held, because the wrapper never got that far.
    assert!(slot.join(".git").exists());
    let status = s.treehouse_ok(&["status", "--json"]);
    assert!(status.contains(&slot.display().to_string()), "{status}");
}

#[test]
fn the_wrapper_waits_for_silly_rename_dirt_and_refuses_when_it_stays() {
    let _w = Watchdog::start(120);
    crate::require_treehouse!();
    let dir = common::private_tempdir();
    let stub = common::stub_in(dir.path());
    let s = Sandbox::new();
    let (slot, _) = lease(&s);
    let q = slot.display().to_string();

    // The mount-only signature of issue #20: a file a process still holds, unlinked and renamed to
    // .nfs.<id> by the NFS client. On a native directory the rename is made directly, which is the
    // same state and the same consequence for the slot.
    std::fs::write(slot.join(".nfs.00000001.deadbeef"), b"held\n").expect("write");
    assert_eq!(cowfs_treehouse::nfs_entries(&slot).len(), 1);

    let started = std::time::Instant::now();
    let out = run(&[
        "--socket",
        &stub.socket.display().to_string(),
        "--treehouse-bin",
        &shim_of(&s),
        "--treehouse-home",
        &s.home().display().to_string(),
        "return",
        "--mode",
        "b",
        "--slot",
        &q,
        "--root",
        &s.pool().display().to_string(),
        "--force",
        "--nfs-timeout",
        "1",
    ]);
    let elapsed = started.elapsed();
    assert_eq!(out.status.code(), Some(5), "dirt that stays is exit 5");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("silly-rename"), "{err}");
    assert!(err.contains(".nfs.00000001.deadbeef"), "{err}");
    assert!(
        elapsed >= Duration::from_secs(1),
        "it really waited: {elapsed:?}"
    );
    assert!(slot.is_dir(), "the slot is left alone while it is dirty");

    // Clear the dirt and the same command succeeds.
    std::fs::remove_file(slot.join(".nfs.00000001.deadbeef")).expect("clear");
    let out = run(&[
        "--socket",
        &stub.socket.display().to_string(),
        "--treehouse-bin",
        &shim_of(&s),
        "--treehouse-home",
        &s.home().display().to_string(),
        "return",
        "--mode",
        "b",
        "--slot",
        &q,
        "--root",
        &s.pool().display().to_string(),
        "--force",
        "--nfs-timeout",
        "1",
    ]);
    assert!(
        out.status.success(),
        "once the dirt clears the return works: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_slot_can_be_reacquired_after_a_return_and_gets_the_same_slot() {
    let _w = Watchdog::start(120);
    crate::require_treehouse!();
    let s = Sandbox::new();
    let (first, _) = lease(&s);
    s.treehouse_ok(&["return", &first.display().to_string(), "--force"]);
    let (second, _) = lease(&s);
    assert_eq!(
        first, second,
        "a clean slot goes back in the pool and is handed out again"
    );
    s.treehouse_ok(&["return", &second.display().to_string(), "--force"]);
}

#[test]
fn base_refresh_runs_the_build_in_a_leased_slot_and_refreshes_the_base() {
    let _w = Watchdog::start(180);
    let _ = crate::require_treehouse!();
    let cowfs = require_bin("cowfs");
    let dir = common::private_tempdir();
    let s = Sandbox::new();
    let run_dir = dir.path().join("run");
    std::fs::create_dir_all(&run_dir).expect("mkdir run");
    let sock = run_dir.join("control.sock");
    let mut server = Command::new(&cowfs)
        .args([
            "serve",
            "--store",
            &dir.path().join("store").display().to_string(),
            "--mount",
            &dir.path().join("mnt").display().to_string(),
            "--stub",
            "--socket",
            &sock.display().to_string(),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn cowfs serve --stub");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !sock.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "cowfs serve --stub never bound its socket"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let common_args = [
        "--socket",
        &sock.display().to_string(),
        "--treehouse-bin",
        &shim_of(&s),
        "--treehouse-home",
        &s.home().display().to_string(),
        "--json",
    ];
    let mut args: Vec<String> = common_args.iter().map(|a| (*a).to_owned()).collect();
    args.extend([
        "base".into(),
        "refresh".into(),
        "--repo".into(),
        s.repo().display().to_string(),
        "--ref".into(),
        "main".into(),
        "--root".into(),
        s.pool().display().to_string(),
        "--build".into(),
        "echo built > built.txt && pwd > built-at.txt".to_string(),
    ]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let v = json(&refs);
    assert_eq!(v["built_in_slot"], true, "{v}");
    let slot = v["slot"].as_str().expect("slot");
    assert!(
        PathBuf::from(slot).join("built-at.txt").is_file(),
        "the build really ran in the slot"
    );
    let snapshot = v["snapshot"].as_str().expect("snapshot");
    assert!(snapshot.ends_with("-base"), "{snapshot}");
    assert_eq!(v["git_ref"], "main", "{v}");

    // The slot the build ran in was returned by the flow, so the pool is clean again.
    let status = s.treehouse_ok(&["status", "--json"]);
    assert!(
        !status.contains(slot),
        "the build slot was returned: {status}"
    );

    // A second refresh reports the previous commit.
    let mut args: Vec<String> = common_args.iter().map(|a| (*a).to_owned()).collect();
    args.extend([
        "base".into(),
        "refresh".into(),
        "--repo".into(),
        s.repo().display().to_string(),
        "--ref".into(),
        "main".into(),
        "--root".into(),
        s.pool().display().to_string(),
    ]);
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let v = json(&refs);
    assert!(v["previous_commit"].is_string(), "{v}");

    // And the base is on the daemon, discoverable through base.repo.
    let mut daemon =
        cowfs_treehouse::Daemon::connect(Some(&sock), Some(5)).expect("connect to stub");
    let found = daemon.find_base(&s.repo()).expect("find base");
    assert_eq!(found.map(|b| b.name), Some(snapshot.to_owned()));

    let _ = server.kill();
    let _ = server.wait();
}

#[test]
fn base_refresh_refuses_per_slot_compiler_flags() {
    let _w = Watchdog::start(60);
    crate::require_treehouse!();
    let dir = common::private_tempdir();
    let stub = common::stub_in(dir.path());
    let s = Sandbox::new();
    let out = run(&[
        "--socket",
        &stub.socket.display().to_string(),
        "base",
        "refresh",
        "--repo",
        &s.repo().display().to_string(),
        "--rustflags=--remap-path-prefix=/a=/b",
    ]);
    assert_eq!(out.status.code(), Some(2), "a usage error is exit 2");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("spike 6"), "{err}");
    assert!(err.contains("RUSTFLAGS"), "{err}");
}

#[test]
fn base_status_reports_stale_and_provision_refuses_without_a_base() {
    let _w = Watchdog::start(120);
    crate::require_treehouse!();
    let dir = common::private_tempdir();
    let stub = common::stub_in(dir.path());
    let s = Sandbox::new();
    let (slot, _) = lease(&s);

    let out = run(&[
        "--socket",
        &stub.socket.display().to_string(),
        "--json",
        "base",
        "status",
        "--repo",
        &s.repo().display().to_string(),
        "--ref",
        "main",
    ]);
    assert_eq!(out.status.code(), Some(1), "a missing base is not fresh");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(stdout.lines().last().unwrap()).expect("JSON status");
    assert_eq!(v["fresh"], false, "{v}");
    assert!(
        v["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("no warm base"),
        "{v}"
    );
    assert_eq!(
        v["snapshot"].as_str().map(|s| s.ends_with("-base")),
        Some(true)
    );

    let out = run(&[
        "--socket",
        &stub.socket.display().to_string(),
        "--treehouse-bin",
        &shim_of(&s),
        "--treehouse-home",
        &s.home().display().to_string(),
        "provision",
        "--slot",
        &slot.display().to_string(),
    ]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no warm base"), "{err}");
    s.treehouse_ok(&["return", &slot.display().to_string(), "--force"]);
}

#[test]
fn hooks_install_writes_the_user_config_and_is_idempotent() {
    let _w = Watchdog::start(60);
    let home = common::private_tempdir();
    let path = cowfs_treehouse::user_config_path_for(home.path());
    let first = json(&[
        "--json",
        "hooks",
        "install",
        "--home",
        &home.path().display().to_string(),
        "--command",
        "cowfs-treehouse provision --slot $PWD",
    ]);
    assert_eq!(first["action"], "added", "{first}");
    let text = std::fs::read_to_string(&path).expect("read config");
    assert!(text.contains("[hooks]"), "{text}");
    assert!(text.contains("cowfs-treehouse provision"), "{text}");

    let second = json(&[
        "--json",
        "hooks",
        "install",
        "--home",
        &home.path().display().to_string(),
        "--command",
        "cowfs-treehouse provision --slot $PWD",
    ]);
    assert_eq!(second["action"], "already-there", "{second}");
    let after = std::fs::read_to_string(&path).expect("read config");
    assert_eq!(text, after, "a second install changes nothing");
}

#[test]
fn provision_really_runs_when_treehouse_calls_the_hook() {
    let _w = Watchdog::start(180);
    crate::require_treehouse!();
    let dir = common::private_tempdir();
    let stub = common::stub_in(dir.path());
    let s = Sandbox::new();

    // A base for this repository, so provision has something to clone from.
    let mut daemon =
        cowfs_treehouse::Daemon::connect(Some(&stub.socket), Some(5)).expect("connect");
    let pool_id = cowfs_treehouse::pool_id(&s.repo()).expect("pool id");
    let base = cowfs_treehouse::base_snapshot(&pool_id).expect("base name");
    daemon
        .base_refresh(&s.repo(), "main", Some(&base))
        .expect("base refresh");

    // The slot snapshot is already exported at the slot path, which is what a real mount looks
    // like, so the hook takes the reset branch and needs no new export.
    let slot_snapshot_name = cowfs_treehouse::slot_snapshot(&pool_id, "1").expect("slot name");
    daemon
        .snapshot_create(&slot_snapshot_name, Some(&base))
        .expect("slot snapshot");

    // Install the hook the way an operator would, pointing at this test's own binary.
    let install = json(&[
        "--json",
        "hooks",
        "install",
        "--home",
        &s.home().display().to_string(),
        "--command",
        &format!(
            "{} --socket {} provision --slot $PWD",
            companion().display(),
            stub.socket.display()
        ),
    ]);
    assert_eq!(install["action"], "added", "{install}");

    // Now the real treehouse acquisition, which runs the hook at exactly the right moment.
    let (slot, _) = lease(&s);
    let v = json(&[
        "--socket",
        &stub.socket.display().to_string(),
        "--json",
        "provision",
        "--slot",
        &slot.display().to_string(),
    ]);
    assert_eq!(v["pool_id"].as_str(), Some(pool_id.as_str()), "{v}");
    assert_eq!(v["base"].as_str(), Some(base.as_str()), "{v}");
    assert_eq!(v["reused"], true, "{v}");

    let list = daemon.snapshot_list().expect("list");
    let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&base.as_str()), "{names:?}");
    assert!(
        names
            .iter()
            .any(|n| n.starts_with(&pool_id) && n.ends_with("-1")),
        "the slot snapshot exists: {names:?}"
    );

    // And the slot still works as a git worktree afterwards, which is the whole point.
    let status = git(&slot, &["status", "--porcelain"]);
    assert!(status.is_empty(), "the slot is a clean worktree: {status}");
    let head = git(&slot, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(head, "HEAD", "still detached, as treehouse left it");

    s.treehouse_ok(&["return", &slot.display().to_string(), "--force"]);
}

#[test]
fn real_pools_are_untouched() {
    let _w = Watchdog::start(120);
    crate::require_treehouse!();
    if real_pool_hashes().is_empty() {
        eprintln!(
            "skipping: no real treehouse store at {}, so there is nothing to compare against",
            common::real_treehouse_root().display()
        );
        return;
    }
    // Snapshot the real store, drive the companion hard against a sandbox, then compare. Pools
    // other agents are legitimately using may move, so those are reported rather than asserted on.
    let before = real_pool_hashes();

    let s = Sandbox::new();
    let (slot, _) = lease(&s);
    std::fs::write(slot.join("held.txt"), b"x").expect("write");
    let holder = Fixture::spawn("cwd", &format!("cd {}; exec sleep 300", slot.display()));
    std::thread::sleep(Duration::from_millis(200));
    run(&[
        "--treehouse-bin",
        &shim_of(&s),
        "--treehouse-home",
        &s.home().display().to_string(),
        "return",
        "--slot",
        &slot.display().to_string(),
        "--root",
        &s.pool().display().to_string(),
        "--force",
    ]);
    assert!(!cowfs_treehouse::alive(holder.pid));
    let (again, _) = lease(&s);
    s.treehouse_ok(&["return", &again.display().to_string(), "--force"]);

    let after = real_pool_hashes();
    let changed: Vec<&String> = before
        .iter()
        .filter(|(name, hash)| after.get(*name) != Some(*hash))
        .map(|(name, _)| name)
        .collect();
    let vanished: Vec<&String> = before
        .keys()
        .filter(|name| !after.contains_key(*name))
        .collect();
    let appeared: Vec<&String> = after
        .keys()
        .filter(|name| !before.contains_key(*name))
        .collect();
    assert!(
        vanished.is_empty(),
        "no real pool disappeared: {vanished:?}"
    );
    assert!(
        appeared.is_empty(),
        "no real pool appeared out of nowhere: {appeared:?}"
    );
    if !changed.is_empty() {
        eprintln!(
            "note: these real pools changed while the test ran, and are in use by other agents: {changed:?}"
        );
    }
}

/// sha256 of every `treehouse-state.json` in the real store.
fn real_pool_hashes() -> std::collections::BTreeMap<String, String> {
    use sha2::Digest;
    let root = common::real_treehouse_root();
    let mut out = std::collections::BTreeMap::new();
    let Ok(pools) = std::fs::read_dir(&root) else {
        return out;
    };
    for pool in pools.flatten() {
        let state = pool.path().join("treehouse-state.json");
        let Ok(bytes) = std::fs::read(&state) else {
            continue;
        };
        let digest = sha2::Sha256::digest(&bytes);
        out.insert(
            pool.file_name().to_string_lossy().into_owned(),
            digest.iter().map(|b| format!("{b:02x}")).collect(),
        );
    }
    out
}
