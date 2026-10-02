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

use cowfs_ctl::{Client, ClientOptions, NoParams, Request, Response};
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
    backend: cowfs_daemon::BackendKind,
}

impl Host {
    /// A host for one backend. `None` when this machine cannot mount at all.
    fn new(backend: cowfs_daemon::BackendKind) -> Option<Host> {
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
            backend,
        };
        Some(host)
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
            .arg("--backend")
            .arg(match self.backend {
                cowfs_daemon::BackendKind::Core => "core",
                cowfs_daemon::BackendKind::Path => "path",
            })
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
                cowfs_ctl::ClientError::Server(err) => (
                    1,
                    format!(
                        "{}: {}{}",
                        err.code,
                        err.message,
                        err.details
                            .as_ref()
                            .map_or_else(String::new, |d| format!(" [{d}]"))
                    ),
                ),
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
    let Some(mut h) = Host::new(cowfs_daemon::BackendKind::Path) else {
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
    let Some(mut h) = Host::new(cowfs_daemon::BackendKind::Path) else {
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
    let Some(mut host) = Host::new(cowfs_daemon::BackendKind::Path) else {
        return;
    };
    host.start();
    let (code, body) = host.call(Request::SnapshotCreate(cowfs_ctl::SnapshotCreate {
        name: "base".into(),
        from: None,
    }));
    assert_eq!(code, 0, "a snapshot to export: {body}");
    let target = host.slot("7");
    // Through the control methods now, which is where a client reaches them: the request goes
    // over the socket, the framework validates it and hands the handler its snapshot lock.
    ok(
        &host,
        Request::MountSnapshot(cowfs_ctl::MountSnapshot {
            name: "base".into(),
            path: target.display().to_string(),
            expect_no_holders: true,
        }),
    );
    assert!(
        cowfs_daemon::mounts::is_mounted(&target),
        "the export is not mounted"
    );
    ok(
        &host,
        Request::UnmountSnapshot(cowfs_ctl::UnmountSnapshot {
            path: target.display().to_string(),
        }),
    );
    assert!(
        !cowfs_daemon::mounts::is_mounted(&target),
        "the export outlived unmount_snapshot"
    );
}

/// Bytes a file of `n` bytes holds, built so a read that is short, shifted or repeated cannot
/// look right.
fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

/// The core backend end to end: a real store, a real mount, real bytes.
///
/// Everything here goes through the mount or the control socket, so it is the whole stack:
/// `cowfs-core`'s `Vfs`, the adapter, the control plane, the store. A restart and a kill are in
/// here because the only way to know a snapshot survived is to reopen the store and look.
#[test]
#[ignore = "needs a real mount adapter"]
fn the_core_backend_serves_real_bytes_and_survives_a_restart_and_a_kill() {
    let Some(mut h) = Host::new(cowfs_daemon::BackendKind::Core) else {
        return;
    };
    h.start();
    let big = pattern(5 << 20);
    let many = 300;

    // A snapshot, then bytes through the mount: one multi-MiB file and many small ones, because
    // those take different paths (chunked writes against the store, one write per file).
    ok(
        &h,
        Request::SnapshotCreate(cowfs_ctl::SnapshotCreate {
            name: "base".into(),
            from: None,
        }),
    );
    let base = h.mount.join("base");
    std::fs::write(base.join("big.bin"), &big).expect("write the big file");
    for i in 0..many {
        std::fs::write(base.join(format!("small-{i}")), format!("small {i}\n"))
            .unwrap_or_else(|e| panic!("write small-{i}: {e}"));
    }
    std::fs::create_dir_all(base.join("d").join("e")).expect("mkdir");
    std::fs::write(base.join("d").join("e").join("f"), b"nested\n").expect("nested write");
    assert_eq!(
        read(&base.join("big.bin")),
        big,
        "the big file reads back wrong"
    );
    for i in 0..many {
        assert_eq!(
            read(&base.join(format!("small-{i}"))),
            format!("small {i}\n").into_bytes(),
            "small-{i} reads back wrong"
        );
    }
    assert_eq!(read(&base.join("d").join("e").join("f")), b"nested\n");

    // fsync through the mount is what makes the bytes survive a kill, so do it explicitly.
    sync_dir(&base);

    // A clone, then a write into the clone that the base must not see: this is the O(1) fork,
    // and it is the property the whole design rests on.
    ok(
        &h,
        Request::SnapshotCreate(cowfs_ctl::SnapshotCreate {
            name: "slot".into(),
            from: Some("base".into()),
        }),
    );
    let slot = h.mount.join("slot");
    assert_eq!(
        read(&slot.join("big.bin")),
        big,
        "the clone lost the big file"
    );
    std::fs::write(slot.join("only-slot"), b"x").expect("write into the clone");
    sync_dir(&slot);
    assert!(
        !base.join("only-slot").exists(),
        "the clone leaked into base"
    );
    assert_eq!(h.names(), ["base", "slot"]);

    // The clone's own file is dropped and base's file is kept by a reset. Read the store through
    // a fresh daemon-side view instead of the mount: what the mount shows right after a reset is
    // bounded by the adapter's cache (actimeo 120 s on NFS), not by the reset.
    ok(
        &h,
        Request::SnapshotReset(cowfs_ctl::SnapshotReset {
            name: "slot".into(),
            from: "base".into(),
            expect_no_holders: true,
        }),
    );
    assert_eq!(
        h.names(),
        ["base", "slot"],
        "a reset keeps one snapshot per name"
    );
    assert!(
        !h.snapshot_has("slot", "only-slot"),
        "the reset left the clone's file"
    );
    assert!(
        h.snapshot_has("slot", "big.bin"),
        "the reset lost the base's file"
    );

    // SIGTERM: unmount, close the store, leave nothing. Then the same daemon again on the same
    // store, which is the only proof the bytes were made durable and the store reopened.
    let mount = h.mount.clone();
    h.kill("TERM");
    assert!(
        !cowfs_daemon::mounts::is_mounted(&mount),
        "the mount outlived SIGTERM"
    );
    assert!(!h.socket.exists(), "the socket outlived SIGTERM");
    h.start();
    assert!(
        cowfs_daemon::mounts::is_mounted(&mount),
        "the second daemon did not mount"
    );
    assert_eq!(
        h.names(),
        ["base", "slot"],
        "the snapshots did not survive SIGTERM"
    );
    assert_eq!(
        read(&h.mount.join("base").join("big.bin")),
        big,
        "the big file did not survive SIGTERM"
    );
    assert!(
        !h.snapshot_has("base", "only-slot"),
        "a file from the clone appeared in base"
    );
    assert!(
        h.snapshot_has("base", "d/e/f"),
        "a nested file went missing"
    );

    // SIGKILL: nothing can clean up, so the next start sweeps the mount, reopens the store (the
    // lock is gone with the process) and finds the fsynced bytes.
    h.kill("KILL");
    assert!(
        cowfs_daemon::mounts::is_mounted(&mount),
        "SIGKILL should have left the mount behind, otherwise this proves nothing"
    );
    h.start();
    assert!(
        cowfs_daemon::mounts::is_mounted(&mount),
        "the third daemon did not mount"
    );
    assert_eq!(
        read(&h.mount.join("base").join("big.bin")),
        big,
        "the fsynced bytes did not survive SIGKILL"
    );
    assert_eq!(
        h.names(),
        ["base", "slot"],
        "the snapshots did not survive SIGKILL"
    );
    let (code, body) = h.call(Request::Status(Default::default()));
    assert_eq!(code, 0, "{body}");
    let status: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(status["block_count"].as_u64().unwrap_or(0) > 0, "{body}");
    ok(&h, Request::Shutdown(NoParams {}));
}

/// The core refuses to open a store with unacknowledged damage, and says so.
#[test]
#[ignore = "needs a real mount adapter"]
fn the_core_refuses_a_store_that_reports_damage_and_does_not_acknowledge_it() {
    let Some(mut h) = Host::new(cowfs_daemon::BackendKind::Core) else {
        return;
    };
    h.start();
    ok(
        &h,
        Request::SnapshotCreate(cowfs_ctl::SnapshotCreate {
            name: "base".into(),
            from: None,
        }),
    );
    let f = h.mount.join("base").join("f");
    std::fs::write(&f, b"durable\n").unwrap();
    sync_file(&f);
    ok(&h, Request::Shutdown(NoParams {}));
    assert!(!h.socket.exists());

    // Destroy a pack: the data a completed sync made durable is gone, and nothing acknowledged
    // it. Opening must refuse and name the loss.
    let packs = std::fs::read_dir(h.store.join("packs")).expect("the store has packs");
    let mut hit = false;
    for entry in packs.flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "pack") {
            let bytes = std::fs::read(&p).unwrap();
            let m = bytes.len() / 2;
            std::fs::write(&p, &bytes[..m]).unwrap();
            hit = true;
            break;
        }
    }
    assert!(hit, "no pack file to damage");
    let out = Command::new(daemon_bin())
        .args(["--store", &h.store.display().to_string()])
        .args(["--mount", &h.mount.display().to_string()])
        .args(["--socket", &h.socket.display().to_string()])
        .stdin(Stdio::null())
        .output()
        .expect("the daemon binary runs");
    assert_eq!(
        out.status.code(),
        Some(1),
        "a damaged store must not be served"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("corruption") || err.contains("lost"), "{err}");
    assert!(!h.socket.exists(), "a refused store bound nothing");
}

/// A path in the store's snapshot namespace as the daemon itself sees it, without the mount's
/// cache in the way: a second daemon opened on the same store, asked over the control socket.
impl Host {
    fn names(&self) -> Vec<String> {
        let (code, body) = self.call(Request::SnapshotList(Default::default()));
        assert_eq!(code, 0, "snapshot_list: {body}");
        let listed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let mut names: Vec<String> = listed["snapshots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_owned())
            .collect();
        names.sort();
        names
    }

    /// True when `snapshot` has `path`, read through a fresh export rather than the default
    /// mount. A new mount has no cached attributes, so this reports what the store holds rather
    /// than what the adapter last saw.
    fn snapshot_has(&self, snapshot: &str, path: &str) -> bool {
        let at = self.root.join(slot_for(snapshot, path)).join("repo");
        let (code, body) = self.call(Request::MountSnapshot(cowfs_ctl::MountSnapshot {
            name: snapshot.into(),
            path: at.display().to_string(),
            expect_no_holders: true,
        }));
        assert_eq!(code, 0, "exporting {snapshot}: {body}");
        let there = at.join(path).exists();
        let (code, body) = self.call(Request::UnmountSnapshot(cowfs_ctl::UnmountSnapshot {
            path: at.display().to_string(),
        }));
        assert_eq!(code, 0, "removing the export: {body}");
        there
    }
}

/// A slot name for one check, unique per call so no export is ever reused.
fn slot_for(snapshot: &str, path: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "s{}-{snapshot}-{}-{}",
        NEXT.fetch_add(1, Ordering::SeqCst),
        path.replace(['/', '.'], "_"),
        std::process::id()
    )
}

fn read(p: &std::path::Path) -> Vec<u8> {
    std::fs::read(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn ok(h: &Host, request: Request) -> String {
    let (code, body) = h.call(request);
    if code != 0 {
        let holders: serde_json::Value =
            serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
        panic!(
            "the request failed: {body}\nholders: {}",
            holders
                .get("holders")
                .map_or_else(|| body.clone(), |h| h.to_string())
        );
    }
    body
}

fn sync_file(p: &std::path::Path) {
    let f = std::fs::File::open(p).expect("open to fsync");
    f.sync_all()
        .unwrap_or_else(|e| panic!("fsync {}: {e}", p.display()));
}

fn sync_dir(p: &std::path::Path) {
    let d = std::fs::File::open(p).expect("open the directory to fsync");
    let _ = d.sync_all();
}
