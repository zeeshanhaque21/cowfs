//! AppleDouble translation at the protocol level: sidecars are a view of xattrs, never inodes.
mod common;

use common::*;
use cowfs_nfs::{MountOptions, Sidecar};
use cowfs_vfs::{Error, Vfs, ROOT_INO};

fn sidecar_with(name: &str, value: &[u8]) -> Vec<u8> {
    Sidecar::from_xattrs([(name.as_bytes().to_vec(), value.to_vec())]).encode()
}

#[test]
fn sidecar_writes_become_xattrs_of_the_real_file_and_no_sidecar_inode_exists() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), MountOptions::default());
    let root = c.root.clone();
    c.create_file(&root, "doc");
    let side = c.create_file(&root, "._doc");
    let doc = vfs.lookup(ROOT_INO, b"doc").unwrap().ino;

    let bytes = sidecar_with("user.color", b"blue");
    assert_eq!(c.write(&side, 0, &bytes, 2).0, OK);
    assert_eq!(vfs.getxattr(doc, b"user.color"), Ok(b"blue".to_vec()));
    assert_eq!(
        vfs.lookup(ROOT_INO, b"._doc").err(),
        Some(Error::NotFound),
        "not stored"
    );

    let back = c.read(&side, 0, 1 << 16).1;
    assert_eq!(
        Sidecar::decode(&back)
            .unwrap()
            .attrs
            .get(b"user.color".as_slice()),
        Some(&b"blue".to_vec())
    );
    assert_eq!(c.names(&root), vec!["doc"], "not listed");

    assert_eq!(c.write(&side, 0, &Sidecar::default().encode(), 2).0, OK);
    assert!(
        vfs.listxattr(doc).unwrap().is_empty(),
        "an attribute-free sidecar clears the xattrs"
    );
}

#[test]
fn removing_a_sidecar_removes_the_xattrs_and_partial_writes_are_buffered() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), MountOptions::default());
    let root = c.root.clone();
    c.create_file(&root, "doc");
    let doc = vfs.lookup(ROOT_INO, b"doc").unwrap().ino;
    let side = c.create_file(&root, "._doc");

    let bytes = sidecar_with("user.k", b"v");
    let (first, rest) = bytes.split_at(1000);
    assert_eq!(c.write(&side, 0, first, 0).0, OK);
    assert!(
        vfs.listxattr(doc).unwrap().is_empty(),
        "a half written file is not applied"
    );
    assert_eq!(c.write(&side, 1000, rest, 0).0, OK);
    assert_eq!(
        vfs.getxattr(doc, b"user.k"),
        Ok(b"v".to_vec()),
        "applied once complete"
    );

    assert_eq!(c.remove(&root, "._doc"), OK);
    assert!(
        vfs.listxattr(doc).unwrap().is_empty(),
        "REMOVE of the sidecar drops the xattrs"
    );
    assert_eq!(c.lookup(&root, "._doc").0, NOENT);
    assert_eq!(c.remove(&root, "._doc"), NOENT);
}

#[test]
fn xattrs_follow_renames_and_hardlinks_and_die_with_the_file() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), MountOptions::default());
    let root = c.root.clone();
    let f = c.create_file(&root, "a");
    let side = c.create_file(&root, "._a");
    c.write(&side, 0, &sidecar_with("user.k", b"v"), 2);

    assert_eq!(c.rename(&root, "a", &root, "b"), OK);
    assert_eq!(
        c.rename(&root, "._a", &root, "._b"),
        OK,
        "the client moves the sidecar too: a no-op"
    );
    let side_b = c.must_lookup(&root, "._b");
    assert_eq!(
        Sidecar::decode(&c.read(&side_b, 0, 1 << 16).1)
            .unwrap()
            .attrs
            .len(),
        1
    );
    assert_eq!(c.lookup(&root, "._a").0, NOENT);

    assert_eq!(c.link(&f, &root, "h").0, OK);
    let side_h = c.must_lookup(&root, "._h");
    assert_eq!(
        Sidecar::decode(&c.read(&side_h, 0, 1 << 16).1)
            .unwrap()
            .attrs
            .len(),
        1,
        "hardlinks share xattrs"
    );

    assert_eq!(c.remove(&root, "b"), OK);
    assert_eq!(c.remove(&root, "h"), OK);
    assert_eq!(c.lookup(&root, "._b").0, NOENT);
    let (_, d) = c.mkdir(&root, "dir");
    let d = d.unwrap();
    let ds = c.create_file(&root, "._dir");
    c.write(&ds, 0, &sidecar_with("user.d", b"1"), 2);
    let dir = vfs.lookup(ROOT_INO, b"dir").unwrap().ino;
    assert_eq!(
        vfs.getxattr(dir, b"user.d"),
        Ok(b"1".to_vec()),
        "directory sidecars work"
    );
    let _ = d;
}

#[test]
fn real_files_cannot_be_made_with_sidecar_names() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), MountOptions::default());
    let root = c.root.clone();
    let (st, _, _) = c.create(&root, "._orphan", 1, sattr_mode(0o644), [0; 8]);
    assert_ne!(st, OK, "no file to attach the attributes to");
    assert_eq!(
        c.mkdir(&root, "._d").0,
        nfsserve::nfs::nfsstat3::NFS3ERR_ACCES as u32
    );
    assert_eq!(
        c.symlink(&root, "._l", "t").0,
        nfsserve::nfs::nfsstat3::NFS3ERR_ACCES as u32
    );
    assert!(vfs.lookup(ROOT_INO, b"._orphan").is_err());
}
