//! The one snapshot-name rule, shared by the backend and the control API.
//!
//! Both crates used to carry their own copy of this rule and had already drifted: the backend
//! refused a name reserved for an interrupted snapshot swap and the control API accepted it, so
//! the API took a request the backend could never hold.
//! The rule is here so there is nothing left to drift.
//!
//! A name is non-empty, at most [`NAME_MAX`] bytes, has no leading `.` (which also rules out `.`,
//! `..`, `._*` and `.nfs*`), does not contain [`RESERVED`], no `/`, and no control character.
//! Names are `&str`, so they are UTF-8 by construction;
//! [`validate_snapshot_name_bytes`] is what an adapter or the control API calls on bytes that came
//! off a wire or a path.

use unicode_normalization::UnicodeNormalization;

/// Longest snapshot name in bytes.
pub const NAME_MAX: usize = 255;

/// Reserved in a snapshot name, because it is the marker of a staged swap
/// (`<target>.cowfs-swap<N>`), and a caller must not be able to create or move a snapshot into it.
pub const RESERVED: &str = ".cowfs-swap";

/// Why a snapshot name was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameError {
    /// Empty.
    Empty,
    /// Longer than [`NAME_MAX`] bytes.
    TooLong,
    /// Starts with a dot.
    LeadingDot,
    /// Contains [`RESERVED`].
    Reserved,
    /// Contains a slash.
    Slash,
    /// Contains a control character.
    Control,
    /// Not valid UTF-8.
    NotUtf8,
}

impl NameError {
    /// The reason, worded as it appears in an error message.
    pub fn why(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::TooLong => "longer than 255 bytes",
            Self::LeadingDot => "must not start with a dot",
            Self::Reserved => "is reserved for an interrupted snapshot swap",
            Self::Slash => "must not contain a slash",
            Self::Control => "must not contain control characters",
            Self::NotUtf8 => "not valid UTF-8",
        }
    }
}

/// True for a name only the swap protocol may use.
pub fn is_reserved(name: &str) -> bool {
    name.contains(RESERVED)
}

/// The API-level snapshot name rule.
pub fn validate_snapshot_name(name: &str) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name.len() > NAME_MAX {
        return Err(NameError::TooLong);
    }
    if name.starts_with('.') {
        return Err(NameError::LeadingDot);
    }
    if is_reserved(name) {
        return Err(NameError::Reserved);
    }
    if name.contains('/') {
        return Err(NameError::Slash);
    }
    if name.chars().any(char::is_control) {
        return Err(NameError::Control);
    }
    Ok(())
}

/// [`validate_snapshot_name`] for bytes that came off a wire or a path.
pub fn validate_snapshot_name_bytes(name: &[u8]) -> Result<(), NameError> {
    let s = std::str::from_utf8(name).map_err(|_| NameError::NotUtf8)?;
    validate_snapshot_name(s)
}

/// The collision key of a snapshot name: NFC, lowercased, NFC again.
/// Two names with the same key alias each other on a case-insensitive or normalising mount, so a
/// backend refuses to hold both.
///
/// Folding can grow a name past [`NAME_MAX`] (255 bytes of dotted capital I fold to 378 bytes), so
/// a key longer than the bound is replaced by a hash of the folded form.
/// That keeps the key a legal snapshot name, at the cost of a theoretical hash collision.
pub fn name_key(name: &str) -> String {
    let folded: String = name
        .nfc()
        .collect::<String>()
        .to_lowercase()
        .nfc()
        .collect();
    if folded.len() <= NAME_MAX {
        return folded;
    }
    let digest = blake3::hash(folded.as_bytes()).to_hex().to_string();
    format!("#{digest}")
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
    fn the_reserved_marker_is_refused_anywhere_in_the_name() {
        for bad in [
            "a.cowfs-swap0",
            "slot.cowfs-swap0",
            "slot.cowfs-swap1",
            "a.cowfs-swap",
            "slot.cowfs-swap0/repo",
        ] {
            assert_eq!(
                validate_snapshot_name(bad),
                Err(NameError::Reserved),
                "{bad:?}"
            );
        }
        assert!(is_reserved("slot.cowfs-swap0"));
        assert!(!is_reserved("slot"));
        // the marker itself has no leading dot, so only the reserved rule can refuse it
        assert!(validate_snapshot_name("cowfs-swap").is_ok());
        assert!(validate_snapshot_name("slot.cowfs-swap").is_err());
    }

    #[test]
    fn keys_fold_case_and_normalisation() {
        assert_eq!(name_key("Foo"), name_key("fOO"));
        assert_eq!(name_key("caf\u{e9}"), name_key("cafe\u{301}"));
        assert_ne!(name_key("a"), name_key("b"));
        let long = "\u{130}".repeat(126);
        assert!(long.len() <= NAME_MAX && long.len() > 250, "{}", long.len());
        let key = name_key(&long);
        assert!(
            key.len() <= NAME_MAX,
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
    fn a_key_is_always_a_legal_name() {
        for n in [
            "a",
            "Foo",
            "caf\u{e9}",
            &"\u{130}".repeat(126),
            &"x".repeat(255),
        ] {
            let key = name_key(n);
            assert!(key.len() <= NAME_MAX, "{n:?}: {} bytes", key.len());
            assert!(validate_snapshot_name(&key).is_ok(), "{n:?}: {key:?}");
        }
    }

    #[test]
    fn non_utf8_bytes_are_refused() {
        assert!(validate_snapshot_name_bytes(b"a").is_ok());
        assert_eq!(
            validate_snapshot_name_bytes(&[0xff, 0xfe]),
            Err(NameError::NotUtf8)
        );
        assert_eq!(validate_snapshot_name_bytes(b"a/b"), Err(NameError::Slash));
        assert_eq!(
            validate_snapshot_name_bytes(b".x"),
            Err(NameError::LeadingDot)
        );
        assert_eq!(
            validate_snapshot_name_bytes(b"a.cowfs-swap0"),
            Err(NameError::Reserved)
        );
    }

    #[test]
    fn every_refusal_says_why() {
        for bad in ["", "a/b", ".x", "a.cowfs-swap0", "a\nb", &"x".repeat(256)] {
            let why = validate_snapshot_name(bad).unwrap_err().why();
            assert!(!why.is_empty(), "{bad:?}");
        }
    }
}
