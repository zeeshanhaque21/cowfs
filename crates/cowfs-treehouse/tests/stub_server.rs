//! The snapshot-native half of the companion, driven against `cowfs-ctl`'s `StubHandler`.
//!
//! What is proven here is the companion's own logic: the create-or-reset choice, the atomic
//! `expect_no_holders` path, base refresh and staleness, naming, idempotency, exit-code mapping.
//! What is not proven here is the mount, because the stub has no mount: materialising a snapshot
//! at a path is the one request this stub does not serve. The daemon and `cowfs-ctl` have
//! `mount_snapshot` and `unmount_snapshot`, but the companion does not call them yet
//! (`docs/v1-treehouse.md`, gap 1).

mod common;

use common::{private_tempdir, stub_in, Fixture, Watchdog};
use cowfs_ctl::{Hold, HoldKind, ProcessInfo, StubHandler};
use cowfs_treehouse::{
    base_status, pool_id, slot_of, slot_snapshot, CowfsMaterialiser, Daemon, Error, Materialiser,
    Provision, RecordingMaterialiser, EXIT_BUSY, EXIT_ERROR, EXIT_NOT_RUNNING, EXIT_OK,
};
use std::sync::{Arc, Mutex, PoisonError};

/// A stub daemon plus a throwaway repository, which is what a slot path needs to be real.
struct Fixture2 {
    _dir: tempfile::TempDir,
    stub: common::Stub,
    repo: std::path::PathBuf,
    daemon: Daemon,
}

fn fixture() -> Fixture2 {
    let dir = private_tempdir();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("mkdir repo");
    git(&repo, &["init", "-q", "."]);
    git(&repo, &["config", "user.email", "t@example.invalid"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("README.md"), b"hello\n").expect("write");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    git(&repo, &["branch", "-M", "main"]);
    let stub = stub_in(dir.path());
    let daemon = Daemon::connect(Some(&stub.socket), Some(5)).expect("connect");
    Fixture2 {
        _dir: dir,
        stub,
        repo,
        daemon,
    }
}

fn git(cwd: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
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

fn holder(pid: u32, kind: HoldKind, path: &str) -> ProcessInfo {
    ProcessInfo {
        pid,
        command: format!("sleep {pid}"),
        holds: vec![Hold {
            kind,
            path: path.to_owned(),
        }],
    }
}

impl Fixture2 {
    /// The pool id treehouse would derive for this repository.
    fn pool_id(&self) -> String {
        pool_id(&self.repo).expect("pool id")
    }

    /// A real slot of the shape treehouse creates: an actual `git worktree add` under the pool, so
    /// the pool id resolves through the worktree the same way it does in production.
    fn slot(&self, name: &str) -> std::path::PathBuf {
        let id = self.pool_id();
        let dir = self
            ._dir
            .path()
            .join("pool/.treehouse")
            .join(&id)
            .join(name);
        std::fs::create_dir_all(dir.parent().expect("parent")).expect("mkdir pool slot");
        let path = dir.join("repo");
        git(
            &self.repo,
            &[
                "worktree",
                "add",
                "--detach",
                "-q",
                &path.display().to_string(),
                "main",
            ],
        );
        path
    }

    /// Records a warm base for this repository, which is what `base refresh` produces.
    fn with_base(&mut self) -> String {
        let name = cowfs_treehouse::base_snapshot(&self.pool_id()).expect("base name");
        self.daemon
            .base_refresh(&self.repo, "main", Some(&name))
            .expect("base refresh");
        name
    }
}

#[test]
fn a_slot_is_created_from_the_warm_base() {
    let _w = Watchdog::start(60);
    let mut f = fixture();
    let base = f.with_base();
    let slot = f.slot("1");
    let materialiser = RecordingMaterialiser::default();

    let out = Provision {
        daemon: &mut f.daemon,
        materialiser: &materialiser,
        slot_path: slot.clone(),
        pool_id: None,
        busy_timeout: cowfs_treehouse::DEFAULT_BUSY_TIMEOUT,
    }
    .run()
    .expect("provision");

    let id = f.pool_id();
    assert_eq!(out.pool_id, id);
    assert_eq!(out.slot, "1");
    assert_eq!(
        out.snapshot,
        slot_snapshot(&id, "1").expect("snapshot name")
    );
    assert_eq!(out.base, base);
    assert!(!out.reused, "a first acquisition creates the snapshot");
    assert_eq!(materialiser.calls.borrow().len(), 1);
    let list = f.daemon.snapshot_list().expect("list");
    let created = list
        .iter()
        .find(|s| s.name == out.snapshot)
        .expect("created");
    assert_eq!(
        created.parent.as_deref(),
        Some(base.as_str()),
        "the slot is a clone of the warm base, not of the empty tree"
    );
}

#[test]
fn a_second_acquisition_resets_rather_than_recreating() {
    let _w = Watchdog::start(60);
    let mut f = fixture();
    f.with_base();
    let slot = f.slot("1");
    let materialiser = RecordingMaterialiser::default();
    let run = |f: &mut Fixture2| {
        Provision {
            daemon: &mut f.daemon,
            materialiser: &materialiser,
            slot_path: slot.clone(),
            pool_id: None,
            busy_timeout: cowfs_treehouse::DEFAULT_BUSY_TIMEOUT,
        }
        .run()
        .expect("provision")
    };
    let first = run(&mut f);
    assert!(!first.reused);
    let second = run(&mut f);
    assert!(
        second.reused,
        "an existing slot snapshot is reset, not created again"
    );
    assert_eq!(first.snapshot, second.snapshot);
    // Resetting must not go through the materialiser: the snapshot is already in place.
    assert_eq!(
        materialiser.calls.borrow().len(),
        1,
        "{:?}",
        materialiser.calls
    );
}

#[test]
fn provisioning_is_idempotent_and_converges_when_repeated() {
    let _w = Watchdog::start(60);
    let mut f = fixture();
    f.with_base();
    let slot = f.slot("2");
    let materialiser = RecordingMaterialiser::default();
    let mut runs = Vec::new();
    for _ in 0..4 {
        runs.push(
            Provision {
                daemon: &mut f.daemon,
                materialiser: &materialiser,
                slot_path: slot.clone(),
                pool_id: None,
                busy_timeout: cowfs_treehouse::DEFAULT_BUSY_TIMEOUT,
            }
            .run()
            .expect("provision"),
        );
    }
    let names: Vec<&str> = runs.iter().map(|r| r.snapshot.as_str()).collect();
    assert!(
        names.windows(2).all(|w| w[0] == w[1]),
        "every run derives the same snapshot: {names:?}"
    );
    assert!(
        runs.iter().all(|r| !r.gitdir_rewritten),
        "git already wrote the correct link, so no run rewrites it: {:?}",
        runs.iter().map(|r| r.gitdir_rewritten).collect::<Vec<_>>()
    );
    assert_eq!(
        f.daemon.snapshot_list().expect("list").len(),
        2,
        "the base and one slot, no leftovers from repeated runs"
    );
}

#[test]
fn a_partial_run_leaves_no_trace_and_the_next_run_converges() {
    let _w = Watchdog::start(60);
    let mut f = fixture();
    f.with_base();
    let slot = f.slot("3");
    let materialiser = RecordingMaterialiser::default();

    // The first two steps of the flow, with no .git rewrite: exactly what a kill between them
    // leaves behind.
    let id = f.pool_id();
    let name = slot_snapshot(&id, "3").expect("name");
    let base = cowfs_treehouse::base_snapshot(&id).expect("base");
    materialiser.materialise(&name, &slot).expect("materialise");
    f.daemon
        .snapshot_create(&name, Some(&base))
        .expect("create");

    // The rerun must not fail on the snapshot that already exists.
    let out = Provision {
        daemon: &mut f.daemon,
        materialiser: &materialiser,
        slot_path: slot.clone(),
        pool_id: None,
        busy_timeout: cowfs_treehouse::DEFAULT_BUSY_TIMEOUT,
    }
    .run()
    .expect("the rerun converges");
    assert!(
        out.reused,
        "the rerun resets rather than failing on the existing snapshot"
    );
    assert_eq!(out.snapshot, name);
    let want = cowfs_treehouse::worktree_git_dir(&slot).expect("git dir");
    assert_eq!(
        std::fs::read_to_string(slot.join(".git")).expect("read .git"),
        format!("gitdir: {}\n", want.display()),
        "the link git wrote is still correct after the rerun"
    );
    assert_eq!(
        f.daemon.snapshot_list().expect("list").len(),
        2,
        "the base and one slot, no leftovers from the interrupted run"
    );
    let _ = &id;
}

#[test]
fn a_slot_git_file_git_already_wrote_is_left_alone() {
    let _w = Watchdog::start(60);
    let mut f = fixture();
    f.with_base();
    let slot = f.slot("7");
    let before = std::fs::read_to_string(slot.join(".git")).expect("read .git");
    let materialiser = RecordingMaterialiser::default();
    let out = Provision {
        daemon: &mut f.daemon,
        materialiser: &materialiser,
        slot_path: slot.clone(),
        pool_id: None,
        busy_timeout: cowfs_treehouse::DEFAULT_BUSY_TIMEOUT,
    }
    .run()
    .expect("provision");
    assert!(
        !out.gitdir_rewritten,
        "git already wrote the right link, so nothing needs rewriting"
    );
    assert_eq!(
        std::fs::read_to_string(slot.join(".git")).expect("read .git"),
        before
    );
}

#[test]
fn provisioning_without_a_warm_base_says_so_instead_of_creating_an_empty_slot() {
    let _w = Watchdog::start(60);
    let mut f = fixture();
    let slot = f.slot("1");
    let materialiser = RecordingMaterialiser::default();
    let err = Provision {
        daemon: &mut f.daemon,
        materialiser: &materialiser,
        slot_path: slot.clone(),
        pool_id: None,
        busy_timeout: cowfs_treehouse::DEFAULT_BUSY_TIMEOUT,
    }
    .run()
    .expect_err("no base");
    let msg = err.to_string();
    assert!(msg.contains("no warm base"), "{msg}");
    assert!(msg.contains("base refresh"), "{msg}");
    assert!(materialiser.calls.borrow().is_empty());
    assert!(
        f.daemon.snapshot_list().expect("list").is_empty(),
        "nothing was created"
    );
}

#[test]
fn a_holder_makes_the_atomic_reset_refuse_and_change_nothing() {
    let _w = Watchdog::start(60);
    let mut f = fixture();
    let base = f.with_base();
    let slot = f.slot("1");
    let materialiser = RecordingMaterialiser::default();
    let out = Provision {
        daemon: &mut f.daemon,
        materialiser: &materialiser,
        slot_path: slot.clone(),
        pool_id: None,
        busy_timeout: cowfs_treehouse::DEFAULT_BUSY_TIMEOUT,
    }
    .run()
    .expect("provision");

    // A holder appears, which is the race the task asks about: after the scan, before the swap.
    f.stub.handler.add_process(
        &out.snapshot,
        holder(4242, HoldKind::Fd, &slot.display().to_string()),
    );

    let err = f
        .daemon
        .snapshot_reset(&out.snapshot, &base, true)
        .expect_err("must refuse while held");
    assert!(matches!(err, Error::Busy(_)), "{err:?}");
    assert_eq!(
        err.exit_code(),
        EXIT_BUSY,
        "a held slot is exit 5, not exit 1"
    );

    // Nothing changed, and the base is still the parent.
    let list = f.daemon.snapshot_list().expect("list");
    let slot_info = list.iter().find(|s| s.name == out.snapshot).expect("slot");
    assert_eq!(slot_info.parent.as_deref(), Some(base.as_str()));

    // With the holder gone the very same call succeeds.
    f.daemon
        .snapshot_rm(&out.snapshot, false)
        .expect("remove while held needs force");
    f.daemon
        .snapshot_create(&out.snapshot, Some(&base))
        .expect("recreate");
    let forced = f
        .daemon
        .snapshot_reset(&out.snapshot, &base, false)
        .expect("force skips the check");
    assert_eq!(forced.name, out.snapshot);
}

#[test]
fn a_holder_appears_between_the_scan_and_the_swap_and_is_still_caught() {
    let _w = Watchdog::start(60);
    let mut f = fixture();
    let base = f.with_base();
    let slot = f.slot("4");
    let materialiser = RecordingMaterialiser::default();
    let out = Provision {
        daemon: &mut f.daemon,
        materialiser: &materialiser,
        slot_path: slot.clone(),
        pool_id: None,
        busy_timeout: cowfs_treehouse::DEFAULT_BUSY_TIMEOUT,
    }
    .run()
    .expect("provision");

    // The scan a return does first, which sees nothing.
    assert!(f.daemon.ps(&out.snapshot).expect("ps").is_empty());

    // Then a writer shows up, and the authoritative check inside the operation still refuses.
    f.stub.handler.add_process(
        &out.snapshot,
        holder(777, HoldKind::Lock, &slot.display().to_string()),
    );
    let err = f
        .daemon
        .snapshot_reset(&out.snapshot, &base, true)
        .expect_err("the in-operation check is the authoritative one");
    assert!(matches!(err, Error::Busy(_)), "{err:?}");

    // The scan now names it, which is what the error message is for.
    let holders = f.daemon.ps(&out.snapshot).expect("ps");
    assert_eq!(holders.len(), 1);
    assert_eq!(holders[0].pid, 777);
    let described = cowfs_treehouse::describe(&holders);
    assert!(described[0].contains("lock"), "{:?}", described[0]);
}

#[test]
fn busy_maps_to_exit_five_and_a_missing_snapshot_to_exit_one() {
    let _w = Watchdog::start(60);
    let mut f = fixture();
    f.stub
        .handler
        .add_process("held", holder(9, HoldKind::Cwd, "/x"));
    f.daemon.snapshot_create("held", None).expect("create held");
    let busy = f.daemon.snapshot_rm("held", true).expect_err("busy");
    assert_eq!(busy.exit_code(), EXIT_BUSY);
    assert!(busy.to_string().contains("holder"), "{busy}");

    let missing = f.daemon.snapshot_rm("nope", true).expect_err("not found");
    assert_eq!(missing.exit_code(), EXIT_ERROR);
    assert!(missing.to_string().contains("not_found"), "{missing}");
}

#[test]
fn a_missing_daemon_is_exit_three() {
    let _w = Watchdog::start(30);
    let dir = private_tempdir();
    let err =
        Daemon::connect(Some(&dir.path().join("run/absent.sock")), Some(2)).expect_err("no daemon");
    assert_eq!(err.exit_code(), EXIT_NOT_RUNNING, "{err}");
}

#[test]
fn base_refresh_records_the_ref_and_promotion_is_idempotent() {
    let _w = Watchdog::start(60);
    let mut f = fixture();
    let name = cowfs_treehouse::base_snapshot(&f.pool_id()).expect("base name");
    let first = f
        .daemon
        .base_refresh(&f.repo, "main", Some(&name))
        .expect("refresh");
    assert_eq!(first.snapshot.name, name);
    assert_eq!(
        first
            .snapshot
            .base
            .as_ref()
            .and_then(|b| b.git_ref.as_deref()),
        Some("main")
    );
    assert_eq!(first.previous_commit, None);

    // Promote is idempotent, so refreshing twice is safe.
    let promoted = f.daemon.snapshot_promote(&name).expect("promote");
    assert!(promoted.base.is_some());
    let again = f.daemon.snapshot_promote(&name).expect("promote again");
    assert_eq!(promoted.name, again.name);

    let second = f
        .daemon
        .base_refresh(&f.repo, "main", Some(&name))
        .expect("refresh again");
    assert!(
        second.previous_commit.is_some(),
        "the old commit is reported"
    );
}

#[test]
fn base_status_calls_a_fresh_base_fresh_and_a_moved_ref_stale() {
    let _w = Watchdog::start(60);
    let mut f = fixture();
    let missing = base_status(&mut f.daemon, &f.repo, "main").expect("status");
    assert!(!missing.fresh);
    assert!(
        missing.reason.contains("no warm base"),
        "{}",
        missing.reason
    );

    f.with_base();
    let fresh = base_status(&mut f.daemon, &f.repo, "main").expect("status");
    assert!(
        !fresh.fresh,
        "the stub records stub-main, which is not the real commit"
    );
    assert!(
        fresh.reason.contains("cannot be decided") || fresh.reason.contains("stale"),
        "{}",
        fresh.reason
    );
    assert_eq!(
        fresh.head_commit.as_deref().map(str::len),
        Some(40),
        "a real commit"
    );

    // A repository with no such ref says so rather than claiming freshness.
    let bogus = base_status(&mut f.daemon, &f.repo, "no-such-ref").expect("status");
    assert!(!bogus.fresh);
    assert_eq!(bogus.head_commit, None);
}

#[test]
fn base_status_agrees_with_the_base_of_a_repository_the_pool_id_distinguishes() {
    let _w = Watchdog::start(60);
    let mut f = fixture();
    let id = f.pool_id();
    f.with_base();
    let status = base_status(&mut f.daemon, &f.repo, "main").expect("status");
    assert_eq!(status.pool_id, id);
    assert_eq!(
        status.snapshot,
        cowfs_treehouse::base_snapshot(&id).expect("base")
    );
    // The base was found through base.repo, not by name guessing.
    let found = f.daemon.find_base(&f.repo).expect("find");
    assert_eq!(found.map(|b| b.name), Some(status.snapshot));
}

#[test]
fn a_slot_path_outside_the_pool_layout_is_refused() {
    let _w = Watchdog::start(30);
    let dir = private_tempdir();
    let stub = stub_in(dir.path());
    let mut daemon = Daemon::connect(Some(&stub.socket), Some(5)).expect("connect");
    let materialiser = RecordingMaterialiser::default();
    // A path with no parent has no slot name to read, and that is a usage error.
    let err = Provision {
        daemon: &mut daemon,
        materialiser: &materialiser,
        slot_path: std::path::PathBuf::from("/"),
        pool_id: Some("pool-abcdef".into()),
        busy_timeout: cowfs_treehouse::DEFAULT_BUSY_TIMEOUT,
    }
    .run()
    .expect_err("no slot name");
    assert!(matches!(err, Error::Usage(_)), "{err:?}");
    assert!(err.to_string().contains("treehouse slot path"), "{err}");

    // A pool-shaped path with no base is a different, also explicit, failure.
    let shaped = dir.path().join(".treehouse/pool-abcdef/1/repo");
    std::fs::create_dir_all(&shaped).expect("mkdir");
    let err = Provision {
        daemon: &mut daemon,
        materialiser: &materialiser,
        slot_path: shaped,
        pool_id: None,
        busy_timeout: cowfs_treehouse::DEFAULT_BUSY_TIMEOUT,
    }
    .run()
    .expect_err("no base");
    assert!(matches!(err, Error::Unsupported(_)), "{err:?}");
    assert!(err.to_string().contains("no warm base"), "{err}");
}

#[test]
fn the_recording_materialiser_refuses_to_claim_it_can_mount() {
    let _w = Watchdog::start(30);
    let r = RecordingMaterialiser::default();
    assert!(!r.available());
    assert!(!CowfsMaterialiser.available());
    let err = CowfsMaterialiser
        .materialise("snap", std::path::Path::new("/slot"))
        .expect_err("the real one cannot yet");
    let msg = err.to_string();
    assert!(msg.contains("mount_snapshot"), "{msg}");
    assert!(msg.contains("gap 1"), "{msg}");
    assert_eq!(err.exit_code(), EXIT_ERROR);
}

#[test]
fn a_live_holder_process_is_terminated_only_with_force_and_only_when_signalable() {
    let _w = Watchdog::start(60);
    // A process that ignores SIGTERM, so the escalation to SIGKILL is exercised for real.
    let f = Fixture::spawn("trap", "trap '' TERM; exec sleep 300");
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(cowfs_treehouse::alive(f.pid));
    assert!(cowfs_treehouse::signalable(f.pid));

    let done = cowfs_treehouse::terminate(&[f.pid], std::time::Duration::from_millis(300));
    assert_eq!(done.signalled, vec![f.pid]);
    assert_eq!(
        done.killed,
        vec![f.pid],
        "a process that ignores SIGTERM must escalate to SIGKILL"
    );
    assert!(done.survivors.is_empty(), "{done:?}");
    assert!(!cowfs_treehouse::alive(f.pid));
}

#[test]
fn terminating_never_touches_this_process_or_its_ancestors() {
    let _w = Watchdog::start(30);
    let ancestry = cowfs_treehouse::protected_ancestry();
    assert!(ancestry.len() >= 2, "a test runner has an ancestry");
    for pid in &ancestry {
        assert!(!cowfs_treehouse::signalable(*pid));
    }
    let out = cowfs_treehouse::terminate(
        &ancestry.iter().copied().collect::<Vec<_>>(),
        cowfs_treehouse::GRACE,
    );
    assert!(out.signalled.is_empty(), "{out:?}");
    assert!(out.killed.is_empty(), "{out:?}");
    assert!(
        cowfs_treehouse::alive(std::process::id()),
        "we are still here"
    );
}

#[test]
fn a_slot_name_is_read_from_the_path_and_refused_when_it_is_not_a_component() {
    let _w = Watchdog::start(30);
    assert_eq!(slot_of(std::path::Path::new("/p/1/repo")), Some("1"));
    assert!(cowfs_treehouse::slot_snapshot("p", "..").is_err());
    assert!(cowfs_treehouse::slot_snapshot("p", "a/b").is_err());
    assert!(cowfs_treehouse::slot_snapshot("p", "").is_err());
    assert_eq!(
        slot_snapshot("p-abc123", "12").expect("name"),
        "p-abc123-12"
    );
}

#[test]
fn a_report_of_checks_drives_its_own_exit_code() {
    let _w = Watchdog::start(30);
    let mut r = cowfs_treehouse::Report::new();
    r.pass("mount type", "nfs");
    assert_eq!(r.exit_code(), EXIT_OK);
    r.fail("no .nfs* dirt", ".nfs.0001 in /pool/1/repo");
    assert_eq!(r.exit_code(), EXIT_ERROR);
    assert_eq!(r.failures(), 1);
}

/// A handler whose `swap` answers `busy` a fixed number of times and then behaves.
///
/// The control protocol has no wait-until-free, so the companion polls. This is the only way to
/// prove the poll actually retries: the stub handler has no way to remove a holder once added.
#[derive(Debug)]
struct FlakySwap {
    inner: StubHandler,
    remaining: std::sync::Mutex<u32>,
    calls: std::sync::atomic::AtomicU32,
}

impl FlakySwap {
    fn new(busy_for: u32) -> FlakySwap {
        FlakySwap {
            inner: StubHandler::new("store", "mnt"),
            remaining: Mutex::new(busy_for),
            calls: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn attempts(&self) -> u32 {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl cowfs_ctl::ControlHandler for FlakySwap {
    fn status(&self) -> cowfs_ctl::CtlResult<cowfs_ctl::Status> {
        self.inner.status()
    }
    fn snapshot_list(&self) -> cowfs_ctl::CtlResult<Vec<cowfs_ctl::SnapshotInfo>> {
        self.inner.snapshot_list()
    }
    fn snapshot_create(
        &self,
        params: cowfs_ctl::SnapshotCreate,
    ) -> cowfs_ctl::CtlResult<cowfs_ctl::SnapshotInfo> {
        self.inner.snapshot_create(params)
    }
    fn holders(&self, snapshot: &str) -> cowfs_ctl::CtlResult<Vec<cowfs_ctl::ProcessInfo>> {
        self.inner.holders(snapshot)
    }
    fn snapshot_rename(
        &self,
        from: &str,
        to: &str,
    ) -> cowfs_ctl::CtlResult<cowfs_ctl::SnapshotInfo> {
        self.inner.snapshot_rename(from, to)
    }
    fn base_refresh(
        &self,
        params: cowfs_ctl::BaseRefreshParams,
        ctx: &cowfs_ctl::OpContext<'_>,
    ) -> cowfs_ctl::CtlResult<cowfs_ctl::BaseRefreshReport> {
        self.inner.base_refresh(params, ctx)
    }
    fn mount_info(&self) -> cowfs_ctl::CtlResult<cowfs_ctl::MountInfo> {
        self.inner.mount_info()
    }
    fn import(
        &self,
        params: cowfs_ctl::ImportParams,
        ctx: &cowfs_ctl::OpContext<'_>,
    ) -> cowfs_ctl::CtlResult<cowfs_ctl::ImportReport> {
        self.inner.import(params, ctx)
    }
    fn swap(
        &self,
        name: &str,
        from: &str,
        guard: &cowfs_ctl::HolderGuard<'_>,
    ) -> cowfs_ctl::CtlResult<cowfs_ctl::SnapshotInfo> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut left = self
            .remaining
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *left > 0 {
            *left -= 1;
            return Err(cowfs_ctl::CtlError::new(
                cowfs_ctl::ErrorCode::Busy,
                format!("snapshot {name:?} is still busy"),
            ));
        }
        self.inner.swap(name, from, guard)
    }
    fn remove(&self, name: &str, guard: &cowfs_ctl::HolderGuard<'_>) -> cowfs_ctl::CtlResult<()> {
        self.inner.remove(name, guard)
    }
}

/// A busy snapshot that frees up inside the wait is retried, not failed.
#[test]
fn a_swap_that_is_busy_then_free_is_retried_until_it_succeeds() {
    let _w = Watchdog::start(60);
    let dir = private_tempdir();
    let sock_dir = dir.path().join("run");
    std::fs::create_dir_all(&sock_dir).expect("mkdir");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let socket = sock_dir.join("control.sock");
    let flaky = Arc::new(FlakySwap::new(3));
    let server = cowfs_ctl::Server::start(
        &socket,
        Arc::clone(&flaky) as Arc<dyn cowfs_ctl::ControlHandler>,
        cowfs_ctl::ServerOptions::default(),
    )
    .expect("server");
    let mut daemon = Daemon::connect(Some(&socket), Some(5)).expect("connect");

    daemon.snapshot_create("base", None).expect("create base");
    daemon.snapshot_create("slot", None).expect("create slot");

    let started = std::time::Instant::now();
    let info = daemon
        .snapshot_reset_wait("slot", "base", std::time::Duration::from_secs(10))
        .expect("the retry succeeds once the holder is gone");
    assert_eq!(info.parent.as_deref(), Some("base"));
    assert_eq!(flaky.attempts(), 4, "one attempt plus three retries");
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(200),
        "it really waited between attempts: {:?}",
        started.elapsed()
    );
    server.shutdown();
}

/// A snapshot that stays busy fails with `busy` when the wait runs out, never looping forever.
#[test]
fn a_swap_that_stays_busy_fails_within_its_bounded_wait() {
    let _w = Watchdog::start(60);
    let dir = private_tempdir();
    let sock_dir = dir.path().join("run");
    std::fs::create_dir_all(&sock_dir).expect("mkdir");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&sock_dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    let socket = sock_dir.join("control.sock");
    let flaky = Arc::new(FlakySwap::new(u32::MAX));
    let server = cowfs_ctl::Server::start(
        &socket,
        Arc::clone(&flaky) as Arc<dyn cowfs_ctl::ControlHandler>,
        cowfs_ctl::ServerOptions::default(),
    )
    .expect("server");
    let mut daemon = Daemon::connect(Some(&socket), Some(5)).expect("connect");
    daemon.snapshot_create("base", None).expect("create base");
    daemon.snapshot_create("slot", None).expect("create slot");

    let err = daemon
        .snapshot_reset_wait("slot", "base", std::time::Duration::from_millis(300))
        .expect_err("never frees");
    assert_eq!(err.exit_code(), EXIT_BUSY, "{err}");
    assert!(err.to_string().contains("still held"), "{err}");
    assert!(
        flaky.attempts() > 1,
        "it really retried: {}",
        flaky.attempts()
    );
    server.shutdown();
}

/// A non-busy failure is returned at once: retrying it cannot help.
#[test]
fn a_non_busy_failure_is_not_retried() {
    let _w = Watchdog::start(60);
    let mut daemon_calls = 0u32;
    let err = cowfs_treehouse::poll_busy(std::time::Duration::from_secs(5), || {
        daemon_calls += 1;
        Err::<(), _>(Error::Cowfs("not_found: snapshot \"nope\"".into()))
    })
    .expect_err("must fail");
    assert_eq!(daemon_calls, 1, "one attempt only, {daemon_calls}");
    assert_eq!(err.exit_code(), EXIT_ERROR, "{err}");
    assert!(err.to_string().contains("not_found"), "{err}");
}
