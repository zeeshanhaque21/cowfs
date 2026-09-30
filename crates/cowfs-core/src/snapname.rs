//! Snapshot name rules, the same ones the CLI enforces (`crates/cowfs-ctl/src/validate.rs` on
//! branch `v1/13-cli`).
//!
//! Two names that alias each other on a case-insensitive or normalising mount must not both exist,
//! so the collision key is NFC, lowercased, NFC again.
//!
//! The test table is copied from the CLI's so the two can be reconciled into one shared crate
//! later (see `docs/v1-core.md`, "Requests of store and meta"). The CLI's `name_key` is
//! reproduced here rather than depended on, because the two crates are on different branches.

use unicode_normalization::UnicodeNormalization;

use crate::{ControlError, NAME_MAX};

/// The API-level snapshot name rule: non-empty, at most 255 bytes, no `/`, no control characters
/// (NUL, newline and ESC included), and no leading `.` (which also rules out `.`, `..`, `._*` and
/// `.nfs*`). Names are `&str`, so they are UTF-8 by construction and non-UTF-8 cannot be expressed;
/// [`validate_snapshot_name_bytes`] is what an adapter or the control API calls before encoding.
pub fn validate_snapshot_name(name: &str) -> Result<(), ControlError> {
    let bad = |why: &'static str| Err(ControlError::InvalidName(why));
    if name.is_empty() {
        return bad("empty");
    }
    if name.len() > NAME_MAX {
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

/// [`validate_snapshot_name`] for bytes that came off a wire or a path.
pub fn validate_snapshot_name_bytes(name: &[u8]) -> Result<(), ControlError> {
    let s = std::str::from_utf8(name).map_err(|_| ControlError::InvalidName("not valid UTF-8"))?;
    validate_snapshot_name(s)
}

/// The collision key of a snapshot name: NFC, lowercased, NFC again. Two names with the same key
/// alias each other on a case-insensitive or normalising mount, so a backend refuses to hold both.
pub fn name_key(name: &str) -> String {
    name.nfc()
        .collect::<String>()
        .to_lowercase()
        .nfc()
        .collect()
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
    fn non_utf8_bytes_are_refused() {
        assert!(validate_snapshot_name_bytes(b"a").is_ok());
        assert!(validate_snapshot_name_bytes(&[0xff, 0xfe]).is_err());
        assert!(validate_snapshot_name_bytes(b"a/b").is_err());
        assert!(validate_snapshot_name_bytes(b".x").is_err());
    }
}
