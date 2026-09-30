use cowfs_ctl::{validate_snapshot_name, ClientError, CtlError, ErrorCode};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};

/// Six hex characters: the first three bytes of `sha256`, the same derivation treehouse's
/// `internal/vcs.ShortHash` uses for a pool directory name.
pub fn short6(input: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(input.as_bytes());
    digest[..3].iter().map(|b| format!("{b:02x}")).collect()
}

/// A path with symlinks resolved, used for every path that acts as an identity rather than a
/// location. `git rev-parse --show-toplevel` already reports a physical path, while a caller on
/// macOS usually holds a `/var/...` symlink to `/private/var/...`; without this the two spell the
/// same repository differently and a `base.repo` lookup misses.
pub fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The main repository root of the repository containing `dir`, or `dir` itself when git cannot
/// answer. Mirrors treehouse's `gitvcs.FindMainRepoRootFrom`.
pub fn main_repo_root(dir: &Path) -> Result<PathBuf> {
    let Ok(toplevel) = git(dir, &["rev-parse", "--show-toplevel"]) else {
        return Ok(dir.to_path_buf());
    };
    let toplevel = canonical(Path::new(&toplevel));
    let Ok(common) = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) else {
        return Ok(toplevel);
    };
    let common = PathBuf::from(common);
    if common.file_name().is_some_and(|n| n == ".git") {
        if let Some(parent) = common.parent() {
            return Ok(parent.to_path_buf());
        }
    }
    Ok(toplevel)
}

/// `{basename(repo_root)}-{short6}`, byte-identical to treehouse's `config.ResolvePoolDir`. The
/// hash of the `origin` URL keeps two repositories with the same directory name distinct on a mount
/// that every repository shares.
///
/// `repo_root` must be the main repository root, not a linked worktree, or the hash will not match
/// the pool directory treehouse derived.
pub fn pool_id(repo_root: &Path) -> Result<String> {
    if !repo_root.is_absolute() {
        return Err(Error::Usage(format!(
            "repository root {} must be an absolute path",
            repo_root.display()
        )));
    }
    let name = repo_root
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    if name.is_empty() {
        return Err(Error::Usage(format!(
            "{} has no directory name to derive a pool id from",
            repo_root.display()
        )));
    }
    // treehouse hashes the remote URL and falls back to the absolute path when there is none.
    let hash_input = match git(repo_root, &["remote", "get-url", "origin"]) {
        Ok(url) if !url.is_empty() => url,
        _ => canonical(repo_root).display().to_string(),
    };
    finish(format!("{name}-{}", short6(&hash_input)))
}

/// The warm base snapshot name of a pool. Derived, never configurable: a configurable base name is
/// how two repositories end up sharing one warm base.
pub fn base_snapshot(pool_id: &str) -> Result<String> {
    finish(format!("{pool_id}-base"))
}

/// The main checkout snapshot name of a pool.
pub fn main_snapshot(pool_id: &str) -> Result<String> {
    finish(format!("{pool_id}-main"))
}

/// The empty snapshot a returned slot is reset to, so treehouse's own `git reset --hard` and
/// `git clean -xdf` run against nothing instead of against a warm build tree.
///
/// It is not the base: a base is what a *new* slot is cloned from, and resetting a returned slot to
/// the base would leave the warm artifacts in place for treehouse to walk.
pub fn empty_snapshot(pool_id: &str) -> Result<String> {
    finish(format!("{pool_id}-empty"))
}

/// A treehouse slot name, which becomes one path component of the slot path and one component of
/// the snapshot name, so it has to be a single safe component and not just a legal snapshot name.
/// Without this, a slot name of `..` would derive the snapshot name `pool-..`, which passes
/// `validate_snapshot_name` and is still wrong.
pub fn validate_slot(slot: &str) -> Result<()> {
    if slot.is_empty() {
        return Err(Error::Usage("a slot name must not be empty".into()));
    }
    if slot == "." || slot == ".." {
        return Err(Error::Usage(format!(
            "slot name {slot:?} is not a directory name"
        )));
    }
    if slot.contains('/') || slot.contains('\\') || slot.contains('\0') {
        return Err(Error::Usage(format!(
            "slot name {slot:?} must be a single path component"
        )));
    }
    Ok(())
}

/// The snapshot name behind one treehouse slot.
pub fn slot_snapshot(pool_id: &str, slot: &str) -> Result<String> {
    validate_slot(slot)?;
    finish(format!("{pool_id}-{slot}"))
}

/// The treehouse slot name of a slot path, `{pool}/{slot}/{repo}` or `{pool}/{slot}/{repo}-{slot}`.
pub fn slot_of(slot_path: &Path) -> Option<&str> {
    slot_path
        .parent()
        .and_then(Path::file_name)
        .and_then(std::ffi::OsStr::to_str)
}

/// The pool directory name of a slot path, which is the pool id by construction.
pub fn pool_id_of_slot_path(slot_path: &Path) -> Option<String> {
    slot_path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_owned)
}

fn finish(name: String) -> Result<String> {
    // A derived name that starts with a dash would be read as an option by any tool it is passed
    // to, which the API validator allows and this must not.
    if name.starts_with('-') {
        return Err(Error::Usage(format!(
            "derived snapshot name {name:?} starts with a dash"
        )));
    }
    validate_snapshot_name(&name).map_err(|e| from_ctl_error(&e))?;
    Ok(name)
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| Error::Io(format!("cannot run git {}: {e}", args.join(" "))))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// A `git rev-parse <rev>`, or `None` when the ref does not exist.
pub fn resolve_commit(repo_root: &Path, git_ref: &str) -> Option<String> {
    let rev = format!("{git_ref}^{{commit}}");
    let out = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", &rev])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Maps a control-API client failure onto the documented exit codes.
pub fn from_client_error(e: &ClientError) -> Error {
    if e.is_not_running() {
        return Error::NotRunning(e.to_string());
    }
    if e.is_timeout() {
        return Error::Timeout(e.to_string());
    }
    if let ClientError::Server(ctl) = e {
        return from_ctl_error(ctl);
    }
    Error::Cowfs(e.to_string())
}

/// Maps a daemon error frame onto the documented exit codes, so `busy` is exit 5 and not exit 1.
pub fn from_ctl_error(e: &CtlError) -> Error {
    if e.code == ErrorCode::Busy {
        return Error::Busy(e.message.clone());
    }
    Error::Cowfs(format!("{}: {}", e.code.as_str(), e.message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short6_is_six_lowercase_hex_chars() {
        let h = short6("https://github.com/zeeshanhaque21/cowfs");
        assert_eq!(h.len(), 6, "{h}");
        assert!(h
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
        assert_eq!(h, short6("https://github.com/zeeshanhaque21/cowfs"));
        assert_ne!(h, short6("https://github.com/other/cowfs"));
    }

    #[test]
    fn names_carry_the_pool_id() {
        let id = "cowfs-7c1bf8";
        assert_eq!(base_snapshot(id).unwrap(), "cowfs-7c1bf8-base");
        assert_eq!(main_snapshot(id).unwrap(), "cowfs-7c1bf8-main");
        assert_eq!(slot_snapshot(id, "3").unwrap(), "cowfs-7c1bf8-3");
    }

    #[test]
    fn base_and_main_cannot_collide_with_a_slot_suffix() {
        // A slot suffix is a hex short hash or a decimal slot name, never "base" or "main".
        for slot in ["1", "21", "a3f9", "0"] {
            let n = slot_snapshot("cowfs-7c1bf8", slot).unwrap();
            assert_ne!(n, "cowfs-7c1bf8-base");
            assert_ne!(n, "cowfs-7c1bf8-main");
        }
    }

    #[test]
    fn an_empty_pool_id_is_refused_rather_than_making_a_dash_name() {
        assert!(base_snapshot("").is_err());
        assert!(main_snapshot("").is_err());
        assert!(slot_snapshot("", "1").is_err());
    }

    #[test]
    fn derived_names_go_through_the_api_validator() {
        assert!(base_snapshot(".hidden").is_err());
        assert!(base_snapshot("has/slash").is_err());
        // A dot inside a slot name cannot reach the start of the snapshot name, so it is safe even
        // though the snapshot validator would reject it as a leading dot.
        assert_eq!(slot_snapshot("cowfs", ".nfs1").unwrap(), "cowfs-.nfs1");
        assert!(base_snapshot("").is_err());
    }

    #[test]
    fn a_slot_name_must_be_one_path_component() {
        // These would otherwise derive `pool-..`, which the snapshot validator accepts.
        for bad in ["", ".", "..", "a/b", "a\\b", "a\0b"] {
            assert!(slot_snapshot("pool", bad).is_err(), "{bad:?}");
        }
        for ok in ["1", "21", "a3f9", "slot-1"] {
            assert!(slot_snapshot("pool", ok).is_ok(), "{ok:?}");
        }
    }

    #[test]
    fn relative_repo_root_is_refused() {
        assert!(pool_id(Path::new("relative/path")).is_err());
    }

    #[test]
    fn slot_geometry() {
        let p = Path::new("/pool/.treehouse/cowfs-7c1bf8/3/cowfs");
        assert_eq!(slot_of(p), Some("3"));
        assert_eq!(pool_id_of_slot_path(p).as_deref(), Some("cowfs-7c1bf8"));
        assert_eq!(pool_id_of_slot_path(Path::new("/pool")), None);
    }
}
