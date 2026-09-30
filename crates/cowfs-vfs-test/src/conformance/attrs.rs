//! `setattr` and the timestamp rules.
//!
//! Decisions pinned here: `setattr` is all or nothing; times given with `SetTime::At` are
//! stored exactly (nanoseconds included); an explicit `mtime` in the same call as a
//! truncate wins over the truncate's automatic mtime; every `setattr` that changes
//! something bumps ctime; link, unlink, rename, create, mkdir, symlink and rmdir set
//! mtime and ctime of the parent directory (both parents for rename) and ctime of the
//! file they touch, and leave that file's mtime alone.

use cowfs_vfs::{Error, FileKind, SetAttr, SetTime, Timestamp, MODE_MASK, ROOT_INO};

use super::basic::near;
use super::{Ctx, Outcome};

const T1: Timestamp = Timestamp {
    secs: 5_000_000,
    nanos: 123_456_789,
};
const T2: Timestamp = Timestamp {
    secs: 6_000_000,
    nanos: 987_654_321,
};

fn mode(m: u32) -> SetAttr {
    SetAttr {
        mode: Some(m),
        ..Default::default()
    }
}

pub fn setattr_mode_is_masked(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let a = c.fs.setattr(f, mode(0o170_755))?;
    ensure_eq!(a.mode, 0o755, "mode with file-type bits");
    ensure_eq!(c.fs.getattr(f)?.mode, 0o755, "getattr after masked setattr");
    ensure_eq!(
        c.fs.setattr(f, mode(0o7777))?.mode,
        0o7777,
        "all permission bits"
    );
    ensure_eq!(c.fs.setattr(f, mode(0))?.mode, 0, "mode 0");
    ensure_eq!(
        c.fs.setattr(f, mode(u32::MAX))?.mode,
        MODE_MASK,
        "all bits set"
    );
    let d = c.dir(ROOT_INO, "d")?;
    ensure_eq!(
        c.fs.setattr(d, mode(0o40_700))?.mode,
        0o700,
        "directory mode"
    );
    Ok(())
}

pub fn setattr_size_on_directory_is_isdir(c: &Ctx) -> Outcome {
    let d = c.dir(ROOT_INO, "d")?;
    let before = c.fs.getattr(d)?;
    let bad = SetAttr {
        size: Some(0),
        mode: Some(0o700),
        ..Default::default()
    };
    ensure_err!(c.fs.setattr(d, bad), Error::IsDir, "size on a directory");
    let after = c.fs.getattr(d)?;
    ensure_eq!(after.mode, before.mode, "mode changed by a failed setattr");
    ensure_eq!(
        after.ctime,
        before.ctime,
        "ctime changed by a failed setattr"
    );
    ensure_eq!(
        c.fs.setattr(d, mode(0o700))?.mode,
        0o700,
        "mode-only setattr on a directory"
    );
    Ok(())
}

pub fn setattr_size_on_symlink_is_invalid(c: &Ctx) -> Outcome {
    let s = c.symlink(ROOT_INO, b"s", b"target")?.ino;
    let before = c.fs.getattr(s)?;
    ensure_err!(
        c.fs.setattr(
            s,
            SetAttr {
                size: Some(0),
                ..Default::default()
            }
        ),
        Error::InvalidArgument,
        "size on a symlink"
    );
    let after = c.fs.getattr(s)?;
    ensure_eq!(
        after.size,
        before.size,
        "symlink size after failed truncate"
    );
    ensure_eq!(
        c.fs.readlink(s)?,
        b"target".to_vec(),
        "symlink target after failed truncate"
    );
    Ok(())
}

pub fn setattr_failure_changes_nothing(c: &Ctx) -> Outcome {
    let s = c.symlink(ROOT_INO, b"s", b"target")?.ino;
    let before = c.fs.getattr(s)?;
    let bad = SetAttr {
        mode: Some(0o600),
        size: Some(1),
        atime: Some(SetTime::At(T1)),
        mtime: Some(SetTime::At(T2)),
    };
    ensure_err!(
        c.fs.setattr(s, bad),
        Error::InvalidArgument,
        "mixed setattr on a symlink"
    );
    let after = c.fs.getattr(s)?;
    ensure!(
        (after.mode, after.atime, after.mtime, after.ctime)
            == (before.mode, before.atime, before.mtime, before.ctime),
        "failed setattr partially applied: {before:?} -> {after:?}"
    );
    Ok(())
}

pub fn setattr_combined_changes_apply(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    c.write_all(f, 0, b"0123456789")?;
    let a = c.fs.setattr(
        f,
        SetAttr {
            mode: Some(0o600),
            size: Some(3),
            atime: Some(SetTime::At(T1)),
            mtime: Some(SetTime::At(T2)),
        },
    )?;
    ensure_eq!((a.mode, a.size), (0o600, 3), "mode and size");
    ensure_eq!(a.atime, T1, "atime");
    ensure_eq!(
        a.mtime,
        T2,
        "explicit mtime must win over the truncate's mtime"
    );
    let g = c.fs.getattr(f)?;
    ensure_eq!(
        (g.mode, g.size, g.atime, g.mtime),
        (a.mode, a.size, a.atime, a.mtime),
        "getattr after setattr"
    );
    ensure_eq!(
        c.content(f)?,
        b"012".to_vec(),
        "content after combined setattr"
    );
    Ok(())
}

pub fn setattr_times_now_and_at(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let a = c.fs.setattr(
        f,
        SetAttr {
            atime: Some(SetTime::At(T1)),
            ..Default::default()
        },
    )?;
    ensure_eq!(a.atime, T1, "atime At");
    ensure!(a.mtime != T1, "setting atime changed mtime");
    let a = c.fs.setattr(
        f,
        SetAttr {
            mtime: Some(SetTime::At(T2)),
            ..Default::default()
        },
    )?;
    ensure_eq!((a.atime, a.mtime), (T1, T2), "mtime At leaves atime alone");
    c.tick();
    let before = Timestamp::now();
    let a = c.fs.setattr(
        f,
        SetAttr {
            atime: Some(SetTime::Now),
            ..Default::default()
        },
    )?;
    let after = Timestamp::now();
    ensure!(
        near(a.atime, before, after) && a.atime > T1,
        "atime Now is {:?}",
        a.atime
    );
    ensure_eq!(a.mtime, T2, "atime Now must not touch mtime");
    let a = c.fs.setattr(
        f,
        SetAttr {
            mtime: Some(SetTime::Now),
            ..Default::default()
        },
    )?;
    ensure!(
        near(a.mtime, before, Timestamp::now()) && a.mtime > T2,
        "mtime Now is {:?}",
        a.mtime
    );
    let g = c.fs.getattr(f)?;
    ensure_eq!(
        (g.atime, g.mtime),
        (a.atime, a.mtime),
        "getattr after time changes"
    );
    Ok(())
}

pub fn setattr_bumps_ctime(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let old = c.fs.getattr(f)?;
    c.tick();
    let a = c.fs.setattr(f, mode(0o600))?;
    ensure!(a.ctime > old.ctime, "chmod did not bump ctime");
    ensure_eq!(a.mtime, old.mtime, "chmod changed mtime");
    c.tick();
    let b = c.fs.setattr(
        f,
        SetAttr {
            atime: Some(SetTime::At(T1)),
            ..Default::default()
        },
    )?;
    ensure!(b.ctime > a.ctime, "utimes did not bump ctime");
    c.fs.setattr(f, SetAttr::default())?;
    Ok(())
}

pub fn namespace_ops_update_times(c: &Ctx) -> Outcome {
    let d = c.dir(ROOT_INO, "d")?;
    let f = c.file(d, "f")?;
    let mut dir = c.fs.getattr(d)?;
    let mut file = c.fs.getattr(f)?;
    macro_rules! step {
        ($what:expr, $op:expr, $file_ctime:expr) => {{
            c.tick();
            $op;
            let (nd, nf) = (c.fs.getattr(d)?, c.fs.getattr(f)?);
            ensure!(
                nd.mtime > dir.mtime,
                "{} did not update the parent mtime",
                $what
            );
            ensure!(
                nd.ctime > dir.ctime,
                "{} did not update the parent ctime",
                $what
            );
            if $file_ctime {
                ensure!(
                    nf.ctime > file.ctime,
                    "{} did not update the file ctime",
                    $what
                );
            }
            ensure_eq!(nf.mtime, file.mtime, "{} changed the file mtime", $what);
            dir = nd;
            file = nf;
        }};
    }
    step!(
        "link",
        {
            c.link(f, d, b"g")?;
        },
        true
    );
    step!(
        "unlink of a link",
        {
            c.fs.unlink(d, b"g")?;
        },
        true
    );
    step!(
        "create",
        {
            c.file(d, "new")?;
        },
        false
    );
    step!(
        "mkdir",
        {
            c.dir(d, "sub")?;
        },
        false
    );
    step!(
        "symlink",
        {
            c.symlink(d, b"sl", b"x")?;
        },
        false
    );
    step!(
        "rmdir",
        {
            c.fs.rmdir(d, b"sub")?;
        },
        false
    );
    step!(
        "rename within the directory",
        {
            c.fs.rename(d, b"f", d, b"f2", Default::default())?;
        },
        true
    );
    let e = c.dir(ROOT_INO, "e")?;
    let e0 = c.fs.getattr(e)?;
    c.tick();
    c.fs.rename(d, b"f2", e, b"f3", Default::default())?;
    let (nd, ne) = (c.fs.getattr(d)?, c.fs.getattr(e)?);
    ensure!(
        nd.mtime > dir.mtime && nd.ctime > dir.ctime,
        "rename did not update the source directory times"
    );
    ensure!(
        ne.mtime > e0.mtime && ne.ctime > e0.ctime,
        "rename did not update the destination directory times"
    );
    let moved = c.fs.getattr(f)?;
    ensure_eq!(moved.kind, FileKind::Regular, "renamed file kind");
    ensure!(
        moved.ctime > file.ctime,
        "cross-directory rename did not update the file ctime"
    );
    Ok(())
}
