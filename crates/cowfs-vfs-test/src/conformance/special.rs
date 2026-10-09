//! Special files: fifos, sockets, character and block devices (issue #107).
//!
//! Decisions pinned here: `mknod` creates a name with attributes and nothing else, so the new
//! node has `nlink` 1, size 0 and no blocks; a device keeps its `rdev` and every other kind
//! reports 0; `read`, `write` and a size change are `InvalidArgument` like on a symlink; a bad
//! kind or a device number on a fifo or socket is `InvalidArgument` and creates nothing; hardlink,
//! unlink, rename, `open`, mode and times work as for a regular file.

use cowfs_vfs::{makedev, Error, FileKind, RenameFlags, SetAttr, SetTime, Timestamp, ROOT_INO};

use super::{Ctx, Failure, Outcome};

const ALL_KINDS: [(FileKind, u64); 4] = [
    (FileKind::Fifo, 0),
    (FileKind::Socket, 0),
    (FileKind::CharDevice, makedev(1, 2)),
    (FileKind::BlockDevice, makedev(8, 16)),
];

/// The kinds this backend can create. A device node needs `CAP_MKNOD` on a real kernel: a
/// backend run with `no_device_privilege` (the host probe found this process lacks it) that
/// answers `PermissionDenied` to a device is checked with the fifo and the socket only, and the
/// device case is left to the backends that allow it (MemVfs, Core, a privileged run). Any other
/// `PermissionDenied` is a failure.
fn kinds(c: &Ctx) -> std::result::Result<Vec<(FileKind, u64)>, Failure> {
    match c.mknod(
        ROOT_INO,
        b".probe",
        FileKind::CharDevice,
        0o600,
        makedev(1, 3),
    ) {
        Ok(a) => {
            c.fs.unlink(ROOT_INO, b".probe")?;
            c.forget_all(a.ino);
            Ok(ALL_KINDS.to_vec())
        }
        Err(Error::PermissionDenied) if c.no_device_privilege => Ok(ALL_KINDS[..2].to_vec()),
        Err(e) => Err(e.into()),
    }
}

const T1: Timestamp = Timestamp {
    secs: 1_000_000,
    nanos: 111,
};

fn name(kind: FileKind) -> Vec<u8> {
    format!("{kind:?}").into_bytes()
}

pub fn mknod_fifo_attrs(c: &Ctx) -> Outcome {
    let before = c.fs.getattr(ROOT_INO)?;
    c.tick();
    let a = c.mknod(ROOT_INO, b"p", FileKind::Fifo, 0o640, 0)?;
    ensure_eq!(a.kind, FileKind::Fifo, "kind");
    ensure_eq!(a.mode, 0o640, "mode");
    ensure_eq!(
        (a.nlink, a.size, a.blocks, a.rdev),
        (1, 0, 0, 0),
        "nlink, size, blocks, rdev"
    );
    // ctime is not compared for equality: a backend that applies the mode after creating the node
    // bumps it. It must never be older than the creation time.
    ensure!(a.atime == a.mtime, "a new node has one creation time");
    ensure!(
        a.ctime >= a.mtime,
        "ctime must not precede the creation time"
    );
    let l = c.lookup(ROOT_INO, b"p")?;
    ensure_eq!(l, a, "lookup after mknod");
    ensure_eq!(c.fs.getattr(a.ino)?, a, "getattr after mknod");
    let after = c.fs.getattr(ROOT_INO)?;
    ensure!(
        after.mtime > before.mtime && after.ctime > before.ctime,
        "the parent's mtime and ctime must move"
    );
    Ok(())
}

pub fn mknod_socket_attrs(c: &Ctx) -> Outcome {
    let a = c.mknod(ROOT_INO, b"s", FileKind::Socket, 0o600, 0)?;
    ensure_eq!(a.kind, FileKind::Socket, "kind");
    ensure_eq!(
        (a.mode, a.nlink, a.size, a.blocks, a.rdev),
        (0o600, 1, 0, 0, 0),
        "mode, nlink, size, blocks, rdev"
    );
    ensure_eq!(c.lookup(ROOT_INO, b"s")?, a, "lookup after mknod");
    Ok(())
}

pub fn mknod_masks_mode(c: &Ctx) -> Outcome {
    let a = c.mknod(ROOT_INO, b"p", FileKind::Fifo, 0o170644, 0)?;
    ensure_eq!(a.mode, 0o644, "type bits are not permission bits");
    let b = c.mknod(ROOT_INO, b"q", FileKind::Fifo, 0o7777, 0)?;
    ensure_eq!(b.mode, 0o7777, "setuid, setgid, sticky and rwx are kept");
    Ok(())
}

pub fn mknod_device_attrs_keep_rdev(c: &Ctx) -> Outcome {
    if kinds(c)?.len() < 4 {
        return Ok(()); // devices need privilege here: covered by MemVfs and Core
    }
    let big = makedev(u32::MAX, u32::MAX);
    for (kind, rdev) in [
        (FileKind::CharDevice, makedev(1, 2)),
        (FileKind::BlockDevice, makedev(8, 16)),
        (FileKind::CharDevice, big),
        (FileKind::BlockDevice, makedev(0, 0)),
    ] {
        let n = format!("{kind:?}-{rdev:x}").into_bytes();
        let a = c.mknod(ROOT_INO, &n, kind, 0o600, rdev)?;
        ensure_eq!((a.kind, a.rdev), (kind, rdev), "mknod result for {kind:?}");
        ensure_eq!(
            (a.nlink, a.size, a.blocks),
            (1, 0, 0),
            "nlink, size, blocks"
        );
        ensure_eq!(c.fs.getattr(a.ino)?.rdev, rdev, "getattr rdev");
        ensure_eq!(c.lookup(ROOT_INO, &n)?.rdev, rdev, "lookup rdev");
        let listed = c.fs.readdir_attrs(ROOT_INO, 0, 1000)?;
        let e = listed.entries.iter().find(|e| e.entry.name == n);
        ensure_eq!(
            e.map(|e| (e.entry.kind, e.attr.rdev)),
            Some((kind, rdev)),
            "readdir_attrs for {kind:?}"
        );
    }
    Ok(())
}

pub fn mknod_existing_is_exists(c: &Ctx) -> Outcome {
    c.file(ROOT_INO, "f")?;
    c.dir(ROOT_INO, "d")?;
    for (kind, rdev) in kinds(c)? {
        for existing in [&b"f"[..], b"d"] {
            ensure_err!(
                c.mknod(ROOT_INO, existing, kind, 0o644, rdev),
                Error::Exists,
                "mknod {kind:?} over {existing:?}"
            );
        }
        c.mknod(ROOT_INO, &name(kind), kind, 0o644, rdev)?;
        ensure_err!(
            c.mknod(ROOT_INO, &name(kind), kind, 0o644, rdev),
            Error::Exists,
            "second mknod {kind:?}"
        );
    }
    Ok(())
}

pub fn mknod_in_file_is_not_dir(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    for (kind, rdev) in kinds(c)? {
        ensure_err!(
            c.mknod(f, b"x", kind, 0o644, rdev),
            Error::NotDir,
            "mknod {kind:?} in a file"
        );
    }
    Ok(())
}

pub fn mknod_stale_parent(c: &Ctx) -> Outcome {
    ensure_err!(
        c.mknod(super::basic::BOGUS, b"x", FileKind::Fifo, 0o644, 0),
        Error::Stale,
        "mknod in a parent that never existed"
    );
    Ok(())
}

pub fn mknod_invalid_names(c: &Ctx) -> Outcome {
    let long = vec![b'a'; cowfs_vfs::NAME_MAX + 1];
    for bad in [&b""[..], b".", b"..", b"a/b", b"a\0b"] {
        ensure_err!(
            c.mknod(ROOT_INO, bad, FileKind::Fifo, 0o644, 0),
            Error::InvalidArgument,
            "mknod {bad:?}"
        );
    }
    ensure_err!(
        c.mknod(ROOT_INO, &long, FileKind::Fifo, 0o644, 0),
        Error::NameTooLong,
        "mknod with a name over NAME_MAX"
    );
    let ok = vec![b'a'; cowfs_vfs::NAME_MAX];
    c.mknod(ROOT_INO, &ok, FileKind::Fifo, 0o644, 0)?;
    Ok(())
}

pub fn mknod_rejects_bad_arguments(c: &Ctx) -> Outcome {
    for kind in [FileKind::Regular, FileKind::Directory, FileKind::Symlink] {
        ensure_err!(
            c.mknod(ROOT_INO, b"x", kind, 0o644, 0),
            Error::InvalidArgument,
            "mknod of {kind:?}"
        );
    }
    for kind in [FileKind::Fifo, FileKind::Socket] {
        ensure_err!(
            c.mknod(ROOT_INO, b"x", kind, 0o644, makedev(1, 2)),
            Error::InvalidArgument,
            "mknod of {kind:?} with a device number"
        );
    }
    ensure_eq!(
        c.names(ROOT_INO)?,
        Vec::<Vec<u8>>::new(),
        "a rejected mknod creates nothing"
    );
    Ok(())
}

pub fn special_readdir_kinds(c: &Ctx) -> Outcome {
    for (kind, rdev) in kinds(c)? {
        c.mknod(ROOT_INO, &name(kind), kind, 0o644, rdev)?;
    }
    for (kind, _) in kinds(c)? {
        let want = name(kind);
        let e = c.list(ROOT_INO)?.into_iter().find(|e| e.name == want);
        ensure_eq!(e.map(|e| e.kind), Some(kind), "readdir kind of {kind:?}");
    }
    Ok(())
}

pub fn special_io_is_invalid(c: &Ctx) -> Outcome {
    for (kind, rdev) in kinds(c)? {
        let a = c.mknod(ROOT_INO, &name(kind), kind, 0o644, rdev)?;
        ensure_err!(
            c.fs.write(a.ino, 0, b"x"),
            Error::InvalidArgument,
            "write to {kind:?}"
        );
        ensure_err!(
            c.fs.read(a.ino, 0, 1),
            Error::InvalidArgument,
            "read of {kind:?}"
        );
        let resize = SetAttr {
            size: Some(10),
            ..SetAttr::default()
        };
        ensure_err!(
            c.fs.setattr(a.ino, resize),
            Error::InvalidArgument,
            "truncate of {kind:?}"
        );
        ensure_eq!(c.fs.getattr(a.ino)?.size, 0, "size after refused truncate");
        ensure_err!(
            c.fs.readlink(a.ino),
            Error::InvalidArgument,
            "readlink of {kind:?}"
        );
    }
    Ok(())
}

pub fn special_setattr_mode_and_times(c: &Ctx) -> Outcome {
    for (kind, rdev) in kinds(c)? {
        let a = c.mknod(ROOT_INO, &name(kind), kind, 0o644, rdev)?;
        c.tick();
        let b = c.fs.setattr(
            a.ino,
            SetAttr {
                mode: Some(0o600),
                atime: Some(SetTime::At(T1)),
                mtime: Some(SetTime::At(T1)),
                ..SetAttr::default()
            },
        )?;
        ensure_eq!(
            (b.mode, b.atime, b.mtime, b.kind, b.rdev),
            (0o600, T1, T1, kind, rdev),
            "setattr on {kind:?}"
        );
        ensure!(b.ctime > a.ctime, "setattr must bump ctime of {kind:?}");
    }
    Ok(())
}

pub fn special_open_release(c: &Ctx) -> Outcome {
    for (kind, rdev) in kinds(c)? {
        let a = c.mknod(ROOT_INO, &name(kind), kind, 0o644, rdev)?;
        let h = c.fs.open(a.ino)?;
        c.fs.flush(a.ino)?;
        c.fs.release(h)?;
    }
    Ok(())
}

pub fn special_hardlink_unlink_rename(c: &Ctx) -> Outcome {
    for (kind, rdev) in kinds(c)? {
        let n = name(kind);
        let a = c.mknod(ROOT_INO, &n, kind, 0o644, rdev)?;
        let l = c.link(a.ino, ROOT_INO, b"alias")?;
        ensure_eq!(
            (l.ino, l.nlink, l.kind, l.rdev),
            (a.ino, 2, kind, rdev),
            "link"
        );
        c.fs.unlink(ROOT_INO, &n)?;
        let still = c.fs.getattr(a.ino)?;
        ensure_eq!(
            (still.nlink, still.kind, still.rdev),
            (1, kind, rdev),
            "after unlink"
        );
        // rename over a special target replaces it, a directory target is IsDir
        let f = c.file(ROOT_INO, "f")?;
        c.fs.rename(ROOT_INO, b"f", ROOT_INO, b"alias", RenameFlags::default())?;
        ensure_eq!(
            c.lookup(ROOT_INO, b"alias")?.ino,
            f,
            "rename replaced {kind:?}"
        );
        ensure_eq!(
            c.fs.getattr(a.ino)?.nlink,
            0,
            "replaced node lost its last name"
        );
        c.fs.unlink(ROOT_INO, b"alias")?;
        let b = c.mknod(ROOT_INO, &n, kind, 0o644, rdev)?;
        c.dir(ROOT_INO, "dir")?;
        ensure_err!(
            c.fs.rename(ROOT_INO, &n, ROOT_INO, b"dir", RenameFlags::default()),
            Error::IsDir,
            "rename {kind:?} over a directory"
        );
        c.fs.rmdir(ROOT_INO, b"dir")?;
        c.fs.rename(ROOT_INO, &n, ROOT_INO, b"moved", RenameFlags::default())?;
        ensure_eq!(
            c.lookup(ROOT_INO, b"moved")?.ino,
            b.ino,
            "rename keeps the inode"
        );
        c.fs.unlink(ROOT_INO, b"moved")?;
    }
    Ok(())
}

pub fn special_unlinked_with_handle_survives(c: &Ctx) -> Outcome {
    for (kind, rdev) in kinds(c)? {
        let n = name(kind);
        let a = c.mknod(ROOT_INO, &n, kind, 0o644, rdev)?;
        let h = c.fs.open(a.ino)?;
        c.fs.unlink(ROOT_INO, &n)?;
        let g = c.fs.getattr(a.ino)?;
        ensure_eq!(
            (g.nlink, g.kind, g.rdev),
            (0, kind, rdev),
            "unlinked but open"
        );
        c.fs.release(h)?;
        c.forget_all(a.ino);
    }
    Ok(())
}

pub fn special_dir_ops_error(c: &Ctx) -> Outcome {
    for (kind, rdev) in kinds(c)? {
        let n = name(kind);
        c.mknod(ROOT_INO, &n, kind, 0o644, rdev)?;
        ensure_err!(c.fs.rmdir(ROOT_INO, &n), Error::NotDir, "rmdir of {kind:?}");
        ensure_err!(
            c.mkdir(ROOT_INO, &n, 0o755),
            Error::Exists,
            "mkdir over {kind:?}"
        );
        c.fs.unlink(ROOT_INO, &n)?;
        ensure_err!(
            c.lookup(ROOT_INO, &n),
            Error::NotFound,
            "lookup after unlink of {kind:?}"
        );
    }
    Ok(())
}

/// The character device 0:0 is Linux's whiteout: the kernel lets anyone make it, so a backend a
/// normal user reaches (a client of a mount, a native directory) must not refuse it (issue #243).
/// It keeps its kind, mode and an `rdev` of 0, and has no data. Off Linux a device needs
/// privilege, so `PermissionDenied` passes there.
pub fn mknod_whiteout_char_device(c: &Ctx) -> Outcome {
    let a = match c.mknod(ROOT_INO, b"wo", FileKind::CharDevice, 0o644, 0) {
        Err(Error::PermissionDenied) if !cfg!(target_os = "linux") => return Ok(()),
        r => r?,
    };
    ensure_eq!(
        (a.kind, a.mode, a.rdev, a.nlink, a.size, a.blocks),
        (FileKind::CharDevice, 0o644, 0, 1, 0, 0),
        "kind, mode, rdev, nlink, size, blocks"
    );
    ensure_eq!(c.lookup(ROOT_INO, b"wo")?, a, "lookup after mknod");
    let listed = c.fs.readdir_attrs(ROOT_INO, 0, 1000)?;
    let e = listed.entries.iter().find(|e| e.entry.name == b"wo");
    ensure_eq!(
        e.map(|e| (e.entry.kind, e.attr.rdev)),
        Some((FileKind::CharDevice, 0)),
        "readdir_attrs"
    );
    c.fs.unlink(ROOT_INO, b"wo")?;
    Ok(())
}
