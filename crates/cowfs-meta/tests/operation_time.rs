//! `Tx::set_now` makes `ctime` the time of the change rather than the time of the transaction.
//!
//! This is the deterministic proof at the metadata boundary, where the timestamps can be stated
//! outright instead of being read off a clock. The public `Core` proof, which cannot state a time and
//! must therefore compare against what the `Vfs` reported at the moment of the operation, is
//! `crates/cowfs-core/tests/operation_time.rs`.

use cowfs_meta::{Meta, Options, Snapshot, Timestamp};

fn t(secs: i64) -> Timestamp {
    Timestamp { secs, nanos: 0 }
}

/// Distinct, ordered times well clear of each other, so a stamp from the wrong one is unmistakable.
const T1: Timestamp = Timestamp {
    secs: 1_000_000,
    nanos: 0,
};
const T2: Timestamp = Timestamp {
    secs: 2_000_000,
    nanos: 0,
};
const T3: Timestamp = Timestamp {
    secs: 3_000_000,
    nanos: 0,
};

/// A metadata store in a fresh directory. `Meta::open` takes the database file, not the directory.
fn store(dir: &std::path::Path) -> Meta {
    Meta::open(dir.join("meta.redb"), Options::default()).expect("a metadata store opens")
}

fn ctime(s: &Snapshot, ino: u64) -> Timestamp {
    s.getattr(cowfs_meta::Ino(ino))
        .expect("the inode is there")
        .ctime
}

#[test]
fn a_transaction_without_set_now_still_uses_the_wall_clock() {
    let dir = tempfile::tempdir().unwrap();
    let meta = store(dir.path());
    let s = meta.new_snapshot("s").unwrap();
    let before = Timestamp::now();
    let d = s
        .batch(|tx| tx.mkdir(cowfs_meta::ROOT_INO, b"d", 0o755))
        .unwrap();
    let after = Timestamp::now();
    let got = ctime(&s, d.ino.0);
    assert!(
        got >= before && got <= after,
        "the default stamp must be the wall clock: {got:?} not in {before:?}..{after:?}"
    );
}

#[test]
fn set_now_stamps_the_new_inode_the_parent_and_the_affected_inode() {
    let dir = tempfile::tempdir().unwrap();
    let meta = store(dir.path());
    let s = meta.new_snapshot("s").unwrap();

    let d = s
        .batch(|tx| {
            tx.set_now(T1);
            tx.mkdir(cowfs_meta::ROOT_INO, b"d", 0o755)
        })
        .unwrap();
    let f = s
        .batch(|tx| {
            tx.set_now(T2);
            tx.create(d.ino, b"f", 0o644)
        })
        .unwrap();

    assert_eq!(
        ctime(&s, f.ino.0),
        T2,
        "the new inode takes its operation's time"
    );
    assert_eq!(
        ctime(&s, d.ino.0),
        T2,
        "the parent takes the same reading the operation stamped it with"
    );
    assert_eq!(
        ctime(&s, cowfs_meta::ROOT_INO.0),
        T1,
        "an inode this batch did not touch keeps the time it was last changed"
    );
}

#[test]
fn a_mixed_transaction_stamps_each_operation_with_its_own_time() {
    let dir = tempfile::tempdir().unwrap();
    let meta = store(dir.path());
    let s = meta.new_snapshot("s").unwrap();
    let d = s
        .batch(|tx| {
            tx.set_now(T1);
            tx.mkdir(cowfs_meta::ROOT_INO, b"d", 0o755)
        })
        .unwrap();
    s.batch(|tx| {
        // one transaction, three operations, three times
        tx.set_now(T2);
        let f = tx.create(d.ino, b"f", 0o644)?;
        tx.set_now(T3);
        tx.link(f.ino, d.ino, b"f2")?;
        Ok(())
    })
    .unwrap();

    assert_eq!(
        ctime(&s, d.ino.0),
        T3,
        "the parent must carry the last change to it, not the first"
    );
    let linked = s.lookup(d.ino, b"f2").unwrap();
    assert_eq!(
        ctime(&s, linked.ino.0),
        T3,
        "the linked inode is the same inode, so it takes the link's time"
    );
    assert_eq!(linked.nlink, 2, "both names are one inode");
}

#[test]
fn set_now_does_not_replace_an_explicit_atime_or_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let meta = store(dir.path());
    let s = meta.new_snapshot("s").unwrap();
    let f = s
        .batch(|tx| {
            tx.set_now(T1);
            tx.create(cowfs_meta::ROOT_INO, b"f", 0o644)
        })
        .unwrap();

    let asked = t(9_000_000);
    s.batch(|tx| {
        // the stamp moves ctime only: a caller statement about mtime is not a clock reading
        tx.set_now(T2);
        tx.setattr(
            f.ino,
            cowfs_meta::SetAttr {
                mode: None,
                atime: Some(asked),
                mtime: Some(asked),
                size: None,
            },
        )
    })
    .unwrap();

    let a = s.getattr(f.ino).unwrap();
    assert_eq!(a.mtime, asked, "an explicit mtime must survive set_now");
    assert_eq!(a.atime, asked, "an explicit atime must survive set_now");
    assert_eq!(a.ctime, T2, "ctime is the only field the stamp owns");
}

#[test]
fn a_later_transaction_does_not_move_an_earlier_ctime() {
    let dir = tempfile::tempdir().unwrap();
    let meta = store(dir.path());
    let s = meta.new_snapshot("s").unwrap();
    let f = s
        .batch(|tx| {
            tx.set_now(T1);
            tx.create(cowfs_meta::ROOT_INO, b"f", 0o644)
        })
        .unwrap();

    s.batch(|tx| {
        tx.set_now(T3);
        tx.mkdir(cowfs_meta::ROOT_INO, b"unrelated", 0o755)
    })
    .unwrap();

    assert_eq!(
        ctime(&s, f.ino.0),
        T1,
        "an unrelated later transaction restamped an inode it did not touch"
    );
}
