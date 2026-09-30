//! Symlinks.
//!
//! Spike 6 lesson: a passthrough server followed the link on SETATTR, so times set on a
//! symlink landed on its target (or failed with ENOENT when the link dangled).
//!
//! Decisions pinned here: `Vfs` never resolves links, so `readlink` returns the target
//! bytes verbatim; a symlink's size is the length of its target; its `nlink` counts names
//! like a file's; `setattr` of times always applies to the link itself.

use cowfs_vfs::{Error, FileKind, SetAttr, SetTime, Timestamp, ROOT_INO};

use super::{Ctx, Outcome};

const T1: Timestamp = Timestamp {
    secs: 1_000_000,
    nanos: 111,
};
const T2: Timestamp = Timestamp {
    secs: 2_000_000,
    nanos: 222,
};

pub fn symlink_create_and_readlink(c: &Ctx) -> Outcome {
    let a = c.symlink(ROOT_INO, b"l", b"some/target")?;
    ensure_eq!(a.kind, FileKind::Symlink, "kind");
    ensure_eq!(a.nlink, 1, "nlink");
    ensure_eq!(c.fs.readlink(a.ino)?, b"some/target".to_vec(), "readlink");
    let l = c.lookup(ROOT_INO, b"l")?;
    ensure_eq!(
        (l.ino, l.kind),
        (a.ino, FileKind::Symlink),
        "lookup of the link"
    );
    let e = c.list(ROOT_INO)?;
    ensure_eq!((e.len(), e[0].kind), (1, FileKind::Symlink), "listing kind");
    Ok(())
}

pub fn symlink_dangling_ok(c: &Ctx) -> Outcome {
    let a = c.symlink(ROOT_INO, b"dangling", b"/does/not/exist")?;
    ensure_eq!(
        c.fs.readlink(a.ino)?,
        b"/does/not/exist".to_vec(),
        "dangling target"
    );
    ensure_eq!(
        c.fs.getattr(a.ino)?.kind,
        FileKind::Symlink,
        "getattr of a dangling link"
    );
    c.fs.setattr(
        a.ino,
        SetAttr {
            atime: Some(SetTime::At(T1)),
            mtime: Some(SetTime::At(T2)),
            ..Default::default()
        },
    )?;
    Ok(())
}

pub fn symlink_size_is_target_length(c: &Ctx) -> Outcome {
    for (i, len) in [1usize, 5, 255, 1000, 4095].into_iter().enumerate() {
        let target = vec![b'x'; len];
        let a = c.symlink(ROOT_INO, format!("l{i}").as_bytes(), &target)?;
        ensure_eq!(a.size, len as u64, "size returned by symlink");
        ensure_eq!(c.fs.getattr(a.ino)?.size, len as u64, "size from getattr");
        ensure_eq!(c.fs.readlink(a.ino)?, target, "target of length {len}");
    }
    Ok(())
}

pub fn symlink_to_symlink(c: &Ctx) -> Outcome {
    let a = c.symlink(ROOT_INO, b"a", b"b")?.ino;
    let b = c.symlink(ROOT_INO, b"b", b"a")?.ino;
    let s = c.symlink(ROOT_INO, b"self", b"self")?.ino;
    ensure_eq!(c.fs.readlink(a)?, b"b".to_vec(), "a -> b");
    ensure_eq!(c.fs.readlink(b)?, b"a".to_vec(), "b -> a");
    ensure_eq!(c.fs.readlink(s)?, b"self".to_vec(), "self -> self");
    ensure_eq!(
        c.fs.getattr(a)?.kind,
        FileKind::Symlink,
        "a is not resolved"
    );
    Ok(())
}

pub fn unlink_symlink_keeps_target(c: &Ctx) -> Outcome {
    let t = c.file(ROOT_INO, "t")?;
    c.write_all(t, 0, b"target data")?;
    let d = c.dir(ROOT_INO, "d")?;
    c.symlink(ROOT_INO, b"lf", b"t")?;
    c.symlink(ROOT_INO, b"ld", b"d")?;
    c.fs.unlink(ROOT_INO, b"lf")?;
    c.fs.unlink(ROOT_INO, b"ld")?;
    ensure_eq!(
        c.content(c.lookup(ROOT_INO, b"t")?.ino)?,
        b"target data".to_vec(),
        "file target after unlinking its link"
    );
    ensure_eq!(
        c.lookup(ROOT_INO, b"d")?.ino,
        d,
        "directory target after unlinking its link"
    );
    ensure_eq!(c.fs.getattr(t)?.nlink, 1, "target nlink");
    Ok(())
}

pub fn setattr_times_on_symlink_never_touch_target(c: &Ctx) -> Outcome {
    let t = c.file(ROOT_INO, "t")?;
    c.fs.setattr(
        t,
        SetAttr {
            atime: Some(SetTime::At(T1)),
            mtime: Some(SetTime::At(T1)),
            ..Default::default()
        },
    )?;
    let l = c.symlink(ROOT_INO, b"l", b"t")?.ino;
    let before = c.fs.getattr(t)?;
    c.tick();
    let a = c.fs.setattr(
        l,
        SetAttr {
            atime: Some(SetTime::At(T2)),
            mtime: Some(SetTime::At(T2)),
            ..Default::default()
        },
    )?;
    ensure_eq!((a.atime, a.mtime), (T2, T2), "times of the link");
    ensure_eq!(c.fs.getattr(l)?.mtime, T2, "times of the link from getattr");
    let after = c.fs.getattr(t)?;
    ensure_eq!(
        (after.atime, after.mtime, after.ctime),
        (before.atime, before.mtime, before.ctime),
        "times of the target"
    );
    let a = c.fs.setattr(
        l,
        SetAttr {
            mtime: Some(SetTime::Now),
            ..Default::default()
        },
    )?;
    ensure!(a.mtime > T2, "SetTime::Now on a link is {:?}", a.mtime);
    ensure_eq!(
        c.fs.getattr(t)?.mtime,
        T1,
        "target mtime after SetTime::Now on the link"
    );

    let d = c.dir(ROOT_INO, "d")?;
    let dl = c.symlink(ROOT_INO, b"dl", b"d")?.ino;
    let dbefore = c.fs.getattr(d)?;
    c.fs.setattr(
        dl,
        SetAttr {
            mtime: Some(SetTime::At(T2)),
            ..Default::default()
        },
    )?;
    let dafter = c.fs.getattr(d)?;
    ensure_eq!(
        (dafter.mtime, dafter.ctime),
        (dbefore.mtime, dbefore.ctime),
        "directory times after setattr on a link to it"
    );
    Ok(())
}

pub fn readlink_on_non_symlink_is_invalid(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let d = c.dir(ROOT_INO, "d")?;
    ensure_err!(
        c.fs.readlink(f),
        Error::InvalidArgument,
        "readlink of a file"
    );
    ensure_err!(
        c.fs.readlink(d),
        Error::InvalidArgument,
        "readlink of a directory"
    );
    Ok(())
}

pub fn symlink_over_existing_is_exists(c: &Ctx) -> Outcome {
    c.file(ROOT_INO, "f")?;
    let s = c.symlink(ROOT_INO, b"s", b"one")?.ino;
    ensure_err!(
        c.symlink(ROOT_INO, b"f", b"x"),
        Error::Exists,
        "symlink over a file"
    );
    ensure_err!(
        c.symlink(ROOT_INO, b"s", b"two"),
        Error::Exists,
        "symlink over a symlink"
    );
    ensure_eq!(
        c.fs.readlink(s)?,
        b"one".to_vec(),
        "target after a rejected symlink"
    );
    Ok(())
}

pub fn symlink_binary_target(c: &Ctx) -> Outcome {
    let target: Vec<u8> = vec![0xff, b'/', b'.', b'.', b'/', 0xc3, 0x28, b' ', b'\n', 0x01];
    let a = c.symlink(ROOT_INO, b"bin", &target)?;
    ensure_eq!(a.size, target.len() as u64, "size of a binary target");
    ensure_eq!(c.fs.readlink(a.ino)?, target, "binary target");
    Ok(())
}
