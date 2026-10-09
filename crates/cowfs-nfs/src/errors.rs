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
        Error::FileTooBig => nfsstat3::NFS3ERR_FBIG,
        Error::PermissionDenied => nfsstat3::NFS3ERR_ACCES,
        Error::TooManyLinks => nfsstat3::NFS3ERR_MLINK,
        Error::NotSupported | Error::NoAttr => nfsstat3::NFS3ERR_NOTSUPP,
        Error::Stale => nfsstat3::NFS3ERR_STALE,
        Error::ReadOnly => nfsstat3::NFS3ERR_ROFS,
        Error::CrossDevice => nfsstat3::NFS3ERR_XDEV,
        Error::Corrupt(_) | Error::Io(_) => nfsstat3::NFS3ERR_IO,
        // The Vfs says the call is worth repeating later: the NFSv3 status that asks for that.
        Error::Retry => nfsstat3::NFS3ERR_JUKEBOX,
        other => unknown(other),
    }
}

/// `Error` is `#[non_exhaustive]`, so a variant added after this build has no status here. It is
/// a server fault, never something benign, and it is logged so it cannot pass unnoticed.
fn unknown(e: &Error) -> nfsstat3 {
    eprintln!("cowfs-nfs: the Vfs reported an unknown error: {e:?}");
    nfsstat3::NFS3ERR_SERVERFAULT
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One more than `KNOWN` has entries whenever the trait gains a variant.
    const VARIANTS: usize = 20;

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
            (Error::FileTooBig, nfsstat3::NFS3ERR_FBIG),
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
            (Error::Retry, nfsstat3::NFS3ERR_JUKEBOX),
        ];
        for (e, want) in table {
            assert_eq!(nfsstat(&e), want, "{e:?}");
        }
    }

    /// `Error` is `#[non_exhaustive]`, so a new variant cannot be caught by the compiler here.
    /// It has to be caught by hand: KNOWN is every variant this build knows, and VARIANTS is how
    /// many there are. Adding a variant to the trait without adding it to KNOWN fails this test,
    /// which is where the decision about its status belongs.
    const KNOWN: &[Error] = &[
        Error::NotFound,
        Error::Exists,
        Error::NotDir,
        Error::IsDir,
        Error::NotEmpty,
        Error::InvalidArgument,
        Error::NameTooLong,
        Error::NoSpace,
        Error::FileTooBig,
        Error::PermissionDenied,
        Error::TooManyLinks,
        Error::NotSupported,
        Error::Stale,
        Error::NoAttr,
        Error::Range,
        Error::ReadOnly,
        Error::CrossDevice,
        Error::Corrupt(String::new()),
        Error::Io(String::new()),
        Error::Retry,
    ];

    #[test]
    fn every_variant_of_the_trait_has_a_status() {
        assert_eq!(KNOWN.len(), VARIANTS, "cowfs-vfs::Error gained a variant");
        for e in KNOWN {
            assert_ne!(
                nfsstat(e),
                nfsstat3::NFS3ERR_SERVERFAULT,
                "{e:?} must not fall through to the unknown arm"
            );
        }
    }

    /// The wildcard arm itself, reached only through `unknown` because no value of a
    /// `#[non_exhaustive]` enum outside its crate can be an unknown variant.
    #[test]
    fn an_unknown_variant_is_a_logged_server_fault() {
        assert_eq!(unknown(&Error::Retry), nfsstat3::NFS3ERR_SERVERFAULT);
    }
}
