use crate::error::{CtlError, CtlResult};
use unicode_normalization::UnicodeNormalization;

/// Longest snapshot name in bytes.
pub const MAX_NAME_BYTES: usize = 255;
/// Longest path in bytes.
pub const MAX_PATH_BYTES: usize = 4096;
/// Longest git ref in bytes.
pub const MAX_REF_BYTES: usize = 255;

/// Replaces control characters with their escapes so untrusted text is safe to print.
pub fn escape_control(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// The API-level snapshot name rule: non-empty, at most 255 bytes, no `/`, no control characters
/// (NUL, newline and ESC included), and no leading `.` (which also rules out `.`, `..`, `._*`
/// and `.nfs*`). Names are UTF-8 by construction, since the wire format is JSON.
pub fn validate_snapshot_name(name: &str) -> CtlResult<()> {
    let bad = |why: &str| {
        Err(CtlError::invalid(format!(
            "invalid snapshot name {name:?}: {why}"
        )))
    };
    if name.is_empty() {
        return bad("empty");
    }
    if name.len() > MAX_NAME_BYTES {
        return bad("longer than 255 bytes");
    }
    if name.starts_with('.') {
        return bad("must not start with a dot");
    }
    if name.contains('/') {
        return bad("must not contain a slash");
    }
    if name.chars().any(char::is_control) {
        return bad("must not contain control characters");
    }
    Ok(())
}

/// A directory named relative to the daemon's mount, which is what `ps` accepts for a treehouse
/// slot in mode (a): such a slot is a directory inside a snapshot, not a snapshot.
///
/// Unlike a snapshot name this may contain `/` and may start with a dot, because `.treehouse` is a
/// real directory in a pool and a slot is three levels below it. What it may not do is leave the
/// mount: an absolute path or a `..` component is refused here, and the daemon re-checks the
/// resolved path against its own mount before it scans anything.
///
/// A leading `.` component is refused too. It names the mount itself, and a scan of the whole mount
/// is a scan of the wrong size with no bound but the deadline, after which every return on the
/// machine blocks. A `.` further along is harmless and stays legal.
pub fn validate_mount_relative(name: &str) -> CtlResult<()> {
    let bad = |why: &str| {
        Err(CtlError::invalid(format!(
            "invalid mount-relative name {name:?}: {why}"
        )))
    };
    if name.is_empty() {
        return bad("empty");
    }
    if name.len() > MAX_PATH_BYTES {
        return bad("longer than 4096 bytes");
    }
    if name.starts_with('/') {
        return bad("must be relative to the mount");
    }
    if name.chars().any(char::is_control) {
        return bad("must not contain control characters");
    }
    let mut components = name.split('/');
    let first = components.next().unwrap_or_default();
    if first == "." {
        return bad("must not name the mount itself");
    }
    // A component of `..` is the only way out, and an empty one would name a directory twice.
    if components.clone().any(|c| c.is_empty() || c == "..") {
        return bad("must not contain a `..` or empty component");
    }
    if name.ends_with('/') {
        return bad("must not end with a slash");
    }
    Ok(())
}

/// The collision key of a snapshot name: NFC, lowercased, NFC again. Two names with the same key
/// alias each other on a case-insensitive or normalising mount, so a backend must refuse to hold
/// both.
///
/// Folding can grow a name past `MAX_NAME_BYTES` (255 bytes of dotted capital I fold to 382
/// bytes), so a key longer than the bound is replaced by a hash of the folded form. That keeps
/// the key a valid, bounded snapshot name, at the cost of a theoretical hash collision.
pub fn name_key(name: &str) -> String {
    let folded: String = name
        .nfc()
        .collect::<String>()
        .to_lowercase()
        .nfc()
        .collect();
    if folded.len() <= MAX_NAME_BYTES {
        return folded;
    }
    let digest = blake3::hash(folded.as_bytes()).to_hex().to_string();
    format!("#{digest}")
}

/// An absolute path with no control characters, at most 4096 bytes. `what` names the field in the error.
pub fn validate_abs_path(what: &str, path: &str) -> CtlResult<()> {
    let bad = |why: &str| Err(CtlError::invalid(format!("invalid {what} {path:?}: {why}")));
    if path.len() > MAX_PATH_BYTES {
        return bad("longer than 4096 bytes");
    }
    if !path.starts_with('/') {
        return bad("must be an absolute path");
    }
    if path.chars().any(char::is_control) {
        return bad("must not contain control characters");
    }
    Ok(())
}

/// A repository path: absolute, so it can never be read as an option by a tool it is passed to.
pub fn validate_repo(repo: &str) -> CtlResult<()> {
    validate_abs_path("repo", repo)
}

/// A git ref: at most 255 bytes, no leading `-`, no control characters or spaces, none of
/// `~ ^ : ? * [ \`, no `..` or `@{`, no trailing `/` or `.lock`.
pub fn validate_git_ref(git_ref: &str) -> CtlResult<()> {
    let bad = |why: &str| {
        Err(CtlError::invalid(format!(
            "invalid git ref {git_ref:?}: {why}"
        )))
    };
    if git_ref.is_empty() || git_ref.len() > MAX_REF_BYTES {
        return bad("empty or longer than 255 bytes");
    }
    if git_ref.starts_with('-') {
        return bad("must not start with a dash");
    }
    if git_ref
        .chars()
        .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
    {
        return bad("contains a forbidden character");
    }
    if git_ref.contains("..")
        || git_ref.contains("@{")
        || git_ref.ends_with('/')
        || git_ref.ends_with(".lock")
    {
        return bad("forbidden sequence");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["a", "slot-1", "caf\u{e9}", &"x".repeat(255)] {
            assert!(validate_snapshot_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            ".x",
            "._x",
            ".nfs1",
            "a/b",
            "a\0b",
            "a\nb",
            "a\u{1b}b",
            "\u{85}",
            &"x".repeat(256),
        ] {
            assert!(validate_snapshot_name(bad).is_err(), "{bad:?}");
        }
    }

    /// A mode (a) slot is `.treehouse/{pool}/{slot}/{repo}` below the mount, so the name has to
    /// carry a dot directory, three levels and slashes, still be unable to leave the mount, and not
    /// be able to name the mount itself.
    #[test]
    fn mount_relative_names() {
        for ok in [
            "snap",
            ".treehouse/repo-abc123/1/repo",
            "base/.treehouse/cowfs-7c1bf8/2/cowfs",
            "a/./b",
        ] {
            assert!(validate_mount_relative(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/snap",
            "../snap",
            ".treehouse/../../elsewhere",
            "a/../b",
            ".treehouse/repo/1/repo/",
            "a//b",
            // A leading `.` is the mount itself, which is the whole mount to scan.
            ".",
            "./snap",
            "a/./../b",
            "a\0b",
            "a\nb",
            &"x".repeat(4097),
        ] {
            assert!(validate_mount_relative(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn keys_fold_case_and_normalisation() {
        assert_eq!(name_key("Foo"), name_key("fOO"));
        assert_eq!(name_key("caf\u{e9}"), name_key("cafe\u{301}"));
        assert_ne!(name_key("a"), name_key("b"));
        let long = "\u{130}".repeat(126);
        assert!(
            long.len() <= MAX_NAME_BYTES && long.len() > 250,
            "{}",
            long.len()
        );
        let key = name_key(&long);
        assert!(
            key.len() <= MAX_NAME_BYTES,
            "the key must be bounded, got {} bytes",
            key.len()
        );
        assert!(validate_snapshot_name(&key).is_ok(), "{key}");
        assert_ne!(name_key(&format!("{long}a")), name_key(&long));
        for n in ["\u{df}", "i", "I", "\u{212a}", "\u{c5}"] {
            assert!(validate_snapshot_name(&name_key(n)).is_ok(), "{n}");
        }
    }

    #[test]
    fn refs_and_paths() {
        for ok in ["main", "refs/heads/x", "v1.2.3", "0123abcd"] {
            assert!(validate_git_ref(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "-x",
            "--upload-pack=x",
            "a b",
            "a\nb",
            "a..b",
            "a@{1}",
            "a:b",
            "x/",
            "x.lock",
            "a\\b",
        ] {
            assert!(validate_git_ref(bad).is_err(), "{bad:?}");
        }
        assert!(validate_repo("/srv/r").is_ok());
        for bad in ["r", "-x", "/a\nb", "/a\0b"] {
            assert!(validate_repo(bad).is_err(), "{bad:?}");
        }
    }
}
