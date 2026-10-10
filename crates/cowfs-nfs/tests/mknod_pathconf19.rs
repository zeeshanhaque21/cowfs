//! MKNOD and PATHCONF values over raw NFSv3, against the in-process server over `MemVfs`.
mod common;

use common::*;
use cowfs_nfs::MountOptions;
use nfsserve::nfs::{ftype3, nfs_fh3, nfsstat3, post_op_attr, specdata3};

fn mknod_args(dir: &nfs_fh3, n: &str, kind: ftype3, mode: u32, dev: (u32, u32)) -> Args {
    let mut a = Args::new()
        .put(&dirop(dir, n))
        .put(&(kind as u32))
        .put(&sattr_mode(mode));
    if matches!(kind, ftype3::NF3CHR | ftype3::NF3BLK) {
        a = a.put(&specdata3 {
            specdata1: dev.0,
            specdata2: dev.1,
        });
    }
    a
}

/// Seconds since the epoch, as an NFS `nfstime3` carries them.
fn wall_secs() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32
}

/// MKNOD as root (AUTH_NULL counts as root).
fn mknod(c: &mut Nfs, dir: &nfs_fh3, n: &str, kind: ftype3, dev: (u32, u32)) -> u32 {
    c.call(11, mknod_args(dir, n, kind, 0o640, dev)).0
}

#[test]
fn mknod_creates_fifo_socket_and_devices_with_type_mode_and_rdev() {
    let (_s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    for (kind, n, dev) in [
        (ftype3::NF3FIFO, "fifo", (0, 0)),
        (ftype3::NF3SOCK, "sock", (0, 0)),
        (ftype3::NF3CHR, "chr", (1, 3)),
        (ftype3::NF3BLK, "blk", (8, 16)),
    ] {
        let before = wall_secs();
        assert_eq!(mknod(&mut c, &root, n, kind, dev), OK, "MKNOD {n}");
        let after = wall_secs();
        let fh = c.must_lookup(&root, n);
        let (st, a) = c.getattr(&fh);
        let a = a.expect("attributes");
        assert_eq!(st, OK);
        assert_eq!(a.ftype as u32, kind as u32, "{n} type");
        assert_eq!(a.mode, 0o640, "{n} mode");
        assert_eq!((a.nlink, a.size), (1, 0), "{n} nlink and size");
        assert_eq!((a.rdev.specdata1, a.rdev.specdata2), dev, "{n} rdev");
        // Issue 326: the ctime of a fresh special node survives the wire as wall-clock time
        // (not zero, not the epoch default), within 5 s either side of the call like the
        // Portable conformance check `mknod_ctime_is_wall_clock`. `MemVfs` stamps atime, mtime
        // and ctime with the same value, so this does not pin that the wire reads the *ctime*
        // field; `convert.rs`'s unit test with distinct times does. Saturating adds: a
        // far-future ctime is clamped to `u32::MAX` and must fail the assert, not overflow.
        assert!(
            a.ctime.seconds != 0
                && a.ctime.seconds.saturating_add(5) >= before
                && a.ctime.seconds <= after.saturating_add(5),
            "{n} ctime {} outside [{before}, {after}] +-5 s",
            a.ctime.seconds
        );
        assert_eq!(
            mknod(&mut c, &root, n, kind, dev),
            EXIST,
            "second MKNOD {n}"
        );
    }
}

#[test]
fn mknod_refuses_regular_and_directory_types_with_badtype_and_creates_nothing() {
    let (_s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    let before = c.names(&root);
    let badtype = nfsstat3::NFS3ERR_BADTYPE as u32;
    for (kind, n) in [
        (ftype3::NF3REG, "reg"),
        (ftype3::NF3DIR, "dir"),
        (ftype3::NF3LNK, "lnk"),
    ] {
        let a = Args::new().put(&dirop(&root, n)).put(&(kind as u32));
        assert_eq!(c.call(11, a).0, badtype, "MKNOD {n}");
        assert_eq!(c.lookup(&root, n).0, NOENT, "{n} must not exist");
    }
    assert_eq!(c.names(&root), before, "readdir shows no new entry");
}

#[test]
fn mknod_of_a_device_needs_root_but_a_fifo_does_not() {
    let (_s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    let perm = nfsstat3::NFS3ERR_PERM as u32;
    for (kind, n) in [(ftype3::NF3CHR, "chr"), (ftype3::NF3BLK, "blk")] {
        let (st, _) = c.call_as(1000, 11, mknod_args(&root, n, kind, 0o600, (1, 3)));
        assert_eq!(st, perm, "MKNOD {n} as uid 1000");
        assert_eq!(c.lookup(&root, n).0, NOENT, "{n} must not exist");
        let (st, _) = c.call_as(0, 11, mknod_args(&root, n, kind, 0o600, (1, 3)));
        assert_eq!(st, OK, "MKNOD {n} as root");
    }
    for (kind, n) in [(ftype3::NF3FIFO, "fifo"), (ftype3::NF3SOCK, "sock")] {
        let (st, _) = c.call_as(1000, 11, mknod_args(&root, n, kind, 0o600, (0, 0)));
        assert_eq!(st, OK, "MKNOD {n} as uid 1000");
    }
}

/// The whiteout (character device 0:0) is exempt from the root rule exactly as in the FUSE
/// adapter; a block 0:0 and any other character device are not.
#[test]
fn mknod_whiteout_is_allowed_for_any_caller_but_no_other_device() {
    let (_s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    let perm = nfsstat3::NFS3ERR_PERM as u32;
    let (st, _) = c.call_as(
        1000,
        11,
        mknod_args(&root, "wo", ftype3::NF3CHR, 0o644, (0, 0)),
    );
    assert_eq!(st, OK, "whiteout as uid 1000");
    for (kind, n, dev) in [
        (ftype3::NF3BLK, "blk00", (0, 0)),
        (ftype3::NF3CHR, "chr01", (0, 1)),
        (ftype3::NF3BLK, "blk80", (8, 0)),
    ] {
        let (st, _) = c.call_as(1000, 11, mknod_args(&root, n, kind, 0o600, dev));
        assert_eq!(st, perm, "MKNOD {n} as uid 1000");
        assert_eq!(c.lookup(&root, n).0, NOENT, "{n} must not exist");
    }
}

#[test]
fn pathconf_reports_the_advertised_values() {
    let (_s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    let (st, mut r) = c.call(20, Args::new().put(&root));
    assert_eq!(st, OK);
    let _a: post_op_attr = dec(&mut r);
    let linkmax: u32 = dec(&mut r);
    let name_max: u32 = dec(&mut r);
    let no_trunc: bool = dec(&mut r);
    let chown_restricted: bool = dec(&mut r);
    let case_insensitive: bool = dec(&mut r);
    let case_preserving: bool = dec(&mut r);
    assert_eq!(name_max, 255);
    assert!(no_trunc);
    assert!(chown_restricted);
    assert!(!case_insensitive);
    assert!(case_preserving);
    // Advertised nfsserve default, not enforced by cowfs; known limitation tracked in #19.
    assert_eq!(linkmax, 65000);
}
