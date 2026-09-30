//! The interface between mount adapters and the cowfs core. Contract: `docs/v1-architecture.md`.

mod error;
mod types;
mod vfs;

pub use error::{Error, Result};
pub use types::{
    validate_name, Attr, DirEntry, FileHandle, FileKind, Ino, ReadDir, RenameFlags, SetAttr,
    SetTime, StatFs, Timestamp, XattrFlags, MODE_MASK, NAME_MAX, ROOT_INO,
};
pub use vfs::Vfs;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(validate_name(b"a"), Ok(()));
        assert_eq!(validate_name(b""), Err(Error::InvalidArgument));
        assert_eq!(validate_name(b"."), Err(Error::InvalidArgument));
        assert_eq!(validate_name(b".."), Err(Error::InvalidArgument));
        assert_eq!(validate_name(b"a/b"), Err(Error::InvalidArgument));
        assert_eq!(validate_name(b"a\0b"), Err(Error::InvalidArgument));
        assert_eq!(validate_name(&[b'x'; NAME_MAX]), Ok(()));
        assert_eq!(
            validate_name(&[b'x'; NAME_MAX + 1]),
            Err(Error::NameTooLong)
        );
    }

    #[test]
    fn errno_maps_to_platform_values() {
        assert_eq!(Error::NotFound.errno(), libc::ENOENT);
        assert_eq!(Error::NotEmpty.errno(), libc::ENOTEMPTY);
        assert_eq!(Error::Io("x".into()).errno(), libc::EIO);
        assert_ne!(Error::NoAttr.errno(), 0);
    }

    #[test]
    fn vfs_is_object_safe() {
        fn takes(_: &dyn Vfs) {}
        let _ = takes;
    }
}
