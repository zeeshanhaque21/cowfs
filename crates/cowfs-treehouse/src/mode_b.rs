use cowfs_ctl::SnapshotInfo;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::ctl::Daemon;
use crate::error::{Error, Result};
use crate::naming;
use crate::th::Treehouse;

/// How a snapshot is made to appear at a slot's path.
///
/// This is the one thing mode (b) needs from the mount that the companion does not drive yet: the
/// daemon and `cowfs-ctl` have `mount_snapshot` and `unmount_snapshot`, but nothing here calls them.
/// The seam keeps everything else testable today and names the missing wiring exactly once. See
/// `docs/v1-treehouse.md`, gap 1.
pub trait Materialiser {
    /// Makes `snapshot` the content of `path`, which must be a directory treehouse has just
    /// created and is about to use as a git worktree.
    fn materialise(&self, snapshot: &str, path: &Path) -> Result<()>;
    /// Whether this materialiser really does it, so callers can refuse to pretend.
    fn available(&self) -> bool {
        true
    }
}

/// The real one, not wired yet: it would call the daemon's `mount_snapshot`. That needs
/// `Provision::run` to create the snapshot before mounting, the slot under `--export-root`, and an
/// owner that calls `unmount_snapshot` when the slot is returned.
#[derive(Clone, Copy, Debug, Default)]
pub struct CowfsMaterialiser;

impl Materialiser for CowfsMaterialiser {
    fn materialise(&self, snapshot: &str, path: &Path) -> Result<()> {
        Err(Error::Unsupported(format!(
            "this daemon cannot make snapshot {snapshot:?} appear at {}: the \
             companion does not call the daemon's mount_snapshot yet (docs/v1-treehouse.md, gap 1): Provision::run must create \
             the snapshot first, the slot must live under --export-root, and something must own \
             unmount_snapshot at return. The rest of mode (b) is implemented and tested; this call \
             is the only thing waiting on that wiring.",
            path.display()
        )))
    }

    fn available(&self) -> bool {
        false
    }
}

/// A materialiser that only records what it was asked for, so the snapshot half of mode (b) can be
/// exercised against the stub daemon on a native directory.
#[derive(Clone, Debug, Default)]
pub struct RecordingMaterialiser {
    /// Every `(snapshot, path)` pair it was asked for, in order.
    pub calls: std::cell::RefCell<Vec<(String, PathBuf)>>,
}

impl Materialiser for RecordingMaterialiser {
    fn materialise(&self, snapshot: &str, path: &Path) -> Result<()> {
        self.calls
            .borrow_mut()
            .push((snapshot.to_owned(), path.to_path_buf()));
        Ok(())
    }

    fn available(&self) -> bool {
        false
    }
}

/// What a slot acquisition produced.
#[derive(Clone, Debug, Serialize)]
pub struct Acquired {
    /// The pool id, which is also the treehouse pool directory name.
    pub pool_id: String,
    /// The treehouse slot name.
    pub slot: String,
    /// The worktree path treehouse handed out.
    pub path: PathBuf,
    /// The snapshot now behind that path.
    pub snapshot: String,
    /// The warm base the slot was cloned from.
    pub base: String,
    /// The lease identity, for a pinned return.
    pub lease_id: String,
    /// False when `create` made the snapshot and true when `reset` replaced an existing one.
    pub reused: bool,
    /// True when the slot's `.git` file was rewritten to point at its own worktree bookkeeping.
    pub gitdir_rewritten: bool,
}

/// The `post_create` entry point and the body of `get`: makes the slot's snapshot an O(1) clone
/// of the repository's warm base, then points the slot's `.git` file back at the bookkeeping
/// `git worktree add` already wrote.
///
/// Every step is idempotent, so an interrupted run converges on the next one: the create-or-reset
/// choice depends only on whether the snapshot exists, and the `.git` rewrite is a single small
/// write whose result is checked.
pub struct Provision<'a> {
    /// The daemon.
    pub daemon: &'a mut Daemon,
    /// How a snapshot reaches a path.
    pub materialiser: &'a dyn Materialiser,
    /// The slot directory treehouse created.
    pub slot_path: PathBuf,
    /// Force the pool id instead of deriving it, for a caller that already knows it.
    pub pool_id: Option<String>,
    /// How long a busy snapshot is retried, default [`crate::ctl::DEFAULT_BUSY_TIMEOUT`].
    pub busy_timeout: Duration,
}

impl std::fmt::Debug for Provision<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Provision")
            .field("slot_path", &self.slot_path)
            .field("pool_id", &self.pool_id)
            .field("materialiser", &self.materialiser.available())
            .finish_non_exhaustive()
    }
}

impl Provision<'_> {
    /// Runs the flow and reports what happened.
    pub fn run(&mut self) -> Result<Acquired> {
        let slot = naming::slot_of(&self.slot_path)
            .ok_or_else(|| {
                Error::Usage(format!(
                    "{} is not a treehouse slot path, which is {{pool}}/{{slot}}/{{repo}}",
                    self.slot_path.display()
                ))
            })?
            .to_owned();
        // Identity comes from the pool directory first, because the pool directory *is* the pool
        // id and that needs no git at all. Only a slot outside a pool falls back to the repository,
        // and a run that crashed between the reset and the `.git` repair has a broken link, so
        // anything derived from git would be unavailable exactly when it is most needed.
        let pool_id = match &self.pool_id {
            Some(id) => id.clone(),
            None => match naming::pool_id_of_slot_path(&self.slot_path) {
                Some(id) => id,
                None => naming::pool_id(&naming::main_repo_root(&self.slot_path)?)?,
            },
        };
        let snapshot = naming::slot_snapshot(&pool_id, &slot)?;
        let base = naming::base_snapshot(&pool_id)?;

        // Resolved before the swap, while the slot's own `.git` link is still valid. After the
        // reset that link belongs to the base, so git can no longer say where this slot's
        // bookkeeping is and a repair that asked then would be guessing.
        let git_dir = worktree_git_dir(&self.slot_path).ok();
        let repo = git_dir
            .as_deref()
            .and_then(|d| d.parent())
            .and_then(|d| d.parent())
            .and_then(|d| d.parent())
            .map(Path::to_path_buf);

        self.require_base(&base, repo.as_deref())?;

        let existing = self.snapshot_exists(&snapshot)?;
        if existing {
            // A recycled slot's previous holder may still be exiting, so a busy is retried rather
            // than failed on.
            self.daemon
                .snapshot_reset_wait(&snapshot, &base, self.busy_timeout)?;
        } else {
            self.materialiser.materialise(&snapshot, &self.slot_path)?;
            self.daemon.snapshot_create(&snapshot, Some(&base))?;
        }
        let gitdir_rewritten = match &git_dir {
            Some(dir) => rewrite_git_link(&self.slot_path.join(".git"), dir)?,
            // A slot whose git metadata does not resolve is not a worktree this flow understands,
            // so it is reported as untouched rather than guessed at.
            None => false,
        };
        Ok(Acquired {
            pool_id,
            slot,
            path: self.slot_path.clone(),
            snapshot,
            base,
            lease_id: String::new(),
            reused: existing,
            gitdir_rewritten,
        })
    }

    /// Whether the slot's snapshot already exists, which decides create-or-reset.
    fn snapshot_exists(&mut self, name: &str) -> Result<bool> {
        Ok(self.daemon.snapshot_list()?.iter().any(|s| s.name == name))
    }

    /// Checks the warm base is there before anything is changed.
    ///
    /// The name is derived from the pool id, so it is unambiguous on a mount shared by every
    /// repository. When the repository also resolved, `base.repo` is checked as well, which is the
    /// lookup the control API documents; when it did not, the name alone has to be enough, because
    /// that is the rerun-after-a-crash case.
    fn require_base(&mut self, base: &str, repo: Option<&Path>) -> Result<()> {
        let wanted = repo.map(naming::canonical);
        let found = self.daemon.snapshot_list()?.into_iter().any(|s| {
            if s.name != base {
                return false;
            }
            match (&wanted, s.base.as_ref().and_then(|b| b.repo.as_deref())) {
                (Some(want), Some(got)) => naming::canonical(Path::new(got)) == *want,
                // A promoted base has all-null fields, so the name is all it carries.
                (Some(_), None) => true,
                // The slot's git metadata is unusable, so the name is all there is.
                (None, _) => true,
            }
        });
        if found {
            return Ok(());
        }
        Err(Error::Unsupported(format!(
            "no warm base for this repository: run `cowfs-treehouse base refresh` first, \
             expected snapshot {base:?}"
        )))
    }
}

/// What to do with a slot's `.git` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitLink {
    /// It is a real repository, not a worktree link. Overwriting it would destroy a checkout, so
    /// it is left exactly as it is.
    LeaveDirectory,
    /// It already points where it should.
    AlreadyCorrect,
    /// It must be replaced with this content.
    Write(String),
}

/// Decides what to do with a slot's `.git`, as a pure decision so every shape can be tested
/// without a repository. `dir` is where `git worktree add` put this slot's bookkeeping.
pub fn plan_git_link(dot_git: &Path, dir: &Path) -> GitLink {
    if dot_git.is_dir() {
        return GitLink::LeaveDirectory;
    }
    // Exactly the format `git worktree add` writes, so a slot git has just set up reads as already
    // correct and no write happens.
    let wanted = format!("gitdir: {}\n", dir.display());
    match std::fs::read_to_string(dot_git) {
        Ok(current) if current == wanted => GitLink::AlreadyCorrect,
        _ => GitLink::Write(wanted),
    }
}

/// Applies [`plan_git_link`], reporting whether a write happened.
pub fn rewrite_git_link(dot_git: &Path, dir: &Path) -> Result<bool> {
    match plan_git_link(dot_git, dir) {
        GitLink::AlreadyCorrect | GitLink::LeaveDirectory => Ok(false),
        GitLink::Write(content) => std::fs::write(dot_git, content)
            .map_err(|e| Error::Io(format!("cannot write {}: {e}", dot_git.display())))
            .map(|()| true),
    }
}
/// `{main}/.git/worktrees/{leaf}`, the directory git keeps a linked worktree's state in.
///
/// The leaf is the last component of the slot path, not the slot name: `git worktree add` names the
/// bookkeeping directory after the worktree's own directory name, which for a treehouse slot is
/// `{repo}`, or `{repo}-{slot}` when `unique_leaf` is on. Using the slot name here would point the
/// slot at bookkeeping git never created.
pub fn worktree_git_dir(slot_path: &Path) -> Result<PathBuf> {
    let leaf = slot_path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| {
            Error::Usage(format!(
                "{} has no directory name for git to have named the worktree after",
                slot_path.display()
            ))
        })?;
    let main = naming::main_repo_root(slot_path)?;
    let common = git_common_dir(&main)?;
    Ok(common.join("worktrees").join(leaf))
}

fn git_common_dir(repo_root: &Path) -> Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(repo_root)
        .output()
        .map_err(|e| Error::Io(format!("cannot run git rev-parse: {e}")))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "git rev-parse --git-common-dir failed in {}",
            repo_root.display()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if text.is_empty() {
        return Err(Error::Io(
            "git rev-parse --git-common-dir printed nothing".to_owned(),
        ));
    }
    Ok(PathBuf::from(text))
}

/// A lease that comes back unless it is deliberately kept.
///
/// A pool slot is a scarce, shared resource: on the real cowfs pool `max_trees` is 12 and eleven
/// other agents hold slots, so every early return, failed build and missing daemon must give the
/// slot back rather than burn it. The guard returns it on drop and is disarmed only on success.
#[derive(Debug)]
pub struct LeaseGuard<'a> {
    treehouse: &'a Treehouse,
    slot: PathBuf,
    lease_id: String,
    armed: bool,
}

impl<'a> LeaseGuard<'a> {
    /// Leases a slot and arms the guard.
    pub fn acquire(treehouse: &'a Treehouse, repo: &Path) -> Result<LeaseGuard<'a>> {
        let lease = treehouse.get_lease(repo, &[])?;
        Ok(LeaseGuard {
            treehouse,
            slot: lease.path,
            lease_id: lease.lease_id,
            armed: true,
        })
    }

    /// The leased worktree path.
    pub fn slot(&self) -> &Path {
        &self.slot
    }

    /// The lease identity, which is what the release is pinned to.
    pub fn lease_id(&self) -> &str {
        &self.lease_id
    }

    /// Keeps the lease: the caller is taking the slot and will release it itself.
    pub fn keep(&mut self) {
        self.armed = false;
    }
}

impl Drop for LeaseGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Best effort: the caller's own error is the one that matters, and a second attempt would
        // fail the same way. Never panics out of a drop.
        if let Err(e) = self
            .treehouse
            .return_slot(&self.slot, true, self.lease_id.as_str())
        {
            eprintln!(
                "cowfs-treehouse: could not return the leased slot {}: {e}",
                self.slot.display()
            );
        }
    }
}

/// Acquires a slot and provisions it, which is `cowfs-treehouse get`.
pub fn get(
    daemon: &mut Daemon,
    th: &Treehouse,
    materialiser: &dyn Materialiser,
    repo: &Path,
    extra_treehouse: &[String],
) -> Result<Acquired> {
    let main = naming::main_repo_root(repo)?;
    let extra: Vec<&str> = extra_treehouse.iter().map(String::as_str).collect();
    let lease = th.get_lease(&main, &extra)?;
    let lease_id = lease.lease_id.clone();
    let slot_path = lease.path.clone();
    // The materialiser refuses today (gap 1), so without this every mode (b) attempt would burn a
    // pool slot. The caller still gets the original failure.
    let mut guard = LeaseGuard {
        treehouse: th,
        slot: lease.path.clone(),
        lease_id: lease.lease_id.clone(),
        armed: true,
    };
    let mut out = Provision {
        daemon,
        materialiser,
        slot_path,
        pool_id: None,
        busy_timeout: crate::ctl::DEFAULT_BUSY_TIMEOUT,
    }
    .run()?;
    guard.keep();
    out.lease_id = lease_id;
    Ok(out)
}

/// How fresh a warm base is against a git ref.
#[derive(Clone, Debug, Serialize)]
pub struct BaseStatus {
    /// The pool id.
    pub pool_id: String,
    /// The warm base snapshot name.
    pub snapshot: String,
    /// The commit the base was built from, when the daemon recorded one.
    pub base_commit: Option<String>,
    /// The commit the ref points at now.
    pub head_commit: Option<String>,
    /// False when the base is missing, unrecorded, or behind.
    pub fresh: bool,
    /// One line saying why, for a human.
    pub reason: String,
}

/// Compares the base's recorded commit against the ref, which is the only staleness signal the
/// control API exposes. A daemon that does not record a real commit can never be fresh, and says
/// so rather than claiming a base is good.
pub fn base_status(daemon: &mut Daemon, repo: &Path, git_ref: &str) -> Result<BaseStatus> {
    let main = naming::main_repo_root(repo)?;
    let pool_id = naming::pool_id(&main)?;
    let snapshot = naming::base_snapshot(&pool_id)?;
    let head = naming::resolve_commit(&main, git_ref);
    let base = daemon.find_base(&main)?;
    let base_commit = base
        .as_ref()
        .and_then(|b| b.base.as_ref())
        .and_then(|b| b.commit.clone());
    let (fresh, reason) = match (&base, &base_commit, &head) {
        (None, _, _) => (
            false,
            format!("no warm base {snapshot} for this repository"),
        ),
        (Some(_), None, _) => (
            false,
            "the daemon recorded no commit for the base, so freshness cannot be decided".to_owned(),
        ),
        (Some(_), Some(b), None) => (
            false,
            format!(
                "git ref {git_ref:?} does not resolve in the repository, and the base is at {b}"
            ),
        ),
        (Some(_), Some(b), Some(h)) if b == h => (true, format!("the base is at {h}")),
        (Some(_), Some(b), Some(h)) => (
            false,
            format!("the base is at {b} and {git_ref} is at {h}, so it is stale"),
        ),
    };
    Ok(BaseStatus {
        pool_id,
        snapshot,
        base_commit,
        head_commit: head,
        fresh,
        reason,
    })
}

/// What a base refresh did.
#[derive(Clone, Debug, Serialize)]
pub struct BaseRefreshed {
    /// The pool id.
    pub pool_id: String,
    /// The warm base snapshot name.
    pub snapshot: String,
    /// The ref it was built from.
    pub git_ref: String,
    /// The commit the base records now.
    pub commit: Option<String>,
    /// The commit the base recorded before, when there was one.
    pub previous_commit: Option<String>,
    /// True when the refresh command was run in a leased slot first.
    pub built_in_slot: bool,
    /// The slot it ran in, when it did.
    pub slot: Option<PathBuf>,
}

/// Refreshes the warm base, optionally running the repository's own build in a real treehouse slot
/// first so the artifacts the daemon snapshots are ones the build actually produced.
///
/// Refuses any attempt to inject per-slot compiler flags: spike 6 measured that a warm `target/`
/// built with a slot-specific `--remap-path-prefix` recompiles every unit, because `RUSTFLAGS` is
/// part of cargo's fingerprint. A base built that way would dirty every slot it is cloned into.
pub struct BaseRefresh<'a> {
    /// The daemon.
    pub daemon: &'a mut Daemon,
    /// The treehouse binary, for the optional build slot.
    pub treehouse: Option<&'a Treehouse>,
    /// The main checkout.
    pub repo: PathBuf,
    /// The ref to build from.
    pub git_ref: String,
    /// The build command, run in a leased slot before the refresh.
    pub build: Option<String>,
    /// An already-leased slot to build in, instead of acquiring one.
    pub slot: Option<PathBuf>,
    /// The pool's canonical build path. `None` builds at the slot's own path, as before.
    pub canonical: Option<Canonical>,
}

impl std::fmt::Debug for BaseRefresh<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BaseRefresh")
            .field("repo", &self.repo)
            .field("git_ref", &self.git_ref)
            .field("build", &self.build)
            .field("slot", &self.slot)
            .field("canonical", &self.canonical)
            .finish_non_exhaustive()
    }
}

impl BaseRefresh<'_> {
    /// Runs the flow.
    pub fn run(&mut self) -> Result<BaseRefreshed> {
        let main = naming::main_repo_root(&self.repo)?;
        let pool_id = naming::pool_id(&main)?;
        let snapshot = naming::base_snapshot(&pool_id)?;

        let (built_in_slot, slot) = match (&self.build, &self.slot) {
            (None, _) => (false, None),
            (Some(_), Some(p)) => {
                run_build(
                    p,
                    self.build.as_deref().unwrap_or_default(),
                    self.canonical.as_ref(),
                )?;
                (true, Some(p.clone()))
            }
            (Some(_), None) => {
                let th = self.treehouse.ok_or_else(|| {
                    Error::Usage(
                        "a build needs a treehouse root so a slot can be leased".to_owned(),
                    )
                })?;
                let mut guard = LeaseGuard::acquire(th, &main)?;
                let slot = guard.slot().to_path_buf();
                // The `?` runs while the guard is still armed, so a failed build hands the slot
                // back on the way out instead of burning one of the pool's.
                run_build(
                    &slot,
                    self.build.as_deref().unwrap_or_default(),
                    self.canonical.as_ref(),
                )?;
                guard.keep();
                (true, Some(slot))
            }
        };

        let report = self
            .daemon
            .base_refresh(&main, &self.git_ref, Some(&snapshot))?;
        // Idempotent, and it is what makes the base visible as a base rather than a clone.
        self.daemon.snapshot_promote(&snapshot)?;
        Ok(BaseRefreshed {
            pool_id,
            snapshot,
            git_ref: self.git_ref.clone(),
            commit: report.snapshot.base.as_ref().and_then(|b| b.commit.clone()),
            previous_commit: report.previous_commit,
            built_in_slot,
            slot,
        })
    }
}

/// Where a build should appear, when the pool owner has opted into canonical paths.
///
/// Opt-in and Linux-only. Absent, the build runs at the slot's own path exactly as before, so
/// existing macOS and Linux behaviour and every existing config file keep working unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Canonical {
    /// The path every build of this pool appears at. Must already exist and be a directory: the
    /// companion never creates it, because a global path is the pool owner's decision.
    pub dir: PathBuf,
    /// `scripts/cowfs-ns-run.sh`. Explicit rather than searched for, so a distributed binary does
    /// not depend on the caller's working directory.
    pub helper: PathBuf,
}

impl Canonical {
    /// Checks what can be checked without running anything.
    pub fn validate(&self) -> Result<()> {
        // The path checks come first: a relative or missing path is a usage error on every
        // platform, and reporting that is more useful than reporting the platform.
        if !self.dir.is_absolute() {
            return Err(Error::Usage(format!(
                "--canonical must be an absolute path, not: {}",
                self.dir.display()
            )));
        }
        if !self.dir.is_dir() {
            return Err(Error::Usage(format!(
                "--canonical {} must already exist as a directory the pool owner created. This \
                 companion will not create it, and will not guess a system path for one",
                self.dir.display()
            )));
        }
        if !self.helper.is_file() {
            return Err(Error::Usage(format!(
                "--ns-helper {} is not a file. Pass the path to scripts/cowfs-ns-run.sh, so the \
                 helper is found explicitly rather than through the working directory",
                self.helper.display()
            )));
        }
        if !cfg!(target_os = "linux") {
            return Err(Error::Unsupported(
                "canonical build paths need Linux mount namespaces. macOS normalises embedded \
                 paths with compiler flags such as --remap-path-prefix instead, and the design \
                 keeps that. Drop --canonical to build at the slot's own path"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    /// The arguments for a build, without the program name and without running anything.
    ///
    /// The helper is the program, so it is not repeated here: `Command::new` already supplies it,
    /// and passing it again would make the helper read its own path as the first argument.
    ///
    /// `sh -c` is kept for the build command itself, because that command is user configuration and
    /// has always been a shell string. It is passed as one argv element, so nothing in it is ever
    /// re-split, and the canonical directory is a separate argv element rather than part of it.
    pub fn args(&self, dir: &Path, command: &str) -> Vec<std::ffi::OsString> {
        vec![
            "--src".into(),
            dir.as_os_str().to_owned(),
            "--canonical".into(),
            self.dir.as_os_str().to_owned(),
            "--".into(),
            "/bin/sh".into(),
            "-c".into(),
            command.into(),
        ]
    }
}

impl Canonical {
    /// The command that runs a build through the helper, without running it.
    ///
    /// Cargo's incremental state is the one thing a canonical path cannot make reproducible: it
    /// holds per-session random names and bytes (issue 171, measured in
    /// `docs/verification/evidence/cargo171.md`). The route therefore sets `CARGO_INCREMENTAL=0`,
    /// overriding any value the caller set. Only this route sets it, so a build at the slot's own
    /// path is untouched.
    pub fn build_command(&self, dir: &Path, command: &str) -> Command {
        let mut child = Command::new(&self.helper);
        child
            .args(self.args(dir, command))
            .current_dir(dir)
            .env("CARGO_INCREMENTAL", "0");
        child
    }
}

/// Runs a build command in `dir`.
///
/// With a `canonical`, the command runs inside a mount namespace where `dir` also appears at the
/// pool's canonical path, so artifacts that embed an absolute path come out identical across slots.
/// Without one, the command runs at `dir` itself, which is what every existing caller gets.
pub fn run_build(dir: &Path, command: &str, canonical: Option<&Canonical>) -> Result<()> {
    let mut cmd = match canonical {
        None => {
            let mut c = Command::new("/bin/sh");
            c.arg("-c").arg(command).current_dir(dir);
            c
        }
        Some(c) => {
            c.validate()?;
            // One exit code, two meanings: the helper forwards the command's own code, and it also
            // uses 77 for its own refusal. A probe with a command that must succeed tells them
            // apart, so a namespace this host cannot create is reported as unmeasurable and a real
            // build failure stays a failure. Never the other way round.
            let probe = Command::new(&c.helper)
                .args(c.args(dir, "exit 0"))
                .output()
                .map_err(|e| {
                    Error::Io(format!(
                        "cannot run the namespace helper {}: {e}",
                        c.helper.display()
                    ))
                })?;
            if !probe.status.success() {
                return Err(Error::Unsupported(format!(
                    "UNMEASURABLE: no mount namespace on this host, so the build did not run. {}",
                    crate::th::tail(&String::from_utf8_lossy(&probe.stderr))
                )));
            }
            c.build_command(dir, command)
        }
    };
    let status = cmd
        .status()
        .map_err(|e| Error::Io(format!("cannot run the build command: {e}")))?;
    if !status.success() {
        return Err(Error::Io(format!(
            "the build command failed in {} with {status}",
            dir.display()
        )));
    }
    Ok(())
}

/// Options of `cowfs-treehouse base promote`.
#[derive(Clone, Debug)]
pub struct PromoteOptions {
    /// The snapshot to promote.
    pub snapshot: String,
    /// Verify the snapshot exists first.
    pub dry_run: bool,
}

/// Promotes a snapshot to a base, which is always explicit.
pub fn base_promote(daemon: &mut Daemon, opts: &PromoteOptions) -> Result<SnapshotInfo> {
    if opts.dry_run {
        return daemon
            .snapshot_list()?
            .into_iter()
            .find(|s| s.name == opts.snapshot)
            .ok_or_else(|| Error::Cowfs(format!("snapshot {:?} does not exist", opts.snapshot)));
    }
    daemon.snapshot_promote(&opts.snapshot)
}

/// Installs the `post_create` hook into the treehouse user config, which is the only place
/// treehouse reads hooks from.
///
/// Idempotent, and it never overwrites a `post_create` that is not its own: a hook the operator
/// wrote is theirs, and silently replacing it is how a build step disappears without a trace.
pub const HOOK_SENTINEL: &str = "# managed by cowfs-treehouse hooks install";

/// The path treehouse reads user-level hooks from.
pub fn user_config_path(home: &Path) -> PathBuf {
    home.join(".config/treehouse/config.toml")
}

/// `user_config_path` as a free function, so a caller outside this crate can find the file
/// `hooks install` writes without reaching into the module.
pub fn user_config_path_for(home: &Path) -> PathBuf {
    user_config_path(home)
}

/// What `hooks install` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookAction {
    /// The hook was already installed and correct.
    AlreadyThere,
    /// The hook was added.
    Added,
    /// A `post_create` belonging to somebody else is there, so nothing was written.
    Refused(String),
}

/// Adds `cowfs-treehouse provision` as the `post_create` hook.
pub fn hooks_install(home: &Path, provision_command: &str) -> Result<HookAction> {
    let path = user_config_path(home);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.contains(HOOK_SENTINEL) {
        if existing.contains(provision_command) {
            return Ok(HookAction::AlreadyThere);
        }
        return Ok(HookAction::Refused(format!(
            "{} carries the cowfs-treehouse sentinel but a different command; edit it by hand",
            path.display()
        )));
    }
    if let Some(other) = foreign_post_create(&existing) {
        return Ok(HookAction::Refused(format!(
            "{} already declares post_create = {other:?}; add `{}` to that list by hand",
            path.display(),
            provision_command
        )));
    }
    let block = format!("\n[hooks]\n{HOOK_SENTINEL}\npost_create = [\"{provision_command}\"]\n");
    let mut next = existing;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(&block);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Io(format!("cannot create {}: {e}", parent.display())))?;
    }
    std::fs::write(&path, next)
        .map_err(|e| Error::Io(format!("cannot write {}: {e}", path.display())))?;
    Ok(HookAction::Added)
}

/// The first `post_create` entry in a config, when it is not ours.
fn foreign_post_create(config: &str) -> Option<String> {
    let mut in_hooks = false;
    for line in config.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_hooks = t == "[hooks]";
            continue;
        }
        if !in_hooks {
            continue;
        }
        if let Some(rest) = t.strip_prefix("post_create") {
            return Some(rest.trim().to_owned());
        }
    }
    None
}

/// How long a flow waits for `.nfs*` dirt by default.
pub const DEFAULT_NFS_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a flow allows treehouse itself, by default.
pub const DEFAULT_TREEHOUSE_TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_install_is_idempotent() {
        let home = tempfile::tempdir().expect("tempdir");
        let first = hooks_install(home.path(), "cowfs-treehouse provision").expect("first");
        assert_eq!(first, HookAction::Added);
        let second = hooks_install(home.path(), "cowfs-treehouse provision").expect("second");
        assert_eq!(second, HookAction::AlreadyThere);
        let text = std::fs::read_to_string(user_config_path(home.path())).expect("read");
        assert_eq!(text.matches("post_create").count(), 1, "{text}");
    }

    #[test]
    fn hooks_install_refuses_a_foreign_post_create() {
        let home = tempfile::tempdir().expect("tempdir");
        let path = user_config_path(home.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "[hooks]\npost_create = [\"./setup.sh\"]\n").expect("write");
        let action = hooks_install(home.path(), "cowfs-treehouse provision").expect("refused");
        assert!(matches!(action, HookAction::Refused(_)), "{action:?}");
        let text = std::fs::read_to_string(&path).expect("read");
        assert_eq!(text, "[hooks]\npost_create = [\"./setup.sh\"]\n");
    }

    #[test]
    fn hooks_install_appends_to_a_config_without_hooks() {
        let home = tempfile::tempdir().expect("tempdir");
        let path = user_config_path(home.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, "max_trees = 16").expect("write");
        hooks_install(home.path(), "cowfs-treehouse provision").expect("added");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.starts_with("max_trees = 16\n"), "{text}");
        assert!(text.contains("[hooks]"), "{text}");
        assert!(text.contains(HOOK_SENTINEL), "{text}");
    }

    #[test]
    fn a_git_directory_is_left_alone_because_it_is_a_real_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        let slot = dir.path().join("slot");
        std::fs::create_dir_all(slot.join(".git/objects")).expect("mkdir .git");
        assert_eq!(
            plan_git_link(&slot.join(".git"), &dir.path().join("worktrees/repo")),
            GitLink::LeaveDirectory
        );
        assert!(slot.join(".git/objects").is_dir());
    }

    #[test]
    fn a_git_file_that_is_not_a_link_is_replaced_and_one_that_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let setup = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .status()
                .expect("git")
                .success());
        };
        setup(&["init", "-q", "."]);
        setup(&["config", "user.email", "t@example.invalid"]);
        setup(&["config", "user.name", "t"]);
        std::fs::write(dir.path().join("a"), b"x").expect("write");
        setup(&["add", "-A"]);
        setup(&["commit", "-qm", "i"]);
        setup(&["branch", "-M", "main"]);
        let wt = dir.path().join("wt");
        setup(&[
            "worktree",
            "add",
            "--detach",
            "-q",
            &wt.display().to_string(),
            "main",
        ]);

        // Untouched: git already wrote the link, so nothing is written.
        let want = worktree_git_dir(&wt).expect("git dir");
        let as_git_wrote = std::fs::read_to_string(wt.join(".git")).expect("read .git");
        assert_eq!(
            plan_git_link(&wt.join(".git"), &want),
            GitLink::AlreadyCorrect
        );
        assert_eq!(as_git_wrote, format!("gitdir: {}\n", want.display()));
        assert!(want.join("gitdir").is_file(), "git created the bookkeeping");

        // What a reset leaves behind, which is the case that has to be repaired.
        std::fs::write(wt.join(".git"), "gitdir: /elsewhere/.git/worktrees/other\n")
            .expect("write .git");
        let GitLink::Write(content) = plan_git_link(&wt.join(".git"), &want) else {
            panic!("a wrong link must be replaced");
        };
        assert_eq!(content, as_git_wrote, "the repair restores what git wrote");
        assert!(wt.join(".git").is_file());
    }

    #[test]
    fn a_foreign_post_create_outside_the_hooks_table_is_not_mistaken_for_one() {
        assert!(foreign_post_create("post_create = [\"x\"]\n").is_none());
        assert!(foreign_post_create("[other]\npost_create = [\"x\"]\n").is_none());
        // The trailing `=` is kept so the refusal message reads like the config line it quotes.
        assert_eq!(
            foreign_post_create("[hooks]\npost_create = [\"x\"]\n").as_deref(),
            Some("= [\"x\"]")
        );
    }
}
