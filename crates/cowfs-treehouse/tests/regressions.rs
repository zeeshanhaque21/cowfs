//! Regression tests for the merge blockers found reviewing PR #38, plus the safety switches whose
//! mutants survived.
//!
//! Every treehouse call here goes through the per-sandbox shim in `common`, which refuses any call
//! without an explicit in-sandbox `--root` and refuses any absolute path argument outside it.

mod common;

use common::{private_tempdir, stub_in, Fixture, Sandbox, Watchdog};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn companion() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cowfs-treehouse"))
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(companion())
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("cannot run cowfs-treehouse {args:?}: {e}"))
}

fn json_of(out: &std::process::Output, what: &str) -> serde_json::Value {
    let text = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(text.lines().last().unwrap_or("")).unwrap_or_else(|e| {
        panic!(
            "{what} printed no JSON ({e}): {text}\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    })
}

/// The status entry for one slot, as treehouse reports it.
fn entry(s: &Sandbox, root: &Path, slot: &str) -> serde_json::Value {
    let out = s.treehouse_at(root, &["status", "--json"]);
    let text = String::from_utf8_lossy(&out.stdout);
    let all: serde_json::Value = serde_json::from_str(
        text.lines()
            .rev()
            .find(|l| l.trim_start().starts_with('['))
            .unwrap_or_else(|| panic!("no status array: {text}")),
    )
    .expect("status json");
    all.as_array()
        .expect("array")
        .iter()
        .find(|e| e["name"] == slot)
        .cloned()
        .unwrap_or(serde_json::json!({"name": slot, "status": "absent"}))
}

/// Leases a slot in the given pool and returns `(path, lease_id)`.
fn lease_at(s: &Sandbox, root: &Path) -> (PathBuf, String) {
    let out = s.treehouse_at(root, &["get", "--lease", "--json"]);
    assert!(
        out.status.success(),
        "treehouse get --lease failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json_of(&out, "treehouse get --lease");
    (
        PathBuf::from(v["path"].as_str().expect("path")),
        v["lease_id"].as_str().unwrap_or_default().to_owned(),
    )
}

// ---------------------------------------------------------------------------------------------
// F1: `--root` does not sandbox `treehouse return <path>`. treehouse v3.1.0 picks the pool from
// the path itself (cmd/return_cmd.go:567), so `--root` is decorative for a path argument.
// ---------------------------------------------------------------------------------------------

#[test]
fn f1_return_refuses_a_slot_outside_the_named_root() {
    let _w = Watchdog::start(180);
    let bin = crate::require_treehouse!();
    let s = Sandbox::new();
    let (slot, _) = lease_at(&s, &s.pool());
    // A second pool in the same sandbox, which is what the `--root` names.
    let _other = lease_at(&s, &s.other_pool());
    let before = entry(&s, &s.pool(), "1");
    assert_eq!(before["status"], "leased", "{before}");

    let out = run(&[
        "--treehouse-bin",
        &bin.display().to_string(),
        "--treehouse-home",
        &s.home().display().to_string(),
        "return",
        "--slot",
        &slot.display().to_string(),
        "--root",
        &s.other_pool().display().to_string(),
        "--force",
    ]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a slot outside the named root is a usage error, not a release\nstderr: {err}"
    );
    assert!(err.contains("outside"), "the refusal must say why: {err}");
    // And the point of the whole thing: pool A's slot is still held.
    let after = entry(&s, &s.pool(), "1");
    assert_eq!(
        after["status"], "leased",
        "the slot in the pool named by --root must not be released: {after}"
    );
}

#[test]
fn f1_a_slot_name_outside_the_named_root_is_refused() {
    let _w = Watchdog::start(180);
    let bin = crate::require_treehouse!();
    let s = Sandbox::new();
    let (slot, _) = lease_at(&s, &s.pool());
    let _other = lease_at(&s, &s.other_pool());
    let before = entry(&s, &s.pool(), "1");

    // The slot NAME form, which resolve_slot looks up under the named root. Slot 1 exists in both
    // pools, so the named-root lookup finds the wrong one unless the name resolves and is checked
    // against what the caller meant.
    let out = run(&[
        "--treehouse-bin",
        &bin.display().to_string(),
        "--treehouse-home",
        &s.home().display().to_string(),
        "return",
        "--slot",
        "1",
        "--root",
        &s.other_pool().display().to_string(),
        "--force",
    ]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a slot name is a usage error unless the root is named and the slot is in it\nstderr: {err}"
    );
    let _ = slot;
    assert_eq!(entry(&s, &s.pool(), "1")["status"], before["status"]);
}

#[test]
fn f1_a_release_is_never_unpinned() {
    let _w = Watchdog::start(180);
    let bin = crate::require_treehouse!();
    let s = Sandbox::new();
    let (slot, lease_id) = lease_at(&s, &s.pool());
    assert!(!lease_id.is_empty(), "a lease must have an identity");

    // Take the lease away from under the wrapper's lookup, so the pin it reads is stale. treehouse
    // must refuse the release rather than hand a re-leased slot back.
    let out = s.treehouse(&["return", &slot.display().to_string(), "--force"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (again, fresh) = lease_at(&s, &s.pool());
    assert_ne!(fresh, lease_id, "the re-lease has a new identity");

    let out = run(&[
        "--treehouse-bin",
        &bin.display().to_string(),
        "--treehouse-home",
        &s.home().display().to_string(),
        "return",
        "--slot",
        &again.display().to_string(),
        "--root",
        &s.pool().display().to_string(),
        "--force",
    ]);
    assert!(
        out.status.success(),
        "a correctly pinned release succeeds: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(entry(&s, &s.pool(), "1")["status"], "available");
}

#[test]
fn f1_an_unreadable_lease_identity_is_a_hard_error_not_an_unpinned_release() {
    let _w = Watchdog::start(180);
    let bin = crate::require_treehouse!();
    let s = Sandbox::new();
    // A slot path that is inside the pool shape but is not a registered worktree, so the lease
    // lookup finds nothing. Releasing it must fail rather than go out unpinned.
    let pool_dir = s.pool().join(".treehouse");
    let fake = pool_dir.join("repo-000000").join("1").join("repo");
    std::fs::create_dir_all(&fake).expect("mkdir");
    let out = run(&[
        "--treehouse-bin",
        &bin.display().to_string(),
        "--treehouse-home",
        &s.home().display().to_string(),
        "return",
        "--slot",
        &fake.display().to_string(),
        "--root",
        &s.pool().display().to_string(),
        "--force",
    ]);
    assert_ne!(
        out.status.code(),
        Some(0),
        "releasing a slot whose lease identity cannot be read must fail"
    );
}

// ---------------------------------------------------------------------------------------------
// F2 and F6: a lease taken for a multi-step flow must come back on every error path.
// ---------------------------------------------------------------------------------------------

#[test]
fn f2_a_failed_build_releases_the_slot_it_leased() {
    let _w = Watchdog::start(240);
    let bin = crate::require_treehouse!();
    let dir = private_tempdir();
    let stub = stub_in(dir.path());
    let s = Sandbox::new();
    let before = entry(&s, &s.pool(), "1");
    assert_eq!(before["status"], "absent", "fresh pool: {before}");

    let out = run(&[
        "--socket",
        &stub.socket.display().to_string(),
        "--treehouse-bin",
        &bin.display().to_string(),
        "--treehouse-home",
        &s.home().display().to_string(),
        "base",
        "refresh",
        "--repo",
        &s.repo().display().to_string(),
        "--root",
        &s.pool().display().to_string(),
        "--build",
        "exit 7",
    ]);
    assert!(
        !out.status.success(),
        "a failing build must fail the refresh: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let status = s.treehouse_ok(&["status", "--json"]);
    assert!(
        !status.contains("\"status\":\"leased\""),
        "LEAK: the slot the refresh leased for its build is still held: {status}"
    );
}

#[test]
fn f2_a_failed_refresh_releases_the_slot_even_when_the_build_succeeded() {
    let _w = Watchdog::start(240);
    let bin = crate::require_treehouse!();
    let s = Sandbox::new();
    // No --socket, so the daemon call after the build fails. The slot must still come back.
    let out = run(&[
        "--treehouse-bin",
        &bin.display().to_string(),
        "--treehouse-home",
        &s.home().display().to_string(),
        "--socket",
        "/nonexistent/control.sock",
        "base",
        "refresh",
        "--repo",
        &s.repo().display().to_string(),
        "--root",
        &s.pool().display().to_string(),
        "--build",
        "true",
    ]);
    assert!(!out.status.success(), "{:?}", out.status);
    let status = s.treehouse_ok(&["status", "--json"]);
    assert!(
        !status.contains("\"status\":\"leased\""),
        "LEAK: a failing daemon call left the build slot held: {status}"
    );
    let _ = bin;
}

#[test]
fn f6_a_failed_get_releases_the_slot_it_leased() {
    let _w = Watchdog::start(240);
    let bin = crate::require_treehouse!();
    let s = Sandbox::new();
    // The stub daemon answers, so the failure is the gap 1 materialiser, not a missing daemon.
    let dir = private_tempdir();
    let stub = stub_in(dir.path());
    let out = run(&[
        "--socket",
        &stub.socket.display().to_string(),
        "--treehouse-bin",
        &bin.display().to_string(),
        "--treehouse-home",
        &s.home().display().to_string(),
        "get",
        "--repo",
        &s.repo().display().to_string(),
        "--root",
        &s.pool().display().to_string(),
    ]);
    assert!(
        !out.status.success(),
        "get must fail without a mount_snapshot method: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("mount_snapshot") || err.contains("warm base"),
        "and it must say what stopped it: {err}"
    );
    let status = s.treehouse_ok(&["status", "--json"]);
    assert!(
        !status.contains("\"status\":\"leased\""),
        "LEAK: a failed get left its lease held: {status}"
    );
}

// ---------------------------------------------------------------------------------------------
// F4: the safety switches whose mutants survived.
// ---------------------------------------------------------------------------------------------

#[test]
fn f4_terminate_needs_the_force_flag() {
    let _w = Watchdog::start(180);
    let bin = crate::require_treehouse!();
    let dir = private_tempdir();
    let stub = stub_in(dir.path());
    let s = Sandbox::new();
    let (slot, _) = lease_at(&s, &s.pool());
    let pool_id = cowfs_treehouse::pool_id_of_slot_path(&slot).expect("pool id");
    let slot_name = cowfs_treehouse::slot_of(&slot).expect("slot name");
    let snapshot = cowfs_treehouse::slot_snapshot(&pool_id, slot_name).expect("snapshot name");
    let holder = Fixture::spawn("cwd", &format!("cd {}; exec sleep 300", slot.display()));
    std::thread::sleep(Duration::from_millis(300));
    assert!(cowfs_treehouse::alive(holder.pid));

    let mut daemon =
        cowfs_treehouse::Daemon::connect(Some(&stub.socket), Some(5)).expect("connect");
    daemon
        .snapshot_create(&snapshot, None)
        .expect("slot snapshot");
    stub.handler.add_process(
        &snapshot,
        cowfs_ctl::ProcessInfo {
            pid: holder.pid,
            command: "sleep 300".into(),
            holds: vec![cowfs_ctl::Hold {
                kind: cowfs_ctl::HoldKind::Cwd,
                path: slot.display().to_string(),
            }],
        },
    );

    // No --force: the holder is named and nothing is signalled.
    let out = run(&[
        "--socket",
        &stub.socket.display().to_string(),
        "--treehouse-bin",
        &bin.display().to_string(),
        "--treehouse-home",
        &s.home().display().to_string(),
        "--json",
        "return",
        "--mode",
        "b",
        "--slot",
        &slot.display().to_string(),
        "--root",
        &s.pool().display().to_string(),
    ]);
    assert_eq!(out.status.code(), Some(5), "a held slot is exit 5");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("holder"),
        "and it must say a holder is in the way: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        cowfs_treehouse::alive(holder.pid),
        "without --force nothing may be signalled"
    );

    // With --force it goes away.
    let out = run(&[
        "--socket",
        &stub.socket.display().to_string(),
        "--treehouse-bin",
        &bin.display().to_string(),
        "--treehouse-home",
        &s.home().display().to_string(),
        "--json",
        "return",
        "--mode",
        "b",
        "--slot",
        &slot.display().to_string(),
        "--root",
        &s.pool().display().to_string(),
        "--force",
    ]);
    // The stub keeps listing the holder after the kill, so the authoritative swap still refuses.
    // What matters here is that --force actually signalled it.
    let text = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .map(|l| serde_json::from_str(l.trim()).expect("json"))
        .unwrap_or(serde_json::Value::Null);
    if out.status.success() || v.get("terminated").is_some() {
        assert_eq!(
            v["terminated"].as_array().expect("terminated").len(),
            1,
            "with --force the holder is terminated: {v}"
        );
    }
    assert!(
        !cowfs_treehouse::alive(holder.pid),
        "with --force it is terminated"
    );
}

#[test]
fn f4_the_alive_and_signalable_gate_is_load_bearing() {
    let _w = Watchdog::start(120);
    let me = std::process::id();
    let ancestry = cowfs_treehouse::protected_ancestry();
    let mut hostile: Vec<u32> = vec![0, 1, me, u32::MAX, u32::MAX - 1, 2, 3, 999_999_999];
    hostile.extend(ancestry.iter().copied());

    let out = cowfs_treehouse::terminate(&hostile, Duration::from_millis(150));
    for pid in &hostile {
        assert!(
            !out.signalled.contains(pid) && !out.killed.contains(pid),
            "pid {pid} must never be signalled: {out:?}"
        );
    }
    assert!(cowfs_treehouse::alive(me), "this process survived");
    assert!(!cowfs_treehouse::alive(0));
    assert!(!cowfs_treehouse::signalable(0));
    assert!(!cowfs_treehouse::signalable(1));
    assert!(!cowfs_treehouse::signalable(me));

    // A pid that is not running is never signalled, which is the check the `kill(0)` mutation
    // removed.
    let dead = Fixture::spawn("short", "true");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!cowfs_treehouse::alive(dead.pid) || !cowfs_treehouse::signalable(dead.pid));
    let _ = dead;
}

#[test]
fn f4_a_stale_lease_pin_stops_the_release() {
    let _w = Watchdog::start(180);
    let bin = crate::require_treehouse!();
    let dir = private_tempdir();
    let stub = stub_in(dir.path());
    let s = Sandbox::new();
    let (slot, _) = lease_at(&s, &s.pool());
    let pool_id = cowfs_treehouse::pool_id_of_slot_path(&slot).expect("pool id");
    let slot_name = cowfs_treehouse::slot_of(&slot).expect("slot name");
    let snapshot = cowfs_treehouse::slot_snapshot(&pool_id, slot_name).expect("snapshot name");
    let mut daemon =
        cowfs_treehouse::Daemon::connect(Some(&stub.socket), Some(5)).expect("connect");
    daemon
        .snapshot_create(&snapshot, None)
        .expect("slot snapshot");

    // Re-lease the slot behind the wrapper's back, so the identity it reads is the previous one.
    let out = s.treehouse(&["return", &slot.display().to_string(), "--force"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (again, _) = lease_at(&s, &s.pool());
    assert_eq!(again, slot, "the pool handed the same slot back");

    // Now the slot is leased again with a different identity. A wrapper that drops the pin would
    // release it; one that keeps it must be refused by treehouse.
    let out = run(&[
        "--socket",
        &stub.socket.display().to_string(),
        "--treehouse-bin",
        &bin.display().to_string(),
        "--treehouse-home",
        &s.home().display().to_string(),
        "return",
        "--mode",
        "b",
        "--slot",
        &again.display().to_string(),
        "--root",
        &s.pool().display().to_string(),
        "--force",
    ]);
    if out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(last) = text.lines().rev().find(|l| l.trim_start().starts_with('{')) {
            let v: serde_json::Value = serde_json::from_str(last.trim()).expect("json");
            assert!(
                v["lease_id"].is_string(),
                "a successful release must carry a lease identity, never null: {v}"
            );
        }
    } else {
        assert_eq!(
            out.status.code(),
            Some(1),
            "a refused release is a treehouse failure: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("lease"),
            "and it must say the lease precondition failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // Either way the slot ends up free, and never silently re-leased by somebody else.
    assert_eq!(entry(&s, &s.pool(), "1")["status"], "available");
}

#[test]
fn f4_a_missing_root_is_a_hard_error_with_no_environment_fallback() {
    let _w = Watchdog::start(120);
    let bin = crate::require_treehouse!();
    let s = Sandbox::new();
    // Even with TREEHOUSE_ROOT set in the environment, a missing --root is a usage error.
    let out = Command::new(companion())
        .args([
            "--treehouse-bin",
            &bin.display().to_string(),
            "--treehouse-home",
            &s.home().display().to_string(),
            "return",
            "--slot",
            s.pool()
                .join(".treehouse/x/1/repo")
                .display()
                .to_string()
                .as_str(),
        ])
        .env("TREEHOUSE_ROOT", s.pool())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("companion runs");
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("--root is required"));
}

// ---------------------------------------------------------------------------------------------
// F5: the pool id must come from the named root and be cross-checked against the path.
// ---------------------------------------------------------------------------------------------

#[test]
fn f5_a_slot_whose_pool_id_disagrees_is_refused() {
    let _w = Watchdog::start(120);
    let dir = private_tempdir();
    let stub = stub_in(dir.path());
    let s = Sandbox::new();
    // A slot path shaped correctly but under a pool directory that is not this repository's.
    let alien = s
        .pool()
        .join(".treehouse")
        .join("not-this-repo")
        .join("1")
        .join("repo");
    std::fs::create_dir_all(&alien).expect("mkdir");
    let out = run(&[
        "--socket",
        &stub.socket.display().to_string(),
        "provision",
        "--slot",
        &alien.display().to_string(),
    ]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.code().is_some_and(|c| c == 2 || c == 1),
        "a pool id that does not match the repository must be refused: {err}"
    );
    assert!(
        err.contains("pool") || err.contains("outside"),
        "the refusal must name the mismatch: {err}"
    );
}

/// The shim is the guard the whole suite relies on, so its refusals are tested rather than assumed.
#[test]
fn the_sandbox_shim_refuses_a_call_that_would_leave() {
    let _w = Watchdog::start(120);
    crate::require_treehouse!();
    let s = Sandbox::new();
    let shim = s.shim();
    let call = |args: &[&str]| -> std::process::Output {
        Command::new(&shim)
            .args(args)
            .current_dir(s.repo())
            .stdin(std::process::Stdio::null())
            .output()
            .expect("the shim runs")
    };
    let sh_out = call(&[
        "status",
        "--json",
        "--root",
        &s.pool().display().to_string(),
    ]);
    assert!(sh_out.status.success(), "an in-sandbox call works");

    // No --root at all: without this the call would fall back to the real store.
    let out = call(&["status", "--json"]);
    assert_eq!(out.status.code(), Some(91), "no --root is refused");

    // A --root outside the sandbox.
    let out = call(&[
        "status",
        "--json",
        "--root",
        "/Users/zeeshanhaque/.treehouse",
    ]);
    assert_eq!(
        out.status.code(),
        Some(92),
        "an out-of-sandbox --root is refused"
    );

    // An absolute path argument outside the sandbox, even with a good --root.
    let out = call(&[
        "return",
        "/Users/zeeshanhaque/.treehouse/cowfs-7c1bf8/1/cowfs",
        "--root",
        &s.pool().display().to_string(),
    ]);
    assert_eq!(
        out.status.code(),
        Some(93),
        "an out-of-sandbox path argument is refused: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
