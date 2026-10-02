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

/// Runs `git -C repo <args>` and returns the checkout path it reported, which `git worktree
/// add` prints as the last line of stdout.
fn git_worktree(repo: &Path, args: &[&str], what: &str) -> CtlResult<PathBuf> {
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
    let dir = String::from_utf8_lossy(&out.stdout)
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(PathBuf::from);
    dir.filter(|d| d.is_dir()).ok_or_else(|| {
        CtlError::new(
            ErrorCode::Internal,
            format!("git worktree {what} did not report where it worked"),
        )
    })
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

    // A working tree is not necessarily clean, so the base is a fresh checkout of the ref.
    let dir = git_worktree(
        &repo,
        &["worktree", "add", "--detach", commit.as_str()],
        "add",
    )?;
    let result = replace(&dir, &name, backend, snaps, ctx).and_then(|()| {
        let mut info = snaps.promote(&name).map_err(|e| {
            CtlError::new(ErrorCode::IoError, format!("cannot promote {name:?}: {e}"))
        })?;
        info.base = Some(BaseMeta {
            repo: Some(params.repo.clone()),
            git_ref: Some(params.git_ref.clone()),
            commit: Some(commit.clone()),
        });
        Ok(BaseRefreshReport {
            snapshot: info,
            previous_commit: previous,
        })
    });
    let _ = git_worktree(
        &repo,
        &["worktree", "remove", "--force", &dir.display().to_string()],
        "remove",
    );
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
    let staging = format!(".cowfs-import-{name}");
    if snaps.list().unwrap_or_default().contains(&staging) {
        snaps
            .remove(&staging)
            .map_err(|e| io_err(&format!("cannot clear {staging:?}"), e))?;
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
