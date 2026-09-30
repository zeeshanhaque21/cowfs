//! Pure conversions between kernel protocol values and `cowfs-vfs` types. No kernel access.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cowfs_vfs::{RenameFlags, Timestamp, XattrFlags};

const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;
const RENAME_NOREPLACE: u32 = 1;
const RENAME_EXCHANGE: u32 = 2;
const XATTR_CREATE: i32 = 1;
const XATTR_REPLACE: i32 = 2;

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
