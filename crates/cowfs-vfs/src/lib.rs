//! The interface between mount adapters and the cowfs core. Contract: `docs/v1-architecture.md`.
//!
//! The doc comments on `Vfs`, `Attr` and `Ino` are the contract: the conformance suite in
//! `cowfs-vfs-test` enforces them and an implementation that follows them passes it.
//! `Error::errno` depends on `libc`, so this crate builds on macOS and Linux only.

#[cfg(not(unix))]
compile_error!("cowfs-vfs needs libc errno values and builds on unix only (macOS and Linux)");

mod error;
mod types;
mod vfs;

pub use error::{Error, Result};
pub use types::FallocMode;
pub use types::{
    dev_major, dev_minor, makedev, mknod_needs_root, validate_name, Attr, DirEntry, DirEntryPlus, FileHandle,
    FileKind, Ino, ReadDir, ReadDirPlus, RenameFlags, SetAttr, SetTime, StatFs, Timestamp,
    XattrFlags, MODE_MASK, NAME_MAX, ROOT_INO,
};
pub use vfs::Vfs;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mknod_privilege_rule() {
        let (chr, blk) = (FileKind::CharDevice, FileKind::BlockDevice);
        // The whiteout is exactly a character device numbered 0:0.
        assert!(!mknod_needs_root(chr, 0));
        assert!(mknod_needs_root(blk, 0), "a block 0:0 is not a whiteout");
        assert!(mknod_needs_root(chr, makedev(0, 1)));
        assert!(mknod_needs_root(chr, makedev(1, 0)));
        assert!(mknod_needs_root(blk, makedev(8, 0)));
        for k in [FileKind::Fifo, FileKind::Socket] {
            assert!(!mknod_needs_root(k, 0), "{k:?}");
        }
    }

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
    fn errno_table_is_exact() {
        #[cfg(target_os = "macos")]
        let no_attr = libc::ENOATTR;
        #[cfg(not(target_os = "macos"))]
        let no_attr = libc::ENODATA;
        let table = [
            (Error::NotFound, libc::ENOENT),
            (Error::Exists, libc::EEXIST),
            (Error::NotDir, libc::ENOTDIR),
            (Error::IsDir, libc::EISDIR),
            (Error::NotEmpty, libc::ENOTEMPTY),
            (Error::InvalidArgument, libc::EINVAL),
            (Error::NameTooLong, libc::ENAMETOOLONG),
            (Error::NoSpace, libc::ENOSPC),
            (Error::FileTooBig, libc::EFBIG),
            (Error::PermissionDenied, libc::EACCES),
            (Error::TooManyLinks, libc::EMLINK),
            (Error::NotSupported, libc::ENOTSUP),
            (Error::Stale, libc::ESTALE),
            (Error::NoAttr, no_attr),
            (Error::Range, libc::ERANGE),
            (Error::ReadOnly, libc::EROFS),
            (Error::CrossDevice, libc::EXDEV),
            (Error::Corrupt("x".into()), libc::EIO),
            (Error::Io("x".into()), libc::EIO),
            (Error::Retry, libc::EAGAIN),
        ];
        for (e, errno) in &table {
            assert_eq!(e.errno(), *errno, "{e:?}");
        }
        let mut seen: Vec<i32> = table.iter().map(|(_, n)| *n).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len(),
            table.len() - 1,
            "only Corrupt and Io share an errno"
        );
    }

    #[test]
    fn constants_are_pinned() {
        assert_eq!(MODE_MASK, 0o7777);
        assert_eq!(NAME_MAX, 255);
        assert_eq!(ROOT_INO, 1);
    }

    #[test]
    fn vfs_is_object_safe_and_shareable() {
        fn takes(_: &dyn Vfs) {}
        fn shares(v: std::sync::Arc<dyn Vfs>) {
            std::thread::spawn(move || takes(&*v));
        }
        let _ = (takes, shares);
    }
}
