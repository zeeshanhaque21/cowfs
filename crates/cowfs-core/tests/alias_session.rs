//! The session contract for an inode number: once a number is handed out it names the same inode
//! for the whole session, whatever the adapter does with `forget`.
//!
//! Ported from issue #53, where a stateless NFS adapter forgets as soon as it hands the attributes
//! out, and the alias release turned the number the client still holds into `Stale`.

mod common;

use common::*;
use cowfs_core::{Core, Options};
use cowfs_vfs::{Error, Vfs, ROOT_INO};

/// The reproducer from the issue, verbatim: `mkdir d`, forget it the way a stateless adapter
/// does, wait for the background flusher to commit, then use the number again.
#[test]
fn a_forgotten_directory_keeps_its_number_after_the_commit() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("base").unwrap();
    let v = c.snapshot_view("base").unwrap();

    let d = v.mkdir(ROOT_INO, b"d", 0o755).unwrap();
    v.forget(d.ino, 1);
    c.sync().unwrap();

    assert!(
        v.getattr(d.ino).is_ok(),
        "the number the client holds went Stale after the commit: {:?}",
        v.getattr(d.ino)
    );
    let found = v.lookup(ROOT_INO, b"d").unwrap();
    assert_eq!(
        found.ino, d.ino,
        "the same inode is reachable under two numbers in one session"
    );
}

/// The issue's `mkdir a; mkdir a/b` through the core, with a file written and read back.
#[test]
fn a_directory_created_through_the_session_still_takes_children_after_the_commit() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("base").unwrap();
    let v = c.snapshot_view("base").unwrap();

    let a = v.mkdir(ROOT_INO, b"a", 0o755).unwrap();
    v.forget(a.ino, 1);
    c.sync().unwrap();

    let b = v.mkdir(a.ino, b"b", 0o755).unwrap();
    v.forget(b.ino, 1);
    c.sync().unwrap();
    let f = v.create(b.ino, b"f", 0o644).unwrap();
    assert_eq!(v.write(f.ino, 0, b"payload").unwrap(), 7);
    c.sync().unwrap();
    assert_eq!(v.read(f.ino, 0, 64).unwrap(), b"payload");
    assert_eq!(
        v.getattr(b.ino).unwrap().kind,
        cowfs_vfs::FileKind::Directory
    );
    assert_eq!(
        v.getattr(f.ino).unwrap().ino,
        f.ino,
        "the file's own number moved"
    );
}

/// A rename keeps the one number: the same inode must not appear under two numbers because a name
/// moved.
#[test]
fn a_rename_keeps_one_number() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("base").unwrap();
    let v = c.snapshot_view("base").unwrap();

    let f = mkfile(&v, ROOT_INO, "a", b"x");
    v.forget(f.ino, 1);
    c.sync().unwrap();
    v.rename(ROOT_INO, b"a", ROOT_INO, b"b", Default::default())
        .unwrap();
    c.sync().unwrap();

    assert_eq!(v.lookup(ROOT_INO, b"b").unwrap().ino, f.ino);
    assert_eq!(v.read(f.ino, 0, 8).unwrap(), b"x");
    assert_eq!(v.lookup(ROOT_INO, b"a"), Err(Error::NotFound));
}

/// A hardlink shares one inode, so it shares one number: the rule that two snapshots sharing
/// content report different numbers must not be read as "every name has its own number".
#[test]
fn a_hardlink_keeps_one_number() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("base").unwrap();
    let v = c.snapshot_view("base").unwrap();

    let f = mkfile(&v, ROOT_INO, "a", b"x");
    v.forget(f.ino, 1);
    c.sync().unwrap();
    let linked = v.link(f.ino, ROOT_INO, b"b").unwrap();
    c.sync().unwrap();

    assert_eq!(linked.ino, f.ino, "a hardlink got a second number");
    assert_eq!(v.lookup(ROOT_INO, b"b").unwrap().ino, f.ino);
    assert_eq!(v.getattr(f.ino).unwrap().nlink, 2);
}

/// Once every name is gone the number may go `Stale`: a client holding a handle to an unlinked
/// file is allowed to get `ESTALE`, and that is the only release point.
#[test]
fn an_unlinked_inode_goes_stale() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("base").unwrap();
    let v = c.snapshot_view("base").unwrap();

    let f = mkfile(&v, ROOT_INO, "a", b"x");
    v.forget(f.ino, 1);
    c.sync().unwrap();
    v.unlink(ROOT_INO, b"a").unwrap();
    c.sync().unwrap();
    assert_eq!(v.getattr(f.ino), Err(Error::Stale));
    assert_eq!(v.lookup(ROOT_INO, b"a"), Err(Error::NotFound));
}

/// The issue's shell session, with the real background flusher rather than an explicit sync: the
/// daemon runs with one, and the commit is what releases the alias under the old rule.
#[test]
fn the_issue_session_with_the_background_flusher() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(
        dir.path(),
        Options {
            background: true,
            ..Options::default()
        },
    )
    .unwrap();
    c.create_snapshot("base").unwrap();
    let v = c.snapshot_view("base").unwrap();
    let d = v.mkdir(ROOT_INO, b"d", 0o755).unwrap();
    v.forget(d.ino, 1);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(
        v.getattr(d.ino).is_ok(),
        "getattr after the background commit"
    );
    let e = v
        .mkdir(d.ino, b"e", 0o755)
        .unwrap_or_else(|err| panic!("mkdir d/e with the flusher running: {err:?}"));
    v.forget(e.ino, 1);
    let f = v.create(e.ino, b"f", 0o644).unwrap();
    assert_eq!(v.write(f.ino, 0, b"x").unwrap(), 1);
    v.forget(f.ino, 1);
    assert_eq!(read_all(&v, f.ino), b"x");
}
