//! Special files (issue #107) through a real Core: kind and device number survive a sync, a fork
//! and a reopen, `check` accepts them, and removing the last name frees them.

mod common;

use common::test_opts;
use cowfs_core::Core;
use cowfs_vfs::{makedev, Error, FileKind, Vfs, ROOT_INO};

fn root_of(v: &dyn Vfs, name: &[u8]) -> cowfs_vfs::Attr {
    v.lookup(ROOT_INO, name).expect("lookup")
}

#[test]
fn special_nodes_survive_sync_fork_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let dev = makedev(8, 16);
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.create_snapshot("work").unwrap();
        let v = c.snapshot_view("work").unwrap();
        let p = v.mknod(ROOT_INO, b"p", FileKind::Fifo, 0o640, 0).unwrap();
        v.mknod(ROOT_INO, b"s", FileKind::Socket, 0o600, 0).unwrap();
        let b = v
            .mknod(ROOT_INO, b"b", FileKind::BlockDevice, 0o660, dev)
            .unwrap();
        assert_eq!((p.kind, p.nlink, p.size, p.rdev), (FileKind::Fifo, 1, 0, 0));
        assert_eq!((b.kind, b.rdev), (FileKind::BlockDevice, dev));
        c.sync().unwrap();
        c.check().unwrap();
        c.fork_snapshot("work", "copy").unwrap();
        c.sync().unwrap();
        c.check().unwrap();
    }
    let c = Core::open(dir.path(), test_opts()).expect("reopen");
    c.check().unwrap();
    for snap in ["work", "copy"] {
        let v = c.snapshot_view(snap).unwrap();
        let p = root_of(&v, b"p");
        assert_eq!(
            (p.kind, p.mode, p.rdev),
            (FileKind::Fifo, 0o640, 0),
            "{snap}"
        );
        assert_eq!(root_of(&v, b"s").kind, FileKind::Socket, "{snap}");
        let b = root_of(&v, b"b");
        assert_eq!(
            (b.kind, b.mode, b.rdev),
            (FileKind::BlockDevice, 0o660, dev),
            "{snap}"
        );
        let listed = v.readdir_attrs(ROOT_INO, 0, 100).unwrap();
        let kinds: Vec<_> = listed
            .entries
            .iter()
            .map(|e| (e.entry.name.clone(), e.attr.kind))
            .collect();
        assert!(
            kinds.contains(&(b"b".to_vec(), FileKind::BlockDevice)),
            "{snap}: {kinds:?}"
        );
        assert_eq!(v.read(b.ino, 0, 1), Err(Error::InvalidArgument));
    }
    // the copy is independent: removing the name in one snapshot leaves the other
    let w = c.snapshot_view("work").unwrap();
    w.unlink(ROOT_INO, b"b").unwrap();
    c.sync().unwrap();
    c.check().unwrap();
    assert_eq!(w.lookup(ROOT_INO, b"b"), Err(Error::NotFound));
    let k = c.snapshot_view("copy").unwrap();
    assert_eq!(root_of(&k, b"b").rdev, dev);
}

#[test]
fn mknod_in_a_read_only_root_and_bad_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert_eq!(
        c.mknod(ROOT_INO, b"p", FileKind::Fifo, 0o600, 0),
        Err(Error::ReadOnly)
    );
    c.create_snapshot("work").unwrap();
    let v = c.snapshot_view("work").unwrap();
    assert_eq!(
        v.mknod(ROOT_INO, b"p", FileKind::Regular, 0o600, 0),
        Err(Error::InvalidArgument)
    );
    assert_eq!(
        v.mknod(ROOT_INO, b"p", FileKind::Fifo, 0o600, 1),
        Err(Error::InvalidArgument)
    );
    c.check().unwrap();
}
