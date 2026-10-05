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

#[path = "guard/reader.rs"]
mod guard;

use guard::{cleanup, identity_matches, read_mount_table, signal_if_ours, MountState};
use std::ffi::OsStr;
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
    /// Every child this fixture spawned, with the identity it had at spawn time. A signal is only
    /// ever sent after that identity is re-read and still matches.
    identities: Vec<guard::ChildIdentity>,
}

impl Gate {
    /// Mints a fresh, unique attempt and only ever creates inside it.
    ///
    /// There is no cleanup here and that is the point. The previous version derived `dir`, `store`
    /// and `mount` from a fixed name and recursively deleted any of them that already existed, which
    /// meant a cancelled macOS CI job left a mount at exactly the path the next run walked, and it
    /// walked it before the daemon, and therefore before any mount-state check, had a chance to run.
    /// A fresh root means the question never arises, and `preflight` refuses a collision instead of
    /// resolving it.
    fn new(case: &str) -> Gate {
        let tag = format!("gate-{case}");
        let paths = guard::attempt(&gate_dir(), &tag);
        guard::preflight(&paths)
            .unwrap_or_else(|why| panic!("refusing to start the fixture: {why}"));
        fs::create_dir_all(paths.dir.join("pool")).expect("the export root");
        fs::create_dir_all(&paths.mount).expect("the mount point");
        Gate {
            dir: paths.dir,
            socket: paths.socket,
            store: paths.store,
            mount: paths.mount,
            child: None,
            identities: Vec::new(),
        }
    }

    fn start(&mut self) {
        assert!(self.child.is_none(), "already running");
        let log = fs::File::create(self.dir.join("daemon.log")).expect("the daemon log");
        let err = log.try_clone().expect("a second log handle");
        let bin = test_bin().join("cowfs-daemon");
        let child = Command::new(&bin)
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
        // Identity captured here, before any mount wait and before anything can fail, so a panic
        // below still leaves `Drop` a verified child to stop and a mount it knows how to unmount.
        self.identities.push(guard::identity_for(
            child.id(),
            &bin,
            &[OsStr::new("--store"), self.store.as_os_str()],
            &self.store,
            &self.socket,
        ));
        // Recorded before anything can fail. Asserting first leaked a live daemon and a live mount,
        // and `Drop` then walked the mount.
        self.child = Some(child);
        let deadline = Instant::now() + SETTLE;
        while Instant::now() < deadline {
            if self.cli(&["status"]).is_ok() {
                // The local tri-state reader, not `cowfs_daemon::mounts::is_mounted`. That helper
                // returns false both when `/sbin/mount` cannot be run and when its prefix match on
                // unescaped output misses a path containing a space, and it belongs to another
                // owner, so this fixture asserts readiness with the same reader it tears down with.
                assert_eq!(
                    self.mount_state(),
                    MountState::Mounted,
                    "the daemon answered but the mount table does not name {}",
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
        let identity = self
            .identities
            .iter()
            .find(|i| i.pid == pid)
            .cloned()
            .unwrap_or_else(|| panic!("pid {pid} was never recorded as ours"));
        assert!(
            identity_matches(&identity),
            "refusing to SIGKILL {pid}: its identity no longer matches the daemon this test spawned"
        );
        assert_eq!(
            signal_if_ours(&identity, "KILL"),
            guard::SignalOutcome::Signalled,
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
        let pid = child.id();
        // TERM only if the identity still matches; otherwise the handle below still stops a child
        // this process owns, which needs no identity and cannot hit a stranger.
        if let Some(id) = self.identities.iter().find(|i| i.pid == pid).cloned() {
            let _ = signal_if_ours(&id, "TERM");
        }
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
    /// One absolute deadline for the whole teardown, computed once. A slow step cannot buy a later
    /// step a fresh budget, which is how a fixture ends up taking minutes to fail.
    fn drop(&mut self) {
        let deadline = Instant::now() + guard::TEARDOWN_BUDGET;
        self.stop();

        // Anything this fixture spawned that is still running. Each signal is preceded by a fresh
        // identity read, and a mismatch is a preserve, not a signal. Children already reaped through
        // their own handle have a free pid and are not signalled at all.
        for id in &self.identities {
            if !identity_matches(id) {
                continue;
            }
            match signal_if_ours(id, "KILL") {
                guard::SignalOutcome::Signalled => {}
                guard::SignalOutcome::Refused(why) => {
                    eprintln!("PRESERVE: pid {} not signalled: {why}", id.pid);
                }
            }
        }

        // Unmount, bounded, and only when the table actually names this path. macOS `umount` has no
        // `-z`, so a successful exit must be confirmed against the table before anything is deleted.
        let mut state = self.mount_state();
        if state == MountState::Mounted {
            let left = guard::CHILD_BUDGET.min(deadline.saturating_duration_since(Instant::now()));
            if !left.is_zero() {
                match guard::spawn_bounded(
                    Path::new("/sbin/umount"),
                    &[OsStr::new("-f"), self.mount.as_os_str()],
                    &self.store,
                    &self.socket,
                    left,
                ) {
                    Ok(mut um) => {
                        if !um.finished {
                            eprintln!(
                                "PRESERVE: umount of {} did not finish inside {left:?}, stopping it \
                                 through its own handle",
                                self.mount.display()
                            );
                            um.stop_owned();
                        }
                    }
                    Err(e) => eprintln!("umount of {} could not start: {e}", self.mount.display()),
                }
            }
            // Whatever happened above, the table decides whether this path is safe to walk.
            let until = deadline.min(Instant::now() + Duration::from_secs(15));
            while Instant::now() < until {
                state = self.mount_state();
                if state != MountState::Mounted {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }

        // Fail closed, through the one seam that may authorise a recursive delete. `Mounted` and
        // `Unknown` both preserve, and so does anything this fixture did not create.
        let owned = true;
        let verdict = cleanup(state, owned, "fixture", || {
            for suffix in ["", ".lock"] {
                let _ = fs::remove_file(format!("{}{suffix}", self.socket.display()));
            }
            cowfs_vfs_path::force_remove_dir_all(&self.dir);
            cowfs_vfs_path::force_remove_dir_all(&self.store);
            cowfs_vfs_path::force_remove_dir_all(&self.mount);
            Ok(())
        });
        if let guard::Cleanup::Preserved(why) = verdict {
            eprintln!(
                "PRESERVE: {} is {state:?} in the mount table; {why}. The store and fixture are \
                 left in place rather than walking a filesystem that may still be mounted",
                self.mount.display()
            );
        }
    }
}

impl Gate {
    /// Classifies this fixture's own mount point. Never used to decide anything on its own: the
    /// answer decides whether `Drop` may delete, and only `Absent` allows that.
    fn mount_state(&self) -> MountState {
        let left = guard::CHILD_BUDGET;
        let (state, reader) = read_mount_table(&self.mount, &self.store, &self.socket, left);
        if let Some(mut r) = reader {
            // The reader was spawned by this fixture and is finished or bounded; stopping it through
            // its own handle cannot touch anything else.
            r.stop_owned();
        }
        state
    }
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
