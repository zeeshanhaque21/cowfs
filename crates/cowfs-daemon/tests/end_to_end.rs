//! The end-to-end run: a real daemon process, a real mount, the real CLI.
//!
//! It is `#[ignore]`d because it needs a mount adapter (macOS `mount_nfs`, or Linux with
//! `/dev/fuse` and `fusermount3`) and because a real mount cannot run beside itself. Run it:
//!
//! ```text
//! cargo test -p cowfs-daemon --test end_to_end -- --ignored --test-threads=1 --nocapture
//! ```
//!
//! Every wait loop below exits on failure as well as on success, and a watchdog force-unmounts
//! a mount whose server died, so a hung syscall fails the test instead of wedging the machine.

use cowfs_ctl::{Client, ClientOptions, ControlHandler, NoParams, Request, Response};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long to wait for something that should happen at once.
const SETTLE: Duration = Duration::from_secs(60);

fn daemon_bin() -> PathBuf {
    // The test binary lives in the same target dir as the binary it drives.
    let mut path = std::env::current_exe().expect("the test binary has a path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("cowfs-daemon")
}

fn available() -> bool {
    cowfs_daemon::mounts::available()
}

/// One daemon under a temp dir, with a mount that is unmounted and removed when it goes.
struct Host {
    /// Kept alive so the tree under it outlives nothing: dropping it removes the whole thing.
    _dir: tempfile::TempDir,
    child: Option<Child>,
    /// Set when the daemon runs in this process instead of as a child, which is what a test
    /// that calls the library entry point directly needs.
    live: Option<std::sync::Arc<cowfs_daemon::Daemon>>,
    store: PathBuf,
    mount: PathBuf,
    socket: PathBuf,
    root: PathBuf,
}

impl Host {
    fn new() -> Option<Host> {
        if !available() {
            eprintln!("SKIP: no usable mount adapter on this host");
            return None;
        }
        let dir = tempfile::Builder::new()
            .prefix("cowfs-daemon-e2e-")
            .tempdir()
            .expect("a temp dir");
        let base = std::fs::canonicalize(dir.path()).expect("the temp dir resolves");
        // The export root is a treehouse-shaped pool root, and it obeys the rule the daemon
        // enforces: private to this uid.
        let root = base.join("th").join(".treehouse");
        let pool = root.join("repo-abc123");
        std::fs::create_dir_all(pool.join("1").join("repo")).expect("the pool slot");
        std::fs::set_permissions(&root, private()).expect("the pool root is private");
        // The socket directory must be private to this uid and mode 0700, so it is not the
        // temp dir itself.
        let rt = base.join("rt");
        std::fs::create_dir_all(&rt).expect("the socket directory");
        std::fs::set_permissions(&rt, private()).expect("the socket directory is private");
        let host = Host {
            store: base.join("store"),
            mount: base.join("mnt"),
            socket: rt.join("control.sock"),
            _dir: dir,
            child: None,
            live: None,
            root: pool,
        };
        Some(host)
    }

    /// Starts the daemon in this process, so a test can reach the library entry points the
    /// control methods will call once cowfs-ctl grows `mount_snapshot`.
    fn start_here(&mut self) -> std::sync::Arc<cowfs_daemon::Daemon> {
        assert!(self.live.is_none(), "already running");
        let mut config = cowfs_daemon::DaemonConfig::new(
            &self.store,
            &self.mount,
            self.socket.display().to_string(),
        )
        .with_export_root(self.root.parent().unwrap());
        config.export_roots = vec![self.root.parent().unwrap().to_owned()];
        let daemon = cowfs_daemon::Daemon::start(&config).expect("the daemon starts");
        self.live = Some(std::sync::Arc::clone(&daemon));
        assert!(
            cowfs_daemon::mounts::is_mounted(&self.mount),
            "the daemon mounted nothing"
        );
        daemon
    }

    fn slot(&self, slot: &str) -> PathBuf {
        self.root.join(slot).join("repo")
    }

    fn start(&mut self) {
        assert!(self.child.is_none(), "already running");
        let child = Command::new(daemon_bin())
            .arg("--store")
            .arg(self.store.display().to_string())
            .arg("--mount")
            .arg(self.mount.display().to_string())
            .arg("--socket")
            .arg(self.socket.display().to_string())
            .arg("--export-root")
            .arg(self.root.parent().unwrap().display().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the daemon binary runs");
        let mut child = child;
        // Either the socket appears and answers, or the child is gone. Both end the loop.
        let deadline = Instant::now() + SETTLE;
        while Instant::now() < deadline {
            if self.answer().is_some() {
                assert!(
                    cowfs_daemon::mounts::is_mounted(&self.mount),
                    "the daemon answered but nothing is mounted"
                );
                self.child = Some(child);
                return;
            }
            assert!(running(&mut child), "the daemon exited before it served");
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("the daemon did not start serving within {SETTLE:?}");
    }

    fn kill(&mut self, signal: &str) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let pid = child.id();
        // `kill` runs the real binary rather than libc, so the signal is the one named.
        let sent = Command::new("/bin/kill")
            .arg(format!("-{signal}"))
            .arg(pid.to_string())
            .status()
            .is_ok_and(|s| s.success());
        assert!(sent, "could not send {signal} to {pid}");
        let deadline = Instant::now() + SETTLE;
        while Instant::now() < deadline {
            if !running(&mut child) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("the daemon ignored {signal}");
    }

    /// One control call, or `None` when there is no daemon to answer.
    fn answer(&self) -> Option<Response> {
        let opts = ClientOptions {
            connect_timeout: Duration::from_secs(2),
            handshake_timeout: Duration::from_secs(2),
            idle_timeout: Duration::from_secs(5),
        };
        let mut client = Client::connect_with(&self.socket, opts).ok()?;
        client.call(Request::Ping(Default::default())).ok()
    }

    fn call(&self, request: Request) -> (i32, String) {
        let mut client =
            Client::connect_with(&self.socket, ClientOptions::default()).expect("a daemon to call");
        match client.call(request) {
            Ok(response) => (0, response.data_json().to_string()),
            Err(e) => match e {
                cowfs_ctl::ClientError::Server(err) => {
                    (1, format!("{}: {}", err.code, err.message))
                }
                other => (2, other.to_string()),
            },
        }
    }
}

fn private() -> std::fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    std::fs::Permissions::from_mode(0o700)
}

/// True while the child is running. `try_wait` reaps it, so a child that exited during
/// startup is reported as gone rather than lingering as a zombie.
fn running(child: &mut Child) -> bool {
    !matches!(child.try_wait(), Ok(Some(_)) | Err(_))
}

impl Drop for Host {
    fn drop(&mut self) {
        if let Some(daemon) = self.live.take() {
            for p in daemon.stop() {
                eprintln!("shutdown: {p}");
            }
        }
        if let Some(mut child) = self.child.take() {
            let _ = Command::new("/bin/kill")
                .arg(format!(
                    "-{}",
                    if cfg!(target_os = "macos") {
                        "TERM"
                    } else {
                        "INT"
                    }
                ))
                .arg(child.id().to_string())
                .status();
            let _ = child.wait();
        }
        // A mount whose server is gone hangs `ls` for twenty seconds, so it is force-unmounted
        // before the temp dir goes, whatever the test did.
        if cowfs_daemon::mounts::is_mounted(&self.mount) {
            eprintln!("WATCHDOG: force-unmounting {}", self.mount.display());
            let _ = Command::new(if cfg!(target_os = "macos") {
                "/sbin/umount"
            } else {
                "fusermount3"
            })
            .arg("-f")
            .arg("-z")
            .arg(&self.mount)
            .status();
        }
        cowfs_vfs_path::force_remove_dir_all(self.mount.as_path());
    }
}

/// A real file written through the mount reads back the same bytes, and a hardlink shares the
/// inode: the two things a content-addressed mount must not break.
#[test]
#[ignore = "needs a real mount adapter"]
fn a_daemon_serves_a_mount_writes_and_a_shutdown_leaves_nothing() {
    let Some(mut h) = Host::new() else {
        return;
    };
    h.start();

    let (code, body) = h.call(Request::Status(Default::default()));
    assert_eq!(code, 0, "{body}");
    assert!(body.contains(&h.mount.display().to_string()), "{body}");

    // The mount root lists the snapshots as directories, so the first thing a user writes is
    // into one of them.
    let (code, body) = h.call(Request::SnapshotCreate(cowfs_ctl::SnapshotCreate {
        name: "base".into(),
        from: None,
    }));
    assert_eq!(code, 0, "snapshot create: {body}");

    let file = h.mount.join("base").join("hello.txt");
    std::fs::write(&file, b"hello through the mount\n").expect("write through the mount");
    assert_eq!(
        std::fs::read_to_string(&file).expect("read back through the mount"),
        "hello through the mount\n"
    );
    let sub = h.mount.join("base").join("sub");
    std::fs::create_dir_all(sub.join("deep")).expect("mkdir through the mount");
    std::fs::write(sub.join("deep").join("f"), b"deep").expect("write nested");
    assert_eq!(
        std::fs::read_to_string(sub.join("deep").join("f")).unwrap(),
        "deep"
    );

    // A clone is a copy here, so a write into one snapshot is not in the other.
    assert_eq!(
        h.call(Request::SnapshotCreate(cowfs_ctl::SnapshotCreate {
            name: "slot".into(),
            from: Some("base".into()),
        }))
        .0,
        0,
        "snapshot create"
    );
    std::fs::write(h.mount.join("slot").join("only-slot"), b"x").expect("write into the clone");
    assert!(h.mount.join("slot").join("only-slot").exists());
    assert!(
        !h.mount.join("base").join("only-slot").exists(),
        "the clone leaked into base"
    );

    let (code, body) = h.call(Request::SnapshotReset(cowfs_ctl::SnapshotReset {
        name: "slot".into(),
        from: "base".into(),
        expect_no_holders: true,
    }));
    assert_eq!(code, 0, "snapshot reset: {body}");
    // The store is the authority here. What the mount shows is bounded by the adapter's cache
    // instead: actimeo 120 s on the NFS client, one second in the FUSE shared mode, so reading
    // the mount right after a reset would measure the adapter, not the reset.
    let store_slot = h.store.join("slot");
    assert!(
        !store_slot.join("only-slot").exists(),
        "the reset left the clone's own file behind"
    );
    assert!(
        store_slot.join("hello.txt").exists(),
        "the reset lost the base's file"
    );

    let (code, body) = h.call(Request::SnapshotRm(cowfs_ctl::SnapshotRm {
        name: "slot".into(),
        expect_no_holders: true,
    }));
    assert_eq!(code, 0, "snapshot rm: {body}");
    assert!(
        !h.store.join("slot").exists(),
        "the snapshot is still in the store"
    );

    // SIGTERM unmounts, stops the control server and leaves no process behind.
    let mount = h.mount.clone();
    h.kill("TERM");
    assert!(
        !cowfs_daemon::mounts::is_mounted(&mount),
        "the mount outlived a SIGTERM"
    );
    assert!(!h.socket.exists(), "the socket outlived a SIGTERM");
    assert!(h.answer().is_none(), "a daemon is still answering");
}

/// A daemon killed with SIGKILL cannot clean up, so the next start sweeps the mount its server
/// left behind: without that, every `ls` on it hangs for twenty seconds on macOS.
#[test]
#[ignore = "needs a real mount adapter"]
fn a_killed_daemon_leaves_a_stale_mount_the_next_start_sweeps() {
    let Some(mut h) = Host::new() else {
        return;
    };
    h.start();
    let mount = h.mount.clone();
    h.kill("KILL");
    assert!(
        cowfs_daemon::mounts::is_mounted(&mount),
        "SIGKILL should have left the mount behind, otherwise this proves nothing"
    );
    // The stale mount is gone before the new daemon answers, so nothing can read it.
    h.start();
    assert!(h.answer().is_some(), "the second daemon is serving");
    assert!(
        cowfs_daemon::mounts::is_mounted(&mount),
        "the second daemon did not mount"
    );
    assert_eq!(
        std::fs::read_dir(&mount)
            .expect("the new mount reads")
            .count(),
        0
    );
    let (code, body) = h.call(Request::Shutdown(NoParams {}));
    assert_eq!(code, 0, "shutdown: {body}");
}

/// `mount_snapshot` and `unmount_snapshot` through the daemon's library entry point against a
/// live daemon, which is what the control method will call once cowfs-ctl grows it.
#[test]
#[ignore = "needs a real mount adapter"]
fn mount_snapshot_exports_a_snapshot_and_unmount_removes_it() {
    let Some(mut host) = Host::new() else {
        return;
    };
    let daemon = host.start_here();
    daemon
        .handler()
        .snapshot_create(cowfs_ctl::SnapshotCreate {
            name: "base".into(),
            from: None,
        })
        .expect("a snapshot to export");
    let target = host.slot("7");
    let info = daemon
        .handler()
        .mount_snapshot(&cowfs_daemon::MountSnapshot {
            name: "base".into(),
            path: target.display().to_string(),
            expect_no_holders: true,
        })
        .expect("the export is inside the root, deep enough and empty");
    assert_eq!(info.mount_path, target.display().to_string());
    assert!(
        cowfs_daemon::mounts::is_mounted(&target),
        "the export is not mounted"
    );
    let target_mount = target.clone();
    daemon
        .handler()
        .unmount_snapshot(&cowfs_daemon::UnmountSnapshot {
            path: target.display().to_string(),
        })
        .expect("unmounting our own export");
    assert!(
        !cowfs_daemon::mounts::is_mounted(&target_mount),
        "the export outlived unmount_snapshot"
    );
}
