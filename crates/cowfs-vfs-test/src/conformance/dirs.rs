//! `mkdir` and `rmdir`.
//!
//! Decisions pinned here: adding an entry to a directory that was removed but is still
//! referenced (a lookup was not forgotten) is `NotFound`, like ENOENT; a removed directory
//! reports `nlink` 0.

use cowfs_vfs::{Error, RenameFlags, ROOT_INO};

use super::{Ctx, Outcome};

pub fn mkdir_rmdir_errors(c: &Ctx) -> Outcome {
    let d = c.dir(ROOT_INO, "d")?;
    c.file(d, "f")?;
    c.dir(d, "sub")?;
    ensure_err!(
        c.fs.rmdir(ROOT_INO, b"d"),
        Error::NotEmpty,
        "rmdir of a directory with a file and a subdirectory"
    );
    c.fs.unlink(d, b"f")?;
    ensure_err!(
        c.fs.rmdir(ROOT_INO, b"d"),
        Error::NotEmpty,
        "rmdir of a directory with a subdirectory"
    );
    c.fs.rmdir(d, b"sub")?;
    ensure_err!(c.fs.rmdir(d, b"sub"), Error::NotFound, "second rmdir");
    ensure_err!(c.fs.unlink(d, b"f"), Error::NotFound, "second unlink");
    c.file(ROOT_INO, "file")?;
    c.symlink(ROOT_INO, b"link", b"d")?;
    ensure_err!(
        c.fs.rmdir(ROOT_INO, b"file"),
        Error::NotDir,
        "rmdir of a file"
    );
    ensure_err!(
        c.fs.rmdir(ROOT_INO, b"link"),
        Error::NotDir,
        "rmdir of a symlink to a directory"
    );
    ensure_err!(
        c.fs.unlink(ROOT_INO, b"d"),
        Error::IsDir,
        "unlink of a directory"
    );
    ensure_err!(
        c.fs.rmdir(ROOT_INO, b"missing"),
        Error::NotFound,
        "rmdir of a missing name"
    );
    ensure_err!(
        c.fs.unlink(ROOT_INO, b"missing"),
        Error::NotFound,
        "unlink of a missing name"
    );
    c.fs.rmdir(ROOT_INO, b"d")?;
    ensure_err!(
        c.lookup(ROOT_INO, b"d"),
        Error::NotFound,
        "lookup after rmdir"
    );
    ensure_err!(
        c.mkdir(ROOT_INO, b"file", 0o755),
        Error::Exists,
        "mkdir over a file"
    );
    Ok(())
}

pub fn rmdir_updates_parent(c: &Ctx) -> Outcome {
    let p = c.dir(ROOT_INO, "p")?;
    let d = c.dir(p, "d")?;
    let before = c.fs.getattr(p)?;
    ensure_eq!(before.nlink, 3, "parent nlink with one subdirectory");
    c.tick();
    c.fs.rmdir(p, b"d")?;
    let after = c.fs.getattr(p)?;
    ensure_eq!(after.nlink, 2, "parent nlink after rmdir");
    ensure!(
        after.mtime > before.mtime && after.ctime > before.ctime,
        "rmdir did not update parent times"
    );
    ensure_eq!(
        c.fs.getattr(d)?.nlink,
        0,
        "nlink of a removed but referenced directory"
    );
    ensure!(
        c.list(p)?.is_empty(),
        "parent still lists the removed directory"
    );
    Ok(())
}

pub fn deeply_nested_directories(c: &Ctx) -> Outcome {
    const DEPTH: usize = 200;
    let mut chain = vec![ROOT_INO];
    for i in 0..DEPTH {
        let parent = chain[i];
        chain.push(c.dir(parent, &format!("d{i}"))?);
    }
    let leaf = c.file(chain[DEPTH], "leaf")?;
    c.write_all(leaf, 0, b"deep")?;
    let mut cur = ROOT_INO;
    for i in 0..DEPTH {
        cur = c.lookup(cur, format!("d{i}").as_bytes())?.ino;
    }
    ensure_eq!(
        c.lookup(cur, b"leaf")?.ino,
        leaf,
        "leaf reached by walking down"
    );
    ensure_eq!(
        c.fs.getattr(chain[DEPTH])?.nlink,
        2,
        "bottom directory nlink"
    );
    ensure_eq!(
        c.fs.getattr(chain[DEPTH - 1])?.nlink,
        3,
        "second lowest directory nlink"
    );
    let moved = c.dir(ROOT_INO, "elsewhere")?;
    ensure_err!(
        c.fs.rename(ROOT_INO, b"d0", chain[DEPTH], b"x", RenameFlags::default()),
        Error::InvalidArgument,
        "moving the top of the chain under its bottom"
    );
    ensure_err!(
        c.fs.rmdir(chain[DEPTH - 2], b"d198"),
        Error::NotEmpty,
        "rmdir of a directory that still has a child"
    );
    c.fs.rename(
        chain[DEPTH - 1],
        b"d199",
        moved,
        b"tail",
        RenameFlags::default(),
    )?;
    ensure_eq!(
        c.fs.getattr(chain[DEPTH - 1])?.nlink,
        2,
        "old parent nlink after the move"
    );
    ensure_eq!(
        c.fs.getattr(moved)?.nlink,
        3,
        "new parent nlink after the move"
    );
    c.fs.rmdir(chain[DEPTH - 2], b"d198")?;
    Ok(())
}

pub fn create_in_removed_directory_fails(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let d = c.dir(ROOT_INO, "d")?;
    c.fs.rmdir(ROOT_INO, b"d")?;
    ensure_err!(
        c.create(d, b"x", 0o644),
        Error::NotFound,
        "create in a removed directory"
    );
    ensure_err!(
        c.mkdir(d, b"x", 0o755),
        Error::NotFound,
        "mkdir in a removed directory"
    );
    ensure_err!(
        c.symlink(d, b"x", b"t"),
        Error::NotFound,
        "symlink in a removed directory"
    );
    ensure_err!(
        c.link(f, d, b"x"),
        Error::NotFound,
        "link into a removed directory"
    );
    ensure_err!(
        c.fs.rename(ROOT_INO, b"f", d, b"x", RenameFlags::default()),
        Error::NotFound,
        "rename into a removed directory"
    );
    ensure_eq!(
        c.lookup(ROOT_INO, b"f")?.ino,
        f,
        "source of the rejected rename"
    );
    Ok(())
}
