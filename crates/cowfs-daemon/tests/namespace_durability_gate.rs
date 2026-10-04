//! The CI gate for issue #90: a rename the caller synced survives a killed daemon.
//!
//! `namespace_durability.rs` holds the wide matrix and is `#[ignore]`d, because each rep starts a
//! daemon, mounts, kills and mounts again and that does not belong in every `cargo test`. This file
//! is the one that must not rot: a single rep of the two variants the issue measured as lost, not
//! ignored, macOS only, so `cargo test --workspace` runs it on the macOS runner.
//!
//! It is a real deliverable, not a barrier unit test: a real `cowfs-daemon` binary, the real CLI,
//! a real NFSv3 loopback mount, a private store, `SIGKILL` of the pid this test started, and the
//! same store reopened by a fresh daemon on a fresh mount.
//!
//! There is no skip path. If the mount cannot be made the test fails and says so, because a gate
//! that reports success without having mounted is the failure this issue was about. The one
//! exception is a host with no NFS client at all, which is reported as its own failure naming
//! `mount_nfs`, not as a pass.
//!
//! ```text
//! cargo test -p cowfs-daemon --test namespace_durability_gate
//! ```

#![cfg(target_os = "macos")]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// One rep is bounded, so the waits are short. Nothing here should take a minute.
const SETTLE: Duration = Duration::from_secs(45);

const SNAP: &str = "snap";
const OLD: &str = "old.txt";
const NEW: &str = "new.txt";
const SIBLING: &str = "sibling.txt";
const BODY: &[u8] = b"cowfs namespace durability gate payload 0123456789 abcdefghijklmnop\n";

/// What the caller does after `rename` returns. The two the issue measured as lost.
#[derive(Clone, Copy, Debug)]
enum After {
    /// `fsync` of the parent directory: the POSIX habit.
    ParentDir,
    /// `fsync` of a read-only descriptor of the renamed file.
    ReadOnlyFd,
}

impl After {
    const CASES: [After; 2] = [After::ParentDir, After::ReadOnlyFd];

    fn name(self) -> &'static str {
        match self {
            After::ParentDir => "fsync-parent-dir",
            After::ReadOnlyFd => "fsync-read-only-fd",
        }
    }

    fn apply(self, dir: &Path) {
        match self {
            After::ParentDir => fs::File::open(dir)
                .and_then(|d| d.sync_all())
                .expect("fsync the parent directory"),
            After::ReadOnlyFd => fs::File::open(dir.join(NEW))
                .and_then(|f| f.sync_all())
                .expect("fsync a read-only descriptor of the renamed file"),
        }
    }
}

/// Artifacts under `bench/out/durability90-gate/`, created and removed per run.
fn gate_dir() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bench/out/durability90-gate");
    fs::create_dir_all(&root).expect("the gate artifact directory");
    // `is_mounted` compares the mount table verbatim, and the table holds the canonical path, so an
    // unresolved `../..` would make a mounted filesystem look unmounted.
    fs::canonicalize(&root).unwrap_or(root)
}

fn test_bin() -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary has a path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path
}

fn running(child: &mut Child) -> bool {
    !matches!(child.try_wait(), Ok(Some(_)) | Err(_))
}

/// One private store, one mount, one socket. Every pid here is started by this test and recorded,
/// so shutdown can be exact and never a process group.
struct Gate {
    dir: PathBuf,
    socket: PathBuf,
    store: PathBuf,
    mount: PathBuf,
    child: Option<Child>,
    pids: Vec<u32>,
}

impl Gate {
    fn new(case: &str) -> Gate {
        let tag = format!("gate-{case}");
        let root = gate_dir();
        let dir = root.join(&tag);
        let store = root.join(format!("store-{tag}"));
        let mount = root.join(format!("mnt-{tag}"));
        for p in [&dir, &store, &mount] {
            if p.exists() {
                cowfs_vfs_path::force_remove_dir_all(p);
            }
        }
        fs::create_dir_all(dir.join("pool")).expect("the export root");
        fs::create_dir_all(&mount).expect("the mount point");
        // `sun_path` is 104 bytes, so the socket goes on the short TMPDIR, not under this long path.
        let socket =
            std::env::temp_dir().join(format!("d90gate-{}-{tag}.sock", std::process::id()));
        Gate {
            dir,
            socket,
            store,
            mount,
            child: None,
            pids: Vec::new(),
        }
    }

    fn start(&mut self) {
        assert!(self.child.is_none(), "already running");
        let log = fs::File::create(self.dir.join("daemon.log")).expect("the daemon log");
        let err = log.try_clone().expect("a second log handle");
        let child = Command::new(test_bin().join("cowfs-daemon"))
            .arg("--store")
            .arg(self.store.display().to_string())
            .arg("--mount")
            .arg(self.mount.display().to_string())
            .arg("--socket")
            .arg(self.socket.display().to_string())
            .arg("--export-root")
            .arg(self.dir.join("pool").display().to_string())
            .arg("--backend")
            .arg("core")
            .stdin(Stdio::null())
            .stdout(Stdio::from(err))
            .stderr(Stdio::from(log))
            .spawn()
            .expect("the daemon binary runs");
        self.pids.push(child.id());
        // Recorded before anything can fail, so a panic below still leaves `Drop` a pid to stop and
        // a mounted filesystem it knows how to unmount. Asserting first leaked a live daemon and a
        // live mount, and `Drop` then walked the mount.
        self.child = Some(child);
        let deadline = Instant::now() + SETTLE;
        while Instant::now() < deadline {
            if self.cli(&["status"]).is_ok() {
                assert!(
                    cowfs_daemon::mounts::is_mounted(&self.mount),
                    "the daemon answered but nothing is mounted at {}",
                    self.mount.display()
                );
                return;
            }
            assert!(
                running(self.child.as_mut().expect("recorded")),
                "the daemon exited before it served"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        self.stop();
        panic!("the daemon did not start serving within {SETTLE:?}");
    }

    fn cli(&self, args: &[&str]) -> Result<String, String> {
        let out = Command::new(test_bin().join("cowfs"))
            .arg("--socket")
            .arg(&self.socket)
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "cowfs {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn snap(&self) -> PathBuf {
        self.mount.join(SNAP)
    }

    /// `SIGKILL` of the pid this test started, then wait for it to be gone. `/bin/kill` names the
    /// signal and no process group is ever touched.
    fn sigkill(&mut self) -> u32 {
        let mut child = self.child.take().expect("a running daemon");
        let pid = child.id();
        debug_assert!(
            self.pids.contains(&pid),
            "every signalled pid was started here"
        );
        assert!(
            Command::new("/bin/kill")
                .arg("-9")
                .arg(pid.to_string())
                .status()
                .is_ok_and(|s| s.success()),
            "could not SIGKILL {pid}"
        );
        let deadline = Instant::now() + SETTLE;
        while Instant::now() < deadline {
            if !running(&mut child) {
                let _ = child.wait();
                return pid;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("the daemon ignored SIGKILL");
    }

    fn stop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = Command::new("/bin/kill")
            .arg("-TERM")
            .arg(child.id().to_string())
            .status();
        let deadline = Instant::now() + SETTLE;
        while Instant::now() < deadline {
            if !running(&mut child) {
                let _ = child.wait();
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        self.stop();
        // Only ever signal a pid this test started, by number, never a group.
        for pid in &self.pids {
            if running_pid(*pid) {
                let _ = Command::new("/bin/kill")
                    .arg("-KILL")
                    .arg(pid.to_string())
                    .status();
            }
        }
        // macOS `umount` has no `-z`, so wait for the table to agree and never walk a live mount.
        if cowfs_daemon::mounts::is_mounted(&self.mount) {
            let _ = Command::new("/sbin/umount")
                .arg("-f")
                .arg(&self.mount)
                .status();
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline && cowfs_daemon::mounts::is_mounted(&self.mount) {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if cowfs_daemon::mounts::is_mounted(&self.mount) {
            eprintln!(
                "LEAK: {} is still in the mount table, leaving the tree alone",
                self.mount.display()
            );
            return;
        }
        for suffix in ["", ".lock"] {
            let _ = fs::remove_file(format!("{}{suffix}", self.socket.display()));
        }
        cowfs_vfs_path::force_remove_dir_all(&self.dir);
        cowfs_vfs_path::force_remove_dir_all(&self.store);
        cowfs_vfs_path::force_remove_dir_all(&self.mount);
    }
}

/// True while a pid this test recorded is still there.
///
/// Output is discarded: `kill -0` on a pid this process already reaped prints "No such process"
/// to stderr, which is a normal answer here rather than a problem worth putting in a test log.
fn running_pid(pid: u32) -> bool {
    Command::new("/bin/kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// One rep: create the file, rename it, sync the way the case says, `SIGKILL` 2 to 6 ms later, and
/// read the same store back through a fresh daemon and a fresh mount.
fn rep(case: After) {
    let mut g = Gate::new(case.name());
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(g.dir.join("pool"), fs::Permissions::from_mode(0o700))
        .expect("a private export root");
    g.start();
    g.cli(&["snapshot", "create", SNAP])
        .expect("snapshot create");
    let dir = g.snap();

    let old = dir.join(OLD);
    let mut f = fs::File::create(&old).expect("create the file to rename");
    f.write_all(BODY).expect("write the body");
    // Durable before the rename, so the only uncommitted thing at the kill is the rename itself.
    f.sync_all().expect("fsync the file");
    drop(f);

    fs::rename(&old, dir.join(NEW)).expect("rename");
    case.apply(&dir);
    let before = fs::read(dir.join(NEW)).expect("read the renamed file back");
    assert_eq!(before, BODY, "the rename did not read back before the kill");

    std::thread::sleep(Duration::from_millis(3));
    let pid = g.sigkill();
    g.start();

    let dir = g.snap();
    let after = fs::read(dir.join(NEW))
        .unwrap_or_else(|e| panic!("{}: the new name must survive the kill: {e}", case.name()));
    assert_eq!(
        after,
        before,
        "{}: the new name came back with other bytes",
        case.name()
    );
    assert!(
        !dir.join(OLD).exists(),
        "{}: the old name came back too",
        case.name()
    );
    // The control the issue measured as working, kept so a silently broken write path cannot make
    // this gate pass for the wrong reason.
    let sibling = dir.join(SIBLING);
    fs::write(&sibling, b"control\n").expect("write the control sibling");
    fs::File::open(&sibling)
        .and_then(|f| f.sync_all())
        .expect("fsync the control sibling");

    let fsck = g.cli(&["fsck"]).expect("fsck after the kill");
    assert!(
        fsck.contains("ok: ") && fsck.contains("snapshots checked"),
        "{}: fsck: {fsck}",
        case.name()
    );
    eprintln!(
        "durability90 gate {}: pid {pid} SIGKILLed, new name survived",
        case.name()
    );
}

/// A synced namespace survives a killed daemon, on a real mount, in CI.
///
/// Not ignored and not skipped: this is the gate. The host must be able to mount, and if it cannot
/// this fails naming what refused, because a green run that never mounted would be a lie.
#[test]
fn a_synced_namespace_survives_a_killed_daemon_in_ci() {
    assert!(
        cowfs_daemon::mounts::available(),
        "this gate needs an NFS client: /sbin/mount_nfs is missing, so the rename was never \
         exercised and this run proves nothing"
    );
    for case in After::CASES {
        rep(case);
    }
}
