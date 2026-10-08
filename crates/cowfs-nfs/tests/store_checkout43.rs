//! What `Store` mode does to the `._` files a macOS checkout leaves behind (#43).
//!
//! `Store` treats a `._` name like any other name, so a checkout that lays down `file` and then
//! `._file` leaves two stored files in the directory. Nothing merges them, nothing hides them and
//! nothing takes the main file with them when it goes. That is the behaviour this file pins, so a
//! change to it is a change a caller can see, not a quiet one.
mod common;

use common::*;
use cowfs_nfs::{AppleDoubleMode, MountOptions, Sidecar};
use cowfs_vfs::{Vfs, ROOT_INO};

fn stored() -> MountOptions {
    MountOptions {
        appledouble: AppleDoubleMode::Store,
        ..MountOptions::default()
    }
}

/// The bytes of a `._` file carrying one attribute, the way the client writes them.
fn sidecar_with(name: &str, value: &[u8]) -> Vec<u8> {
    Sidecar::from_xattrs([(name.as_bytes().to_vec(), value.to_vec())]).encode()
}

/// A checkout: the file, then its sidecar. `Store` has no view to answer either name from, so
/// both are ordinary stored files, both are listed, and the sidecar's bytes are its own content
/// rather than a synthesis from the main file's extended attributes.
#[test]
fn store_mode_keeps_sidecars_as_real_files_after_a_checkout() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), stored());
    let root = c.root.clone();

    let file = c.create_file(&root, "file");
    assert_eq!(c.write(&file, 0, b"checked out content", 2).0, OK);
    let side = c.create_file(&root, "._file");
    let bytes = sidecar_with("user.color", b"blue");
    assert_eq!(c.write(&side, 0, &bytes, 2).0, OK);

    // Both names are in the directory, and `._file` is a file of its own rather than a view.
    assert_eq!(c.names(&root), vec!["._file", "file"]);
    let main = vfs.lookup(ROOT_INO, b"file").unwrap();
    let side_attr = vfs.lookup(ROOT_INO, b"._file").expect("a real stored file");
    assert_ne!(side_attr.ino, main.ino, "a second inode of its own");
    assert_eq!(side_attr.size, bytes.len() as u64);

    // The bytes read back are the bytes written, not a sidecar synthesised from xattrs: nothing
    // took `user.color` out of the file and nothing left the sidecar unreadable.
    assert_eq!(c.read(&side, 0, 1 << 16).1, bytes);
    assert_eq!(
        Sidecar::decode(&c.read(&side, 0, 1 << 16).1)
            .unwrap()
            .attrs
            .get(b"user.color".as_slice()),
        Some(&b"blue".to_vec())
    );
    assert_eq!(
        vfs.read(main.ino, 0, 4096),
        Ok(b"checked out content".to_vec())
    );
    assert!(
        vfs.listxattr(main.ino).unwrap().is_empty(),
        "Store keeps the attributes in the file, not in the main file"
    );

    // Bytes that are not a sidecar at all are stored as they arrived. `Translate` refuses these,
    // because it would have to drop them; `Store` has no view to drop them for.
    let junk = c.create_file(&root, "._junk");
    assert_eq!(c.write(&junk, 0, b"my real file content", 2).0, OK);
    assert_eq!(c.read(&junk, 0, 100).1, b"my real file content");
}

/// Removing the main file leaves the sidecar. Only `Hide` cascades the removal
/// (`Adapter::remove`), and only `Hide` moves a sidecar with a rename
/// (`Adapter::rename`), so in `Store` the two names are unrelated and the leftover is visible to
/// the next checkout, to `git status`, and to anything that reads the tree.
#[test]
fn store_mode_leaves_the_sidecar_behind_when_the_main_file_is_removed() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), stored());
    let root = c.root.clone();

    let file = c.create_file(&root, "file");
    c.write(&file, 0, b"content", 2);
    let side = c.create_file(&root, "._file");
    let bytes = sidecar_with("user.k", b"v");
    c.write(&side, 0, &bytes, 2);

    assert_eq!(c.remove(&root, "file"), OK);
    assert_eq!(
        vfs.lookup(ROOT_INO, b"file").err(),
        Some(cowfs_vfs::Error::NotFound)
    );
    assert_eq!(
        c.names(&root),
        vec!["._file"],
        "the sidecar is still listed, and is now an orphan"
    );
    assert_eq!(
        c.read(&side, 0, 1 << 16).1,
        bytes,
        "and still readable with the bytes it was given"
    );

    // Rename does not move it either, so the leftover keeps the old name.
    let other = c.create_file(&root, "other");
    c.write(&other, 0, b"other", 2);
    assert_eq!(c.rename(&root, "other", &root, "renamed"), OK);
    assert_eq!(c.names(&root), vec!["._file", "renamed"]);
    assert_eq!(c.rename(&root, "renamed", &root, "file"), OK);
    assert_eq!(
        c.names(&root),
        vec!["._file", "file"],
        "and no `._renamed` appeared"
    );

    // It is still a real file, so it goes the way any other name goes.
    assert_eq!(c.remove(&root, "._file"), OK);
    assert_eq!(c.lookup(&root, "._file").0, NOENT);
    assert_eq!(c.names(&root), vec!["file"]);
}

/// A `._` name with no main file is the same kind of thing in `Store` as one with a main file:
/// an ordinary stored file. `__MACOSX/._name` and a checkout of a tree that tracks `._*` both
/// produce one, and in `Store` it stays readable rather than being turned into a view of a file
/// that is not there.
#[test]
fn store_mode_treats_a_sidecar_with_no_main_file_as_an_ordinary_file() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), stored());
    let root = c.root.clone();
    let (_, d) = c.mkdir(&root, "__MACOSX");
    let d = d.unwrap();

    let orphan = c.create_file(&d, "._x.txt");
    let bytes = sidecar_with("user.note", b"hello");
    assert_eq!(c.write(&orphan, 0, &bytes, 2).0, OK);
    assert_eq!(c.read(&orphan, 0, 1 << 16).1, bytes);
    assert_eq!(c.names(&d), vec!["._x.txt"]);
    let ino = vfs.lookup(ROOT_INO, b"__MACOSX").unwrap().ino;
    assert!(
        vfs.lookup(ino, b"._x.txt").is_ok(),
        "stored under its own name"
    );
    assert_eq!(
        vfs.lookup(ino, b"x.txt").err(),
        Some(cowfs_vfs::Error::NotFound)
    );

    // Nothing was taken from a main file, because there is none.
    assert_eq!(c.lookup(&d, "x.txt").0, NOENT);
    assert_eq!(c.remove(&d, "._x.txt"), OK);
    assert_eq!(c.names(&d), Vec::<String>::new());
}
