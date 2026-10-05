//! `Vfs::sync_namespace`: the names become durable, the file data that is still dirty does not.
//!
//! The crash is `cowfs-daemon`'s `namespace_durability` test, over a real mount. What is checked
//! here is the two things a real mount cannot show: that the name is on the medium before the call
//! returns and after a reopen, and that the barrier costs an unrelated dirty file nothing, because
//! that is what keeps `cargo build` on the mount from turning into one fsync per write.

mod common;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use common::*;
use cowfs_core::{Core, Options};
use cowfs_meta::Meta;
use cowfs_vfs::{Vfs, ROOT_INO};

/// Off the background flusher, so only what a test asks for can make anything durable.
fn opts() -> Options {
    Options {
        background: false,
        ..Options::default()
    }
}

/// The names of `ROOT_INO`, sorted, so a reopen can be compared without depending on order.
fn names(s: &dyn Vfs) -> Vec<String> {
    let mut left: Vec<String> = s
        .readdir(ROOT_INO, 0, 64)
        .unwrap()
        .entries
        .iter()
        .map(|e| String::from_utf8_lossy(&e.name).into_owned())
        .collect();
    left.sort();
    left
}

/// Reopens the store the way the daemon does after a crash: the `Core` and every view of it gone.
fn reopen(dir: &std::path::Path) -> Core {
    Core::open(dir, opts()).unwrap()
}

/// A rename is on the medium once `sync_namespace` returns, and it is still there after the store
/// is closed and opened by a fresh `Core`. No fsync of the renamed file is involved, which is the
/// whole point: on the NFS mount the client never sends that COMMIT.
#[test]
fn a_renamed_name_survives_a_reopen_after_only_a_namespace_barrier() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), opts()).unwrap();
    c.create_snapshot("s").unwrap();
    mkfile(&c.snapshot_view("s").unwrap(), ROOT_INO, "old", b"payload");
    // The file's own bytes are already durable, so the name is the only thing uncommitted.
    c.sync().unwrap();

    let s = c.snapshot_view("s").unwrap();
    s.rename(ROOT_INO, b"old", ROOT_INO, b"new", Default::default())
        .unwrap();
    s.sync_namespace(ROOT_INO).unwrap();
    drop(s);
    drop(c);

    let c = reopen(dir.path());
    let s = c.snapshot_view("s").unwrap();
    assert_eq!(
        names(&s),
        vec!["new".to_string()],
        "the new name was not durable"
    );
    assert_eq!(
        s.read(s.lookup(ROOT_INO, b"new").unwrap().ino, 0, 7)
            .unwrap(),
        b"payload"
    );
    c.check().expect("fsck");
}

/// The barrier is not a flush. A dirty file stays dirty, which is what keeps a `cargo build` on the
/// mount from paying a store round trip per metadata operation.
#[test]
fn a_namespace_barrier_leaves_unrelated_dirty_data_alone() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let s = c.snapshot_view("s").unwrap();
    let body = pattern(512 * 1024, 7);
    let hot = mkfile(&s, ROOT_INO, "hot", &body);
    let cold = mkfile(&s, ROOT_INO, "cold", b"cold");
    s.rename(ROOT_INO, b"cold", ROOT_INO, b"colder", Default::default())
        .unwrap();
    let dirty = c.stats().dirty_bytes;
    assert!(
        dirty > 0,
        "the fixture needs unflushed bytes to be worth protecting"
    );

    s.sync_namespace(ROOT_INO).unwrap();
    assert_eq!(
        c.stats().dirty_bytes,
        dirty,
        "a namespace barrier must not write the file data that is still dirty"
    );

    // The data is still reachable and still correct, because the barrier committed the namespace
    // without naming a block the store does not have.
    assert_eq!(s.read(hot.ino, 0, 8).unwrap(), body[..8]);
    assert!(s.lookup(ROOT_INO, b"colder").is_ok());
    assert_eq!(s.read(cold.ino, 0, 4).unwrap(), b"cold");

    // The file's own fsync is what drains it, unchanged.
    s.fsync(hot.ino, false).unwrap();
    assert_eq!(c.stats().dirty_bytes, 0);
    drop(s);
    drop(c);
    let c = reopen(dir.path());
    let s = c.snapshot_view("s").unwrap();
    assert_eq!(names(&s), vec!["colder".to_string(), "hot".to_string()]);
    assert_eq!(
        s.read(s.lookup(ROOT_INO, b"hot").unwrap().ino, 0, 8)
            .unwrap(),
        body[..8],
        "a durable name must never name bytes the store has lost"
    );
    c.check().expect("fsck");
}

/// A handle reaches its own snapshot and `ROOT_INO` is still the whole mount, because a client
/// that asks for the mount is asking for everything in it.
///
/// Isolation is read from the queue rather than from a reopen, because a `Core` that is dropped
/// cleanly syncs everything on the way out and would make the claim true for the wrong reason.
#[test]
fn the_root_is_the_whole_mount_and_a_handle_is_its_own_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), opts()).unwrap();
    for name in ["one", "two"] {
        c.create_snapshot(name).unwrap();
        let s = c.snapshot_view(name).unwrap();
        mkfile(&s, ROOT_INO, "a", b"x");
        s.sync_namespace(ROOT_INO).unwrap();
    }
    for name in ["one", "two"] {
        c.snapshot_view(name)
            .unwrap()
            .rename(ROOT_INO, b"a", ROOT_INO, b"b", Default::default())
            .unwrap();
    }
    assert_eq!(c.stats().pending_ops, 2, "both renames are queued");

    let one = c.snapshot_view("one").unwrap();
    one.sync_namespace(one.lookup(ROOT_INO, b"b").unwrap().ino)
        .unwrap();
    assert_eq!(
        c.stats().pending_ops,
        1,
        "a handle barrier must reach its own snapshot and no other"
    );

    let root = c.snapshot_view("two").unwrap();
    root.sync_namespace(ROOT_INO).unwrap();
    assert_eq!(
        c.stats().pending_ops,
        0,
        "ROOT_INO must still be the whole mount"
    );
}

/// A barrier whose sync fails reports the failure instead of claiming the namespace is committed,
/// and a retry by the same caller commits exactly one copy of the name.
///
/// `Core::open_with_meta` is the seam for this: the options arrive with the store hook already
/// wired, so the injected fault lands at the sync itself, after the batch has been applied and
/// before anything is durable. The healthy neighbour's dirty bytes are the control: a barrier must
/// not cost them, and must not lose them either.
#[test]
fn a_barrier_whose_sync_fails_reports_the_failure() {
    let dir = tempfile::tempdir().unwrap();
    let fail = Arc::new(AtomicBool::new(false));
    let c = Core::open_with_meta(dir.path(), opts(), |d, mut o| {
        let real = o.before_sync.take().expect("the store hook is wired here");
        let fail = Arc::clone(&fail);
        o.before_sync = Some(Arc::new(move || {
            real()?;
            if fail.load(Ordering::Relaxed) {
                return Err(io::Error::other("injected store sync fault"));
            }
            Ok(())
        }));
        Meta::open(d.join("meta.redb"), o)
    })
    .unwrap();
    c.create_snapshot("s").unwrap();
    let s = c.snapshot_view("s").unwrap();
    let neighbour = pattern(64 * 1024, 7);
    let n = mkfile(&s, ROOT_INO, "neighbour", &neighbour);
    mkfile(&s, ROOT_INO, "old", b"payload");
    s.sync_namespace(ROOT_INO).unwrap();

    s.rename(ROOT_INO, b"old", ROOT_INO, b"new", Default::default())
        .unwrap();
    fail.store(true, Ordering::Relaxed);
    let failed = s.sync_namespace(ROOT_INO);
    fail.store(false, Ordering::Relaxed);

    assert!(
        matches!(failed, Err(cowfs_vfs::Error::Io(_))),
        "a barrier that could not sync must report it, got {failed:?}"
    );
    assert!(
        c.health().last_error.is_some(),
        "a failed sync must be visible in health, not swallowed"
    );
    // The rename is applied and visible, so the caller's retry finds its own work rather than
    // having to undo it, and the neighbour's unflushed bytes are untouched by either outcome.
    assert!(
        s.lookup(ROOT_INO, b"new").is_ok(),
        "the applied rename stays visible"
    );
    assert_eq!(
        s.read(n.ino, 0, 8).unwrap(),
        neighbour[..8],
        "an unrelated dirty file must neither be flushed nor lose bytes"
    );

    // The same caller's retry commits it, and a reopen sees one name, not two.
    s.sync_namespace(ROOT_INO).unwrap();
    drop(s);
    drop(c);
    let c = reopen(dir.path());
    let s = c.snapshot_view("s").unwrap();
    assert_eq!(names(&s), vec!["neighbour".to_string(), "new".to_string()]);
    assert_eq!(
        s.read(s.lookup(ROOT_INO, b"neighbour").unwrap().ino, 0, 8)
            .unwrap(),
        neighbour[..8]
    );
    c.check().expect("fsck");
}

/// An inode that is not there is `Stale`, not a quiet success: a barrier that could not even find
/// what to make durable has not made anything durable.
#[test]
fn a_barrier_on_an_inode_that_does_not_exist_is_stale() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let s = c.snapshot_view("s").unwrap();
    assert_eq!(s.sync_namespace(u64::MAX), Err(cowfs_vfs::Error::Stale));
}
