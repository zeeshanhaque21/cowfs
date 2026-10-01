//! Conversions between `cowfs_vfs` types and NFSv3 wire types.
use cowfs_vfs::{Attr, FileKind, SetAttr, SetTime, Timestamp, MODE_MASK};
use nfsserve::nfs::{
    fattr3, ftype3, nfsstat3, nfstime3, sattr3, set_atime, set_mode3, set_mtime, set_size3,
    specdata3,
};

/// The `fsid` reported for the one file system a server exports ("cowfs" in ASCII).
pub const FSID: u64 = 0x0063_6f77_6673;

/// NFSv3 times are unsigned 32-bit seconds: earlier times clamp to 0 and later ones to `u32::MAX`.
pub fn nfstime(t: Timestamp) -> nfstime3 {
    nfstime3 {
        seconds: u32::try_from(t.secs.max(0)).unwrap_or(u32::MAX),
        nseconds: t.nanos.min(999_999_999),
    }
}

pub fn timestamp(t: nfstime3) -> Timestamp {
    Timestamp {
        secs: i64::from(t.seconds),
        nanos: t.nseconds.min(999_999_999),
    }
}

/// `FileKind` is `#[non_exhaustive]`, so a kind this build does not know is a server fault and
/// not something to serve as a regular file.
pub fn ftype(kind: FileKind) -> Result<ftype3, nfsstat3> {
    Ok(match kind {
        FileKind::Regular => ftype3::NF3REG,
        FileKind::Directory => ftype3::NF3DIR,
        FileKind::Symlink => ftype3::NF3LNK,
        other => {
            eprintln!("cowfs-nfs: the Vfs reported an unknown file kind: {other:?}");
            return Err(nfsstat3::NFS3ERR_SERVERFAULT);
        }
    })
}

pub fn fattr(a: &Attr) -> Result<fattr3, nfsstat3> {
    Ok(fattr3 {
        ftype: ftype(a.kind)?,
        mode: a.mode & MODE_MASK,
        nlink: a.nlink,
        uid: a.uid,
        gid: a.gid,
        size: a.size,
        used: a.blocks.saturating_mul(512),
        rdev: specdata3::default(),
        fsid: FSID,
        fileid: a.ino,
        atime: nfstime(a.atime),
        mtime: nfstime(a.mtime),
        ctime: nfstime(a.ctime),
    })
}

/// uid and gid are dropped: everything belongs to the mounter.
pub fn set_attr(s: &sattr3) -> SetAttr {
    SetAttr {
        mode: match s.mode {
            set_mode3::mode(m) => Some(m & MODE_MASK),
            set_mode3::Void => None,
        },
        size: match s.size {
            set_size3::size(n) => Some(n),
            set_size3::Void => None,
        },
        atime: match s.atime {
            set_atime::SET_TO_SERVER_TIME => Some(SetTime::Now),
            set_atime::SET_TO_CLIENT_TIME(t) => Some(SetTime::At(timestamp(t))),
            set_atime::DONT_CHANGE => None,
        },
        mtime: match s.mtime {
            set_mtime::SET_TO_SERVER_TIME => Some(SetTime::Now),
            set_mtime::SET_TO_CLIENT_TIME(t) => Some(SetTime::At(timestamp(t))),
            set_mtime::DONT_CHANGE => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nfsserve::nfs::uid3;

    fn attr(kind: FileKind) -> Attr {
        Attr {
            ino: 42,
            kind,
            mode: 0o4755,
            nlink: 3,
            uid: 501,
            gid: 20,
            size: 1234,
            blocks: 3,
            atime: Timestamp { secs: 10, nanos: 1 },
            mtime: Timestamp {
                secs: 11,
                nanos: 999_999_999,
            },
            ctime: Timestamp {
                secs: 12,
                nanos: 500,
            },
        }
    }

    #[test]
    fn attributes_convert_exactly() {
        let f = fattr(&attr(FileKind::Regular)).unwrap();
        assert_eq!(f.ftype, ftype3::NF3REG);
        assert_eq!((f.mode, f.nlink, f.uid, f.gid), (0o4755, 3, 501, 20));
        assert_eq!(
            (f.size, f.used, f.fileid, f.fsid),
            (1234, 3 * 512, 42, FSID)
        );
        assert_eq!((f.atime.seconds, f.atime.nseconds), (10, 1));
        assert_eq!((f.mtime.seconds, f.mtime.nseconds), (11, 999_999_999));
        assert_eq!((f.ctime.seconds, f.ctime.nseconds), (12, 500));
        assert_eq!(
            fattr(&attr(FileKind::Directory)).unwrap().ftype,
            ftype3::NF3DIR
        );
        assert_eq!(
            fattr(&attr(FileKind::Symlink)).unwrap().ftype,
            ftype3::NF3LNK
        );
    }

    #[test]
    fn mode_is_masked_and_used_saturates() {
        let mut a = attr(FileKind::Regular);
        a.mode = 0o170_755;
        a.blocks = u64::MAX;
        let f = fattr(&a).unwrap();
        assert_eq!(f.mode, 0o755);
        assert_eq!(f.used, u64::MAX);
    }

    #[test]
    fn times_clamp_to_the_wire_range() {
        assert_eq!(nfstime(Timestamp { secs: -5, nanos: 7 }).seconds, 0);
        assert_eq!(
            nfstime(Timestamp {
                secs: i64::MAX,
                nanos: 0
            })
            .seconds,
            u32::MAX
        );
        assert_eq!(
            nfstime(Timestamp {
                secs: 1,
                nanos: 1_500_000_000
            })
            .nseconds,
            999_999_999
        );
        let t = nfstime3 {
            seconds: u32::MAX,
            nseconds: 5,
        };
        assert_eq!(
            timestamp(t),
            Timestamp {
                secs: i64::from(u32::MAX),
                nanos: 5
            }
        );
        assert_eq!(
            timestamp(nfstime3 {
                seconds: 1,
                nseconds: u32::MAX
            })
            .nanos,
            999_999_999
        );
    }

    #[test]
    fn sattr_maps_to_setattr() {
        let none = set_attr(&sattr3::default());
        assert_eq!(none, SetAttr::default());
        let s = sattr3 {
            mode: set_mode3::mode(0o170_644),
            uid: nfsserve::nfs::set_uid3::uid(0 as uid3),
            gid: nfsserve::nfs::set_gid3::Void,
            size: set_size3::size(9),
            atime: set_atime::SET_TO_SERVER_TIME,
            mtime: set_mtime::SET_TO_CLIENT_TIME(nfstime3 {
                seconds: 5,
                nseconds: 6,
            }),
        };
        let c = set_attr(&s);
        assert_eq!(c.mode, Some(0o644));
        assert_eq!(c.size, Some(9));
        assert_eq!(c.atime, Some(SetTime::Now));
        assert_eq!(c.mtime, Some(SetTime::At(Timestamp { secs: 5, nanos: 6 })));
    }
}
