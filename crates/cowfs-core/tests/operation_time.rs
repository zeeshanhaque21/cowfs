//! A deferred operation's `ctime` is the time of the operation, not the time of the flush that
//! carried it into metadata.
//!
//! `cowfs-meta` stamps every inode a transaction touches with one clock reading, taken when the
//! transaction opened (`Meta::mutate`). A write-back layer that defers operations therefore records
//! the batch time, up to `Options::flush_interval` late. `Tx::set_now` lets the replay stamp each
//! operation with the time the cached node already holds, which is the time the operation happened.
//!
//! Every assertion is exact equality against a time the public `Vfs` reported at the moment of the
//! operation. No sleep, no injected clock, no mocked metadata.

use cowfs_core::{Core, Options};
use cowfs_vfs::{SetAttr, Timestamp, Vfs, ROOT_INO};

fn open(dir: &std::path::Path) -> Core {
    Core::open(
        dir,
        Options {
            background: false,
            ..Default::default()
        },
    )
    .expect("a store opens")
}

/// A store with one snapshot and one directory in it, flushed so the directory is durable.
fn ready(dir: &std::path::Path) -> (Core, cowfs_core::SnapshotView, cowfs_vfs::Ino) {
    let c = open(dir);
    c.create_snapshot("s").unwrap();
    let v = c.snapshot_view("s").unwrap();
    let d = v.mkdir(ROOT_INO, b"d", 0o755).unwrap().ino;
    // the directory is durable before anything is queued inside it, so a lookup after a reopen
    // cannot be answering from a dentry cache
    c.flush().unwrap();
    (c, v, d)
}

/// The `ctime` of `name` under `dir`, from a store read fresh off the medium.
fn reopened_ctime(store: &std::path::Path, dir: &[u8], name: &[u8]) -> Timestamp {
    let c = open(store);
    let v = c.snapshot_view("s").unwrap();
    let parent = v
        .lookup(ROOT_INO, dir)
        .expect("the directory is durable")
        .ino;
    v.lookup(parent, name).expect("the file is durable").ctime
}

#[test]
fn a_deferred_create_keeps_its_own_ctime_across_a_flush_and_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let (c, v, d) = ready(dir.path());

    let f1 = v.create(d, b"f1", 0o644).unwrap().ino;
    let t1 = v.getattr(f1).unwrap().ctime;
    let f2 = v.create(d, b"f1-sibling", 0o644).unwrap().ino;
    let t2 = v.getattr(f2).unwrap().ctime;
    // The non-vacuity control: the two operations are distinct times, so a single later stamp for
    // the whole batch cannot equal the first one. Without this the assertions below would pass on
    // the old behaviour by accident.
    assert!(
        t2 > t1,
        "the two creates must be distinct times: {t1:?} then {t2:?}"
    );

    // One batch, opened strictly after both operations reported their ctime.
    c.flush().unwrap();
    assert_eq!(
        v.getattr(f1).unwrap().ctime,
        t1,
        "a flush must not move a reported ctime"
    );

    drop(v);
    drop(c);
    assert_eq!(
        reopened_ctime(dir.path(), b"d", b"f1"),
        t1,
        "the durable ctime is not the time the create happened"
    );
    assert_eq!(reopened_ctime(dir.path(), b"d", b"f1-sibling"), t2);
}

#[test]
fn a_deferred_write_keeps_its_own_ctime_and_does_not_move_an_older_file() {
    let dir = tempfile::tempdir().unwrap();
    let (c, v, d) = ready(dir.path());

    let quiet = v.create(d, b"quiet", 0o644).unwrap().ino;
    let quiet_at = v.getattr(quiet).unwrap().ctime;

    let written = v.create(d, b"written", 0o644).unwrap().ino;
    let written_at = v.getattr(written).unwrap().ctime;
    assert_eq!(v.write(written, 0, b"hello").unwrap(), 5);
    let after_write = v.getattr(written).unwrap().ctime;
    assert!(
        after_write > written_at,
        "the write must be a later time than the create: {written_at:?} then {after_write:?}"
    );

    // A second, unrelated mutation, so the batch carries three operations at three times.
    v.mkdir(ROOT_INO, b"later", 0o755).unwrap();

    c.flush().unwrap();
    drop(v);
    drop(c);

    assert_eq!(
        reopened_ctime(dir.path(), b"d", b"written"),
        after_write,
        "the durable ctime is not the time the write happened"
    );
    // The file nobody touched after its create keeps the create time: the replay must not restamp
    // every node it carries with one time.
    assert_eq!(
        reopened_ctime(dir.path(), b"d", b"quiet"),
        quiet_at,
        "an untouched file was restamped with another operation's time"
    );
}

#[test]
fn a_deferred_setattr_and_a_mutation_to_the_same_inode_keep_their_own_times() {
    let dir = tempfile::tempdir().unwrap();
    let (c, v, d) = ready(dir.path());

    let f = v.create(d, b"f", 0o644).unwrap().ino;
    let created = v.getattr(f).unwrap().ctime;
    v.setattr(
        f,
        SetAttr {
            mode: Some(0o600),
            ..Default::default()
        },
    )
    .unwrap();
    let after_chmod = v.getattr(f).unwrap().ctime;
    assert!(after_chmod > created, "{created:?} then {after_chmod:?}");

    // An explicit mtime must survive: it is a user statement, not a clock reading, and the final
    // attribute replay must not overwrite it or stamp the node with the batch time.
    let asked = Timestamp {
        secs: 1_000_000,
        nanos: 123_456_789,
    };
    let a = v
        .setattr(
            f,
            SetAttr {
                mtime: Some(cowfs_vfs::SetTime::At(asked)),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        a.mtime, asked,
        "an explicit mtime must be reported back exactly"
    );
    // that setattr is the last change to this inode, so its ctime is the newest of the three
    let after_mtime = v.getattr(f).unwrap().ctime;
    assert!(
        after_mtime > after_chmod,
        "{after_chmod:?} then {after_mtime:?}"
    );

    v.mkdir(ROOT_INO, b"after", 0o755).unwrap();
    c.flush().unwrap();
    drop(v);
    drop(c);

    let c2 = open(dir.path());
    let v2 = c2.snapshot_view("s").unwrap();
    let parent = v2.lookup(ROOT_INO, b"d").unwrap().ino;
    let got = v2.lookup(parent, b"f").unwrap();
    assert_eq!(got.mtime, asked, "an explicit mtime must survive the flush");
    assert_eq!(
        got.ctime, after_mtime,
        "the durable ctime is not the time of the last change to this inode"
    );
}

#[test]
fn a_namespace_change_keeps_its_own_ctime_on_the_parent_and_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let (c, v, d) = ready(dir.path());
    let dir_at_create = v.getattr(d).unwrap().ctime;

    let f = v.create(d, b"f", 0o644).unwrap().ino;
    let f_at_create = v.getattr(f).unwrap().ctime;
    let dir_after_create = v.getattr(d).unwrap().ctime;
    assert!(
        dir_after_create > dir_at_create,
        "{dir_at_create:?} then {dir_after_create:?}"
    );

    v.mkdir(d, b"sub", 0o755).unwrap();
    let dir_after_mkdir = v.getattr(d).unwrap().ctime;
    assert!(
        dir_after_mkdir > dir_after_create,
        "{dir_after_create:?} then {dir_after_mkdir:?}"
    );

    // Both the directory entry and the new inode carry the operation time, not the batch time.
    v.mkdir(ROOT_INO, b"unrelated", 0o755).unwrap();
    c.flush().unwrap();
    drop(v);
    drop(c);

    let c2 = open(dir.path());
    let v2 = c2.snapshot_view("s").unwrap();
    let d2 = v2.lookup(ROOT_INO, b"d").unwrap().ino;
    assert_eq!(
        v2.getattr(d2).unwrap().ctime,
        dir_after_mkdir,
        "the parent directory ctime is not the time of its last entry change"
    );
    let f2 = v2.lookup(d2, b"f").unwrap().ino;
    assert_eq!(
        v2.getattr(f2).unwrap().ctime,
        f_at_create,
        "an untouched file was restamped when its directory changed"
    );
}
