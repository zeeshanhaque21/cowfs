//! Snapshot name rules, as the backend sees them.
//!
//! The rule itself lives in `cowfs-snapname`, which the control API also depends on, so the two
//! cannot drift: they once did, and the control API accepted a name reserved for an interrupted
//! snapshot swap that the backend refuses.
//!
//! Two names that alias each other on a case-insensitive or normalising mount must not both exist,
//! so the collision key is NFC, lowercased, NFC again.

use crate::ControlError;

/// The API-level snapshot name rule. `&str` names are UTF-8 by construction;
/// [`validate_snapshot_name_bytes`] is what an adapter or the control API calls on bytes.
pub fn validate_snapshot_name(name: &str) -> Result<(), ControlError> {
    cowfs_snapname::validate_snapshot_name(name).map_err(ControlError::from)
}

/// [`validate_snapshot_name`] for bytes that came off a wire or a path.
pub fn validate_snapshot_name_bytes(name: &[u8]) -> Result<(), ControlError> {
    cowfs_snapname::validate_snapshot_name_bytes(name).map_err(ControlError::from)
}

/// The collision key of a snapshot name: NFC, lowercased, NFC again. Two names with the same key
/// alias each other on a case-insensitive or normalising mount, so a backend refuses to hold both.
/// The key is bounded by [`cowfs_snapname::NAME_MAX`], so it is itself a legal snapshot name.
pub fn name_key(name: &str) -> String {
    cowfs_snapname::name_key(name)
}

impl From<cowfs_snapname::NameError> for ControlError {
    fn from(e: cowfs_snapname::NameError) -> Self {
        Self::InvalidName(e.why())
    }
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
            "slot.cowfs-swap0",
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
    fn a_key_is_bounded_and_is_a_legal_name() {
        let long = "\u{130}".repeat(126);
        assert!(
            long.len() <= crate::NAME_MAX && long.len() > 250,
            "{}",
            long.len()
        );
        let key = name_key(&long);
        assert!(
            key.len() <= crate::NAME_MAX,
            "the key must be bounded, got {} bytes",
            key.len()
        );
        assert!(validate_snapshot_name(&key).is_ok(), "{key}");
        assert_ne!(name_key(&format!("{long}a")), name_key(&long));
    }

    #[test]
    fn non_utf8_bytes_are_refused() {
        assert!(validate_snapshot_name_bytes(b"a").is_ok());
        assert!(validate_snapshot_name_bytes(&[0xff, 0xfe]).is_err());
        assert!(validate_snapshot_name_bytes(b"a/b").is_err());
        assert!(validate_snapshot_name_bytes(b".x").is_err());
        assert!(validate_snapshot_name_bytes(b"a.cowfs-swap0").is_err());
    }
}
