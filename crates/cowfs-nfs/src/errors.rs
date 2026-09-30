//! The one table that maps `cowfs_vfs::Error` to `nfsstat3`.
use cowfs_vfs::Error;
use nfsserve::nfs::nfsstat3;

pub fn nfsstat(e: &Error) -> nfsstat3 {
    match e {
        Error::NotFound => nfsstat3::NFS3ERR_NOENT,
        Error::Exists => nfsstat3::NFS3ERR_EXIST,
        Error::NotDir => nfsstat3::NFS3ERR_NOTDIR,
        Error::IsDir => nfsstat3::NFS3ERR_ISDIR,
        Error::NotEmpty => nfsstat3::NFS3ERR_NOTEMPTY,
        Error::InvalidArgument | Error::Range => nfsstat3::NFS3ERR_INVAL,
        Error::NameTooLong => nfsstat3::NFS3ERR_NAMETOOLONG,
        Error::NoSpace => nfsstat3::NFS3ERR_NOSPC,
        Error::PermissionDenied => nfsstat3::NFS3ERR_ACCES,
        Error::TooManyLinks => nfsstat3::NFS3ERR_MLINK,
        Error::NotSupported | Error::NoAttr => nfsstat3::NFS3ERR_NOTSUPP,
        Error::Stale => nfsstat3::NFS3ERR_STALE,
        Error::ReadOnly => nfsstat3::NFS3ERR_ROFS,
        Error::CrossDevice => nfsstat3::NFS3ERR_XDEV,
        Error::Corrupt(_) | Error::Io(_) => nfsstat3::NFS3ERR_IO,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_has_its_status() {
        let table = [
            (Error::NotFound, nfsstat3::NFS3ERR_NOENT),
            (Error::Exists, nfsstat3::NFS3ERR_EXIST),
            (Error::NotDir, nfsstat3::NFS3ERR_NOTDIR),
            (Error::IsDir, nfsstat3::NFS3ERR_ISDIR),
            (Error::NotEmpty, nfsstat3::NFS3ERR_NOTEMPTY),
            (Error::InvalidArgument, nfsstat3::NFS3ERR_INVAL),
            (Error::NameTooLong, nfsstat3::NFS3ERR_NAMETOOLONG),
            (Error::NoSpace, nfsstat3::NFS3ERR_NOSPC),
            (Error::PermissionDenied, nfsstat3::NFS3ERR_ACCES),
            (Error::TooManyLinks, nfsstat3::NFS3ERR_MLINK),
            (Error::NotSupported, nfsstat3::NFS3ERR_NOTSUPP),
            (Error::Stale, nfsstat3::NFS3ERR_STALE),
            (Error::NoAttr, nfsstat3::NFS3ERR_NOTSUPP),
            (Error::Range, nfsstat3::NFS3ERR_INVAL),
            (Error::ReadOnly, nfsstat3::NFS3ERR_ROFS),
            (Error::CrossDevice, nfsstat3::NFS3ERR_XDEV),
            (Error::Corrupt("x".into()), nfsstat3::NFS3ERR_IO),
            (Error::Io("x".into()), nfsstat3::NFS3ERR_IO),
        ];
        for (e, want) in table {
            assert_eq!(nfsstat(&e), want, "{e:?}");
        }
    }
}
