//! Issue #20: an open descriptor or a flock in a slot, held by a process that has chdir'd out of
//! it, is invisible to `treehouse return`.
//!
//! `treehouse` finds lingering processes by working directory alone (`internal/process/detect.go`,
//! `p.Cwd()`), so it reports a slot as clean while a process still has a file inside it open. Off a
//! network mount that costs an orphan process; on one it costs the slot, because the reset unlinks
//! the held file, the macOS NFS client silly-renames it to `.nfs*`, and the slot reads dirty while
//! the return still exits 0.
//!
//! What is proven here is the companion's half: a mode (a) slot on a cowfs mount is scanned through
//! the daemon's `ps` before treehouse is asked to release it, a hold treehouse cannot see stops the
//! return with nothing changed, and a daemon that cannot answer the scan stops it too. The kernel
//! half, the real `.nfs*` silly-rename on a live NFS loopback, is measured in
//! `docs/verification/ready-20.md`.
//!
//! Every treehouse invocation goes through the sandbox shim in `common`, so no call can reach the
//! real `~/.treehouse` pools, which are leased to other agents.

mod common;

use common::{Fixture, Sandbox, Watchdog};
use cowfs_ctl::{
    ControlHandler, CtlError, CtlResult, ErrorCode, Hold, HoldKind, ProcessInfo, StubHandler,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

/// The companion binary, so these tests cover the real argument parsing and exit codes.
fn companion() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cowfs-treehouse"))
}

fn run_companion(args: &[String]) -> std::process::Output {
    Command::new(companion())
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run cowfs-treehouse: {e}"))
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The last JSON object a companion run printed, which is where a `ReturnOutcome` lands.
fn last_json(text: &str) -> serde_json::Value {
    let line = text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON object in the output:\n{text}"));
    serde_json::from_str(line).unwrap_or_else(|e| panic!("invalid JSON {line:?}: {e}"))
}

/// A pool whose slots live inside a daemon's mount, which is what makes a mode (a) slot something
/// cowfs can scan: such a slot is a directory inside the mount, not a snapshot.
fn on_mount_pool() -> (Sandbox, common::Stub) {
    let s = Sandbox::new();
    // `stub_in` puts the daemon's mount at `<sandbox>/mnt`, inside the sandbox, and the pool root is
    // that same directory. Both stay inside the shim's guard.
    let stub = common::stub_in(s.dir.path());
    (s, stub)
}

/// The pool root of an on-mount sandbox, which is also the daemon's mount point.
fn mount_of(s: &Sandbox) -> PathBuf {
    s.dir.path().join("mnt")
}

fn lease(s: &Sandbox, root: &Path) -> PathBuf {
    let out = s.treehouse_at(root, &["get", "--lease", "--json"]);
    assert!(
        out.status.success(),
        "treehouse get --lease failed: {}",
        stderr_of(&out)
    );
    PathBuf::from(last_json(&stdout_of(&out))["path"].as_str().expect("path"))
}

/// The `return` invocation, with the daemon and the shimmed treehouse an operator would use.
fn return_args<'a>(
    s: &'a Sandbox,
    socket: &'a Path,
    root: &'a Path,
    slot: &'a Path,
    extra: &[&'a str],
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "--socket".into(),
        socket.display().to_string(),
        "--treehouse-bin".into(),
        s.shim().display().to_string(),
        "--treehouse-home".into(),
        s.home().display().to_string(),
        "--json".into(),
        "return".into(),
        "--slot".into(),
        slot.display().to_string(),
        "--root".into(),
        root.display().to_string(),
    ];
    args.extend(extra.iter().map(|a| (*a).to_owned()));
    args
}

/// True while treehouse still holds a lease on this slot.
///
/// The lease identity, not the `status` column: a slot with an untracked file in it reads `dirty`
/// even though it is still leased, and what has to be proven here is that nothing was released.
fn is_leased(s: &Sandbox, root: &Path, slot: &Path) -> bool {
    let wanted = canon(slot);
    status(s, root).iter().any(|e| {
        e["lease_id"].as_str().is_some_and(|id| !id.is_empty())
            && e["path"]
                .as_str()
                .is_some_and(|p| canon(Path::new(p)) == wanted)
    })
}

/// treehouse reports the path as the pool was addressed, which on macOS can be the `/var` spelling
/// of a `/private/var` directory, so both sides are compared physically.
fn canon(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The slots `treehouse status` reports, as JSON, so a test can see whether one is still leased.
fn status(s: &Sandbox, root: &Path) -> Vec<serde_json::Value> {
    let out = s.treehouse_at(root, &["status", "--json"]);
    assert!(
        out.status.success(),
        "treehouse status failed: {}",
        stderr_of(&out)
    );
    let text = stdout_of(&out);
    let line = text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('['))
        .unwrap_or_else(|| panic!("no JSON array in treehouse status:\n{text}"));
    let parsed: Vec<serde_json::Value> =
        serde_json::from_str(line).unwrap_or_else(|e| panic!("invalid status JSON {line:?}: {e}"));
    parsed
}

/// A real process that has chdir'd out of the slot and still has a file inside it open.
///
/// This is the shape `treehouse return` cannot see: its working directory is `/`, so its only claim
/// on the slot is the descriptor. A detector that only looked at working directories would call the
/// slot clean. The marker is written after the descriptor is open, so the test never races it.
fn holder_outside_the_slot(slot: &Path, file: &Path, marker: &Path) -> Fixture {
    let fixture = Fixture::spawn(
        "issue20-fd-out",
        &format!(
            "cd {}; exec 9<{}; : >{}; cd /; exec sleep 600",
            slot.display(),
            file.display(),
            marker.display()
        ),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !marker.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the holder never opened {}",
            file.display()
        );
        if !cowfs_treehouse::alive(fixture.pid) {
            panic!(
                "the holder fixture {} died before opening the file",
                fixture.pid
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    fixture
}

/// Registers `pid` with the stub under the mount-relative directory of `slot`, which is the name
/// the companion sends for a mode (a) slot.
fn register_stub_holder(stub: &common::Stub, mount: &Path, slot: &Path, pid: u32, kind: HoldKind) {
    let relative = slot
        .strip_prefix(mount)
        .unwrap_or_else(|_| {
            panic!(
                "{} is not inside the mount {}",
                slot.display(),
                mount.display()
            )
        })
        .display()
        .to_string();
    stub.handler.add_process(
        &relative,
        ProcessInfo {
            pid,
            command: "sleep 600".into(),
            holds: vec![Hold {
                kind,
                path: slot.join("held.txt").display().to_string(),
            }],
        },
    );
}

/// The core of the issue: a hold treehouse cannot see, in a slot on the mount, stops the return
/// before treehouse is asked to release anything, and nothing about the slot changes.
#[test]
fn a_mode_a_slot_on_the_mount_is_scanned_and_an_unseen_holder_refuses_the_return() {
    let _w = Watchdog::start(180);
    crate::require_treehouse!();
    let (s, stub) = on_mount_pool();
    let mount = mount_of(&s);
    let slot = lease(&s, &mount);
    let file = slot.join("held.txt");
    std::fs::write(&file, b"held\n").expect("write the file the holder will open");
    let marker = s.dir.path().join("holder-ready");

    let holder = holder_outside_the_slot(&slot, &file, &marker);
    register_stub_holder(&stub, &mount, &slot, holder.pid, HoldKind::Fd);

    let args = return_args(&s, &stub.socket, &mount, &slot, &["--nfs-timeout", "2"]);
    let out = run_companion(&args);
    assert_eq!(
        out.status.code(),
        Some(5),
        "a held slot is exit 5, stderr: {}",
        stderr_of(&out)
    );
    let err = stderr_of(&out);
    assert!(err.contains("treehouse cannot see"), "{err}");
    assert!(err.contains("held.txt"), "the file is named: {err}");
    assert!(err.contains("--force"), "the way out is named: {err}");
    assert!(
        cowfs_treehouse::alive(holder.pid),
        "without --force nothing may be signalled"
    );
    assert!(file.exists(), "the held file is still there");
    assert!(slot.join(".git").exists(), "the slot was not touched");
    assert!(
        is_leased(&s, &mount, &slot),
        "treehouse was never asked to release it"
    );
}

/// With `--force` the same real holder is signalled, really dies, and only then is the slot handed
/// back. This is why the refusal exists: without it treehouse resets a slot that a live process
/// still has a file open in.
#[test]
fn force_kills_the_real_holder_and_only_then_returns_the_slot() {
    let _w = Watchdog::start(180);
    crate::require_treehouse!();
    let (s, stub) = on_mount_pool();
    let mount = mount_of(&s);
    let slot = lease(&s, &mount);
    let file = slot.join("held.txt");
    std::fs::write(&file, b"held\n").expect("write the file the holder will open");
    let marker = s.dir.path().join("holder-ready");
    let holder = holder_outside_the_slot(&slot, &file, &marker);
    register_stub_holder(&stub, &mount, &slot, holder.pid, HoldKind::Fd);

    let args = return_args(
        &s,
        &stub.socket,
        &mount,
        &slot,
        &["--force", "--nfs-timeout", "2"],
    );
    let out = run_companion(&args);
    assert!(
        out.status.success(),
        "a killed holder lets the return through: {}",
        stderr_of(&out)
    );
    let v = last_json(&stdout_of(&out));
    assert_eq!(
        v["terminated"].as_array().map(Vec::len),
        Some(1),
        "the real process was signalled: {v}"
    );
    assert!(
        v["holder_scan"]
            .as_str()
            .unwrap_or_default()
            .contains("scanned"),
        "the outcome says what was scanned: {v}"
    );
    assert!(
        !cowfs_treehouse::alive(holder.pid),
        "the holder is gone, so its descriptor is released"
    );
    assert!(
        !file.exists(),
        "treehouse's own reset cleaned the untracked file, which is only safe because the holder \
         is gone; while it was alive that same unlink is what leaves the slot dirty"
    );
    assert!(
        !is_leased(&s, &mount, &slot),
        "the slot went back to the pool"
    );
}

/// `--force` does not become a licence to signal something this process may not signal. The holder
/// here is the test process itself, which is an ancestor of the companion it starts, and it holds a
/// real open descriptor in the slot for the whole run.
#[test]
fn force_refuses_a_holder_it_may_not_signal_and_leaves_the_descriptor_open() {
    let _w = Watchdog::start(180);
    crate::require_treehouse!();
    let (s, stub) = on_mount_pool();
    let mount = mount_of(&s);
    let slot = lease(&s, &mount);
    let file = slot.join("held.txt");
    std::fs::write(&file, b"held\n").expect("write the file the holder will open");
    // The descriptor is held by this process for the whole run, and nothing here closes it.
    let open = std::fs::File::open(&file).expect("this process opens the file");
    register_stub_holder(&stub, &mount, &slot, std::process::id(), HoldKind::Fd);

    let args = return_args(
        &s,
        &stub.socket,
        &mount,
        &slot,
        &["--force", "--nfs-timeout", "2"],
    );
    let out = run_companion(&args);
    assert_ne!(
        out.status.success(),
        true,
        "a holder this process may not signal must stop the return: {}",
        stdout_of(&out)
    );
    assert_eq!(out.status.code(), Some(5), "stderr: {}", stderr_of(&out));
    let err = stderr_of(&out);
    assert!(err.contains("may not signal"), "{err}");
    assert!(
        err.contains(&std::process::id().to_string()),
        "and it names the pid it will not touch: {err}"
    );
    // Native control: the descriptor is still open and still reads, so nothing was signalled and
    // nothing unlinked it.
    use std::io::Read;
    let mut text = String::new();
    let mut again = open;
    again
        .read_to_string(&mut text)
        .expect("the descriptor this process still holds is readable");
    assert_eq!(text, "held\n", "{text:?}");
    assert!(file.exists(), "the held file is still there");
    assert!(
        is_leased(&s, &mount, &slot),
        "and the lease was never released"
    );
}

/// A pool that is not on a mount keeps working exactly as before: cowfs has nothing to scan, says
/// so, and hands the slot back. This is the off-mount mode (a) case the refusal must not break.
#[test]
fn a_mode_a_slot_off_the_mount_is_reported_as_unscanned_and_still_returns() {
    let _w = Watchdog::start(180);
    crate::require_treehouse!();
    let s = Sandbox::new();
    // A separate directory, so the pool is not inside this daemon's mount.
    let elsewhere = common::private_tempdir();
    let stub = common::stub_in(elsewhere.path());
    let slot = lease(&s, &s.pool());
    let args = return_args(&s, &stub.socket, &s.pool(), &slot, &["--nfs-timeout", "2"]);
    let out = run_companion(&args);
    assert!(
        out.status.success(),
        "an off-mount pool is unaffected: {}",
        stderr_of(&out)
    );
    let v = last_json(&stdout_of(&out));
    assert!(
        v["holder_scan"]
            .as_str()
            .unwrap_or_default()
            .contains("not scanned"),
        "and it never reads as proof: {v}"
    );
    assert!(
        !is_leased(&s, &s.pool(), &slot),
        "the slot went back to the pool"
    );
}

/// A handler that cannot answer the scan, which is what a machine without a usable `lsof` produces.
#[derive(Debug)]
struct CannotScan(StubHandler);

impl ControlHandler for CannotScan {
    fn status(&self) -> CtlResult<cowfs_ctl::Status> {
        self.0.status()
    }
    fn snapshot_list(&self) -> CtlResult<Vec<cowfs_ctl::SnapshotInfo>> {
        self.0.snapshot_list()
    }
    fn snapshot_create(
        &self,
        params: cowfs_ctl::SnapshotCreate,
    ) -> CtlResult<cowfs_ctl::SnapshotInfo> {
        self.0.snapshot_create(params)
    }
    fn holders(&self, _snapshot: &str) -> CtlResult<Vec<ProcessInfo>> {
        Err(CtlError::new(
            ErrorCode::Unsupported,
            "no runnable /usr/sbin/lsof on this machine, so open files and flocks cannot be listed",
        ))
    }
    fn snapshot_rename(&self, from: &str, to: &str) -> CtlResult<cowfs_ctl::SnapshotInfo> {
        self.0.snapshot_rename(from, to)
    }
    fn mount_info(&self) -> CtlResult<cowfs_ctl::MountInfo> {
        self.0.mount_info()
    }
    fn remove(&self, name: &str, guard: &cowfs_ctl::HolderGuard<'_>) -> CtlResult<()> {
        self.0.remove(name, guard)
    }
    fn swap(
        &self,
        name: &str,
        from: &str,
        guard: &cowfs_ctl::HolderGuard<'_>,
    ) -> CtlResult<cowfs_ctl::SnapshotInfo> {
        self.0.swap(name, from, guard)
    }
}

/// The fail-closed half: an unanswered scan is not a clear slot, so the return stops and names the
/// missing capability. Reporting "no holders" here is exactly how a reset lands under a writer that
/// nobody could name.
#[test]
fn a_daemon_that_cannot_answer_the_holder_scan_stops_the_return() {
    let _w = Watchdog::start(180);
    crate::require_treehouse!();
    let s = Sandbox::new();
    let mount = mount_of(&s);
    let socket = blind_socket(&s, &mount);
    let slot = lease(&s, &mount);

    let args = return_args(&s, &socket, &mount, &slot, &["--nfs-timeout", "2"]);
    let out = run_companion(&args);
    assert!(
        !out.status.success(),
        "a return that could not scan must not claim success: {}",
        stdout_of(&out)
    );
    assert_ne!(out.status.code(), Some(5), "it is not busy, it is blind");
    let err = stderr_of(&out);
    assert!(err.contains("cannot scan holders"), "{err}");
    assert!(
        err.contains("lsof"),
        "the reason names the capability: {err}"
    );
    assert!(slot.join(".git").exists(), "the slot was left in place");
    assert!(is_leased(&s, &mount, &slot), "and it was never released");
}

/// A daemon whose `ps` always answers `unsupported`, on its own socket inside the sandbox.
fn blind_socket(s: &Sandbox, mount: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let run_dir = s.dir.path().join("blind-run");
    std::fs::create_dir_all(&run_dir).expect("mkdir run dir");
    std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let socket = run_dir.join("control.sock");
    let blind = Arc::new(CannotScan(StubHandler::new(
        s.dir.path().join("store").display().to_string(),
        mount.display().to_string(),
    )));
    let server = cowfs_ctl::Server::start(
        &socket,
        Arc::clone(&blind) as Arc<dyn ControlHandler>,
        cowfs_ctl::ServerOptions::default(),
    )
    .expect("the blind daemon starts");
    // Leaked deliberately: shutting it down here would close the socket the assertions above need
    // to read the outcome through, and the test process ends right after them anyway.
    std::mem::forget(server);
    socket
}
