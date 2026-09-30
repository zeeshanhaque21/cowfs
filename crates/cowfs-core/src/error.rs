//! The one table that maps store and meta errors to `cowfs_vfs::Error`.

use std::io::ErrorKind;

use cowfs_vfs::Error;

/// Meta error to vfs error. `NotFound` stays `NotFound`; call sites where the inode itself is
/// the subject turn it into `Stale` with [`stale`].
pub(crate) fn from_meta(e: cowfs_meta::Error) -> Error {
    use cowfs_meta::Error as M;
    let e_text = |e: &M| e.to_string();
    match e {
        M::NotFound => Error::NotFound,
        M::Exists => Error::Exists,
        M::NotDir => Error::NotDir,
        M::IsDir => Error::IsDir,
        M::NotEmpty => Error::NotEmpty,
        M::Invalid(_) => Error::InvalidArgument,
        M::NameTooLong => Error::NameTooLong,
        M::NoAttr => Error::NoAttr,
        M::TooBig => Error::Range,
        M::NoSuchSnapshot => Error::Stale,
        M::SnapshotExists => Error::Exists,
        M::Corrupt(m) => Error::Corrupt(m),
        M::Inconsistent(v) => Error::Corrupt(v.join("; ")),
        M::Storage(m) => Error::Io(m),
        M::Hook(e) => from_io(&e),
        M::LimitExceeded(_) => Error::NoSpace,
        M::Format(m) => Error::Corrupt(m),
        M::Conflict | M::NeedsRechunk | M::Reentrant => Error::Io(e_text(&e)),
        M::Closed => Error::Io(e_text(&e)),
    }
}

/// Store error to vfs error.
pub(crate) fn from_store(e: cowfs_store::Error) -> Error {
    use cowfs_store::Error as S;
    match e {
        S::Io(e) => from_io(&e),
        S::NotFound(id) => Error::Corrupt(format!("block {id} named by a file is missing")),
        S::Corrupt { .. } | S::HashMismatch(_) => Error::Corrupt(e.to_string()),
        S::BlockTooLarge(_) | S::BadPack { .. } | S::Locked(_) => Error::Io(e.to_string()),
    }
}

pub(crate) fn from_io(e: &std::io::Error) -> Error {
    if e.kind() == ErrorKind::StorageFull {
        Error::NoSpace
    } else {
        Error::Io(e.to_string())
    }
}

/// A missing inode is `Stale`, everything else is unchanged.
pub(crate) fn stale(e: Error) -> Error {
    if e == Error::NotFound {
        Error::Stale
    } else {
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cowfs_store::BlockId;
    use std::io;

    #[test]
    fn meta_table() {
        use cowfs_meta::Error as M;
        let cases: Vec<(M, Error)> = vec![
            (M::NotFound, Error::NotFound),
            (M::Exists, Error::Exists),
            (M::NotDir, Error::NotDir),
            (M::IsDir, Error::IsDir),
            (M::NotEmpty, Error::NotEmpty),
            (M::Invalid("x"), Error::InvalidArgument),
            (M::NameTooLong, Error::NameTooLong),
            (M::NoAttr, Error::NoAttr),
            (M::TooBig, Error::Range),
            (M::NoSuchSnapshot, Error::Stale),
            (M::SnapshotExists, Error::Exists),
            (M::Corrupt("c".into()), Error::Corrupt("c".into())),
            (
                M::Inconsistent(vec!["a".into(), "b".into()]),
                Error::Corrupt("a; b".into()),
            ),
            (M::Storage("s".into()), Error::Io("s".into())),
            (M::Hook(io::Error::other("h")), Error::Io("h".to_string())),
            (
                M::Hook(io::Error::from(ErrorKind::StorageFull)),
                Error::NoSpace,
            ),
            (M::LimitExceeded("x"), Error::NoSpace),
            (M::Format("f".into()), Error::Corrupt("f".into())),
            (M::Conflict, Error::Io("content version conflict".into())),
            (M::Closed, Error::Io("metadata store is closed".into())),
        ];
        for (m, want) in cases {
            let label = format!("{m:?}");
            assert_eq!(from_meta(m), want, "{label}");
        }
    }

    #[test]
    fn store_table() {
        use cowfs_store::Error as S;
        let id = BlockId::of(b"x");
        assert!(matches!(from_store(S::NotFound(id)), Error::Corrupt(_)));
        assert!(matches!(from_store(S::HashMismatch(id)), Error::Corrupt(_)));
        assert!(matches!(
            from_store(S::Corrupt {
                pack: 0,
                offset: 0,
                reason: "r"
            }),
            Error::Corrupt(_)
        ));
        assert_eq!(
            from_store(S::Io(io::Error::from(ErrorKind::StorageFull))),
            Error::NoSpace
        );
        assert!(matches!(
            from_store(S::Io(io::Error::other("boom"))),
            Error::Io(_)
        ));
        assert!(matches!(from_store(S::BlockTooLarge(1)), Error::Io(_)));
        assert!(matches!(from_store(S::Locked("p".into())), Error::Io(_)));
    }

    #[test]
    fn stale_only_changes_not_found() {
        assert_eq!(stale(Error::NotFound), Error::Stale);
        assert_eq!(stale(Error::Exists), Error::Exists);
    }
}
