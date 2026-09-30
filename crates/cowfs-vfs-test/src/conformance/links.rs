//! Hardlinks.
//!
//! Spike 2 lesson: several names of one inode in one directory made an inode-based readdir
//! cookie duplicate or drop entries.
//!
//! Decisions pinned here: every name of a file reports the same inode; `nlink` counts the
//! names; linking a directory (including the root) is `PermissionDenied`; linking over an
//! existing name is `Exists` and changes nothing; a symlink can be hardlinked like a file.

use std::collections::HashMap;

use cowfs_vfs::{Error, FileKind, ROOT_INO};

use super::{Ctx, Failure, Outcome};

pub fn hardlink_shares_inode(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let a = c.link(f, ROOT_INO, b"g")?;
    ensure_eq!(a.ino, f, "link returns the same inode");
    ensure_eq!(a.kind, FileKind::Regular, "link kind");
    let (x, y) = (c.lookup(ROOT_INO, b"f")?, c.lookup(ROOT_INO, b"g")?);
    ensure_eq!((x.ino, y.ino), (f, f), "lookup of both names");
    let names = c.list(ROOT_INO)?;
    ensure_eq!(names.len(), 2, "entries");
    ensure!(
        names.iter().all(|e| e.ino == f),
        "listing reports different inodes for the two names"
    );
    Ok(())
}

pub fn hardlink_nlink_counts_names(c: &Ctx) -> Outcome {
    let nlink = |i| c.fs.getattr(i).map(|a| a.nlink);
    let f = c.file(ROOT_INO, "f")?;
    ensure_eq!(nlink(f)?, 1, "new file");
    ensure_eq!(
        c.link(f, ROOT_INO, b"g")?.nlink,
        2,
        "nlink returned by link"
    );
    c.link(f, ROOT_INO, b"h")?;
    ensure_eq!(nlink(f)?, 3, "three names");
    c.fs.unlink(ROOT_INO, b"g")?;
    ensure_eq!(nlink(f)?, 2, "after one unlink");
    c.fs.unlink(ROOT_INO, b"f")?;
    ensure_eq!(nlink(f)?, 1, "after two unlinks");
    c.fs.unlink(ROOT_INO, b"h")?;
    ensure_eq!(
        nlink(f)?,
        0,
        "after the last unlink, while still referenced"
    );
    ensure_err!(
        c.link(f, ROOT_INO, b"back"),
        Error::NotFound,
        "linking an inode that has no names"
    );
    Ok(())
}

pub fn hardlink_write_visible_through_other_name(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let g = c.link(f, ROOT_INO, b"g")?.ino;
    c.write_all(f, 0, b"via f")?;
    let seen = c.lookup(ROOT_INO, b"g")?;
    ensure_eq!(seen.size, 5, "size seen through the other name");
    ensure_eq!(
        c.content(seen.ino)?,
        b"via f".to_vec(),
        "content seen through the other name"
    );
    c.write_all(g, 4, b"g!")?;
    ensure_eq!(
        c.content(c.lookup(ROOT_INO, b"f")?.ino)?,
        b"via g!".to_vec(),
        "write through the second name"
    );
    c.fs.setattr(
        f,
        cowfs_vfs::SetAttr {
            mode: Some(0o600),
            ..Default::default()
        },
    )?;
    ensure_eq!(
        c.lookup(ROOT_INO, b"g")?.mode,
        0o600,
        "chmod seen through the other name"
    );
    Ok(())
}

pub fn hardlink_unlink_one_other_survives(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    c.write_all(f, 0, b"survivor")?;
    c.link(f, ROOT_INO, b"g")?;
    c.fs.unlink(ROOT_INO, b"f")?;
    ensure_err!(c.lookup(ROOT_INO, b"f"), Error::NotFound, "unlinked name");
    let g = c.lookup(ROOT_INO, b"g")?;
    ensure_eq!((g.ino, g.nlink), (f, 1), "surviving name");
    ensure_eq!(
        c.content(f)?,
        b"survivor".to_vec(),
        "content after unlinking the other name"
    );
    c.write_all(f, 8, b"!")?;
    ensure_eq!(c.fs.getattr(f)?.size, 9, "still writable");
    Ok(())
}

pub fn hardlink_across_directories(c: &Ctx) -> Outcome {
    let d1 = c.dir(ROOT_INO, "d1")?;
    let d2 = c.dir(ROOT_INO, "d2")?;
    let f = c.file(d1, "f")?;
    c.write_all(f, 0, b"shared")?;
    let a = c.link(f, d2, b"g")?;
    ensure_eq!((a.ino, a.nlink), (f, 2), "link into another directory");
    c.fs.unlink(d1, b"f")?;
    c.fs.rmdir(ROOT_INO, b"d1")?;
    ensure_eq!(
        c.content(c.lookup(d2, b"g")?.ino)?,
        b"shared".to_vec(),
        "content after removing the first directory"
    );
    Ok(())
}

/// The kernel says `EPERM` for a hardlink to a directory, `Error::PermissionDenied` maps to
/// `EACCES`, so both errnos are accepted.
pub fn hardlink_to_directory_is_denied(c: &Ctx) -> Outcome {
    let d = c.dir(ROOT_INO, "d")?;
    ensure_err_any!(
        c.link(d, ROOT_INO, b"d2"),
        [Error::PermissionDenied],
        "link to a directory"
    );
    ensure_err_any!(
        c.link(d, d, b"self"),
        [Error::PermissionDenied],
        "link a directory into itself"
    );
    ensure_err_any!(
        c.link(ROOT_INO, d, b"root"),
        [Error::PermissionDenied],
        "link to the root"
    );
    ensure_eq!(
        c.names(ROOT_INO)?,
        vec![b"d".to_vec()],
        "entries after rejected links"
    );
    Ok(())
}

pub fn hardlink_over_existing_name_is_exists(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let g = c.file(ROOT_INO, "g")?;
    c.write_all(g, 0, b"G")?;
    c.dir(ROOT_INO, "d")?;
    c.symlink(ROOT_INO, b"s", b"x")?;
    for name in [&b"g"[..], b"d", b"s", b"f"] {
        ensure_err!(
            c.link(f, ROOT_INO, name),
            Error::Exists,
            "link over {:?}",
            String::from_utf8_lossy(name)
        );
    }
    ensure_eq!(c.fs.getattr(f)?.nlink, 1, "nlink after rejected links");
    ensure_eq!(
        c.lookup(ROOT_INO, b"g")?.ino,
        g,
        "existing name after rejected link"
    );
    ensure_eq!(
        c.content(g)?,
        b"G".to_vec(),
        "existing content after rejected link"
    );
    Ok(())
}

pub fn hardlink_to_symlink(c: &Ctx) -> Outcome {
    let s = c.symlink(ROOT_INO, b"s", b"target")?.ino;
    let a = c.link(s, ROOT_INO, b"s2")?;
    ensure_eq!(
        (a.ino, a.kind, a.nlink),
        (s, FileKind::Symlink, 2),
        "hardlinked symlink"
    );
    ensure_eq!(
        c.fs.readlink(s)?,
        b"target".to_vec(),
        "target through the hardlink"
    );
    c.fs.unlink(ROOT_INO, b"s")?;
    ensure_eq!(
        c.fs.readlink(c.lookup(ROOT_INO, b"s2")?.ino)?,
        b"target".to_vec(),
        "target after unlinking the original"
    );
    Ok(())
}

const PAIRS: usize = 8000;

/// The exact spike 2 regression: adjacent hardlink pairs in one directory, listed with page
/// sizes that fall on and between the pairs, then removed while listing.
pub fn hardlink_pairs_8000_listed_once_and_removed(c: &Ctx) -> Outcome {
    let d = c.dir(ROOT_INO, "d")?;
    let mut want: HashMap<Vec<u8>, u64> = HashMap::new();
    for i in 0..PAIRS {
        let a = format!("a{i:05}").into_bytes();
        let b = format!("b{i:05}").into_bytes();
        let ino = c.fs.create(d, &a, 0o644)?.ino;
        let l = c.fs.link(ino, d, &b)?;
        ensure_eq!(l.nlink, 2, "nlink of pair {i}");
        want.insert(a, ino);
        want.insert(b, ino);
    }
    for page in [1, 2, 3, 999, 1000, 4096] {
        let got = c.list_paged(d, page)?;
        ensure_eq!(got.len(), 2 * PAIRS, "entries listed with page size {page}");
        let mut seen: HashMap<&[u8], u64> = HashMap::new();
        for e in &got {
            ensure!(
                seen.insert(&e.name, e.ino).is_none(),
                "{:?} listed twice with page size {page}",
                String::from_utf8_lossy(&e.name)
            );
            ensure_eq!(
                want.get(&e.name).copied(),
                Some(e.ino),
                "inode of {:?}",
                String::from_utf8_lossy(&e.name)
            );
        }
    }
    let mut cookie = 0;
    let mut removed = 0;
    loop {
        let r = c.fs.readdir(d, cookie, 1000)?;
        for e in &r.entries {
            c.fs.unlink(d, &e.name)?;
            removed += 1;
        }
        match r.entries.last() {
            Some(l) => cookie = l.cookie,
            None if r.eof => break,
            None => {
                return Err(Failure(
                    "readdir returned no entries and eof is false".into(),
                ))
            }
        }
        if r.eof {
            break;
        }
    }
    ensure_eq!(removed, 2 * PAIRS, "entries removed while listing");
    ensure!(
        c.list(d)?.is_empty(),
        "directory not empty after removing every listed entry"
    );
    c.fs.rmdir(ROOT_INO, b"d")?;
    Ok(())
}

pub fn lookup_nlink_is_fresh(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    c.link(f, ROOT_INO, b"g")?;
    for n in [&b"f"[..], b"g"] {
        let a = c.lookup(ROOT_INO, n)?;
        ensure_eq!(
            a.nlink,
            2,
            "nlink from lookup of {:?} after a link",
            String::from_utf8_lossy(n)
        );
    }
    c.link(f, ROOT_INO, b"h")?;
    ensure_eq!(
        c.lookup(ROOT_INO, b"f")?.nlink,
        3,
        "nlink from lookup after a second link"
    );
    c.fs.unlink(ROOT_INO, b"g")?;
    c.fs.unlink(ROOT_INO, b"h")?;
    ensure_eq!(
        c.lookup(ROOT_INO, b"f")?.nlink,
        1,
        "nlink from lookup after both links were removed"
    );
    Ok(())
}

const LINK_TRY: u32 = 70_000;

/// A backend may have no link limit. If it has one, running into it must be reported as
/// `TooManyLinks`, `nlink` must count exactly the names that were made, and the file must stay
/// fully usable. `MemVfs` has a limit of `LINK_MAX` names.
pub fn hardlink_limit_reports_too_many_links(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    c.write_all(f, 0, b"data")?;
    let d = c.dir(ROOT_INO, "d")?;
    let mut names = 1u32;
    for i in 1..LINK_TRY {
        match c.fs.link(f, d, format!("l{i}").as_bytes()) {
            Ok(a) => {
                names += 1;
                ensure_eq!(a.nlink, names, "nlink returned by link number {i}");
            }
            Err(Error::TooManyLinks) => break,
            Err(e) => return Err(e.into()),
        }
    }
    ensure_eq!(c.fs.getattr(f)?.nlink, names, "nlink after the last link");
    ensure_eq!(
        c.content(f)?,
        b"data".to_vec(),
        "content of a file with many names"
    );
    if names < LINK_TRY {
        ensure_err!(
            c.fs.link(f, d, b"one-more"),
            Error::TooManyLinks,
            "link after the limit was reported"
        );
        ensure_eq!(c.fs.getattr(f)?.nlink, names, "nlink after a rejected link");
        c.fs.unlink(d, b"l1")?;
        c.fs.link(f, d, b"again")?;
    }
    Ok(())
}
