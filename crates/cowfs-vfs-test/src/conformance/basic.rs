//! Creation, lookup, attributes, statfs and the `Stale` rule.
//!
//! The directory `nlink` rule below is a cowfs decision: ext4 uses it, btrfs reports 1 and APFS
//! reports 2 + all children.
//!
//! Decisions pinned here: the root is `ROOT_INO`; a directory reports `nlink` = 2 + its
//! subdirectories; a regular file reports the number of names; a new file has size 0 and
//! `blocks` 0; `uid` and `gid` are the same for every file; `create` and `mkdir` keep only
//! `mode & MODE_MASK`.

use cowfs_vfs::{
    Attr, Error, FileKind, Ino, RenameFlags, SetAttr, Timestamp, XattrFlags, MODE_MASK, NAME_MAX,
    ROOT_INO,
};

use super::{Ctx, Outcome};

pub(super) const BOGUS: Ino = 0x00FF_FFFF_FFFF_F001;
const SLACK_SECS: i64 = 5;

/// True when every field except `atime` matches (a backend may update atime on read).
pub(crate) fn stable_eq(a: &Attr, b: &Attr) -> bool {
    (
        a.ino, a.kind, a.mode, a.nlink, a.uid, a.gid, a.size, a.mtime, a.ctime,
    ) == (
        b.ino, b.kind, b.mode, b.nlink, b.uid, b.gid, b.size, b.mtime, b.ctime,
    )
}

/// True when `t` lies between `before` and `after`, give or take a few seconds of clock skew.
pub(crate) fn near(t: Timestamp, before: Timestamp, after: Timestamp) -> bool {
    t.secs >= before.secs - SLACK_SECS && t.secs <= after.secs + SLACK_SECS
}

pub fn root_is_directory(c: &Ctx) -> Outcome {
    let a = c.fs.getattr(ROOT_INO)?;
    ensure_eq!(a.ino, ROOT_INO, "root inode");
    ensure_eq!(a.kind, FileKind::Directory, "root kind");
    ensure_eq!(a.nlink, 2, "empty root nlink");
    ensure!(
        a.mode <= MODE_MASK,
        "root mode {:o} has bits outside MODE_MASK",
        a.mode
    );
    ensure_err!(
        c.lookup(ROOT_INO, b"nothing"),
        Error::NotFound,
        "lookup in empty root"
    );
    ensure!(
        c.list(ROOT_INO)?.is_empty(),
        "fresh filesystem root is not empty"
    );
    Ok(())
}

pub fn create_lookup_getattr(c: &Ctx) -> Outcome {
    let a = c.create(ROOT_INO, b"f", 0o640)?;
    ensure_eq!(a.kind, FileKind::Regular, "created kind");
    ensure_eq!(a.mode, 0o640, "created mode");
    ensure_eq!(a.size, 0, "created size");
    ensure!(a.ino != ROOT_INO && a.ino != 0, "created inode {}", a.ino);
    let l = c.lookup(ROOT_INO, b"f")?;
    ensure!(
        stable_eq(&a, &l),
        "lookup differs from create: {l:?} vs {a:?}"
    );
    let g = c.fs.getattr(a.ino)?;
    ensure!(
        stable_eq(&a, &g),
        "getattr differs from create: {g:?} vs {a:?}"
    );
    Ok(())
}

pub fn create_existing_is_exists(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    c.write_all(f, 0, b"keep")?;
    ensure_err!(
        c.create(ROOT_INO, b"f", 0o644),
        Error::Exists,
        "create over file"
    );
    ensure_err!(
        c.mkdir(ROOT_INO, b"f", 0o755),
        Error::Exists,
        "mkdir over file"
    );
    ensure_err!(
        c.symlink(ROOT_INO, b"f", b"t"),
        Error::Exists,
        "symlink over file"
    );
    ensure_eq!(
        c.content(f)?,
        b"keep".to_vec(),
        "file content after failed creates"
    );
    c.dir(ROOT_INO, "d")?;
    ensure_err!(
        c.create(ROOT_INO, b"d", 0o644),
        Error::Exists,
        "create over dir"
    );
    Ok(())
}

pub fn lookup_missing_is_not_found(c: &Ctx) -> Outcome {
    ensure_err!(
        c.lookup(ROOT_INO, b"missing"),
        Error::NotFound,
        "lookup missing"
    );
    let d = c.dir(ROOT_INO, "d")?;
    ensure_err!(
        c.lookup(d, b"missing"),
        Error::NotFound,
        "lookup missing in subdirectory"
    );
    Ok(())
}

pub fn lookup_in_file_is_not_dir(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    ensure_err!(c.lookup(f, b"x"), Error::NotDir, "lookup in a file");
    let s = c.symlink(ROOT_INO, b"s", b"f")?.ino;
    ensure_err!(c.lookup(s, b"x"), Error::NotDir, "lookup in a symlink");
    Ok(())
}

pub fn create_in_file_is_not_dir(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    ensure_err!(c.create(f, b"x", 0o644), Error::NotDir, "create in a file");
    ensure_err!(c.mkdir(f, b"x", 0o755), Error::NotDir, "mkdir in a file");
    ensure_err!(c.symlink(f, b"x", b"t"), Error::NotDir, "symlink in a file");
    ensure_err!(c.link(f, f, b"x"), Error::NotDir, "link into a file");
    ensure_err!(c.fs.unlink(f, b"x"), Error::NotDir, "unlink in a file");
    ensure_err!(c.fs.rmdir(f, b"x"), Error::NotDir, "rmdir in a file");
    Ok(())
}

pub fn create_masks_mode(c: &Ctx) -> Outcome {
    let a = c.create(ROOT_INO, b"f", 0o100_644)?;
    ensure_eq!(a.mode, 0o644, "create with file-type bits");
    let d = c.mkdir(ROOT_INO, b"d", 0o040_755)?;
    ensure_eq!(d.mode, 0o755, "mkdir with file-type bits");
    let s = c.create(ROOT_INO, b"suid", 0o4755)?;
    ensure_eq!(s.mode, 0o4755, "setuid bit is a permission bit");
    Ok(())
}

pub fn new_file_attrs(c: &Ctx) -> Outcome {
    let root = c.fs.getattr(ROOT_INO)?;
    let before = Timestamp::now();
    let f = c.create(ROOT_INO, b"f", 0o644)?;
    let d = c.mkdir(ROOT_INO, b"d", 0o755)?;
    let after = Timestamp::now();
    ensure_eq!(f.nlink, 1, "new file nlink");
    ensure_eq!(f.size, 0, "new file size");
    ensure_eq!(f.blocks, 0, "empty file blocks");
    for (what, a) in [("file", &f), ("directory", &d)] {
        ensure_eq!(
            (a.uid, a.gid),
            (root.uid, root.gid),
            "{what} owner differs from root"
        );
        for (n, t) in [("atime", a.atime), ("mtime", a.mtime), ("ctime", a.ctime)] {
            ensure!(
                near(t, before, after),
                "new {what} {n} {t:?} is not near now ({before:?}..{after:?})"
            );
        }
    }
    ensure_eq!(d.nlink, 2, "new directory nlink");
    Ok(())
}

pub fn distinct_files_distinct_inodes(c: &Ctx) -> Outcome {
    let mut seen = std::collections::HashSet::new();
    seen.insert(ROOT_INO);
    for i in 0..100 {
        let ino = c.file(ROOT_INO, &format!("f{i}"))?;
        ensure!(seen.insert(ino), "inode {ino} handed out twice");
        let d = c.dir(ROOT_INO, &format!("d{i}"))?;
        ensure!(seen.insert(d), "inode {d} handed out twice");
    }
    Ok(())
}

pub fn dir_nlink_counts_subdirs(c: &Ctx) -> Outcome {
    let nlink = |ino| c.fs.getattr(ino).map(|a| a.nlink);
    ensure_eq!(nlink(ROOT_INO)?, 2, "empty root");
    let a = c.dir(ROOT_INO, "a")?;
    ensure_eq!(nlink(ROOT_INO)?, 3, "root after mkdir a");
    ensure_eq!(nlink(a)?, 2, "new dir a");
    let b = c.dir(a, "b")?;
    ensure_eq!(nlink(a)?, 3, "a after mkdir a/b");
    ensure_eq!(nlink(ROOT_INO)?, 3, "root unchanged by a/b");
    ensure_eq!(nlink(b)?, 2, "new dir b");
    c.file(a, "file")?;
    c.symlink(a, b"link", b"x")?;
    ensure_eq!(nlink(a)?, 3, "a after adding a file and a symlink");
    c.fs.rmdir(a, b"b")?;
    ensure_eq!(nlink(a)?, 2, "a after rmdir a/b");
    c.dir(ROOT_INO, "c")?;
    c.dir(ROOT_INO, "d")?;
    ensure_eq!(nlink(ROOT_INO)?, 5, "root with three subdirectories");
    Ok(())
}

/// A `StatFs` field of 0 means unknown, not zero (btrfs reports `files` 0), so the inode count
/// is only checked when the backend reports it.
/// The macOS NFS client caches statfs for about 0.2 s, so a check that measures free space
/// right after a write sees old numbers; only inequalities are asserted, never a delta.
pub fn statfs_sane(c: &Ctx) -> Outcome {
    let s = c.fs.statfs()?;
    ensure!(s.block_size > 0, "block_size is 0");
    ensure!(s.blocks > 0, "blocks is 0");
    ensure!(
        s.blocks_free <= s.blocks,
        "blocks_free {} > blocks {}",
        s.blocks_free,
        s.blocks
    );
    ensure!(
        s.blocks_available <= s.blocks_free,
        "blocks_available > blocks_free"
    );
    ensure!(
        s.files == 0 || s.files_free <= s.files,
        "files_free {} > files {}",
        s.files_free,
        s.files
    );
    ensure_eq!(s.name_max as usize, NAME_MAX, "name_max");
    let f = c.file(ROOT_INO, "f")?;
    c.write_all(f, 0, &super::pattern(1 << 20, 3))?;
    let after = c.fs.statfs()?;
    ensure!(
        after.blocks_free <= s.blocks_free,
        "writing 1 MiB grew free space"
    );
    if s.files > 0 {
        ensure!(
            after.files_free < s.files_free,
            "creating a file did not reduce files_free"
        );
    }
    Ok(())
}

/// Every operation on `ino`, which must not exist, returns `Stale`. `real` is a live regular file.
pub(crate) fn all_ops_stale(c: &Ctx, ino: Ino, real: Ino) -> Outcome {
    let fs = &*c.fs;
    ensure_err!(fs.getattr(ino), Error::Stale, "getattr");
    ensure_err!(
        fs.setattr(
            ino,
            SetAttr {
                mode: Some(0o600),
                ..Default::default()
            }
        ),
        Error::Stale,
        "setattr"
    );
    ensure_err!(fs.readlink(ino), Error::Stale, "readlink");
    ensure_err!(fs.lookup(ino, b"x"), Error::Stale, "lookup in it");
    ensure_err!(fs.create(ino, b"x", 0o644), Error::Stale, "create in it");
    ensure_err!(fs.mkdir(ino, b"x", 0o755), Error::Stale, "mkdir in it");
    ensure_err!(fs.symlink(ino, b"x", b"t"), Error::Stale, "symlink in it");
    ensure_err!(fs.link(ino, ROOT_INO, b"x"), Error::Stale, "link of it");
    ensure_err!(fs.link(real, ino, b"x"), Error::Stale, "link into it");
    ensure_err!(fs.unlink(ino, b"x"), Error::Stale, "unlink in it");
    ensure_err!(fs.rmdir(ino, b"x"), Error::Stale, "rmdir in it");
    ensure_err!(
        fs.rename(ino, b"x", ROOT_INO, b"y", RenameFlags::default()),
        Error::Stale,
        "rename from it"
    );
    ensure_err!(
        fs.rename(ROOT_INO, b"real", ino, b"y", RenameFlags::default()),
        Error::Stale,
        "rename into it"
    );
    ensure_err!(fs.open(ino), Error::Stale, "open");
    ensure_err!(fs.read(ino, 0, 10), Error::Stale, "read");
    ensure_err!(fs.write(ino, 0, b"x"), Error::Stale, "write");
    ensure_err!(fs.flush(ino), Error::Stale, "flush");
    ensure_err!(fs.fsync(ino, false), Error::Stale, "fsync");
    ensure_err!(fs.readdir(ino, 0, 10), Error::Stale, "readdir");
    ensure_err!(fs.getxattr(ino, b"user.a"), Error::Stale, "getxattr");
    ensure_err!(
        fs.setxattr(ino, b"user.a", b"v", XattrFlags::default()),
        Error::Stale,
        "setxattr"
    );
    ensure_err!(fs.listxattr(ino), Error::Stale, "listxattr");
    ensure_err!(fs.removexattr(ino, b"user.a"), Error::Stale, "removexattr");
    Ok(())
}

pub fn stale_on_never_existing_inode(c: &Ctx) -> Outcome {
    let real = c.file(ROOT_INO, "real")?;
    all_ops_stale(c, BOGUS, real)
}

pub fn timestamps_track_wall_clock(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let mut last = c.fs.getattr(f)?;
    for i in 0..5u64 {
        c.tick();
        let before = Timestamp::now();
        c.write_all(f, i * 10, b"0123456789")?;
        let after = Timestamp::now();
        let a = c.fs.getattr(f)?;
        ensure!(
            near(a.mtime, before, after),
            "mtime {:?} not near write time",
            a.mtime
        );
        ensure!(
            a.mtime >= last.mtime,
            "mtime went backwards: {:?} < {:?}",
            a.mtime,
            last.mtime
        );
        ensure!(
            a.ctime >= last.ctime,
            "ctime went backwards: {:?} < {:?}",
            a.ctime,
            last.ctime
        );
        ensure!(
            a.ctime >= a.mtime,
            "ctime {:?} behind mtime {:?}",
            a.ctime,
            a.mtime
        );
        last = a;
    }
    Ok(())
}

/// cowfs contract: freeing the last name of a file (and dropping every reference) returns its
/// space. A native filesystem shares its free space with other users, so this is not portable.
pub fn statfs_free_after_unlink(c: &Ctx) -> Outcome {
    let before = c.fs.statfs()?.blocks_free;
    let f = c.file(ROOT_INO, "f")?;
    c.write_all(f, 0, &super::pattern(1 << 20, 1))?;
    ensure!(
        c.fs.statfs()?.blocks_free < before,
        "writing 1 MiB did not consume space"
    );
    c.fs.unlink(ROOT_INO, b"f")?;
    c.forget_all(f);
    ensure_eq!(
        c.fs.statfs()?.blocks_free,
        before,
        "blocks_free after unlinking the only name"
    );
    Ok(())
}
