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

/// The collision key of a snapshot name: NFC, lowercased, NFC again. Two names with the same key
/// alias each other on a case-insensitive or normalising mount, so a backend must refuse to
/// hold both.
pub fn name_key(name: &str) -> String {
    name.nfc()
        .collect::<String>()
        .to_lowercase()
        .nfc()
        .collect()
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

    #[test]
    fn keys_fold_case_and_normalisation() {
        assert_eq!(name_key("Foo"), name_key("fOO"));
        assert_eq!(name_key("caf\u{e9}"), name_key("cafe\u{301}"));
        assert_ne!(name_key("a"), name_key("b"));
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
