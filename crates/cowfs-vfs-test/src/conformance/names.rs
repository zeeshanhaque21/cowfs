//! File names are raw bytes.
//!
//! Decisions pinned here: a name the trait forbids (`validate_name`) is rejected by every
//! operation that creates or looks up a name with exactly the error `validate_name`
//! reports, and a rejected operation changes nothing. `lookup`, `unlink` and `rmdir` of a
//! name longer than `NAME_MAX` are `NameTooLong`, like ENAMETOOLONG. Names are compared
//! byte for byte: no case folding, no Unicode normalisation, no trimming.

use cowfs_vfs::{validate_name, Error, RenameFlags, NAME_MAX, ROOT_INO};

use super::{Ctx, Outcome};

const BAD: [&[u8]; 6] = [b"", b".", b"..", b"a/b", b"a\0b", b"/"];

pub fn validate_name_cases(c: &Ctx) -> Outcome {
    ensure_eq!(NAME_MAX, 255, "NAME_MAX");
    ensure_eq!(validate_name(b"a"), Ok(()), "plain name");
    for bad in BAD {
        ensure_eq!(
            validate_name(bad),
            Err(Error::InvalidArgument),
            "validate_name({bad:?})"
        );
        ensure_err!(
            c.create(ROOT_INO, bad, 0o644),
            Error::InvalidArgument,
            "create {bad:?}"
        );
    }
    let long = vec![b'x'; NAME_MAX + 1];
    ensure_eq!(
        validate_name(&long),
        Err(Error::NameTooLong),
        "validate_name of 256 bytes"
    );
    ensure_err!(
        c.create(ROOT_INO, &long, 0o644),
        Error::NameTooLong,
        "create of 256 bytes"
    );
    ensure!(
        c.list(ROOT_INO)?.is_empty(),
        "a rejected create left an entry behind"
    );
    for ok in [
        &b"a"[..],
        b"...",
        b".hidden",
        b"a.",
        b" ",
        b"-x",
        b"a\\b",
        b"a\nb",
        b"\t",
        b"*?",
    ] {
        ensure_eq!(validate_name(ok), Ok(()), "validate_name({ok:?})");
        let a = c.create(ROOT_INO, ok, 0o644)?;
        ensure_eq!(c.lookup(ROOT_INO, ok)?.ino, a.ino, "lookup of {ok:?}");
    }
    Ok(())
}

pub fn name_max_ok_and_one_more_fails(c: &Ctx) -> Outcome {
    let max = vec![b'm'; NAME_MAX];
    let a = c.create(ROOT_INO, &max, 0o644)?;
    ensure_eq!(
        c.lookup(ROOT_INO, &max)?.ino,
        a.ino,
        "lookup of a 255 byte name"
    );
    ensure!(
        c.names(ROOT_INO)?.contains(&max),
        "255 byte name missing from the listing"
    );
    let dir_max = vec![b'd'; NAME_MAX];
    c.mkdir(ROOT_INO, &dir_max, 0o755)?;
    c.symlink(ROOT_INO, &[b's'; NAME_MAX], b"t")?;
    c.link(a.ino, ROOT_INO, &[b'l'; NAME_MAX])?;

    let long = vec![b'x'; NAME_MAX + 1];
    ensure_err!(
        c.create(ROOT_INO, &long, 0o644),
        Error::NameTooLong,
        "create"
    );
    ensure_err!(c.mkdir(ROOT_INO, &long, 0o755), Error::NameTooLong, "mkdir");
    ensure_err!(
        c.symlink(ROOT_INO, &long, b"t"),
        Error::NameTooLong,
        "symlink"
    );
    ensure_err!(c.link(a.ino, ROOT_INO, &long), Error::NameTooLong, "link");
    ensure_err!(c.lookup(ROOT_INO, &long), Error::NameTooLong, "lookup");
    ensure_err!(c.fs.unlink(ROOT_INO, &long), Error::NameTooLong, "unlink");
    ensure_err!(c.fs.rmdir(ROOT_INO, &long), Error::NameTooLong, "rmdir");
    ensure_err!(
        c.fs.rename(ROOT_INO, &max, ROOT_INO, &long, RenameFlags::default()),
        Error::NameTooLong,
        "rename to a 256 byte name"
    );
    ensure_eq!(
        c.lookup(ROOT_INO, &max)?.ino,
        a.ino,
        "source after a rejected rename"
    );
    ensure_eq!(
        c.list(ROOT_INO)?.len(),
        4,
        "entries after rejected operations"
    );
    Ok(())
}

pub fn non_utf8_names(c: &Ctx) -> Outcome {
    let names: [&[u8]; 5] = [
        &[0xff, 0xfe],
        &[0xc3, 0x28],
        &[0x80],
        &[b'a', 0xff, b'b'],
        &[0xf0, 0x9f, 0x92],
    ];
    let mut inos = Vec::new();
    for n in names {
        inos.push(c.create(ROOT_INO, n, 0o644)?.ino);
    }
    for (n, ino) in names.iter().zip(&inos) {
        ensure_eq!(c.lookup(ROOT_INO, n)?.ino, *ino, "lookup of {n:?}");
    }
    let mut listed: Vec<Vec<u8>> = c.names(ROOT_INO)?;
    listed.sort();
    let mut want: Vec<Vec<u8>> = names.iter().map(|n| n.to_vec()).collect();
    want.sort();
    ensure_eq!(listed, want, "listing of non-UTF-8 names");
    let d = c.mkdir(ROOT_INO, &[0xfe, 0xff], 0o755)?.ino;
    c.create(d, &[0xff], 0o644)?;
    c.fs.rename(
        d,
        &[0xff],
        ROOT_INO,
        &[0xff, 0xff, 0xff],
        RenameFlags::default(),
    )?;
    c.lookup(ROOT_INO, &[0xff, 0xff, 0xff])?;
    Ok(())
}

pub fn names_are_exact_bytes(c: &Ctx) -> Outcome {
    let nfc = "\u{e9}".as_bytes();
    let nfd = "e\u{301}".as_bytes();
    let names: [&[u8]; 8] = [b"a", b"A", b"a ", b" a", b"ab", b"a.", nfc, nfd];
    let mut inos = std::collections::HashSet::new();
    for n in names {
        ensure!(
            inos.insert(c.create(ROOT_INO, n, 0o644)?.ino),
            "{n:?} aliases another name"
        );
    }
    for n in names {
        c.lookup(ROOT_INO, n)?;
    }
    ensure_err!(
        c.lookup(ROOT_INO, b"ABC"),
        Error::NotFound,
        "lookup of a name that was never made"
    );
    ensure_err!(
        c.lookup(ROOT_INO, b"b"),
        Error::NotFound,
        "lookup of a missing name"
    );
    ensure_eq!(
        c.list(ROOT_INO)?.len(),
        names.len(),
        "entries in the listing"
    );
    c.fs.unlink(ROOT_INO, b"a")?;
    c.lookup(ROOT_INO, b"A")?;
    c.lookup(ROOT_INO, b"ab")?;
    ensure_err!(
        c.lookup(ROOT_INO, b"a"),
        Error::NotFound,
        "lookup after unlink of a"
    );
    Ok(())
}

pub fn invalid_names_rejected_by_creating_ops(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    for bad in BAD {
        ensure_err!(
            c.mkdir(ROOT_INO, bad, 0o755),
            Error::InvalidArgument,
            "mkdir {bad:?}"
        );
        ensure_err!(
            c.symlink(ROOT_INO, bad, b"t"),
            Error::InvalidArgument,
            "symlink {bad:?}"
        );
        ensure_err!(
            c.link(f, ROOT_INO, bad),
            Error::InvalidArgument,
            "link {bad:?}"
        );
        ensure_err!(
            c.fs.rename(ROOT_INO, b"f", ROOT_INO, bad, RenameFlags::default()),
            Error::InvalidArgument,
            "rename to {bad:?}"
        );
    }
    ensure_eq!(
        c.names(ROOT_INO)?,
        vec![b"f".to_vec()],
        "entries after rejected operations"
    );
    ensure_eq!(c.fs.getattr(f)?.nlink, 1, "nlink after rejected links");
    Ok(())
}

/// Sidecar names such as `._x` (macOS AppleDouble) and `.DS_Store` are hidden by some adapters,
/// never by a backend: they are plain names.
pub fn appledouble_names_are_ordinary(c: &Ctx) -> Outcome {
    let names: [&[u8]; 5] = [b"._x", b"._", b".DS_Store", b"._.DS_Store", b".localized"];
    for n in names {
        let a = c.create(ROOT_INO, n, 0o644)?;
        ensure_eq!(
            c.lookup(ROOT_INO, n)?.ino,
            a.ino,
            "lookup of {:?}",
            String::from_utf8_lossy(n)
        );
    }
    let mut listed = c.names(ROOT_INO)?;
    listed.sort();
    let mut want: Vec<Vec<u8>> = names.iter().map(|n| n.to_vec()).collect();
    want.sort();
    ensure_eq!(listed, want, "listing of sidecar-style names");
    let d = c.dir(ROOT_INO, "d")?;
    c.create(d, b"._inside", 0o644)?;
    ensure_eq!(
        c.names(d)?,
        vec![b"._inside".to_vec()],
        "sidecar name in a subdirectory"
    );
    Ok(())
}
