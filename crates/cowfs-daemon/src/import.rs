//! `import` and `base_refresh` on a passthrough backend.
//!
//! `import` follows the migration rules in `docs/design.md`: copy the tree in, re-read the
//! source, verify both with `cowfs_ctl::hash_tree`, and only then report success. The report
//! carries both root hashes, so a caller can hash the source itself and compare before it
//! swaps a directory for the mount.
//!
//! `base_refresh` builds the warm base for a repository at a git ref. A passthrough backend
//! has no store to deduplicate against, so the base is a checkout of the ref with the real
//! commit recorded: a synthetic commit would make staleness detection compare nothing.

use crate::backend::{Backend, Snapshots};
use cowfs_ctl::{
    hash_tree, BaseMeta, BaseRefreshParams, BaseRefreshReport, CtlError, CtlResult, ErrorCode,
    ImportMismatch, ImportParams, ImportReport, OpContext, ProgressEvent, Unit, MAX_MISMATCHES,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How often a long import reports progress. The protocol says at most about twenty a second.
const PROGRESS_EVERY: Duration = Duration::from_millis(50);

/// The base snapshot name for a repository, derived and never configurable. The treehouse
/// pool id disambiguates repositories of the same name and this does not, so a repository
/// whose basename is shared with another one on the same store passes `name` explicitly.
pub fn base_name(repo: &str) -> String {
    format!("{}-base", basename(repo))
}

fn basename(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .find(|c| !c.is_empty())
        .unwrap_or("repo")
        .to_owned()
}

/// The commit `repo` has at `git_ref`, or `None` when it cannot be read.
fn git_commit(repo: &str, git_ref: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args([
            "-C",
            repo,
            "rev-parse",
            "--verify",
            &format!("{git_ref}^{{commit}}"),
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|c| c.len() == 40 || c.len() == 64)
}

/// Runs `git -C repo worktree ...` and fails on a nonzero exit.
///
/// It returns nothing. The caller knows the checkout path, because it chose it and passes it as an
/// argument: git prints a human progress line on stdout (`HEAD is now at ...` on 2.39,
/// `Preparing worktree ...` on 2.56), never a documented path, so parsing stdout for one is a
/// version guess. It also used to be read as a path when it was not, which is how a repository ended
/// up holding a directory named after a commit.
fn git_worktree(repo: &Path, args: &[&str], what: &str) -> CtlResult<()> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| CtlError::new(ErrorCode::IoError, format!("git worktree {what}: {e}")))?;
    if !out.status.success() {
        return Err(CtlError::new(
            ErrorCode::IoError,
            format!(
                "git worktree {what} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        ));
    }
    Ok(())
}

/// A checkout path this process owns, for one `base_refresh` attempt.
///
/// It goes beside the repository, never inside it: git's own metadata lives in the repository, and
/// a stray directory there is what this is avoiding. The name carries the pid and the commit, so two
/// concurrent refreshes of one repository cannot collide and a leftover from a killed process is
/// recognisable rather than anonymous.
fn staging_path(repo: &Path, commit: &str) -> CtlResult<PathBuf> {
    let short = commit.get(..12).unwrap_or(commit);
    let parent = repo.parent().filter(|p| !p.as_os_str().is_empty());
    let dir = parent
        .ok_or_else(|| {
            CtlError::invalid(format!(
                "{} has no parent directory to stage a checkout in",
                repo.display()
            ))
        })?
        .join(format!(".cowfs-base-{}-{short}", std::process::id()));
    if dir.exists() {
        // Only ever this pid's own leftover, and only removed when git does not know about it.
        return Err(CtlError::new(
            ErrorCode::IoError,
            format!(
                "the staging path {} already exists from an earlier attempt; remove it once you \
                 have checked it holds nothing you want",
                dir.display()
            ),
        ));
    }
    Ok(dir)
}

/// Whether git still has a worktree registered at `dir`.
///
/// Removal is only attempted for a checkout this call created, so this is the check that the path
/// is really ours and still registered.
///
/// Both sides are compared resolved. git reports the physical path it was given after symlinks, so
/// a caller whose `repo` runs through one (`/var` on macOS, `/tmp` elsewhere) would otherwise never
/// match its own checkout, and the checkout would be left behind.
fn worktree_registered(repo: &Path, dir: &Path) -> bool {
    let out = match std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "list", "--porcelain"])
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(out) => out,
        Err(_) => return false,
    };
    let want = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(|p| p.trim())
        .any(|p| std::fs::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p)) == want)
}

/// `import {path, name}`. A taken name is `already_exists`, per the protocol's collision rule:
/// a caller that wants a fresh import removes the old snapshot or picks a new name.
pub fn run(
    backend: &dyn Backend,
    snaps: &dyn Snapshots,
    params: &ImportParams,
    ctx: &OpContext<'_>,
) -> CtlResult<ImportReport> {
    let source = PathBuf::from(&params.path);
    let meta = std::fs::symlink_metadata(&source)
        .map_err(|e| CtlError::new(ErrorCode::NotFound, format!("{}: {e}", params.path)))?;
    if !meta.is_dir() {
        return Err(CtlError::invalid(format!(
            "{} is not a directory",
            params.path
        )));
    }
    if snaps.list().unwrap_or_default().contains(&params.name) {
        return Err(CtlError::new(
            ErrorCode::AlreadyExists,
            format!("snapshot {:?} already exists", params.name),
        ));
    }
    let before = hash_tree(&source).map_err(|e| {
        CtlError::new(
            ErrorCode::IoError,
            format!("cannot hash {}: {e}", params.path),
        )
    })?;
    ctx.progress(event(
        "hash",
        0,
        Some(before.files),
        Some("hashing the source"),
    ))?;

    let imported = ingest(&source, &backend.store_path().join(&params.name), ctx)?;
    // Re-read the source, then compare the two root hashes. A source that changed under the
    // import is a mismatch, not a success.
    let after = hash_tree(&source).map_err(|e| {
        CtlError::new(
            ErrorCode::IoError,
            format!("cannot re-read {}: {e}", params.path),
        )
    })?;
    let got = hash_tree(&imported).map_err(|e| {
        CtlError::new(
            ErrorCode::IoError,
            format!("cannot hash the imported tree: {e}"),
        )
    })?;

    let mut mismatches: Vec<ImportMismatch> = Vec::new();
    if after.root != before.root {
        mismatches.push(ImportMismatch {
            path: ".".into(),
            reason: format!(
                "the source changed while it was ingested ({} then {})",
                before.root, after.root
            ),
        });
    }
    if got.root != after.root {
        mismatches.push(ImportMismatch {
            path: ".".into(),
            reason: format!(
                "the imported tree differs from the source ({} != {})",
                got.root, after.root
            ),
        });
    }
    let mismatches_truncated = mismatches.len() > MAX_MISMATCHES;
    mismatches.truncate(MAX_MISMATCHES);
    ctx.progress(event("done", got.files, Some(got.files), None))?;

    Ok(ImportReport {
        name: params.name.clone(),
        files: got.files,
        bytes: got.bytes,
        verified: mismatches.is_empty(),
        hash_algorithm: cowfs_ctl::HASH_ALGORITHM.to_owned(),
        source_root_hash: after.root,
        imported_root_hash: got.root,
        mismatches,
        mismatches_truncated,
        stored_bytes: None,
    })
}

/// `base_refresh {repo, git_ref, name?}`. The name defaults to a derived one that two
/// repositories cannot collide on.
pub fn base_refresh(
    backend: &dyn Backend,
    snaps: &dyn Snapshots,
    params: &BaseRefreshParams,
    ctx: &OpContext<'_>,
) -> CtlResult<BaseRefreshReport> {
    let name = params
        .name
        .clone()
        .unwrap_or_else(|| base_name(&params.repo));
    let repo = PathBuf::from(&params.repo);
    let meta = std::fs::symlink_metadata(&repo)
        .map_err(|e| CtlError::new(ErrorCode::NotFound, format!("{}: {e}", params.repo)))?;
    if !meta.is_dir() {
        return Err(CtlError::invalid(format!(
            "{} is not a directory",
            params.repo
        )));
    }
    let previous = snaps
        .list()
        .unwrap_or_default()
        .contains(&name)
        .then(|| snaps.create_meta(&name).ok())
        .flatten()
        .and_then(|i| i.base)
        .and_then(|b| b.commit);
    let commit = git_commit(&params.repo, &params.git_ref).ok_or_else(|| {
        CtlError::not_found(format!(
            "{} has no commit at {:?}",
            params.repo, params.git_ref
        ))
    })?;

    // A working tree is not necessarily clean, so the base is a fresh checkout of the ref, at a
    // path this call chose and will remove again.
    let dir = staging_path(&repo, &commit)?;
    let added = git_worktree(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            &dir.display().to_string(),
            commit.as_str(),
        ],
        "add",
    );
    if let Err(e) = added {
        // git may have created the directory before failing, so the leftover is cleaned here
        // rather than left behind for the next attempt to trip over.
        if dir.exists() && !worktree_registered(&repo, &dir) {
            let _ = std::fs::remove_dir_all(&dir);
        }
        return Err(e);
    }
    let result = replace(&dir, &name, backend, snaps, ctx).and_then(|()| {
        snaps.promote(&name).map_err(|e| {
            CtlError::new(ErrorCode::IoError, format!("cannot promote {name:?}: {e}"))
        })?;
        // Publication is the record being durable, not the response carrying it: a caller that
        // reconnects, or a daemon that restarts, has to find this base again and be told the same
        // commit. So the provenance is written before the report exists, and a write that fails
        // fails the refresh instead of leaving a base that looks published and cannot be found.
        snaps
            .set_base_meta(
                &name,
                &BaseMeta {
                    repo: Some(params.repo.clone()),
                    git_ref: Some(params.git_ref.clone()),
                    commit: Some(commit.clone()),
                },
            )
            .map_err(|e| {
                CtlError::new(
                    ErrorCode::IoError,
                    format!("cannot record where {name:?} was built from: {e}"),
                )
            })?;
        // Re-read rather than reporting what was just written, so the report can only say "fresh"
        // if a later caller reading the same store would agree.
        Ok(BaseRefreshReport {
            snapshot: snaps.create_meta(&name).map_err(|e| {
                CtlError::new(
                    ErrorCode::IoError,
                    format!("cannot read back the base record for {name:?}: {e}"),
                )
            })?,
            previous_commit: previous,
        })
    });
    // Only this call's own checkout is removed, and only while git still lists it, so a path that
    // turned out to belong to something else is left alone. `git worktree remove` also deletes the
    // directory, so there is nothing else to clean up on the success path.
    if worktree_registered(&repo, &dir) {
        let _ = git_worktree(
            &repo,
            &["worktree", "remove", "--force", &dir.display().to_string()],
            "remove",
        );
    }
    result
}

/// Copies `from` into the snapshot `name`, replacing what is there, which is what refreshing a
/// base needs. An existing base of the same name goes only after the new one is in place.
fn replace(
    from: &Path,
    name: &str,
    backend: &dyn Backend,
    snaps: &dyn Snapshots,
    ctx: &OpContext<'_>,
) -> CtlResult<()> {
    let taken = snaps.list().unwrap_or_default().iter().any(|n| n == name);
    // The core refuses a leading dot in a snapshot name, the passthrough store hides one.
    let staging = if backend.ingests_directories() {
        format!(".cowfs-import-{name}")
    } else {
        format!("cowfs-import-{name}")
    };
    if snaps.list().unwrap_or_default().contains(&staging) {
        snaps
            .remove(&staging)
            .map_err(|e| io_err(&format!("cannot clear {staging:?}"), e))?;
    }
    if !backend.ingests_directories() {
        return replace_tree(from, name, &staging, taken, backend, snaps, ctx);
    }
    snaps
        .create(&staging, None)
        .map_err(|e| io_err(&format!("cannot create {staging:?}"), e))?;
    let staged = backend.store_path().join(&staging);
    if let Err(e) = copy_tree(from, &staged, ctx, &mut 0) {
        let _ = snaps.remove(&staging);
        return Err(e);
    }
    if taken {
        snaps
            .remove(name)
            .map_err(|e| io_err(&format!("cannot replace {name:?}"), e))?;
    }
    snaps
        .rename(&staging, name)
        .map_err(|e| io_err(&format!("cannot install {name:?}"), e))
}

/// The tree-native publication step for a backend whose snapshots are trees (the core): the
/// checkout goes in through the backend's own writer, which verifies it before the staging name is
/// visible, and an existing base is replaced by the core's crash-safe `swap`, never removed first.
fn replace_tree(
    from: &Path,
    name: &str,
    staging: &str,
    taken: bool,
    backend: &dyn Backend,
    snaps: &dyn Snapshots,
    ctx: &OpContext<'_>,
) -> CtlResult<()> {
    let mut progress = |done: u64, total: u64| {
        ctx.progress(ProgressEvent {
            phase: "ingest".into(),
            done,
            total: Some(total),
            unit: Unit::Bytes,
            message: None,
        })
        .is_ok()
    };
    if backend.ingest(from, staging, &mut progress)?.is_none() {
        return Err(CtlError::new(
            ErrorCode::Unsupported,
            "this backend has no writer that can ingest a directory",
        ));
    }
    let installed = if taken {
        snaps.swap(name, staging).map(|_| ())
    } else {
        snaps.rename(staging, name)
    };
    if let Err(e) = installed {
        let _ = snaps.remove(staging);
        return Err(io_err(&format!("cannot install {name:?}"), e));
    }
    if taken {
        snaps
            .remove(staging)
            .map_err(|e| io_err(&format!("cannot drop {staging:?}"), e))?;
    }
    Ok(())
}

fn io_err(what: &str, e: std::io::Error) -> CtlError {
    CtlError::new(ErrorCode::IoError, format!("{what}: {e}"))
}

/// Copies `from` into `to`, reporting progress and stopping on cancellation.
fn ingest(from: &Path, to: &Path, ctx: &OpContext<'_>) -> CtlResult<PathBuf> {
    let mut done = 0u64;
    copy_tree(from, to, ctx, &mut done)?;
    Ok(to.to_owned())
}

fn copy_tree(from: &Path, to: &Path, ctx: &OpContext<'_>, done: &mut u64) -> CtlResult<()> {
    std::fs::create_dir_all(to).map_err(|e| {
        CtlError::new(
            ErrorCode::IoError,
            format!("cannot create {}: {e}", to.display()),
        )
    })?;
    let entries = std::fs::read_dir(from).map_err(|e| {
        CtlError::new(
            ErrorCode::IoError,
            format!("cannot read {}: {e}", from.display()),
        )
    })?;
    let mut last = Instant::now();
    for entry in entries.flatten() {
        ctx.check()?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(|e| CtlError::new(ErrorCode::IoError, e.to_string()))?;
        if kind.is_dir() {
            copy_tree(&src, &dst, ctx, done)?;
        } else if kind.is_symlink() {
            let target = std::fs::read_link(&src)
                .map_err(|e| CtlError::new(ErrorCode::IoError, e.to_string()))?;
            std::os::unix::fs::symlink(target, &dst)
                .map_err(|e| CtlError::new(ErrorCode::IoError, e.to_string()))?;
            *done += 1;
        } else {
            std::fs::copy(&src, &dst)
                .map_err(|e| CtlError::new(ErrorCode::IoError, e.to_string()))?;
            *done += 1;
        }
        if last.elapsed() >= PROGRESS_EVERY {
            ctx.progress(event("copy", *done, None, None))?;
            last = Instant::now();
        }
    }
    Ok(())
}

fn event(phase: &str, done: u64, total: Option<u64>, message: Option<&str>) -> ProgressEvent {
    ProgressEvent {
        phase: phase.to_owned(),
        done,
        total,
        unit: Unit::Items,
        message: message.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::PathBackend;
    use std::sync::Arc;

    fn backend() -> (tempfile::TempDir, Arc<dyn Backend>) {
        let dir = tempfile::tempdir().unwrap();
        let b: Arc<dyn Backend> = Arc::new(PathBackend::open(dir.path().join("store")).unwrap());
        (dir, b)
    }

    fn import(b: &dyn Backend, path: &Path, name: &str) -> CtlResult<ImportReport> {
        run(
            b,
            b.snapshots(),
            &ImportParams {
                path: path.display().to_string(),
                name: name.into(),
            },
            &OpContext::detached(),
        )
    }

    #[test]
    fn import_verifies_by_hash_and_reports_both_roots() {
        let (_d, b) = backend();
        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir(src.path().join("sub")).unwrap();
        std::fs::write(src.path().join("sub").join("a"), b"hello").unwrap();
        std::fs::write(src.path().join("b"), b"world").unwrap();
        let report = import(b.as_ref(), src.path(), "imported").unwrap();
        assert!(report.verified, "{report:?}");
        assert_eq!(report.hash_algorithm, "blake3");
        assert_eq!((report.files, report.bytes), (2, 10));
        assert_eq!(report.source_root_hash, report.imported_root_hash);
        assert_eq!(report.mismatches, Vec::new());
        assert!(!report.mismatches_truncated);
        assert_eq!(
            std::fs::read_to_string(b.store_path().join("imported").join("sub").join("a")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn import_keeps_symlinks_as_symlinks() {
        let (_d, b) = backend();
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("a"), b"x").unwrap();
        std::os::unix::fs::symlink("a", src.path().join("l")).unwrap();
        let report = import(b.as_ref(), src.path(), "withlink").unwrap();
        assert!(report.verified, "{report:?}");
        let l = b.store_path().join("withlink").join("l");
        assert!(std::fs::symlink_metadata(&l)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn import_refuses_a_missing_source_a_file_and_a_taken_name() {
        let (_d, b) = backend();
        assert_eq!(
            import(b.as_ref(), Path::new("/no/such/dir"), "x")
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        let src = tempfile::tempdir().unwrap();
        let file = src.path().join("f");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(
            import(b.as_ref(), &file, "x").unwrap_err().code,
            ErrorCode::InvalidParams
        );
        std::fs::write(src.path().join("g"), b"y").unwrap();
        import(b.as_ref(), src.path(), "x").unwrap();
        assert_eq!(
            import(b.as_ref(), src.path(), "x").unwrap_err().code,
            ErrorCode::AlreadyExists
        );
    }

    #[test]
    fn import_stops_when_cancelled_and_leaves_no_snapshot() {
        let (_d, b) = backend();
        let src = tempfile::tempdir().unwrap();
        std::fs::write(src.path().join("g"), b"y").unwrap();
        let token = cowfs_ctl::CancelToken::new();
        token.cancel();
        let e = run(
            b.as_ref(),
            b.snapshots(),
            &ImportParams {
                path: src.path().display().to_string(),
                name: "y".into(),
            },
            &OpContext::new(token, |_| true),
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::Cancelled, "{e}");
        assert!(
            !b.store_path().join("y").exists(),
            "a cancelled import wrote nothing"
        );
    }

    /// A real git repository with one commit, beside a real directory for the staging path to
    /// live in, because the staging path is the repository's parent.
    fn repo(tag: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .stdin(std::process::Stdio::null())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("main.rs"), b"fn main() {}\n").unwrap();
        git(&["add", "main.rs"]);
        git(&[
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            tag,
        ]);
        (dir, repo)
    }

    fn refresh(b: &dyn Backend, repo: &Path, name: &str) -> CtlResult<BaseRefreshReport> {
        base_refresh(
            b,
            b.snapshots(),
            &BaseRefreshParams {
                repo: repo.display().to_string(),
                git_ref: "main".into(),
                name: Some(name.into()),
            },
            &OpContext::detached(),
        )
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Resolved, because git reports the physical path of every worktree it knows.
    fn worktree_paths(repo: &Path) -> Vec<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["worktree", "list", "--porcelain"])
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        let mut paths: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.strip_prefix("worktree "))
            .map(|p| {
                std::fs::canonicalize(p.trim())
                    .unwrap_or_else(|_| PathBuf::from(p.trim()))
                    .display()
                    .to_string()
            })
            .collect();
        paths.sort();
        paths
    }

    fn resolved(p: &Path) -> String {
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .display()
            .to_string()
    }

    /// The defect #98 reported: the refresh returned a commit but wrote none, so the next caller
    /// was told the base had no provenance and `base status` could not find it.
    #[test]
    fn a_refresh_is_still_a_published_base_after_the_daemon_reopens_the_store() {
        let (_d, b) = backend();
        let (_rd, repo) = repo("reopened");
        let store = b.store_path().to_owned();
        let commit = refresh(b.as_ref(), &repo, "warm")
            .unwrap()
            .snapshot
            .base
            .expect("the refresh reports a base")
            .commit;
        drop(b);

        let reopened = PathBackend::open(&store).expect("the same store opens again");
        let info = reopened.snapshots().create_meta("warm").unwrap();
        assert_eq!(
            info.base
                .expect("the base is still a base after a reopen")
                .commit,
            commit,
            "the published commit did not survive the daemon"
        );
    }

    /// A refresh that cannot record where the base came from fails. It does not return success, and
    /// it does not leave something behind that reports itself fresh.
    #[test]
    fn a_refresh_that_cannot_record_its_provenance_fails_and_leaves_no_fresh_base() {
        use std::os::unix::fs::PermissionsExt;

        let (_d, b) = backend();
        let (_rd, repo) = repo("unrecordable");
        // The provenance directory exists but cannot be written, so no record can ever land. Putting a
        // file where the directory belongs refuses earlier still, at the first attempt to clear a
        // stale record, and that is a refusal too rather than a success.
        let root = b.store_path().join(".cowfs-base-meta");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();

        let e = refresh(b.as_ref(), &repo, "warm").unwrap_err();
        assert!(
            e.to_string().contains("warm"),
            "the refusal names the base: {e}"
        );
        // Where the refusal lands depends on which write fails first, so what is asserted is the
        // postcondition: nothing anywhere reports itself a base with a commit.
        if let Ok(info) = b.snapshots().create_meta("warm") {
            assert_eq!(
                info.base, None,
                "nothing reports itself fresh after a failed publication: {info:?}"
            );
        }
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            worktree_paths(&repo),
            vec![resolved(&repo)],
            "the failed refresh left no checkout behind"
        );
    }

    #[test]
    fn a_refresh_publishes_a_base_and_leaves_the_repository_alone() {
        let (_d, b) = backend();
        let (_rd, repo) = repo("the base");
        let before = entries(&repo);
        let report = refresh(b.as_ref(), &repo, "warm").unwrap();
        assert_eq!(report.previous_commit, None);
        let base = report.snapshot.base.expect("the report records a base");
        assert_eq!(base.git_ref.as_deref(), Some("main"));
        assert_eq!(base.commit.as_deref().map(str::len), Some(40));
        // The checkout came from git, so the content is git's, and it is published under the name
        // the caller asked for.
        assert_eq!(
            std::fs::read_to_string(b.store_path().join("warm").join("main.rs")).unwrap(),
            "fn main() {}\n"
        );
        // The repository is exactly as it was: this is the defect that used to leave a directory
        // named after the commit inside it.
        assert_eq!(
            entries(&repo),
            before,
            "the repository gained or lost something"
        );
        assert_eq!(worktree_paths(&repo), vec![resolved(&repo)]);
        let _ = std::fs::read_dir(repo.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .find(|n| n.starts_with(".cowfs-base-"))
            .map(|n| panic!("the staging path {n} was left behind"));
    }

    #[test]
    fn two_refreshes_of_one_repository_both_succeed_and_idempotently_record_the_commit() {
        let (_d, b) = backend();
        let (_rd, repo) = repo("the base");
        let first = refresh(b.as_ref(), &repo, "warm").unwrap();
        let first_commit = first.snapshot.base.clone().unwrap().commit;
        let second = refresh(b.as_ref(), &repo, "warm").unwrap();
        assert_eq!(
            first_commit,
            second.snapshot.base.as_ref().unwrap().commit,
            "refreshing an unchanged repository records the same commit"
        );
        assert_eq!(
            first.previous_commit, None,
            "the first refresh has no earlier base to report"
        );
        assert_eq!(
            entries(&repo),
            [".git", "main.rs"],
            "the repository is untouched"
        );
        assert_eq!(worktree_paths(&repo), vec![resolved(&repo)]);
    }

    /// The tree-native publication of issue 123: the core refuses a directory, so the checkout goes
    /// in through its writer, and the base, its commit and a replacement all survive a reopen.
    #[test]
    fn the_core_publishes_a_warm_base_by_ingesting_the_checkout() {
        use crate::backend::CoreBackend;
        let store = tempfile::tempdir().unwrap();
        let (_rd, repo) = repo("the base");
        let want = git_commit(&repo.display().to_string(), "main").unwrap();
        let core = CoreBackend::open(store.path(), cowfs_core::Options::default()).unwrap();
        let first = refresh(&core, &repo, "warm").expect("a first publication");
        assert_eq!(
            first.snapshot.base.as_ref().unwrap().commit,
            Some(want.clone())
        );
        assert_eq!(first.previous_commit, None);

        // A second refresh replaces a taken name through the core's swap, with no staging left.
        let second = refresh(&core, &repo, "warm").expect("a replacing publication");
        assert_eq!(second.previous_commit, Some(want.clone()));
        let names = core.snapshots().list().unwrap();
        assert_eq!(
            names,
            ["warm"],
            "no staging snapshot is left behind: {names:?}"
        );
        core.close().unwrap();
        drop(core);

        let reopened = CoreBackend::open(store.path(), cowfs_core::Options::default()).unwrap();
        let info = reopened.snapshots().create_meta("warm").unwrap();
        assert_eq!(info.base.as_ref().unwrap().commit, Some(want));
        let view = reopened.snapshot("warm").unwrap();
        let hash = cowfs_ctl::hash_view(view.as_ref(), cowfs_vfs::ROOT_INO).unwrap();
        assert!(hash.files >= 1, "the published tree holds the checkout");
        drop(view);
        assert_eq!(
            entries(&repo),
            [".git", "main.rs"],
            "the repository is untouched"
        );
        assert_eq!(worktree_paths(&repo), vec![resolved(&repo)]);
        reopened.close().unwrap();
    }

    #[test]
    fn a_refresh_that_cannot_add_a_worktree_publishes_no_base_and_leaves_no_residue() {
        // A ref with no commit is the cheapest way to fail before anything is published, and it
        // fails on the same call the old code failed on.
        let (_d, b) = backend();
        let (_rd, repo) = repo("the base");
        let e = base_refresh(
            b.as_ref(),
            b.snapshots(),
            &BaseRefreshParams {
                repo: repo.display().to_string(),
                git_ref: "no-such-ref".into(),
                name: Some("warm".into()),
            },
            &OpContext::detached(),
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound, "{e}");
        assert!(
            !b.store_path().join("warm").exists(),
            "a failed refresh published nothing"
        );
        assert_eq!(entries(&repo), [".git", "main.rs"]);
        assert_eq!(worktree_paths(&repo), vec![resolved(&repo)]);
    }

    #[test]
    fn the_staging_path_is_beside_the_repository_and_never_inside_it() {
        let (_d, repo) = repo("the base");
        let dir = staging_path(&repo, "0123456789abcdef0123").unwrap();
        assert_eq!(
            dir.parent().unwrap(),
            repo.parent().unwrap(),
            "staging must not land inside the repository"
        );
        assert!(dir.to_string_lossy().contains("0123456789ab"), "{dir:?}");
        assert!(
            staging_path(&repo, "0123456789abcdef0123").is_ok(),
            "a free staging path is handed out"
        );
        std::fs::create_dir(&dir).unwrap();
        assert!(staging_path(&repo, "0123456789abcdef0123").is_err());
    }

    #[test]
    fn the_staging_path_is_refused_for_a_repository_with_no_parent() {
        let e = staging_path(Path::new("/"), "0123456789abcdef0123").unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidParams, "{e}");
    }

    #[test]
    fn worktree_registration_is_read_from_git_and_not_guessed() {
        let (_d, repo) = repo("the base");
        assert!(
            worktree_registered(&repo, &repo),
            "the repository's own checkout is registered"
        );
        let stranger = repo.parent().unwrap().join("not-a-worktree");
        assert!(
            !worktree_registered(&repo, &stranger),
            "an unregistered path is not reported as a worktree"
        );
    }

    #[test]
    fn git_worktree_reports_a_failure_rather_than_a_parsed_path() {
        // git 2.39 prints `HEAD is now at <sha> <subject>` on stdout. Nothing here may read that
        // as a path, and a failure must carry git's own stderr.
        let (_d, repo) = repo("the base");
        let e = git_worktree(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                "/proc/nonexistent-checkout",
                "HEAD",
            ],
            "add",
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::IoError, "{e}");
        assert!(
            e.to_string().contains("git worktree add failed"),
            "the error names the operation: {e}"
        );
    }

    #[test]
    fn the_base_name_is_derived_from_the_repository_path() {
        assert_eq!(base_name("/src/myrepo"), "myrepo-base");
        assert_eq!(base_name("/src/myrepo/"), "myrepo-base");
        assert_eq!(base_name("/"), "repo-base");
        // The name is derived, never configured: two callers cannot pick a colliding base for
        // the same repository, and the treehouse companion adds its own `-base` suffix.
        assert_eq!(base_name("/a/myrepo"), base_name("/b/myrepo"));
    }
}
