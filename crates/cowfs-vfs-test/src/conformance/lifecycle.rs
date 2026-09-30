//! Inode lifetime: unlink while open, `forget`, and `Stale`.
//!
//! Decisions pinned here (the trait says an inode "may" be reclaimed): an inode with no
//! names, no open handle and no outstanding reference IS unreachable, so every operation on
//! it returns `Stale`. While a handle or a reference remains it stays fully usable, with
//! `nlink` 0. The suite counts references exactly: one per `create`, `mkdir`, `symlink`,
//! `link` or `lookup` that returned the inode.

use cowfs_vfs::{Error, FileHandle, SetAttr, ROOT_INO};

use super::basic::all_ops_stale;
use super::{Ctx, Outcome};

pub fn unlink_while_open_keeps_data(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    c.write_all(f, 0, b"hello")?;
    let h = c.fs.open(f)?;
    c.forget_all(f);
    c.fs.unlink(ROOT_INO, b"f")?;
    ensure_err!(
        c.lookup(ROOT_INO, b"f"),
        Error::NotFound,
        "name after unlink"
    );
    let a = c.fs.getattr(f)?;
    ensure_eq!(a.nlink, 0, "nlink of an unlinked open file");
    ensure_eq!(a.size, 5, "size of an unlinked open file");
    ensure_eq!(c.content(f)?, b"hello".to_vec(), "read after unlink");
    c.write_all(f, 5, b" world")?;
    ensure_eq!(c.content(f)?, b"hello world".to_vec(), "write after unlink");
    c.fs.setattr(
        f,
        SetAttr {
            size: Some(2),
            ..Default::default()
        },
    )?;
    ensure_eq!(c.content(f)?, b"he".to_vec(), "truncate after unlink");
    c.fs.fsync(f, false)?;
    c.fs.flush(f)?;
    let g = c.file(ROOT_INO, "f")?;
    ensure!(
        g != f,
        "the unlinked inode number was reused while still open"
    );
    c.fs.release(h)?;
    ensure_err!(c.fs.getattr(f), Error::Stale, "unlinked file after release");
    ensure_eq!(
        c.lookup(ROOT_INO, b"f")?.ino,
        g,
        "the new file is untouched"
    );
    Ok(())
}

pub fn unlink_while_open_reclaimed_after_release_and_forget(c: &Ctx) -> Outcome {
    let a = c.fs.create(ROOT_INO, b"a", 0o644)?.ino;
    let h = c.fs.open(a)?;
    c.fs.unlink(ROOT_INO, b"a")?;
    c.fs.release(h)?;
    ensure_eq!(
        c.fs.getattr(a)?.nlink,
        0,
        "unlinked file still referenced after release"
    );
    c.fs.forget(a, 1);
    ensure_err!(c.fs.getattr(a), Error::Stale, "release then forget");

    let b = c.fs.create(ROOT_INO, b"b", 0o644)?.ino;
    let h = c.fs.open(b)?;
    c.fs.unlink(ROOT_INO, b"b")?;
    c.fs.forget(b, 1);
    ensure_eq!(
        c.fs.getattr(b)?.nlink,
        0,
        "unlinked file still open after forget"
    );
    c.fs.release(h)?;
    ensure_err!(c.fs.getattr(b), Error::Stale, "forget then release");
    Ok(())
}

pub fn forget_keeps_inode_with_links(c: &Ctx) -> Outcome {
    let f = c.fs.create(ROOT_INO, b"f", 0o644)?.ino;
    c.fs.forget(f, 1);
    let a = c.fs.getattr(f)?;
    ensure_eq!(
        a.nlink,
        1,
        "file with a name after forgetting every reference"
    );
    let l = c.fs.lookup(ROOT_INO, b"f")?;
    ensure_eq!(l.ino, f, "lookup after forget returns the same inode");
    c.fs.forget(f, 1);
    let d = c.fs.mkdir(ROOT_INO, b"d", 0o755)?.ino;
    c.fs.forget(d, 1);
    ensure_eq!(
        c.fs.lookup(ROOT_INO, b"d")?.ino,
        d,
        "directory after forget"
    );
    Ok(())
}

pub fn forget_keeps_inode_with_handle(c: &Ctx) -> Outcome {
    let f = c.fs.create(ROOT_INO, b"f", 0o644)?.ino;
    let h = c.fs.open(f)?;
    c.fs.unlink(ROOT_INO, b"f")?;
    c.fs.forget(f, 1);
    c.write_all(f, 0, b"still here")?;
    ensure_eq!(
        c.content(f)?,
        b"still here".to_vec(),
        "content with only a handle left"
    );
    c.fs.release(h)?;
    ensure_err!(c.fs.getattr(f), Error::Stale, "after the last handle");

    let g = c.fs.create(ROOT_INO, b"g", 0o644)?.ino;
    c.fs.lookup(ROOT_INO, b"g")?;
    c.fs.lookup(ROOT_INO, b"g")?;
    c.fs.unlink(ROOT_INO, b"g")?;
    c.fs.forget(g, 2);
    ensure_eq!(
        c.fs.getattr(g)?.nlink,
        0,
        "still referenced after a partial forget"
    );
    c.fs.forget(g, 1);
    ensure_err!(c.fs.getattr(g), Error::Stale, "after the last reference");
    Ok(())
}

pub fn rmdir_reclaimed_after_forget(c: &Ctx) -> Outcome {
    let d = c.dir(ROOT_INO, "d")?;
    c.fs.rmdir(ROOT_INO, b"d")?;
    ensure_eq!(
        c.fs.getattr(d)?.nlink,
        0,
        "removed directory while referenced"
    );
    c.forget_all(d);
    ensure_err!(
        c.fs.getattr(d),
        Error::Stale,
        "removed directory after forget"
    );
    ensure_err!(
        c.fs.readdir(d, 0, 10),
        Error::Stale,
        "readdir of a reclaimed directory"
    );
    Ok(())
}

pub fn stale_after_reclaim_for_every_operation(c: &Ctx) -> Outcome {
    let real = c.file(ROOT_INO, "real")?;
    let f = c.file(ROOT_INO, "f")?;
    c.fs.unlink(ROOT_INO, b"f")?;
    c.forget_all(f);
    all_ops_stale(c, f, real)?;
    let d = c.dir(ROOT_INO, "d")?;
    c.fs.rmdir(ROOT_INO, b"d")?;
    c.forget_all(d);
    all_ops_stale(c, d, real)?;
    let s = c.symlink(ROOT_INO, b"s", b"t")?.ino;
    c.fs.unlink(ROOT_INO, b"s")?;
    c.forget_all(s);
    all_ops_stale(c, s, real)
}

pub fn open_directory_ok(c: &Ctx) -> Outcome {
    let d = c.dir(ROOT_INO, "d")?;
    let h = c.fs.open(d)?;
    c.file(d, "f")?;
    ensure_eq!(c.list(d)?.len(), 1, "listing through an open directory");
    c.fs.release(h)?;
    let h = c.fs.open(ROOT_INO)?;
    c.fs.release(h)?;
    Ok(())
}

pub fn two_handles_pin_until_last_release(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    c.write_all(f, 0, b"x")?;
    let h1 = c.fs.open(f)?;
    let h2 = c.fs.open(f)?;
    ensure!(h1 != h2, "two opens returned the same handle");
    c.forget_all(f);
    c.fs.unlink(ROOT_INO, b"f")?;
    c.fs.release(h1)?;
    ensure_eq!(
        c.content(f)?,
        b"x".to_vec(),
        "content after the first release"
    );
    c.fs.release(h2)?;
    ensure_err!(c.fs.getattr(f), Error::Stale, "after the second release");
    Ok(())
}

/// cowfs contract (`Ino` docs): an inode number is never reused for a different file for the
/// lifetime of a mount, and 0 is never a valid number. A stale NFS handle or kernel dentry
/// would otherwise read another file's bytes.
pub fn inode_numbers_are_never_reused(c: &Ctx) -> Outcome {
    let mut seen = std::collections::HashSet::new();
    seen.insert(ROOT_INO);
    for round in 0..200u32 {
        let name = format!("f{round}").into_bytes();
        let f = c.create(ROOT_INO, &name, 0o644)?.ino;
        ensure!(f != 0, "round {round}: created file has inode 0");
        ensure!(seen.insert(f), "round {round}: inode {f} was reused");
        let d = c
            .mkdir(ROOT_INO, format!("d{round}").as_bytes(), 0o755)?
            .ino;
        ensure!(d != 0, "round {round}: created directory has inode 0");
        ensure!(seen.insert(d), "round {round}: inode {d} was reused");
        let s = c
            .symlink(ROOT_INO, format!("s{round}").as_bytes(), b"t")?
            .ino;
        ensure!(s != 0, "round {round}: created symlink has inode 0");
        ensure!(seen.insert(s), "round {round}: inode {s} was reused");
        c.fs.unlink(ROOT_INO, &name)?;
        c.forget_all(f);
        c.fs.rmdir(ROOT_INO, format!("d{round}").as_bytes())?;
        c.forget_all(d);
        c.fs.unlink(ROOT_INO, format!("s{round}").as_bytes())?;
        c.forget_all(s);
        ensure!(
            c.fs.getattr(f).is_err() && c.fs.getattr(d).is_err(),
            "round {round}: a reclaimed inode was still reachable"
        );
    }
    let big = c.file(ROOT_INO, "big")?;
    ensure!(
        big != 0 && seen.insert(big),
        "inode {big} is invalid or reused"
    );
    Ok(())
}

/// `open` never fails because of intent, so a directory and a symlink open fine. `release` of
/// an unknown or already released handle is `InvalidArgument`, and `fsync` succeeds for every
/// kind, including the root, which is the whole-mount barrier.
pub fn open_release_and_fsync_contract(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let d = c.dir(ROOT_INO, "d")?;
    let s = c.symlink(ROOT_INO, b"s", b"t")?.ino;
    for (what, ino) in [("file", f), ("directory", d), ("symlink", s)] {
        let h = c.fs.open(ino)?;
        ensure!(
            c.fs.open(ino)? != h,
            "{what}: two opens returned the same handle"
        );
        c.fs.release(h)?;
        ensure_err!(
            c.fs.release(h),
            Error::InvalidArgument,
            "release of a released handle"
        );
        ensure_err!(
            c.fs.release(FileHandle(0xDEAD_BEEF)),
            Error::InvalidArgument,
            "release of an unknown handle"
        );
        c.fs.flush(ino)?;
        c.fs.fsync(ino, false)?;
        c.fs.fsync(ino, true)?;
    }
    c.fs.fsync(ROOT_INO, false)?;
    c.fs.fsync(ROOT_INO, true)?;
    ensure_err!(c.fs.open(0), Error::Stale, "open of inode 0");
    Ok(())
}
