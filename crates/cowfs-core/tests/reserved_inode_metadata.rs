//! #42 request 4 (Core consumer): a reserved, physical inode number is known to
//! Core before meta has committed its create, so every metadata operation that
//! reads through that number must commit the pending create first instead of
//! answering `Stale`.
//!
//! # The failure this pins, the way a user hits it
//!
//! Under the reservation design a created inode's number is the packed meta
//! number, so `meta_of` answers for it immediately. Before this fix the ops that
//! read metadata through the number (`readdir` of a just-created directory,
//! xattr reads, `preserve_orphan` on unlink) went straight to meta, which had
//! not seen the create yet, and meta answered `NotFound`, mapped to `Stale`.
//! A user saw `readdir`, `getxattr`, `unlink` of a fresh file fail with `Stale`
//! for no reason.
//!
//! The fix: a pending create's own sequence is recorded on the node, so the
//! gates that read meta through it commit the create first, and the meta readers
//! commit a not-yet-committed create before asking meta. The number and bytes
//! are unchanged by the commit, and survive a reopen.

mod common;

use common::*;
use cowfs_core::Core;
use cowfs_vfs::{Error, Vfs, XattrFlags};

/// A freshly created directory whose create is still uncommitted must list
/// empty, not fail with `Stale`. `readdir_empty_directory` is the simplest hit
/// of the failing route.
#[test]
fn readdir_of_a_pending_directory_is_empty_not_stale() {
    let f = fixture();
    let c = &f.core;
    c.create_snapshot("s").unwrap();
    let r = root_entry(c, "s").ino;

    let d = c.mkdir(r, b"d", 0o755).unwrap();
    let listing = c
        .readdir(d.ino, 0, 128)
        .expect("readdir of a just-created directory must not be Stale");
    assert!(
        listing.entries.is_empty(),
        "a new directory listed {} entries",
        listing.entries.len()
    );

    c.sync().unwrap();
    c.check().unwrap();
}

/// xattr reads on a just-created file must answer `NoAttr`/empty, not `Stale`,
/// while the create is still uncommitted.
#[test]
fn xattr_reads_of_a_pending_file_are_absent_not_stale() {
    let f = fixture();
    let c = &f.core;
    c.create_snapshot("s").unwrap();
    let r = root_entry(c, "s").ino;
    let a = c.create(r, b"f", 0o644).unwrap();

    assert_eq!(
        c.getxattr(a.ino, b"user.missing"),
        Err(Error::NoAttr),
        "getxattr of a pending file reported Stale instead of NoAttr"
    );
    assert!(
        c.listxattr(a.ino)
            .expect("listxattr of a pending file")
            .is_empty(),
        "a new file listed xattrs"
    );
    assert_eq!(
        c.removexattr(a.ino, b"user.missing"),
        Err(Error::NoAttr),
        "removexattr of a pending file reported Stale instead of NoAttr"
    );

    c.sync().unwrap();
    c.check().unwrap();
}

/// Setting an xattr, reading it back, then removing it, all on a file created in
/// this session, must round-trip even before an explicit flush.
#[test]
fn xattr_round_trip_on_a_pending_file() {
    let f = fixture();
    let c = &f.core;
    c.create_snapshot("s").unwrap();
    let r = root_entry(c, "s").ino;
    let a = c.create(r, b"f", 0o644).unwrap();

    c.setxattr(a.ino, b"user.k", b"v", XattrFlags::default())
        .expect("setxattr on a pending file");
    assert_eq!(
        c.getxattr(a.ino, b"user.k").expect("getxattr"),
        b"v",
        "the xattr did not round-trip"
    );
    c.removexattr(a.ino, b"user.k")
        .expect("removexattr on a pending file");
    assert_eq!(c.getxattr(a.ino, b"user.k"), Err(Error::NoAttr));

    c.sync().unwrap();
    c.check().unwrap();
}

/// Unlinking a pinned (open) file whose content lives in an uncommitted create
/// must not fail with `Stale`; `preserve_orphan` reads the file's content out of
/// meta and must commit the create first.
#[test]
fn unlink_of_a_pinned_pending_file_is_not_stale() {
    let f = fixture();
    let c = &f.core;
    c.create_snapshot("s").unwrap();
    let r = root_entry(c, "s").ino;
    let bytes = b"pinned-pending-payload";

    let a = c.create(r, b"f", 0o644).unwrap();
    write_all(c, a.ino, 0, bytes);
    // Hold the file open so unlink takes the `preserve_orphan` path.
    c.open(a.ino).unwrap();

    c.unlink(r, b"f")
        .expect("unlink of a pinned pending file must not be Stale");

    c.sync().unwrap();
    c.check().unwrap();
}

/// The end-to-end contract: a created file answers metadata while pending, and
/// after a flush and a fresh `Core::open` it keeps the same inode number and
/// bytes.
#[test]
fn a_pending_metadata_op_keeps_identity_and_bytes_across_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = b"reserved-metadata-identity-payload";

    let created;
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.create_snapshot("s").unwrap();
        let r = root_entry(&c, "s").ino;

        let a = c.create(r, b"f", 0o644).unwrap();
        created = a.ino;
        write_all(&c, created, 0, bytes);

        // Every one of these reads through the still-uncommitted create.
        c.setxattr(created, b"user.k", b"v", XattrFlags::default())
            .expect("setxattr while pending");
        assert_eq!(c.getxattr(created, b"user.k").unwrap(), b"v");
        assert_eq!(read_all(&c, created), bytes);

        c.sync().unwrap();
    }

    let c = Core::open(dir.path(), test_opts()).unwrap();
    let r = root_entry(&c, "s").ino;
    let a = c.lookup(r, b"f").unwrap();
    assert_eq!(a.ino, created, "the inode number changed across a reopen");
    assert_eq!(read_all(&c, a.ino), bytes, "the bytes did not survive");
    assert_eq!(
        c.getxattr(a.ino, b"user.k").unwrap(),
        b"v",
        "the xattr did not survive the reopen"
    );
    c.check().unwrap();
}
