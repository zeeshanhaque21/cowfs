//! Issue #60: the `Vfs` owns the whole `u64` inode space, so the adapter must not spend a bit of
//! it on its own bookkeeping. `cowfs-core` puts the top bit on every virtual inode, so a server
//! that marks a translated sidecar with that bit reads every virtual inode as a sidecar and
//! answers `STALE` without ever reaching the `Vfs`.
//!
//! Both halves are covered: every inode number a `Vfs` may hand out (`1 << 63`, `u64::MAX`, the
//! shape `cowfs-core` uses) and a whole `Vfs` that reports only such numbers, driven through the
//! adapter and through the mount protocol in all three AppleDouble modes.
mod common;

use common::reuse::{ReusingVfs, VIRT};
use common::*;
use cowfs_nfs::{Adapter, AdapterOptions, AppleDoubleMode, MountOptions, Sidecar};
use cowfs_vfs::{Ino, Vfs, ROOT_INO};
use nfsserve::nfs::{nfs_fh3, nfsstat3, sattr3};

const MODES: [AppleDoubleMode; 3] = [
    AppleDoubleMode::Hide,
    AppleDoubleMode::Translate,
    AppleDoubleMode::Store,
];

fn options(appledouble: AppleDoubleMode) -> MountOptions {
    MountOptions {
        appledouble,
        ..MountOptions::default()
    }
}

fn adapter(vfs: std::sync::Arc<dyn Vfs>, appledouble: AppleDoubleMode) -> Adapter {
    Adapter::new(
        vfs,
        AdapterOptions {
            appledouble,
            ..AdapterOptions::default()
        },
    )
    .unwrap()
}

/// The inode numbers a `Vfs` is allowed to use, and the ones `cowfs-core` uses.
#[test]
fn every_inode_number_round_trips_through_a_handle_as_an_ordinary_inode() {
    for mode in MODES {
        let a = adapter(ReusingVfs::virtuals(), mode);
        // None of these is an inode of this file system: the root is `ROOT_INO` and the first
        // inode it hands out is `2 + 1 << 63`.
        for ino in [
            1 << 63,
            VIRT + 100,
            0x8000_0100_0000_0001,
            u64::MAX,
            u64::MAX - 1,
        ] {
            let h = a.handle(ino);
            assert_eq!(
                a.resolve(&h),
                Ok(ino),
                "{mode:?}: inode {ino:#x} must survive a handle as itself"
            );
            assert_eq!(a.getattr(ino).err(), Some(nfsstat3::NFS3ERR_STALE));
            assert_eq!(
                a.readdir(ino, 0, 10, false).err(),
                Some(nfsstat3::NFS3ERR_STALE),
                "{mode:?}: inode {ino:#x} is a directory to the Vfs or not, never a sidecar"
            );
        }
        assert!(
            a.getattr(ROOT_INO).is_ok(),
            "{mode:?}: the root is an ordinary directory, not a sidecar"
        );
        let made = a.create(ROOT_INO, b"f", &sattr3::default(), true);
        assert!(made.is_ok(), "{mode:?}: create in the root: {made:?}");
        let f = made.unwrap().0;
        assert_eq!(
            a.getattr(f).err(),
            None,
            "{mode:?}: an inode of the Vfs is not a sidecar"
        );
        assert_eq!(
            a.readdir(f, 0, 10, false).err(),
            Some(nfsstat3::NFS3ERR_NOTDIR)
        );
    }
}

/// A `Vfs` that reports only numbers with the top bit set, driven through the adapter.
#[test]
fn a_file_system_whose_inodes_all_have_the_top_bit_set_works() {
    for mode in MODES {
        let vfs = ReusingVfs::virtuals();
        let a = adapter(vfs.clone(), mode);
        let root = ROOT_INO;

        let (d, _) = a.mkdir(root, b"a", &sattr3::default()).unwrap();
        let (sub, _) = a.mkdir(d, b"b", &sattr3::default()).unwrap();
        let (f, _) = a.create(d, b"f", &sattr3::default(), true).unwrap();
        a.write(f, 0, b"hello").unwrap();
        assert_eq!(a.read(f, 0, 16).unwrap().0, b"hello".to_vec());

        a.link(f, d, b"h").unwrap();
        a.symlink(d, b"l", b"f").unwrap();
        assert_eq!(a.readlink(a.lookup(d, b"l").unwrap().0).unwrap(), b"f");

        assert_eq!(
            a.readdir(d, 0, 32, true).unwrap().entries.len(),
            4,
            "{mode:?}: b, f, h and l are all listed"
        );

        a.rename(d, b"f", sub, b"f").unwrap();
        let moved = a.lookup(sub, b"f").unwrap().0;
        assert_eq!(a.read(moved, 0, 16).unwrap(), (b"hello".to_vec(), true));
        a.remove(sub, b"f").unwrap();
        a.rmdir(d, b"b").unwrap();
        a.remove(d, b"h").unwrap();
        a.remove(d, b"l").unwrap();
        assert_eq!(
            a.lookup(d, b"l").err(),
            Some(nfsstat3::NFS3ERR_NOENT),
            "{mode:?}: the unlink went through the Vfs"
        );
        assert!(
            vfs.lookup(root, b"a").is_ok(),
            "{mode:?}: and the directory the mount created is still there"
        );
    }
}

/// The same, over the mount protocol, with the numbers the client sees checked.
#[test]
fn a_file_system_whose_inodes_all_have_the_top_bit_set_works_over_the_protocol() {
    for mode in MODES {
        let vfs = ReusingVfs::virtuals();
        let (_s, mut c) = serve(vfs.clone(), options(mode));
        let root = c.root.clone();
        let root_ino = c.attrs(&root).fileid;
        assert_eq!(
            root_ino, ROOT_INO,
            "{mode:?}: the root is inode 1 of the Vfs"
        );

        let (st, a) = c.mkdir(&root, "a");
        assert_eq!(st, OK, "{mode:?}: mkdir a");
        let a = a.unwrap();
        assert_eq!(
            c.attrs(&a).fileid & VIRT,
            VIRT,
            "{mode:?}: a is a virtual inode"
        );
        let (st, b) = c.mkdir(&a, "b");
        assert_eq!(st, OK, "{mode:?}: mkdir a/b");
        let b = b.unwrap();

        let f = c.create_file(&a, "f");
        assert_eq!(c.write(&f, 0, b"hello", 2).0, OK);
        assert_eq!(c.read(&f, 0, 64).1, b"hello");
        assert_eq!(c.commit(&f), OK);

        assert_eq!(c.link(&f, &a, "h").0, OK, "{mode:?}: a hard link");
        assert_eq!(c.symlink(&a, "l", "f").0, OK);
        assert_eq!(c.readdir_page(&a, 0, true, 4096).0, OK);
        let names: Vec<String> = c
            .readdir_page(&a, 0, false, 4096)
            .1
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(names.contains(&"f".to_string()) && names.contains(&"h".to_string()));
        assert!(
            names.contains(&"l".to_string()),
            "{mode:?}: the symlink is listed"
        );

        assert_eq!(
            c.rename(&a, "f", &b, "f"),
            OK,
            "{mode:?}: rename across directories"
        );
        let moved = c.must_lookup(&b, "f");
        assert_eq!(c.read(&moved, 0, 64).1, b"hello");
        assert_eq!(c.remove(&b, "f"), OK);
        assert_eq!(c.rmdir(&a, "b"), OK);
        assert_eq!(c.remove(&a, "h"), OK);
        assert_eq!(c.remove(&a, "l"), OK);
        assert_eq!(c.rmdir(&root, "a"), OK);
        assert!(
            vfs.lookup(ROOT_INO, b"a").is_err(),
            "{mode:?}: the tree is empty again"
        );
    }
}

/// A translated sidecar is not an inode, so it must have a file id of its own that no inode of
/// the `Vfs` can be taken for, in every mode.
#[test]
fn a_sidecar_id_is_never_an_inode_of_the_vfs() {
    let vfs = ReusingVfs::virtuals();
    let (_s, mut c) = serve(vfs.clone(), options(AppleDoubleMode::Translate));
    let root = c.root.clone();
    c.create_file(&root, "doc");
    let doc = c.must_lookup(&root, "doc");
    let doc_id = c.attrs(&doc).fileid;
    let side = c.create_file(&root, "._doc");
    let side_id = c.attrs(&side).fileid;
    assert_ne!(side_id, doc_id, "a sidecar is not the file it belongs to");
    assert!(
        vfs.getattr(side_id).is_err(),
        "and the id it is given is not an inode of the Vfs: {side_id}"
    );

    // The sidecar is a view of the file's attributes: no inode of its own exists.
    let ino = vfs.lookup(ROOT_INO, b"doc").unwrap().ino;
    assert_eq!(c.write(&side, 0, &with_attr("user.k", b"v"), 2).0, OK);
    assert_eq!(vfs.getxattr(ino, b"user.k"), Ok(b"v".to_vec()));
    assert!(
        vfs.lookup(ROOT_INO, b"._doc").is_err(),
        "and no sidecar inode was stored"
    );
    assert_eq!(c.attrs(&doc).fileid, doc_id, "the file keeps its own id");
    assert_eq!(c.attrs(&side).fileid, side_id, "and so does the sidecar");

    // `Hide` and `Store` give the sidecar a real inode, so its id is that inode.
    for mode in [AppleDoubleMode::Hide, AppleDoubleMode::Store] {
        let vfs = ReusingVfs::virtuals();
        let (_s, mut c) = serve(vfs.clone(), options(mode));
        let root = c.root.clone();
        c.create_file(&root, "doc");
        let side = c.create_file(&root, "._doc");
        let stored = vfs
            .lookup(ROOT_INO, b"._doc")
            .expect("a stored sidecar is a real file");
        assert_eq!(
            c.attrs(&side).fileid,
            stored.ino,
            "{mode:?}: the id of a stored sidecar is its own inode"
        );
    }
}

/// A sidecar's id can never be an inode of the `Vfs`, even when the `Vfs` hands out exactly the
/// numbers the adapter gives to sidecars: the inode takes the number and the sidecar moves, and the
/// handle the client holds keeps naming the sidecar.
#[test]
fn a_sidecar_id_and_an_inode_that_wants_the_same_number_never_collide() {
    // The adapter gives sidecars ids from the top of the `u64` space down, and this file system
    // hands out inode numbers from the top too, so the first sidecar id is the first inode.
    let vfs = ReusingVfs::from_the_top();
    let (_s, mut c) = serve(vfs.clone(), options(AppleDoubleMode::Translate));
    let root = c.root.clone();
    c.create_file(&root, "doc");
    let doc = c.must_lookup(&root, "doc");
    let doc_id = c.attrs(&doc).fileid;
    let side = c.create_file(&root, "._doc");
    let side_id = c.attrs(&side).fileid;
    assert_ne!(side_id, doc_id, "a sidecar is never the file it belongs to");
    assert!(
        vfs.getattr(side_id).is_err(),
        "the sidecar id {side_id} is not an inode of the Vfs"
    );

    // Every inode the `Vfs` hands out next is a number the sidecar could have had, and none of
    // them is the number it has.
    for n in ["f", "g", "h"] {
        let f = c.create_file(&root, n);
        let id = c.attrs(&f).fileid;
        assert_eq!(id, vfs.lookup(ROOT_INO, n.as_bytes()).unwrap().ino, "{n}");
        let now = c.attrs(&side).fileid;
        assert!(
            vfs.getattr(now).is_err(),
            "{n}: the sidecar id {now} is still no inode"
        );
        assert_ne!(now, id, "{n}: the inode and the sidecar have different ids");
    }

    // The handle still names the sidecar, whatever number it is given now.
    assert_eq!(c.read(&side, 0, 64).0, OK);
    let ino = vfs.lookup(ROOT_INO, b"doc").unwrap().ino;
    assert_eq!(c.write(&side, 0, &with_attr("user.k", b"v"), 2).0, OK);
    assert_eq!(vfs.getxattr(ino, b"user.k"), Ok(b"v".to_vec()));
    assert_eq!(
        c.attrs(&doc).fileid,
        doc_id,
        "and the file keeps its own id"
    );
}

/// A handle names one thing: the file, or the sidecar of that file. Nothing else.
#[test]
fn a_handle_cannot_be_made_to_change_what_it_names() {
    let vfs = ReusingVfs::virtuals();
    let (_s, mut c) = serve(vfs, options(AppleDoubleMode::Translate));
    let root = c.root.clone();
    c.create_file(&root, "doc");
    let doc = c.must_lookup(&root, "doc");
    let side = c.create_file(&root, "._doc");

    let mut forged = side.data.clone();
    let last = forged.len() - 1;
    forged[last] ^= 0xff;
    assert_eq!(c.getattr(&nfs_fh3 { data: forged }).0, BADHANDLE);

    for cut in [0, 1, 16, 24, 25, 40, side.data.len() - 1] {
        let short = nfs_fh3 {
            data: side.data[..cut].to_vec(),
        };
        assert_eq!(c.getattr(&short).0, BADHANDLE, "{cut}");
    }
    let long = nfs_fh3 {
        data: [side.data.as_slice(), &[0]].concat(),
    };
    assert_eq!(c.getattr(&long).0, BADHANDLE, "one byte too many");

    assert_eq!(c.getattr(&doc).0, OK, "the file's own handle is still good");
}

fn with_attr(name: &str, value: &[u8]) -> Vec<u8> {
    Sidecar::from_xattrs([(name.as_bytes().to_vec(), value.to_vec())]).encode()
}

/// The ids the adapter reports are the `Vfs` inode numbers, so a caller can go from one to the
/// other. A sidecar is the exception: it has no inode, only an id of its own.
#[test]
fn a_plain_id_is_the_vfs_inode() {
    let vfs = ReusingVfs::virtuals();
    let (_s, mut c) = serve(vfs.clone(), options(AppleDoubleMode::Hide));
    let root = c.root.clone();
    let f = c.create_file(&root, "f");
    let ino: Ino = vfs.lookup(ROOT_INO, b"f").unwrap().ino;
    assert_eq!(c.attrs(&f).fileid, ino);
    let page = c.readdir_page(&root, 0, true, 4096).1;
    for e in page {
        assert_eq!(e.fileid, e.attr.unwrap().fileid, "one id per entry");
    }
}
