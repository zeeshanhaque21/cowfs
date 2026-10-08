//! AppleDouble translation at the protocol level: sidecars are a view of xattrs, never inodes.
mod common;

use common::*;
use cowfs_nfs::{AppleDoubleMode, MountOptions, Sidecar};

/// The default mode is `Hide`; these tests are about `Translate`.
fn translated() -> MountOptions {
    MountOptions {
        appledouble: AppleDoubleMode::Translate,
        ..MountOptions::default()
    }
}
use cowfs_vfs::{Error, Vfs, ROOT_INO};

fn sidecar_with(name: &str, value: &[u8]) -> Vec<u8> {
    Sidecar::from_xattrs([(name.as_bytes().to_vec(), value.to_vec())]).encode()
}

#[test]
fn sidecar_writes_become_xattrs_of_the_real_file_and_no_sidecar_inode_exists() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), translated());
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
fn removing_a_sidecar_removes_the_xattrs_and_the_client_pattern_works() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), translated());
    let root = c.root.clone();
    c.create_file(&root, "doc");
    let doc = vfs.lookup(ROOT_INO, b"doc").unwrap().ino;
    let side = c.create_file(&root, "._doc");

    // What the client does: lay down the empty 4 KiB file, then the file with the attributes.
    let empty = Sidecar::default().encode();
    let full = sidecar_with("user.k", b"v");
    assert_eq!(c.write(&side, 0, &empty, 0).0, OK);
    assert!(
        vfs.listxattr(doc).unwrap().is_empty(),
        "an empty sidecar holds no attributes"
    );
    assert_eq!(c.write(&side, 0, &full, 2).0, OK);
    assert_eq!(vfs.getxattr(doc, b"user.k"), Ok(b"v".to_vec()));
    // A partial write past the end lands in the buffer and is applied with the rest.
    let long = sidecar_with("user.two", b"w");
    let (head, tail) = long.split_at(4000);
    assert_eq!(c.write(&side, 0, head, 0).0, OK);
    assert_eq!(c.write(&side, 4000, tail, 0).0, OK);
    assert_eq!(vfs.getxattr(doc, b"user.two"), Ok(b"w".to_vec()));

    assert_eq!(c.remove(&root, "._doc"), OK);
    assert!(
        vfs.listxattr(doc).unwrap().is_empty(),
        "REMOVE of the sidecar drops the xattrs"
    );
    assert_eq!(c.lookup(&root, "._doc").0, NOENT);
    assert_eq!(c.remove(&root, "._doc"), NOENT);
}

#[test]
fn a_sidecar_that_is_not_a_sidecar_is_refused_not_dropped() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), translated());
    let root = c.root.clone();
    c.create_file(&root, "doc");
    let side = c.create_file(&root, "._doc");
    let st = c.write(&side, 0, b"my real file content", 2).0;
    assert_ne!(
        st, OK,
        "a real file under a reserved name must not be accepted"
    );
    let doc = vfs.lookup(ROOT_INO, b"doc").unwrap().ino;
    assert!(
        vfs.listxattr(doc).unwrap().is_empty(),
        "and nothing was stored as attributes"
    );
    assert_eq!(
        vfs.lookup(ROOT_INO, b"._doc").err(),
        Some(Error::NotFound),
        "nor as a file"
    );
}

#[test]
fn a_sidecar_without_a_main_file_is_a_real_file() {
    // F1: archives and checkouts put __MACOSX/._name before the name it belongs to.
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), translated());
    let root = c.root.clone();
    let (_, d) = c.mkdir(&root, "__MACOSX");
    let d = d.unwrap();
    let side = c.create_file(&d, "._x.txt");
    assert_eq!(
        c.write(&side, 0, b"sidecar bytes from the archive", 2).0,
        OK
    );
    let ino = vfs.lookup(ROOT_INO, b"__MACOSX").unwrap().ino;
    let stored = vfs.lookup(ino, b"._x.txt").expect("stored as a real file");
    assert_eq!(c.read(&side, 0, 100).1, b"sidecar bytes from the archive");
    assert_eq!(
        c.names(&d),
        vec!["._x.txt"],
        "and listed, because it is a real file"
    );
    assert_eq!(c.remove(&d, "._x.txt"), OK);
    assert!(vfs.lookup(ino, b"._x.txt").is_err());
    c.create_file(&root, "._real");
    assert_eq!(
        c.lookup(&root, "._real").0,
        OK,
        "a bare one is creatable too"
    );
    let _ = stored;
}

#[test]
fn a_stored_sidecar_file_wins_over_the_view_and_survives_the_main_file() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), translated());
    let root = c.root.clone();
    let side = c.create_file(&root, "._x");
    c.write(&side, 0, b"stored first", 2);
    c.create_file(&root, "x");
    let doc = vfs.lookup(ROOT_INO, b"x").unwrap().ino;
    let side2 = c.must_lookup(&root, "._x");
    assert_eq!(
        c.read(&side2, 0, 100).1,
        b"stored first",
        "the real file is the one served"
    );
    assert!(
        vfs.listxattr(doc).unwrap().is_empty(),
        "and no attributes were taken from it"
    );
    assert_eq!(c.names(&root), vec!["._x", "x"]);
    assert_eq!(c.remove(&root, "._x"), OK, "removing it does not touch x");
    assert_eq!(c.lookup(&root, "x").0, OK);
}

#[test]
fn xattrs_follow_renames_and_hardlinks_and_die_with_the_file() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), translated());
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
fn sidecar_names_are_ordinary_names_except_for_a_sidecar_id() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), translated());
    let root = c.root.clone();
    c.create_file(&root, "doc");
    let notdir = nfsserve::nfs::nfsstat3::NFS3ERR_NOTDIR as u32;
    let acc = nfsserve::nfs::nfsstat3::NFS3ERR_ACCES as u32;

    let (_, d) = c.mkdir(&root, "._d");
    assert!(
        d.is_some(),
        "a directory is an ordinary name, like on any other file system"
    );
    let l = c.symlink(&root, "._l", "t");
    assert_eq!(l.0, OK);
    let f = c.must_lookup(&root, "doc");
    assert_eq!(c.link(&f, &root, "._h").0, OK);

    // What is not allowed is using a sidecar as a directory or as the source of a link.
    c.create_file(&root, "._doc");
    let side = c.must_lookup(&root, "._doc");
    assert_eq!(c.mkdir(&side, "x").0, notdir);
    assert_eq!(c.readdir_page(&side, 0, true, 4096).0, notdir);
    assert_eq!(c.remove(&side, "x"), notdir);
    assert_eq!(c.link(&side, &root, "copy").0, acc);
    let _ = vfs;
}

#[test]
fn a_sidecar_write_past_the_cap_is_refused() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), translated());
    let root = c.root.clone();
    c.create_file(&root, "doc");
    let side = c.create_file(&root, "._doc");
    let fbig = nfsserve::nfs::nfsstat3::NFS3ERR_FBIG as u32;
    for (offset, len, why) in [
        (8u64 << 20, 1_u32, "one byte past the cap"),
        (1u64 << 40, 1, "far past the cap"),
        (u64::MAX - 4, 8, "an offset that would overflow"),
    ] {
        let st = c.write(&side, offset, &[0u8; 8], 0).0;
        assert_eq!(st, fbig, "{why} (len {len})");
    }
    let doc = vfs.lookup(ROOT_INO, b"doc").unwrap().ino;
    assert!(
        vfs.listxattr(doc).unwrap().is_empty(),
        "and nothing was stored"
    );
}

/// `._name` is the sidecar of `name` while `name` exists, so the name is a view of that file's
/// extended attributes rather than a name in the directory. CREATE already answers a view for it;
/// MKDIR, SYMLINK and LINK did not check at all, so a real directory, symlink or hard link could
/// take the name, shadow the view for the rest of the mount, and in the LINK case hand the view
/// the bytes of the file it names. `ACCES` is what RENAME already answers when a real file is
/// moved onto a view.
#[test]
fn a_view_cannot_be_made_a_real_object() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), translated());
    let root = c.root.clone();
    let doc = c.create_file(&root, "doc");

    assert_eq!(c.mkdir(&root, "._doc").0, ACCES, "a directory");
    assert_eq!(c.symlink(&root, "._doc", "somewhere").0, ACCES, "a symlink");
    assert_eq!(c.link(&doc, &root, "._doc").0, ACCES, "a hard link");
    assert_eq!(
        vfs.lookup(ROOT_INO, b"._doc").err(),
        Some(Error::NotFound),
        "no real object took the name"
    );
    assert_eq!(c.names(&root), vec!["doc"]);

    // With no `name` to hold the attributes there is no view, and a real object is right.
    assert_eq!(c.mkdir(&root, "._lonely").0, OK);
    assert_eq!(c.symlink(&root, "._link", "somewhere").0, OK);
    assert_eq!(c.link(&doc, &root, "._hard").0, OK);
    assert_eq!(c.names(&root), vec!["._hard", "._link", "._lonely", "doc"]);

    // And a stored `._doc` is already a real file, so the same three calls stay allowed. It has
    // to arrive before `doc` exists, which is what every zip, tar and git checkout does.
    let vfs = memfs();
    let (_s2, mut c) = serve(vfs, translated());
    let root = c.root.clone();
    c.create_file(&root, "._doc");
    c.create_file(&root, "doc");
    // The name is occupied, not a view, so the refusal is EXIST and not ACCES.
    for (what, st) in [
        ("mkdir", c.mkdir(&root, "._doc").0),
        ("symlink", c.symlink(&root, "._doc", "somewhere").0),
    ] {
        assert_eq!(st, EXIST, "{what} onto a real file of that name");
    }
    assert_eq!(
        c.mkdir(&root, "._dir").0,
        OK,
        "no main file, so a real name"
    );
    assert_eq!(c.names(&root), vec!["._dir", "._doc", "doc"]);
}

/// The corruption this allowed: the client sets an extended attribute by creating `._doc` and
/// writing an AppleDouble file to it. With a hard link under that name the write landed on the
/// file itself, so 19 bytes of content became a 4096 byte sidecar and every call answered OK.
#[test]
fn a_link_under_a_view_name_never_rewrites_the_files_content() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), translated());
    let root = c.root.clone();
    let doc = c.create_file(&root, "doc");
    assert_eq!(c.write(&doc, 0, b"the quick brown fox", 2).0, OK);
    let ino = vfs.lookup(ROOT_INO, b"doc").unwrap().ino;

    assert_eq!(c.link(&doc, &root, "._doc").0, ACCES, "refused");

    // The client pattern that used to destroy the content.
    let (st, side, _) = c.create(&root, "._doc", 0, sattr_mode(0o644), [0; 8]);
    assert_eq!(st, OK);
    let side = side.expect("a view of doc");
    assert_eq!(c.write(&side, 0, &sidecar_with("user.k", b"v"), 2).0, OK);
    assert_eq!(c.commit(&side), OK);

    assert_eq!(vfs.read(ino, 0, 4096), Ok(b"the quick brown fox".to_vec()));
    assert_eq!(vfs.getattr(ino).unwrap().size, 19);
    assert_eq!(vfs.getxattr(ino, b"user.k"), Ok(b"v".to_vec()));
    assert_eq!(vfs.getattr(ino).unwrap().nlink, 1, "no second name");
    assert_eq!(c.names(&root), vec!["doc"]);
}

/// `.` and `..` are the two names a `Vfs` refuses to look up and no client can create, so a
/// `._.` or `._..` is never a view. Looking one up has to say so the way every other missing
/// name does; it used to answer `INVAL`, which a client cannot act on.
#[test]
fn a_dot_name_view_is_noent_and_the_name_is_a_real_file() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), translated());
    let root = c.root.clone();
    let (_, sub) = c.mkdir(&root, "sub");
    let sub = sub.unwrap();

    for name in ["._.", "._.."] {
        assert_eq!(c.lookup(&root, name).0, NOENT, "{name} at the root");
        assert_eq!(c.lookup(&sub, name).0, NOENT, "{name} in sub");
    }

    c.create_file(&root, "._.");
    c.create_file(&sub, "._..");
    let sub_ino = vfs.lookup(ROOT_INO, b"sub").unwrap().ino;
    assert!(
        vfs.lookup(ROOT_INO, b"._.").is_ok(),
        "a real file at the root"
    );
    assert!(vfs.lookup(sub_ino, b"._..").is_ok(), "a real file in sub");

    let made = c.must_lookup(&sub, "._..");
    assert_eq!(c.write(&made, 0, b"a real file", 2).0, OK);
    assert_eq!(c.read(&made, 0, 100).1, b"a real file");
    assert!(
        vfs.listxattr(sub_ino).unwrap().is_empty(),
        "the directory kept its own attributes"
    );
    assert_ne!(
        c.lookup(&sub, "._..").1.unwrap().data,
        root.data,
        "`._..` is not a view of the mount root"
    );
    assert_eq!(c.names(&sub), vec!["._.."]);
}

/// The negative control: the guard is about `Translate` only. `Hide` and `Store` keep every
/// `._` name an ordinary name, so all three calls still make a real object.
#[test]
fn outside_translate_every_dot_underscore_name_is_an_ordinary_name() {
    for mode in [AppleDoubleMode::Hide, AppleDoubleMode::Store] {
        let vfs = memfs();
        let opts = MountOptions {
            appledouble: mode,
            ..MountOptions::default()
        };
        let (_s, mut c) = serve(vfs.clone(), opts);
        let root = c.root.clone();
        let doc = c.create_file(&root, "doc");
        assert_eq!(c.mkdir(&root, "._doc").0, OK, "{mode:?} a directory");
        assert_eq!(
            c.symlink(&root, "._sym", "somewhere").0,
            OK,
            "{mode:?} a symlink"
        );
        assert_eq!(c.link(&doc, &root, "._hard").0, OK, "{mode:?} a hard link");
        assert_eq!(
            vfs.lookup(ROOT_INO, b"._doc").map(|a| a.kind),
            Ok(cowfs_vfs::FileKind::Directory),
            "{mode:?}"
        );
        assert_eq!(
            vfs.getattr(vfs.lookup(ROOT_INO, b"doc").unwrap().ino)
                .unwrap()
                .nlink,
            2
        );
    }
}

#[test]
fn the_sidecar_of_a_sidecar_is_a_real_file() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), translated());
    let root = c.root.clone();
    c.create_file(&root, "._x");
    let side = c.create_file(&root, "._._x");
    c.write(&side, 0, b"a real file whose name starts with ._", 2);
    let stored = vfs
        .lookup(ROOT_INO, b"._._x")
        .expect("stored as a real file");
    assert_eq!(
        c.read(&side, 0, 100).1,
        b"a real file whose name starts with ._"
    );
    let inner = vfs.lookup(ROOT_INO, b"._x").unwrap().ino;
    assert!(
        vfs.listxattr(inner).unwrap().is_empty(),
        "and no attributes were taken from the file it is named after"
    );
    assert_eq!(c.names(&root), vec!["._._x", "._x"]);
    let _ = stored;
}
