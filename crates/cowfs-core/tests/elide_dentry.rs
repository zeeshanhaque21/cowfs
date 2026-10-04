//! #94: an unlink whose create was elided recorded its negative dentry as clean, so dropping the
//! caches lost the only record that the name was gone and a later create read the name back out
//! of meta and answered `Exists` where the file had been unlinked.
//!
//! The trigger is the cache drop, not a small cache: the same sequence failed under the default
//! options too. The bug needed an uncommitted removal of a name meta still knew, which is why it
//! took a fork: the forked side inherits the name from meta and its first unlink stays queued.

mod common;

use common::*;
use cowfs_core::{Core, Options};
use cowfs_vfs::{Error, Vfs};

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

fn names(c: &Core, dir: u64) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut cookie = 0;
    loop {
        let r = c.readdir(dir, cookie, 7).unwrap();
        for e in &r.entries {
            out.push(e.name.clone());
        }
        cookie = r.entries.last().map_or(cookie, |l| l.cookie);
        if r.eof {
            break;
        }
    }
    out.sort();
    out
}

/// Create a file, inherit it into a fork, then unlink and recreate it twice in the fork. The
/// second unlink cancels a create that is still queued, so nothing of the create reaches meta and
/// the removal the fork inherited is still queued with it.
fn unlink_and_recreate_twice(c: &Core) -> u64 {
    c.create_snapshot("s0").unwrap();
    let s0 = root_entry(c, "s0").ino;
    let a = c.create(s0, b"f", 0o644).unwrap();
    write_all(c, a.ino, 0, b"first");
    c.forget(a.ino, 1);
    c.fork_snapshot("s0", "s1").unwrap();
    let s1 = root_entry(c, "s1").ino;

    c.unlink(s1, b"f").unwrap();
    let b = c.create(s1, b"f", 0o644).unwrap();
    write_all(c, b.ino, 0, b"second");
    c.forget(b.ino, 1);
    c.unlink(s1, b"f").unwrap();

    let st = c.stats();
    assert_eq!(st.elided, 1, "the second unlink did not elide the queued create");
    s1
}

#[test]
fn create_after_an_elided_unlink_sees_the_name_as_free() {
    let f = fixture_with(tiny());
    let s1 = unlink_and_recreate_twice(&f.core);
    f.core.drop_caches();

    assert_eq!(
        f.core.lookup(s1, b"f"),
        Err(Error::NotFound),
        "the name was unlinked twice, so the mount must not find it"
    );
    let c = f
        .core
        .create(s1, b"f", 0o644)
        .expect("the name was unlinked, so it is free");
    write_all(&f.core, c.ino, 0, b"third");
    f.core.forget(c.ino, 1);

    assert_eq!(f.core.lookup(s1, b"f").unwrap().ino, c.ino);
    assert_eq!(names(&f.core, s1), vec![b"f".to_vec()]);
    assert_eq!(read_all(&f.core, c.ino), b"third");
    assert_eq!(f.core.getattr(c.ino).unwrap().nlink, 1);

    f.core.check().unwrap();
    assert!(f.core.fsck().unwrap().is_clean());
}

#[test]
fn create_after_an_elided_unlink_survives_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), tiny()).unwrap();
    let s1 = unlink_and_recreate_twice(&core);
    let c = core
        .create(s1, b"f", 0o644)
        .expect("the name was unlinked, so it is free");
    write_all(&core, c.ino, 0, b"third");
    core.forget(c.ino, 1);
    core.sync().unwrap();
    core.check().unwrap();
    let committed = core.lookup(s1, b"f").unwrap().ino;
    assert!(core.fsck().unwrap().is_clean());
    drop(core);

    let c = Core::open(dir.path(), tiny()).unwrap();
    let s1 = root_entry(&c, "s1").ino;
    assert_eq!(names(&c, s1), vec![b"f".to_vec()]);
    let reopened = c.lookup(s1, b"f").unwrap().ino;
    assert_eq!(read_all(&c, reopened), b"third");
    assert_ne!(reopened, committed, "a reopened session hands out new inode numbers");
    c.check().unwrap();
    assert!(c.fsck().unwrap().is_clean());
}

#[test]
fn an_unlinked_name_stays_free_after_a_reopen_of_the_whole_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), tiny()).unwrap();
    let s1 = unlink_and_recreate_twice(&core);
    // commit the whole thing, so the fork's inherited name really is gone from meta
    core.sync().unwrap();
    assert_eq!(core.lookup(s1, b"f"), Err(Error::NotFound));
    assert_eq!(names(&core, s1), Vec::<Vec<u8>>::new());
    drop(core);

    let c = Core::open(dir.path(), tiny()).unwrap();
    let s1 = root_entry(&c, "s1").ino;
    assert_eq!(c.lookup(s1, b"f"), Err(Error::NotFound));
    c.create(s1, b"f", 0o644).expect("the name is free in meta too");
    c.check().unwrap();
    assert!(c.fsck().unwrap().is_clean());
}

#[test]
fn a_name_that_is_still_present_still_answers_exists() {
    let f = fixture_with(tiny());
    let c = &f.core;
    c.create_snapshot("s").unwrap();
    let s = root_entry(c, "s").ino;
    let a = c.create(s, b"f", 0o644).unwrap();
    c.forget(a.ino, 1);
    assert_eq!(
        c.create(s, b"f", 0o644),
        Err(Error::Exists),
        "the fix must not make a live name look free"
    );
    // unlinking one of two links frees that name only: POSIX has no nlink gate on a name
    c.link(a.ino, s, b"g").unwrap();
    c.forget(a.ino, 1);
    c.unlink(s, b"f").unwrap();
    let b = c.create(s, b"f", 0o644).expect("the name f is free");
    c.forget(b.ino, 1);
    assert_ne!(b.ino, a.ino, "create after unlink makes a new inode");
    assert_eq!(c.lookup(s, b"g").unwrap().nlink, 1, "g is the old inode, still linked once");
    assert_eq!(names(c, s), vec![b"f".to_vec(), b"g".to_vec()]);
}

#[test]
fn the_same_sequence_under_default_options() {
    let f = fixture_with(Options {
        background: false,
        ..Options::default()
    });
    let s1 = unlink_and_recreate_twice(&f.core);
    f.core.drop_caches();
    assert_eq!(f.core.lookup(s1, b"f"), Err(Error::NotFound));
    f.core.create(s1, b"f", 0o644).expect("default caches");
}
