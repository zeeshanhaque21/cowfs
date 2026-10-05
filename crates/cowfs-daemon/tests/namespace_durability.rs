//! A name the caller synced is still there after the daemon is killed. Issue #90.
//!
//! On this transport a namespace RPC used to be answered from memory, so `rename` was
//! `Ack::Applied`: macOS emits no COMMIT for a directory `fsync`, nor for an `fsync` of a
//! descriptor with no dirty pages, so the POSIX habit bought no durability and nothing told the
//! caller. The repair makes the adapter durable before it answers, so the crash itself is the
//! regression: a real daemon, the real CLI, a real NFS mount, a private store, `SIGKILL` right
//! after the caller returns, and the same store reopened by a fresh daemon.
//!
//! `fsck` clean plus the old name intact would not be enough on their own, so every case compares
//! both names and the bytes, and `DirtySibling` keeps the control the issue measured as working,
//! which proves the rename is committable on this daemon.
//!
//! ```text
//! cargo test -p cowfs-daemon --test namespace_durability -- --ignored --test-threads=1 --nocapture
//! ```

#[path = "evidence/reader.rs"]
mod evidence;
#[path = "guard/reader.rs"]
mod guard;

use evidence::{Provenance, Row};
use guard::{cleanup, MountState};
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long to wait for something that should happen at once.
const SETTLE: Duration = Duration::from_secs(60);

const SNAP: &str = "snap";
const OLD: &str = "old.txt";
const NEW: &str = "new.txt";
const SIBLING: &str = "sibling.txt";
/// Long enough that a lost or truncated body cannot pass as equal.
const BODY: &[u8] = b"cowfs namespace durability payload 0123456789 abcdefghijklmnopqrstuvwxyz\n";

/// What the caller does after `rename` returns.
#[derive(Clone, Copy, Debug)]
enum After {
    /// `fsync` of the parent directory: the POSIX habit, and the case the issue measured as lost.
    ParentDir,
    /// `fsync` of a read-only descriptor of the renamed file: also measured as lost.
    ReadOnlyFd,
    /// The control the issue measured as working: write and `fsync` a sibling file.
    DirtySibling,
    /// No sync at all. A barrier at ack time has to cover this too.
    Nothing,
}

impl After {
    const CASES: [After; 4] = [
        After::ParentDir,
        After::ReadOnlyFd,
        After::DirtySibling,
        After::Nothing,
    ];

    fn name(self) -> &'static str {
        match self {
            After::ParentDir => "fsync-parent-dir",
            After::ReadOnlyFd => "fsync-read-only-fd",
            After::DirtySibling => "write-fsync-sibling",
            After::Nothing => "no-sync-at-all",
        }
    }

    /// The POSIX habit, done on the mounted snapshot.
    fn apply(self, dir: &Path) {
        match self {
            After::ParentDir => fs::File::open(dir)
                .and_then(|d| d.sync_all())
                .expect("fsync the parent directory"),
            After::ReadOnlyFd => fs::File::open(dir.join(NEW))
                .and_then(|f| f.sync_all())
                .expect("fsync a read-only descriptor of the renamed file"),
            After::DirtySibling => {
                let mut f = fs::File::create(dir.join(SIBLING)).expect("create the sibling");
                f.write_all(b"control\n").expect("write the sibling");
                f.sync_all().expect("fsync the sibling");
            }
            After::Nothing => {}
        }
    }
}

/// `bench/out/durability90`, created once and resolved so the evidence lands in the worktree.
fn artifacts() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bench/out/durability90");
    fs::create_dir_all(&root).expect("the artifact directory");
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

fn private(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).expect("a private export root");
}

/// 64-bit FNV-1a, only so the log carries a fingerprint of what was read back.
fn digest(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")
}

/// One private store, one mount, one socket, all owned by this run and removed when it ends.
struct Run {
    dir: PathBuf,
    socket: PathBuf,
    store: PathBuf,
    mount: PathBuf,
    child: Option<Child>,
}

impl Run {
    /// Mints a fresh, unique attempt and only ever creates inside it.
    ///
    /// The previous version reused a fixed `{case}-rep{n}` name and recursively deleted whatever was
    /// already there, mount point included, before the daemon and therefore before any mount-state
    /// check could run. A cancelled run left exactly the path the next run walked.
    fn new(case: &str, rep: usize) -> Run {
        let tag = format!("{case}-rep{rep}");
        let paths = guard::attempt(&artifacts(), &tag);
        guard::preflight(&paths).unwrap_or_else(|why| panic!("refusing to start the run: {why}"));
        fs::create_dir_all(paths.dir.join("pool")).expect("the export root");
        fs::create_dir_all(&paths.mount).expect("the mount point");
        Run {
            dir: paths.dir,
            socket: paths.socket,
            store: paths.store,
            mount: paths.mount,
            child: None,
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
        let mut child = child;
        // Either the socket answers and the mount is up, or the child is gone. Both end the loop.
        let deadline = Instant::now() + SETTLE;
        while Instant::now() < deadline {
            if self.cli(&["status"]).is_ok() {
                assert_eq!(
                    self.mount_state(),
                    MountState::Mounted,
                    "the daemon answered but the mount table does not name {}",
                    self.mount.display()
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

    /// The snapshot the case works in, through the mount.
    fn snap(&self) -> PathBuf {
        self.mount.join(SNAP)
    }

    /// The pid this test started and may signal. Nothing else is ever signalled.
    fn pid(&self) -> u32 {
        self.child.as_ref().expect("a running daemon").id()
    }

    /// `SIGKILL` of the pid this test started, then wait for it to be gone. `kill` runs the real
    /// binary so the signal is the one named, and no process group is ever touched.
    fn sigkill(&mut self) {
        let mut child = self.child.take().expect("a running daemon");
        let pid = child.id();
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
                return;
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

impl Run {
    /// Classifies this run's own mount point through the fail-closed reader, never through
    /// `cowfs_daemon::mounts::is_mounted`, which reports `false` both when `/sbin/mount` cannot be
    /// run and when its unescaped prefix match misses, and which belongs to another owner.
    fn mount_state(&self) -> MountState {
        let (state, reader) =
            guard::read_mount_table(&self.mount, &self.store, &self.socket, guard::CHILD_BUDGET);
        if let Some(mut r) = reader {
            r.stop_owned();
        }
        state
    }
}

impl Drop for Run {
    /// One absolute deadline for the whole teardown, computed once. A slow step cannot buy a later
    /// step a fresh budget. This bounds a test fixture cleaning up after itself and nothing else.
    fn drop(&mut self) {
        let deadline = Instant::now() + guard::TEARDOWN_BUDGET;
        self.stop();
        // Never walk a tree that is still a mount point: a dead server turns the recursive delete
        // into an unbounded hang. macOS `umount` has no `-z`, so wait for the table to agree, and
        // spawn it bounded, because `Command::status` has no timeout of its own.
        let mut state = self.mount_state();
        if state == MountState::Mounted {
            let left = guard::CHILD_BUDGET.min(deadline.saturating_duration_since(Instant::now()));
            if !left.is_zero() {
                match guard::spawn_bounded(
                    std::path::Path::new("/sbin/umount"),
                    &[std::ffi::OsStr::new("-f"), self.mount.as_os_str()],
                    &self.store,
                    &self.socket,
                    left,
                ) {
                    Ok(mut um) => {
                        if !um.finished {
                            eprintln!(
                                "PRESERVE: umount of {} did not finish inside {left:?}",
                                self.mount.display()
                            );
                            um.stop_owned();
                        }
                    }
                    Err(e) => eprintln!("umount of {} could not start: {e}", self.mount.display()),
                }
            }
            let until = deadline.min(Instant::now() + Duration::from_secs(15));
            while Instant::now() < until {
                state = self.mount_state();
                if state != MountState::Mounted {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        // Fail closed. `Mounted` and `Unknown` both preserve, and the table decides, never a bool.
        let owned = true;
        let verdict = cleanup(state, owned, "run", || {
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
                "LEAK: {} is {state:?} in the mount table; {why}. The store and run are left in \
                 place",
                self.mount.display()
            );
        }
    }
}

/// What one rep left behind after the kill, so a failure names the counterexample instead of
/// only saying a name was missing.
struct Outcome {
    case: &'static str,
    rep: usize,
    pid: u32,
    after: Option<Vec<u8>>,
    old_back: Option<Vec<u8>>,
    fsck: String,
}

impl Outcome {
    fn survived(&self) -> bool {
        self.after.as_deref() == Some(BODY) && self.old_back.is_none()
    }

    fn line(&self) -> String {
        format!(
            "{} rep{}: pid {} SIGKILLed, new name {}, old name {}, new digest {}, old digest {}, \
             fsck {:?}",
            self.case,
            self.rep,
            self.pid,
            if self.after.is_some() { "kept" } else { "LOST" },
            if self.old_back.is_some() {
                "CAME BACK"
            } else {
                "gone"
            },
            self.after.as_deref().map(digest).unwrap_or("-".into()),
            self.old_back.as_deref().map(digest).unwrap_or("-".into()),
            self.fsck.trim(),
        )
    }
}

/// One case, one rep: create the file, rename it, sync the way the case says, then kill the
/// daemon 2 to 6 ms later and read the same store back through a fresh daemon and mount.
fn rep(case: After, rep: usize) -> Outcome {
    let mut run = Run::new(case.name(), rep);
    private(&run.dir.join("pool"));
    run.start();
    run.cli(&["snapshot", "create", SNAP])
        .expect("snapshot create");
    let dir = run.snap();

    let old = dir.join(OLD);
    let mut f = fs::File::create(&old).expect("create the file to rename");
    f.write_all(BODY).expect("write the body");
    // Durable before the rename, so the only uncommitted thing left is the rename itself.
    f.sync_all().expect("fsync the file");
    drop(f);

    fs::rename(&old, dir.join(NEW)).expect("rename");
    case.apply(&dir);
    let before = fs::read(dir.join(NEW)).expect("read the renamed file back");
    assert_eq!(before, BODY, "the rename did not read back before the kill");

    let pid = run.pid();
    std::thread::sleep(Duration::from_millis(2 + (rep as u64 % 5)));
    run.sigkill();

    run.start();
    assert_ne!(run.pid(), pid, "the restart reused the killed pid");
    let dir = run.snap();
    let out = Outcome {
        case: case.name(),
        rep,
        pid,
        after: fs::read(dir.join(NEW)).ok(),
        old_back: fs::read(dir.join(OLD)).ok(),
        fsck: run
            .cli(&["fsck"])
            .unwrap_or_else(|e| format!("fsck failed: {e}")),
    };
    if let Some(body) = &out.after {
        assert_eq!(
            body,
            &before,
            "{}: the new name came back with other bytes",
            case.name()
        );
    }
    eprintln!("durability90 {}", out.line());
    out
}

/// Every case the caller can reach, three reps each. Small on purpose: each rep starts a daemon,
/// mounts, kills and mounts again.
#[test]
#[ignore = "mounts a filesystem and kills a daemon; run with --ignored"]
fn a_synced_namespace_survives_a_killed_daemon() {
    if !cowfs_daemon::mounts::available() {
        eprintln!("SKIP: no usable mount adapter on this host");
        return;
    }
    let only = std::env::var("DURABILITY90_ONLY").ok();
    // One attempt, one directory, one provenance record. The revision and every binding are
    // repeated on each row, so a reader of the rows alone is not relying on the manifest having been
    // shipped alongside, and no row can be mistaken for the output of some other tree.
    let attempt = std::env::var("DURABILITY90_ATTEMPT").unwrap_or_else(|_| "local".into());
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let prov = Provenance::collect(
        &repo,
        &[
            "crates/cowfs-daemon/tests/namespace_durability.rs",
            "crates/cowfs-nfs/src/adapter.rs",
            "crates/cowfs-nfs/src/lib.rs",
            "crates/cowfs-core/src/inner.rs",
            "crates/cowfs-core/src/io.rs",
            "crates/cowfs-core/src/view.rs",
            "crates/cowfs-core/src/vfs_impl.rs",
            "crates/cowfs-vfs/src/vfs.rs",
        ],
        &[test_bin().join("cowfs-daemon"), test_bin().join("cowfs")],
    );
    eprintln!(
        "durability90 attempt {attempt}: revision {} ({}), {} bindings",
        prov.revision.head,
        prov.revision.label(),
        prov.bound.len()
    );
    let dir = artifacts().join("repair").join(&attempt);
    let mut rows: Vec<Row> = Vec::new();
    let mut outcomes = Vec::new();
    for case in After::CASES {
        if only.as_deref().is_some_and(|want| want != case.name()) {
            continue;
        }
        // `DURABILITY90_REPS` bounds the matrix for a small provenance run, so a receipt can be
        // regenerated from an actual measurement without paying for the full three-per-case batch.
        let reps: usize = std::env::var("DURABILITY90_REPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(3);
        for nth in 1..=reps {
            let out = rep(case, nth);
            rows.push(Row {
                case: out.case,
                rep: out.rep,
                pid: out.pid,
                new_name_kept: out.after.is_some(),
                old_name_back: out.old_back.is_some(),
                new_digest: out.after.as_deref().map(digest).unwrap_or_default(),
                fsck: out.fsck.trim().to_string(),
            });
            // Rewritten and flushed after every rep, so a run that dies keeps the rows it already
            // proved.
            evidence::write_attempt(&dir, &attempt, &prov, &rows).expect("write the receipts");
            outcomes.push(out);
        }
    }
    for case in After::CASES {
        let kept = outcomes
            .iter()
            .filter(|o| o.case == case.name() && o.survived())
            .count();
        let total = outcomes.iter().filter(|o| o.case == case.name()).count();
        if total == 0 {
            continue;
        }
        assert_eq!(
            kept,
            total,
            "{}: only {kept}/{total} reps kept the rename the caller had synced:\n{}",
            case.name(),
            outcomes
                .iter()
                .filter(|o| o.case == case.name())
                .map(|o| format!("  {}", o.line()))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
}

/// The worker of the native control. Not a test; the control below runs it.
#[test]
#[ignore = "helper process of the native control"]
fn native_helper() {
    let Ok(dir) = std::env::var("DURABILITY90_NATIVE_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let old = dir.join(OLD);
    let mut f = fs::File::create(&old).expect("create the file to rename");
    f.write_all(BODY).expect("write the body");
    f.sync_all().expect("fsync the file");
    drop(f);
    fs::rename(&old, dir.join(NEW)).expect("rename");
    fs::File::open(&dir)
        .and_then(|d| d.sync_all())
        .expect("fsync the parent directory");
    // The rename and the directory fsync have returned and the caller has been told so; the
    // kill lands after that, never in the middle of an operation.
    println!("renamed");
    std::io::stdout().flush().expect("the helper speaks");
    loop {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The same recipe on native APFS, with the worker killed instead of a daemon. It cannot fail on
/// durability, because a process kill does not touch the page cache, so what it proves is that the
/// recipe, the readback and the cleanup are sound, not that the sync mattered.
#[test]
#[ignore = "spawns and kills a helper; run with --ignored"]
fn native_apfs_survives_the_same_recipe() {
    // A fresh root each time. Nothing here is a mount point, so clearing a reused path would be
    // harmless, but a fixed name that is cleared on sight is the pattern this repair removed
    // everywhere else and it has no place left in this file.
    let root = guard::attempt(&artifacts(), "native").dir;
    guard::preflight(&guard::Attempt {
        dir: root.clone(),
        store: root.clone(),
        mount: root.clone(),
        socket: std::env::temp_dir().join("d90-native-control.sock"),
    })
    .unwrap_or_else(|why| panic!("refusing to start the native control: {why}"));
    fs::create_dir_all(&root).expect("the native directory");
    let mut child = Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--exact", "native_helper", "--ignored", "--nocapture"])
        .env("DURABILITY90_NATIVE_DIR", &root)
        .stdout(Stdio::piped())
        .spawn()
        .expect("the helper runs");
    let pid = child.id();
    let mut said = String::new();
    {
        let out = child.stdout.take().expect("the helper's stdout");
        for line in std::io::BufReader::new(out).lines() {
            let line = line.expect("the helper's line");
            if line.contains("renamed") {
                said = line;
                break;
            }
        }
    }
    assert_eq!(
        said.trim(),
        "renamed",
        "the helper never reported the rename"
    );
    assert!(
        Command::new("/bin/kill")
            .arg("-9")
            .arg(pid.to_string())
            .status()
            .is_ok_and(|s| s.success()),
        "could not SIGKILL the helper {pid}"
    );
    let deadline = Instant::now() + SETTLE;
    while Instant::now() < deadline {
        if !running(&mut child) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        fs::read(root.join(NEW)).expect("the native new name"),
        BODY,
        "native APFS lost the rename"
    );
    assert!(!root.join(OLD).exists(), "native APFS kept both names");
    eprintln!(
        "durability90 native: pid {pid} SIGKILLed after the rename and the directory fsync, \
         new name survived, digest {}",
        digest(BODY)
    );
    cowfs_vfs_path::force_remove_dir_all(&root);
}
