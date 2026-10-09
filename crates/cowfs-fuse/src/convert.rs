//! Pure conversions between kernel protocol values and `cowfs-vfs` types. No kernel access.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cowfs_vfs::{Attr, FileKind, RenameFlags, Timestamp, XattrFlags, MODE_MASK};

const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;
const RENAME_NOREPLACE: u32 = 1;
const RENAME_EXCHANGE: u32 = 2;
const XATTR_CREATE: i32 = 1;
const XATTR_REPLACE: i32 = 2;
const MAX_NLINK: u32 = 1 << 30;

/// Converts a timestamp to `SystemTime`, saturating to the epoch if it is out of range.
pub fn to_system_time(ts: Timestamp) -> SystemTime {
    let nanos = Duration::from_nanos(u64::from(ts.nanos.min(999_999_999)));
    let t = if ts.secs >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(ts.secs.unsigned_abs()))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(ts.secs.unsigned_abs()))
    };
    t.and_then(|t| t.checked_add(nanos)).unwrap_or(UNIX_EPOCH)
}

/// Converts a `SystemTime` to a timestamp whose `nanos` is always in `0..1_000_000_000`.
pub fn from_system_time(t: SystemTime) -> Timestamp {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => Timestamp {
            secs: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
            nanos: d.subsec_nanos(),
        },
        Err(e) => {
            let d = e.duration();
            let secs = i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
            match d.subsec_nanos() {
                0 => Timestamp {
                    secs: -secs,
                    nanos: 0,
                },
                n => Timestamp {
                    secs: -secs - 1,
                    nanos: 1_000_000_000 - n,
                },
            }
        }
    }
}

/// True when the `mknod` mode asks for a regular file (the only type cowfs supports).
pub fn mknod_is_regular(mode: u32) -> bool {
    matches!(mode & S_IFMT, 0 | S_IFREG)
}

/// Maps `renameat2` flags. `RENAME_EXCHANGE` and unknown flags are refused with an errno.
pub fn rename_flags(flags: u32) -> Result<RenameFlags, i32> {
    if flags & RENAME_EXCHANGE != 0 {
        return Err(libc::ENOTSUP);
    }
    if flags & !RENAME_NOREPLACE != 0 {
        return Err(libc::EINVAL);
    }
    Ok(RenameFlags {
        no_replace: flags & RENAME_NOREPLACE != 0,
    })
}

/// Maps `setxattr` flags. Unknown bits are `EINVAL`.
pub fn xattr_flags(flags: i32) -> Result<XattrFlags, i32> {
    if flags & !(XATTR_CREATE | XATTR_REPLACE) != 0 {
        return Err(libc::EINVAL);
    }
    Ok(XattrFlags {
        create: flags & XATTR_CREATE != 0,
        replace: flags & XATTR_REPLACE != 0,
    })
}

/// How to answer `getxattr` or `listxattr` for a value of a given length.
#[derive(Debug, PartialEq, Eq)]
pub enum XattrReply {
    /// The caller passed size 0 and only wants to know how big the value is.
    Size(u32),
    /// The value fits in the caller's buffer.
    Data,
    /// The caller's buffer is too small (`ERANGE`).
    TooSmall,
}

/// Applies the xattr size protocol: `size == 0` asks for the length, otherwise the value must fit.
pub fn xattr_reply(size: u32, len: usize) -> XattrReply {
    let Ok(len) = u32::try_from(len) else {
        return XattrReply::TooSmall;
    };
    if size == 0 {
        XattrReply::Size(len)
    } else if len <= size {
        XattrReply::Data
    } else {
        XattrReply::TooSmall
    }
}

/// Encodes attribute names as the NUL-terminated concatenation `listxattr` returns.
pub fn encode_xattr_names(names: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(names.iter().map(|n| n.len() + 1).sum());
    for n in names {
        out.extend_from_slice(n);
        out.push(0);
    }
    out
}

/// Clamps an `Attr` from the `Vfs` to what the kernel accepts, so one bad reply cannot make
/// `stat` fail for the whole mount. Returns the clamped attributes and whether anything changed.
/// A live file reports at least one link, a directory at least two; zero links are kept only
/// when `unlinked_ok` (an unlinked file that is still open).
pub fn sanitize(a: &Attr, unlinked_ok: bool) -> (Attr, bool) {
    let mut o = *a;
    o.mode &= MODE_MASK;
    o.size = o.size.min(i64::MAX as u64);
    o.blocks = o.blocks.min(i64::MAX as u64 >> 9);
    let min_links = match (o.kind, unlinked_ok) {
        // A removed directory keeps no links, like any other unlinked inode.
        (_, true) => 0,
        (FileKind::Directory, false) => 2,
        _ => 1,
    };
    o.nlink = o.nlink.clamp(min_links, MAX_NLINK);
    (o, o != *a)
}

/// Answers `SEEK_DATA` and `SEEK_HOLE`: the `Vfs` has no hole information, so everything below
/// `size` is data and the only hole starts at the end of the file.
pub fn seek(whence: i32, offset: i64, size: u64) -> Result<i64, i32> {
    let Ok(off) = u64::try_from(offset) else {
        return Err(libc::EINVAL);
    };
    if off >= size {
        return Err(libc::ENXIO);
    }
    match whence {
        libc::SEEK_DATA => Ok(offset),
        libc::SEEK_HOLE => i64::try_from(size).map_err(|_| libc::EINVAL),
        _ => Err(libc::EINVAL),
    }
}

/// Only `user.*` and `security.*` (and `trusted.*` for root) are extended attributes cowfs
/// stores. `system.*` (POSIX ACLs) and unknown namespaces are `ENOTSUP`, as the kernel does.
pub fn xattr_name_ok(name: &[u8], root: bool) -> Result<(), i32> {
    let trusted = name.starts_with(b"trusted.");
    if name.starts_with(b"user.") || name.starts_with(b"security.") || (trusted && root) {
        Ok(())
    } else if trusted {
        Err(libc::EPERM)
    } else {
        Err(libc::ENOTSUP)
    }
}

/// Everything belongs to the mounter, so a setattr naming another uid is a chown the filesystem
/// will not make: native answers EPERM to a non-root caller, and cowfs cannot store another owner
/// for any caller. Naming the mounter's uid changes nothing and passes. A gid is never refused:
/// it is accepted and ignored, as native lets an owner chgrp to a group they belong to.
/// Checked before anything is applied, so a refused request is not half done.
pub fn owner_unchanged(uid: Option<u32>, mounter: u32) -> Result<(), i32> {
    match uid {
        Some(u) if u != mounter => Err(libc::EPERM),
        _ => Ok(()),
    }
}

/// Mode-bit permission check for `access(2)` as the mounter's uid and gid.
/// Root may read and write anything, and execute anything with at least one execute bit.
pub fn access_allowed(mode: u32, owner: u32, group: u32, uid: u32, gid: u32, mask: i32) -> bool {
    let want = mask & (libc::R_OK | libc::W_OK | libc::X_OK);
    if want == 0 {
        return true;
    }
    if uid == 0 {
        return want & libc::X_OK == 0 || mode & 0o111 != 0;
    }
    let shift = if uid == owner {
        6
    } else if gid == group {
        3
    } else {
        0
    };
    let have = i32::try_from((mode >> shift) & 0o7).unwrap_or(0);
    have & want == want
}

#[cfg(test)]
mod tests {
    use super::*;
    use cowfs_vfs::Error;

    fn ts(secs: i64, nanos: u32) -> Timestamp {
        Timestamp { secs, nanos }
    }

    #[test]
    fn only_another_uid_is_refused() {
        assert_eq!(owner_unchanged(Some(8), 7), Err(libc::EPERM));
        assert_eq!(owner_unchanged(Some(7), 7), Ok(()));
        assert_eq!(owner_unchanged(None, 7), Ok(()));
    }

    #[test]
    fn time_round_trips_around_the_epoch() {
        for t in [
            ts(0, 0),
            ts(0, 1),
            ts(1_700_000_000, 123_456_789),
            ts(-1, 0),
            ts(-1, 999_999_999),
            ts(-5, 500_000_000),
            ts(-86_400 * 365 * 100, 1),
        ] {
            assert_eq!(from_system_time(to_system_time(t)), t, "{t:?}");
        }
    }

    #[test]
    fn time_conversion_never_panics_on_extremes() {
        let _ = to_system_time(ts(i64::MAX, 999_999_999));
        let _ = to_system_time(ts(i64::MIN, 999_999_999));
        let _ = to_system_time(ts(5, u32::MAX));
        let far = UNIX_EPOCH.checked_sub(Duration::from_secs(1 << 40));
        assert_eq!(from_system_time(far.unwrap_or(UNIX_EPOCH)).nanos, 0);
    }

    #[test]
    fn mknod_accepts_only_regular_files() {
        assert!(mknod_is_regular(0o100_644));
        assert!(mknod_is_regular(0o644));
        for kind in [
            0o010_000, 0o020_000, 0o060_000, 0o140_000, 0o040_000, 0o120_000,
        ] {
            assert!(!mknod_is_regular(kind | 0o644), "{kind:o}");
        }
    }

    #[test]
    fn rename_flag_mapping() {
        assert_eq!(rename_flags(0), Ok(RenameFlags { no_replace: false }));
        assert_eq!(rename_flags(1), Ok(RenameFlags { no_replace: true }));
        assert_eq!(rename_flags(2), Err(libc::ENOTSUP));
        assert_eq!(rename_flags(3), Err(libc::ENOTSUP));
        assert_eq!(rename_flags(4), Err(libc::EINVAL));
        assert_eq!(rename_flags(1 << 20), Err(libc::EINVAL));
    }

    #[test]
    fn xattr_flag_mapping() {
        assert_eq!(xattr_flags(0), Ok(XattrFlags::default()));
        assert_eq!(
            xattr_flags(1).map(|f| (f.create, f.replace)),
            Ok((true, false))
        );
        assert_eq!(
            xattr_flags(2).map(|f| (f.create, f.replace)),
            Ok((false, true))
        );
        assert_eq!(xattr_flags(4), Err(libc::EINVAL));
    }

    #[test]
    fn xattr_size_protocol() {
        assert_eq!(xattr_reply(0, 7), XattrReply::Size(7));
        assert_eq!(xattr_reply(0, 0), XattrReply::Size(0));
        assert_eq!(xattr_reply(7, 7), XattrReply::Data);
        assert_eq!(xattr_reply(100, 7), XattrReply::Data);
        assert_eq!(xattr_reply(6, 7), XattrReply::TooSmall);
        assert_eq!(xattr_reply(0, usize::MAX), XattrReply::TooSmall);
    }

    #[test]
    fn xattr_names_are_nul_terminated() {
        assert_eq!(encode_xattr_names(&[]), b"");
        assert_eq!(
            encode_xattr_names(&[b"user.a".to_vec(), b"user.bc".to_vec()]),
            b"user.a\0user.bc\0"
        );
    }

    #[test]
    fn access_uses_the_right_class() {
        let (o, g) = (1000, 100);
        assert!(access_allowed(0o600, o, g, o, g, libc::R_OK | libc::W_OK));
        assert!(!access_allowed(0o600, o, g, o, g, libc::X_OK));
        assert!(!access_allowed(0o060, o, g, o, 7, libc::R_OK));
        assert!(access_allowed(0o060, o, g, 5, g, libc::R_OK | libc::W_OK));
        assert!(!access_allowed(0o604, o, g, 5, 7, libc::W_OK));
        assert!(access_allowed(0o604, o, g, 5, 7, libc::R_OK));
        assert!(access_allowed(0o000, o, g, 5, 7, libc::F_OK));
    }

    #[test]
    fn access_for_root() {
        assert!(access_allowed(
            0o000,
            1000,
            100,
            0,
            0,
            libc::R_OK | libc::W_OK
        ));
        assert!(!access_allowed(0o644, 1000, 100, 0, 0, libc::X_OK));
        assert!(access_allowed(0o744, 1000, 100, 0, 0, libc::X_OK));
    }

    #[test]
    fn sanitize_clamps_what_the_kernel_would_reject() {
        use cowfs_vfs::Timestamp;
        let t = Timestamp::default();
        let mut a = Attr {
            ino: 3,
            kind: FileKind::Directory,
            mode: 0xFFFF_FFFF,
            nlink: 0,
            uid: 0,
            gid: 0,
            size: u64::MAX,
            blocks: u64::MAX,
            rdev: 0,
            atime: t,
            mtime: t,
            ctime: t,
        };
        let (o, changed) = sanitize(&a, false);
        assert!(changed);
        assert_eq!((o.mode, o.size, o.nlink), (MODE_MASK, i64::MAX as u64, 2));
        assert!(o.blocks <= i64::MAX as u64 >> 9);
        a.kind = FileKind::Regular;
        a.mode = 0o644;
        a.size = 5;
        a.blocks = 1;
        a.nlink = 1;
        assert_eq!(sanitize(&a, false), (a, false));
        a.nlink = 0;
        assert_eq!(sanitize(&a, false).0.nlink, 1);
        assert_eq!(sanitize(&a, true), (a, false));
        a.nlink = u32::MAX;
        assert_eq!(sanitize(&a, false).0.nlink, MAX_NLINK);
        // A removed directory reports no links, which the conformance suite pins.
        a.kind = FileKind::Directory;
        a.nlink = 0;
        assert_eq!(
            sanitize(&a, false).0.nlink,
            2,
            "a live directory has two links"
        );
        assert_eq!(sanitize(&a, true).0.nlink, 0, "a removed one has none");
    }

    #[test]
    fn seek_treats_everything_as_data() {
        assert_eq!(seek(libc::SEEK_DATA, 3, 10), Ok(3));
        assert_eq!(seek(libc::SEEK_HOLE, 3, 10), Ok(10));
        assert_eq!(seek(libc::SEEK_DATA, 10, 10), Err(libc::ENXIO));
        assert_eq!(seek(libc::SEEK_HOLE, 99, 10), Err(libc::ENXIO));
        assert_eq!(seek(libc::SEEK_DATA, -1, 10), Err(libc::EINVAL));
        assert_eq!(seek(libc::SEEK_SET, 1, 10), Err(libc::EINVAL));
    }

    #[test]
    fn xattr_namespaces() {
        assert_eq!(xattr_name_ok(b"user.a", false), Ok(()));
        assert_eq!(xattr_name_ok(b"security.selinux", false), Ok(()));
        assert_eq!(xattr_name_ok(b"trusted.x", true), Ok(()));
        assert_eq!(xattr_name_ok(b"trusted.x", false), Err(libc::EPERM));
        assert_eq!(
            xattr_name_ok(b"system.posix_acl_access", true),
            Err(libc::ENOTSUP)
        );
        assert_eq!(xattr_name_ok(b"system.foo", false), Err(libc::ENOTSUP));
        assert_eq!(xattr_name_ok(b"nonamespace", false), Err(libc::ENOTSUP));
        assert_eq!(xattr_name_ok(b"", false), Err(libc::ENOTSUP));
    }

    #[test]
    fn every_error_has_a_real_errno() {
        let all = [
            Error::NotFound,
            Error::Exists,
            Error::NotDir,
            Error::IsDir,
            Error::NotEmpty,
            Error::InvalidArgument,
            Error::NameTooLong,
            Error::NoSpace,
            Error::PermissionDenied,
            Error::TooManyLinks,
            Error::NotSupported,
            Error::Stale,
            Error::NoAttr,
            Error::Range,
            Error::ReadOnly,
            Error::CrossDevice,
            Error::Corrupt("x".into()),
            Error::Io("x".into()),
        ];
        for e in &all {
            let n = e.errno();
            assert!(n > 0 && n != libc::ENOSYS, "{e:?} -> {n}");
        }
        #[cfg(target_os = "linux")]
        assert_eq!(Error::NoAttr.errno(), libc::ENODATA);
        assert_eq!(Error::Corrupt("x".into()).errno(), libc::EIO);
    }
}
