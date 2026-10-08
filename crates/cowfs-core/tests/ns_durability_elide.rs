//! Issue #94 and issue #90 on one tree: a barrier now runs at every namespace acknowledgement, so
//! the queue commits far more often, and that changes the window in which an elided unlink happens.
//!
//! PR #95 fixed the elided unlink and did not merge PR #96, and PR #96 did not see the elide fix.
//! Neither diff settles this by reading, so it is settled by running both shapes: the #94
//! sequence with no barrier between the steps, where the elide fires, and the same sequence with a
//! barrier after every step, where it cannot. In both, both snapshots' content has to be intact and
//! every name has to be where it should be after a reopen.
//!
//! The elide is an optimisation, so a barrier that stops it firing is benign. What must not happen
//! is a barrier committing an orphan removal whose create it did not commit, which would leave a
//! name recorded as gone, or a name reserved by a create that was elided and never committed.

mod common;

use common::*;
use cowfs_core::{Core, Options};
use cowfs_vfs::{Vfs, ROOT_INO};

fn tiny() -> Options {
    Options {
        background: false,
        max_pending_ops: 16,
        file_flush_bytes: 64 << 10,
        node_cache: 256,
        dentry_cache: 256,
        block_cache_bytes: 1 << 20,
        ..Options::default()
    }
}

fn names(s: &dyn Vfs, dir: u64) -> Vec<String> {
    let mut out = Vec::new();
    let mut cookie = 0;
    loop {
        let r = s.readdir(dir, cookie, 16).unwrap();
        for e in &r.entries {
            out.push(String::from_utf8_lossy(&e.name).into_owned());
        }
        cookie = r.entries.last().map_or(cookie, |l| l.cookie);
        if r.eof {
            break;
        }
    }
    out.sort();
    out
}

fn bytes(s: &dyn Vfs, ino: u64, len: usize) -> Vec<u8> {
    s.read(ino, 0, len as u32).expect("read back")
}

fn barrier(c: &Core, snap: &str) {
    c.snapshot_view(snap)
        .expect("a snapshot view")
        .sync_namespace(ROOT_INO)
        .expect("the namespace barrier");
}

/// The #94 shape with the barrier the NFS transport now runs after the unlink: the elide has
/// already happened, so the barrier commits the removal and nothing may reserve the name.
#[test]
fn an_elided_unlink_stays_elided_across_a_barrier() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), tiny()).unwrap();
    c.create_snapshot("s0").unwrap();
    let s0 = root_entry(&c, "s0").ino;
    let a = c.create(s0, b"f", 0o644).unwrap();
    write_all(&c, a.ino, 0, b"first");
    c.forget(a.ino, 1);
    c.fork_snapshot("s0", "s1").unwrap();
    let s1 = root_entry(&c, "s1").ino;

    c.unlink(s1, b"f").unwrap();
    let b = c.create(s1, b"f", 0o644).unwrap();
    write_all(&c, b.ino, 0, b"second");
    c.forget(b.ino, 1);
    c.unlink(s1, b"f").unwrap();
    assert_eq!(
        c.stats().elided,
        1,
        "the fixture must actually elide a create"
    );

    barrier(&c, "s1");
    assert_eq!(
        c.stats().elided,
        1,
        "the barrier drained the queue, so the elided create is already gone from it"
    );

    // The name is free, and it stays free across a reopen of the whole store.
    let made = c
        .create(s1, b"f", 0o644)
        .expect("the name must be free after the barrier, not reserved by the elided create");
    c.forget(made.ino, 1);
    c.sync().unwrap();
    drop(c);

    let c = Core::open(dir.path(), tiny()).unwrap();
    assert_eq!(
        names(&c.snapshot_view("s1").unwrap(), ROOT_INO),
        vec!["f".to_string()],
        "the name the test created after the barrier must be there"
    );
    c.check().expect("fsck");
}

/// The same sequence with a barrier after every namespace step, which is what a mount does now.
///
/// The elide cannot fire once the create has been committed, and that is the correct outcome rather
/// than a regression: the elide only cancels a create the store never saw. The names and the bytes
/// are what have to be right, in both snapshots, after the caches are dropped and the store is
/// reopened.
#[test]
fn the_elide_seed_survives_a_barrier_between_every_step() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), tiny()).unwrap();
    c.create_snapshot("s0").unwrap();
    let s0 = root_entry(&c, "s0").ino;

    // The seed is Create, Fork, Unlink, Create, Unlink, DropCaches, Create.
    let a = c.create(s0, b"f", 0o644).unwrap();
    write_all(&c, a.ino, 0, b"first");
    c.forget(a.ino, 1);
    barrier(&c, "s0");

    c.fork_snapshot("s0", "s1").unwrap();
    let s1 = root_entry(&c, "s1").ino;
    barrier(&c, "s1");

    c.unlink(s1, b"f").unwrap();
    barrier(&c, "s1");

    let b = c.create(s1, b"f", 0o644).unwrap();
    write_all(&c, b.ino, 0, b"second");
    c.forget(b.ino, 1);
    barrier(&c, "s1");

    c.unlink(s1, b"f").unwrap();
    barrier(&c, "s1");
    assert_eq!(
        c.stats().elided,
        0,
        "with a barrier between the create and the unlink there is nothing queued to elide"
    );

    // Drop the caches, which is what turned a dirty dentry into a lost one before #94.
    c.drop_caches();
    barrier(&c, "s1");

    // The seed's last create must succeed: the name is free in the fork.
    let d = c
        .create(s1, b"f", 0o644)
        .expect("the name must be free after the cache drop");
    write_all(&c, d.ino, 0, b"third");
    c.forget(d.ino, 1);
    barrier(&c, "s1");
    c.sync().unwrap();

    // The base keeps its own copy of the file with its own bytes: a fork is a copy.
    let base = c.snapshot_view("s0").unwrap();
    assert_eq!(names(&base, ROOT_INO), vec!["f".to_string()]);
    let fa = base.lookup(ROOT_INO, b"f").expect("the base file");
    assert_eq!(bytes(&base, fa.ino, 5), b"first");
    drop(base);
    drop(c);

    let c = Core::open(dir.path(), tiny()).unwrap();
    let base = c.snapshot_view("s0").unwrap();
    assert_eq!(names(&base, ROOT_INO), vec!["f".to_string()]);
    let fa = base.lookup(ROOT_INO, b"f").expect("the base file");
    assert_eq!(bytes(&base, fa.ino, 5), b"first", "the base lost its bytes");
    drop(base);

    let fork = c.snapshot_view("s1").unwrap();
    assert_eq!(names(&fork, ROOT_INO), vec!["f".to_string()]);
    let ff = fork.lookup(ROOT_INO, b"f").expect("the fork file");
    assert_eq!(
        bytes(&fork, ff.ino, 5),
        b"third",
        "the fork's own bytes must survive a barrier, a cache drop and a reopen"
    );
    drop(fork);
    c.check().expect("fsck");
}

/// The elide inside the NFS adapter's own pattern: a directory removed and recreated, with a
/// barrier after each step, then a cache drop and a reopen. The name must be free and the removal
/// must reach meta.
#[test]
fn an_elided_rmdir_survives_a_barrier_and_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), tiny()).unwrap();
    c.create_snapshot("s0").unwrap();
    let s0 = root_entry(&c, "s0").ino;
    c.mkdir(s0, b"d", 0o755).unwrap();
    barrier(&c, "s0");
    // A fork, because the fork has to start out holding the directory the test removes from it.
    c.fork_snapshot("s0", "s1").unwrap();
    let s1 = root_entry(&c, "s1").ino;

    c.rmdir(s1, b"d").unwrap();
    barrier(&c, "s1");
    let again = c
        .mkdir(s1, b"d", 0o755)
        .expect("the name must be free again right after the removal");
    c.forget(again.ino, 1);
    barrier(&c, "s1");

    c.drop_caches();
    assert_eq!(
        names(&c.snapshot_view("s1").unwrap(), ROOT_INO),
        vec!["d".to_string()],
        "the recreated directory must survive the cache drop"
    );
    assert!(
        c.mkdir(s1, b"d", 0o755).is_err(),
        "the name is taken by the directory the test created, so this must be refused"
    );

    c.rmdir(s1, b"d")
        .expect("remove the directory that is really there");
    barrier(&c, "s1");
    c.sync().unwrap();
    drop(c);

    let c = Core::open(dir.path(), tiny()).unwrap();
    let base = c.snapshot_view("s0").unwrap();
    assert_eq!(
        names(&base, ROOT_INO),
        vec!["d".to_string()],
        "the base keeps the directory"
    );
    drop(base);
    assert_eq!(
        names(&c.snapshot_view("s1").unwrap(), ROOT_INO),
        Vec::<String>::new(),
        "the fork's directory must be gone from meta after a barrier and a reopen"
    );
    c.check().expect("fsck");
}
