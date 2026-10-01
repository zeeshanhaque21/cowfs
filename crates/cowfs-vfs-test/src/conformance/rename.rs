//! `rename`.
//!
//! Decisions pinned here: the destination is replaced atomically (file over file, directory
//! over an empty directory); file over directory is `IsDir`, directory over file is `NotDir`;
//! a non-empty destination directory is `NotEmpty`; `no_replace` with an existing destination
//! is `Exists` and changes nothing; a replaced inode loses a link like `unlink` would; a rename
//! never changes the inode number of the moved entry; a rename does not change the moved
//! file's mtime but does bump its ctime.

use cowfs_vfs::{Error, FileKind, Ino, RenameFlags, Result, ROOT_INO};

use super::{Ctx, Outcome};

fn mv(c: &Ctx, p: Ino, n: &str, q: Ino, m: &str) -> Result<()> {
    c.fs.rename(p, n.as_bytes(), q, m.as_bytes(), RenameFlags::default())
}

pub fn rename_file_basic(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    c.write_all(f, 0, b"data")?;
    mv(c, ROOT_INO, "f", ROOT_INO, "g")?;
    ensure_err!(
        c.lookup(ROOT_INO, b"f"),
        Error::NotFound,
        "old name after rename"
    );
    let g = c.lookup(ROOT_INO, b"g")?;
    ensure_eq!(g.ino, f, "inode after rename");
    ensure_eq!(g.nlink, 1, "nlink after rename");
    ensure_eq!(c.content(f)?, b"data".to_vec(), "content after rename");
    ensure_eq!(
        c.names(ROOT_INO)?,
        vec![b"g".to_vec()],
        "listing after rename"
    );
    Ok(())
}

pub fn rename_file_over_file(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let g = c.file(ROOT_INO, "g")?;
    c.write_all(f, 0, b"FFFF")?;
    c.write_all(g, 0, b"GGGGGGGG")?;
    let other = c.link(g, ROOT_INO, b"g2")?.ino;
    ensure_eq!(other, g, "hardlink inode");
    mv(c, ROOT_INO, "f", ROOT_INO, "g")?;
    ensure_err!(
        c.lookup(ROOT_INO, b"f"),
        Error::NotFound,
        "source name after replacing rename"
    );
    ensure_eq!(
        c.lookup(ROOT_INO, b"g")?.ino,
        f,
        "destination now names the moved inode"
    );
    ensure_eq!(
        c.content(f)?,
        b"FFFF".to_vec(),
        "content of the destination name"
    );
    let old = c.fs.getattr(g)?;
    ensure_eq!(old.nlink, 1, "replaced inode keeps its other name");
    ensure_eq!(
        c.content(g)?,
        b"GGGGGGGG".to_vec(),
        "content of the replaced inode via its other name"
    );
    Ok(())
}

pub fn rename_file_over_empty_dir_is_isdir(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let d = c.dir(ROOT_INO, "d")?;
    ensure_err!(
        mv(c, ROOT_INO, "f", ROOT_INO, "d"),
        Error::IsDir,
        "file over an empty directory"
    );
    ensure_eq!(
        c.lookup(ROOT_INO, b"f")?.ino,
        f,
        "source after the rejected rename"
    );
    ensure_eq!(
        c.lookup(ROOT_INO, b"d")?.ino,
        d,
        "destination after the rejected rename"
    );
    Ok(())
}

pub fn rename_dir_over_empty_dir(c: &Ctx) -> Outcome {
    let a = c.dir(ROOT_INO, "a")?;
    let inner = c.file(a, "inner")?;
    let b = c.dir(ROOT_INO, "b")?;
    ensure_eq!(c.fs.getattr(ROOT_INO)?.nlink, 4, "root nlink before");
    mv(c, ROOT_INO, "a", ROOT_INO, "b")?;
    ensure_err!(c.lookup(ROOT_INO, b"a"), Error::NotFound, "source name");
    ensure_eq!(
        c.lookup(ROOT_INO, b"b")?.ino,
        a,
        "destination names the moved directory"
    );
    ensure_eq!(
        c.lookup(a, b"inner")?.ino,
        inner,
        "content of the moved directory"
    );
    ensure_eq!(
        c.fs.getattr(ROOT_INO)?.nlink,
        3,
        "root nlink after replacing a directory"
    );
    ensure_eq!(c.fs.getattr(b)?.nlink, 0, "replaced directory nlink");
    c.forget_all(b);
    ensure_err!(
        c.fs.getattr(b),
        Error::Stale,
        "replaced directory after forget"
    );
    Ok(())
}

pub fn rename_dir_over_non_empty_dir_is_not_empty(c: &Ctx) -> Outcome {
    let a = c.dir(ROOT_INO, "a")?;
    let b = c.dir(ROOT_INO, "b")?;
    c.file(b, "keep")?;
    ensure_err!(
        mv(c, ROOT_INO, "a", ROOT_INO, "b"),
        Error::NotEmpty,
        "directory over a non-empty directory"
    );
    ensure_eq!(
        c.lookup(ROOT_INO, b"a")?.ino,
        a,
        "source after the rejected rename"
    );
    ensure_eq!(
        c.lookup(ROOT_INO, b"b")?.ino,
        b,
        "destination after the rejected rename"
    );
    c.lookup(b, b"keep")?;
    Ok(())
}

pub fn rename_dir_over_file_is_not_dir(c: &Ctx) -> Outcome {
    c.dir(ROOT_INO, "d")?;
    c.file(ROOT_INO, "f")?;
    ensure_err!(
        mv(c, ROOT_INO, "d", ROOT_INO, "f"),
        Error::NotDir,
        "directory over a file"
    );
    c.symlink(ROOT_INO, b"s", b"x")?;
    ensure_err!(
        mv(c, ROOT_INO, "d", ROOT_INO, "s"),
        Error::NotDir,
        "directory over a symlink"
    );
    ensure_eq!(c.list(ROOT_INO)?.len(), 3, "entries after rejected renames");
    Ok(())
}

pub fn rename_dir_into_own_subtree_is_invalid(c: &Ctx) -> Outcome {
    let a = c.dir(ROOT_INO, "a")?;
    let b = c.dir(a, "b")?;
    let d = c.dir(b, "c")?;
    ensure_err!(
        mv(c, ROOT_INO, "a", a, "x"),
        Error::InvalidArgument,
        "into itself"
    );
    ensure_err!(
        mv(c, ROOT_INO, "a", b, "x"),
        Error::InvalidArgument,
        "into a child"
    );
    ensure_err!(
        mv(c, ROOT_INO, "a", d, "x"),
        Error::InvalidArgument,
        "into a grandchild"
    );
    ensure_err!(
        mv(c, a, "b", d, "x"),
        Error::InvalidArgument,
        "a subdirectory into its own child"
    );
    ensure_eq!(
        c.lookup(ROOT_INO, b"a")?.ino,
        a,
        "tree after rejected renames"
    );
    ensure_eq!(c.lookup(a, b"b")?.ino, b, "tree after rejected renames");
    ensure_eq!(c.lookup(b, b"c")?.ino, d, "tree after rejected renames");
    mv(c, b, "c", ROOT_INO, "c")?;
    mv(c, ROOT_INO, "a", ROOT_INO, "a2")?;
    Ok(())
}

pub fn rename_cross_directory(c: &Ctx) -> Outcome {
    let d1 = c.dir(ROOT_INO, "d1")?;
    let d2 = c.dir(ROOT_INO, "d2")?;
    let f = c.file(d1, "f")?;
    c.write_all(f, 0, b"payload")?;
    mv(c, d1, "f", d2, "g")?;
    ensure_err!(c.lookup(d1, b"f"), Error::NotFound, "old parent");
    ensure_eq!(c.lookup(d2, b"g")?.ino, f, "new parent");
    ensure_eq!(c.content(f)?, b"payload".to_vec(), "content after moving");
    ensure!(c.list(d1)?.is_empty(), "old parent still lists the file");
    ensure_eq!(c.names(d2)?, vec![b"g".to_vec()], "new parent listing");
    let l = c.symlink(d2, b"l", b"g")?.ino;
    mv(c, d2, "l", d1, "l")?;
    ensure_eq!(
        c.fs.readlink(l)?,
        b"g".to_vec(),
        "symlink moved between directories"
    );
    Ok(())
}

pub fn rename_dir_cross_directory_fixes_nlink(c: &Ctx) -> Outcome {
    let p = c.dir(ROOT_INO, "p")?;
    let q = c.dir(ROOT_INO, "q")?;
    let s = c.dir(p, "s")?;
    let h = c.file(s, "h")?;
    let nlink = |i| c.fs.getattr(i).map(|a| a.nlink);
    ensure_eq!((nlink(p)?, nlink(q)?), (3, 2), "nlinks before");
    mv(c, p, "s", q, "s")?;
    ensure_eq!(
        (nlink(p)?, nlink(q)?, nlink(s)?),
        (2, 3, 2),
        "nlinks of old parent, new parent, moved directory"
    );
    ensure_eq!(c.lookup(q, b"s")?.ino, s, "moved directory");
    ensure_eq!(c.lookup(s, b"h")?.ino, h, "content of the moved directory");
    ensure_eq!(nlink(ROOT_INO)?, 4, "root nlink unchanged");
    mv(c, q, "s", q, "s2")?;
    ensure_eq!(nlink(q)?, 3, "nlink after renaming within one directory");
    Ok(())
}

pub fn rename_onto_same_inode_is_noop(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    c.link(f, ROOT_INO, b"g")?;
    mv(c, ROOT_INO, "f", ROOT_INO, "g")?;
    ensure_eq!(
        c.lookup(ROOT_INO, b"f")?.ino,
        f,
        "first name after renaming onto its hardlink"
    );
    ensure_eq!(
        c.lookup(ROOT_INO, b"g")?.ino,
        f,
        "second name after renaming onto its hardlink"
    );
    ensure_eq!(c.fs.getattr(f)?.nlink, 2, "nlink after the no-op rename");
    mv(c, ROOT_INO, "g", ROOT_INO, "f")?;
    ensure_eq!(
        c.fs.getattr(f)?.nlink,
        2,
        "nlink after the reverse no-op rename"
    );
    mv(c, ROOT_INO, "f", ROOT_INO, "f")?;
    ensure_eq!(c.lookup(ROOT_INO, b"f")?.ino, f, "name renamed onto itself");
    ensure_eq!(c.list(ROOT_INO)?.len(), 2, "entries after no-op renames");
    Ok(())
}

pub fn rename_no_replace(c: &Ctx) -> Outcome {
    let nr = RenameFlags { no_replace: true };
    let f = c.file(ROOT_INO, "f")?;
    let g = c.file(ROOT_INO, "g")?;
    c.write_all(f, 0, b"F")?;
    c.write_all(g, 0, b"G")?;
    ensure_err!(
        c.fs.rename(ROOT_INO, b"f", ROOT_INO, b"g", nr),
        Error::Exists,
        "no_replace onto a file"
    );
    ensure_eq!(
        c.lookup(ROOT_INO, b"f")?.ino,
        f,
        "source after rejected no_replace"
    );
    ensure_eq!(
        c.lookup(ROOT_INO, b"g")?.ino,
        g,
        "destination after rejected no_replace"
    );
    ensure_eq!(c.content(g)?, b"G".to_vec(), "destination content");
    let d = c.dir(ROOT_INO, "d")?;
    ensure_err!(
        c.fs.rename(ROOT_INO, b"f", ROOT_INO, b"d", nr),
        Error::Exists,
        "no_replace onto a directory"
    );
    let e = c.dir(ROOT_INO, "e")?;
    ensure_err!(
        c.fs.rename(ROOT_INO, b"d", ROOT_INO, b"e", nr),
        Error::Exists,
        "no_replace of a directory onto an empty directory"
    );
    ensure_eq!(
        (c.lookup(ROOT_INO, b"d")?.ino, c.lookup(ROOT_INO, b"e")?.ino),
        (d, e),
        "directories after rejected no_replace"
    );
    c.fs.rename(ROOT_INO, b"f", ROOT_INO, b"h", nr)?;
    ensure_eq!(
        c.lookup(ROOT_INO, b"h")?.ino,
        f,
        "no_replace to a free name"
    );
    ensure_err!(
        c.lookup(ROOT_INO, b"f"),
        Error::NotFound,
        "source after a successful no_replace"
    );
    Ok(())
}

pub fn rename_missing_source_is_not_found(c: &Ctx) -> Outcome {
    let g = c.file(ROOT_INO, "g")?;
    ensure_err!(
        mv(c, ROOT_INO, "missing", ROOT_INO, "x"),
        Error::NotFound,
        "missing source"
    );
    ensure_err!(
        mv(c, ROOT_INO, "missing", ROOT_INO, "g"),
        Error::NotFound,
        "missing source over an existing name"
    );
    ensure_eq!(
        c.lookup(ROOT_INO, b"g")?.ino,
        g,
        "destination after the rejected rename"
    );
    let f = c.file(ROOT_INO, "f")?;
    ensure_err!(
        mv(c, f, "x", ROOT_INO, "y"),
        Error::NotDir,
        "source parent is a file"
    );
    ensure_err!(
        mv(c, ROOT_INO, "g", f, "y"),
        Error::NotDir,
        "destination parent is a file"
    );
    Ok(())
}

pub fn rename_open_file_keeps_handle_working(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    c.write_all(f, 0, b"one")?;
    let h = c.fs.open(f)?;
    mv(c, ROOT_INO, "f", ROOT_INO, "g")?;
    c.write_all(f, 3, b"two")?;
    ensure_eq!(
        c.content(f)?,
        b"onetwo".to_vec(),
        "content through the inode after rename"
    );
    ensure_eq!(c.fs.getattr(f)?.nlink, 1, "nlink of a renamed open file");
    let d = c.dir(ROOT_INO, "d")?;
    mv(c, ROOT_INO, "g", d, "h")?;
    ensure_eq!(
        c.content(c.lookup(d, b"h")?.ino)?,
        b"onetwo".to_vec(),
        "content after a cross-directory rename"
    );
    c.fs.release(h)?;

    let victim = c.file(ROOT_INO, "victim")?;
    c.write_all(victim, 0, b"old")?;
    let h2 = c.fs.open(victim)?;
    c.forget_all(victim);
    let winner = c.file(ROOT_INO, "winner")?;
    mv(c, ROOT_INO, "winner", ROOT_INO, "victim")?;
    ensure_eq!(
        c.fs.getattr(victim)?.nlink,
        0,
        "an open replaced file has no names"
    );
    ensure_eq!(
        c.content(victim)?,
        b"old".to_vec(),
        "an open replaced file is still readable"
    );
    c.write_all(victim, 3, b"er")?;
    ensure_eq!(
        c.lookup(ROOT_INO, b"victim")?.ino,
        winner,
        "the name now belongs to the winner"
    );
    c.fs.release(h2)?;
    ensure_err!(
        c.fs.getattr(victim),
        Error::Stale,
        "replaced file after release"
    );
    Ok(())
}

pub fn rename_updates_times(c: &Ctx) -> Outcome {
    let d1 = c.dir(ROOT_INO, "d1")?;
    let d2 = c.dir(ROOT_INO, "d2")?;
    let f = c.file(d1, "f")?;
    let (a, p1, p2) = (c.fs.getattr(f)?, c.fs.getattr(d1)?, c.fs.getattr(d2)?);
    c.tick();
    mv(c, d1, "f", d2, "g")?;
    let (b, q1, q2) = (c.fs.getattr(f)?, c.fs.getattr(d1)?, c.fs.getattr(d2)?);
    ensure!(b.ctime > a.ctime, "rename did not bump the file ctime");
    ensure_eq!(b.mtime, a.mtime, "rename changed the file mtime");
    ensure!(
        q1.mtime > p1.mtime && q1.ctime > p1.ctime,
        "old parent times not updated"
    );
    ensure!(
        q2.mtime > p2.mtime && q2.ctime > p2.ctime,
        "new parent times not updated"
    );
    Ok(())
}

pub fn rename_dir_keeps_contents(c: &Ctx) -> Outcome {
    let a = c.dir(ROOT_INO, "a")?;
    let f = c.file(a, "f")?;
    let sub = c.dir(a, "sub")?;
    let h = c.file(sub, "h")?;
    let l = c.symlink(a, b"l", b"f")?.ino;
    c.write_all(h, 0, b"deep")?;
    mv(c, ROOT_INO, "a", ROOT_INO, "renamed")?;
    let a2 = c.lookup(ROOT_INO, b"renamed")?;
    ensure_eq!(
        (a2.ino, a2.kind),
        (a, FileKind::Directory),
        "renamed directory"
    );
    ensure_eq!(c.lookup(a, b"f")?.ino, f, "file in the renamed directory");
    ensure_eq!(
        c.lookup(a, b"l")?.ino,
        l,
        "symlink in the renamed directory"
    );
    let s = c.lookup(a, b"sub")?.ino;
    ensure_eq!(s, sub, "subdirectory in the renamed directory");
    ensure_eq!(
        c.content(c.lookup(s, b"h")?.ino)?,
        b"deep".to_vec(),
        "deep file content"
    );
    ensure_eq!(c.list(a)?.len(), 3, "entries of the renamed directory");
    Ok(())
}

/// Linux says ENOTEMPTY, APFS and `MemVfs` say EISDIR. Either is fine, success is not.
pub fn rename_file_onto_ancestor_dir_is_rejected(c: &Ctx) -> Outcome {
    let a = c.dir(ROOT_INO, "a")?;
    let f = c.file(a, "f")?;
    ensure_err_any!(
        mv(c, a, "f", ROOT_INO, "a"),
        [Error::IsDir, Error::NotEmpty],
        "a file onto its own non-empty parent directory"
    );
    ensure_eq!(c.lookup(a, b"f")?.ino, f, "file after the rejected rename");
    ensure_eq!(
        c.lookup(ROOT_INO, b"a")?.ino,
        a,
        "directory after the rejected rename"
    );
    Ok(())
}

/// After a directory moves, its new ancestors must be known: moving a former ancestor of the new
/// place under it is still a cycle.
pub fn rename_ancestor_into_moved_dir(c: &Ctx) -> Outcome {
    let p = c.dir(ROOT_INO, "p")?;
    let q = c.dir(p, "q")?;
    let r = c.dir(ROOT_INO, "r")?;
    let deep = c.dir(q, "deep")?;
    mv(c, p, "q", r, "q")?;
    ensure_err!(
        mv(c, ROOT_INO, "r", q, "x"),
        Error::InvalidArgument,
        "moving r under its own moved child"
    );
    ensure_err!(
        mv(c, ROOT_INO, "r", deep, "x"),
        Error::InvalidArgument,
        "moving r under a grandchild that moved with it"
    );
    mv(c, ROOT_INO, "p", q, "p")?;
    ensure_err!(
        mv(c, r, "q", deep, "x"),
        Error::InvalidArgument,
        "moving q under its own child"
    );
    Ok(())
}

/// Moves a directory to another parent and checks the old and new parent both see it: the
/// listings, `nlink` (2 + subdirectories), removal through the new parent, and the old parent
/// becoming removable.
pub fn rename_dir_across_parents_keeps_parent_links(c: &Ctx) -> Outcome {
    let a = c.dir(ROOT_INO, "a")?;
    let b = c.dir(ROOT_INO, "b")?;
    let x = c.dir(a, "x")?;
    let y = c.dir(x, "y")?;
    let nlink = |i| c.fs.getattr(i).map(|a| a.nlink);
    ensure_eq!((nlink(a)?, nlink(b)?, nlink(x)?), (3, 2, 3), "before");
    mv(c, a, "x", b, "moved")?;
    ensure_eq!(
        (nlink(a)?, nlink(b)?, nlink(x)?, nlink(y)?),
        (2, 3, 3, 2),
        "nlink of old parent, new parent, moved directory, its child"
    );
    ensure!(
        c.list(a)?.is_empty(),
        "old parent still lists the directory"
    );
    ensure_eq!(c.names(b)?, vec![b"moved".to_vec()], "new parent listing");
    ensure_eq!(c.lookup(b, b"moved")?.ino, x, "moved directory inode");
    ensure_eq!(c.lookup(x, b"y")?.ino, y, "child of the moved directory");
    ensure_err!(
        c.fs.rmdir(b, b"moved"),
        Error::NotEmpty,
        "rmdir of the moved directory while it has a child"
    );
    c.fs.rmdir(x, b"y")?;
    c.fs.rmdir(b, b"moved")?;
    ensure_eq!(
        nlink(b)?,
        2,
        "new parent after removing the moved directory"
    );
    c.fs.rmdir(ROOT_INO, b"a")?;
    ensure_eq!(nlink(ROOT_INO)?, 3, "root after removing the old parent");
    Ok(())
}
