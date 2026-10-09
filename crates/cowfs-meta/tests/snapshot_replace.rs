//! D13 (issue #286): `Meta::replace_snapshot` gives a snapshot the name another one holds, and
//! removes that other one, in ONE transaction. Compare `snapshot_rename.rs`, which refuses a taken
//! name.
//!
//! What has to hold:
//!
//! - the replacing snapshot keeps its id, tree and inode numbers, and answers to the name,
//! - the old holder is gone exactly as after `remove_snapshot` (its root is queued for the reaper),
//! - the name is never absent: a failed commit leaves the old holder under it, and the store
//!   reopened after a commit has the new holder,
//! - an untaken name is a plain rename, the current name is a no-op, a missing id and a bad name
//!   are refused, and
//! - uncommitted writes in the replacing snapshot are not lost.

use cowfs_meta::{Error, Meta, Options, Snapshot, SnapshotId, Tx, ROOT_INO};

fn opts() -> Options {
    Options {
        node_size: 512,
        sync_every_ops: 1,
        background: false,
        ..Options::default()
    }
}

fn put(s: &Snapshot, name: &str) {
    s.batch(|tx: &mut Tx| {
        tx.create(ROOT_INO, name.as_bytes(), 0o644)?;
        Ok(())
    })
    .unwrap();
}

/// `old` holds "target" with file `o`; `new` holds "staged" with file `n`.
fn pair(m: &Meta) -> (SnapshotId, SnapshotId) {
    let old = m.new_snapshot("target").unwrap();
    put(&old, "o");
    let new = m.new_snapshot("staged").unwrap();
    put(&new, "n");
    m.sync().unwrap();
    (old.id(), new.id())
}

fn names(m: &Meta) -> Vec<(String, u64)> {
    let mut v: Vec<_> = m
        .snapshots()
        .unwrap()
        .into_iter()
        .map(|i| (i.name, i.id.0))
        .collect();
    v.sort();
    v
}

fn has(s: &Snapshot, file: &str) -> bool {
    s.lookup(ROOT_INO, file.as_bytes()).is_ok()
}

#[test]
fn a_replace_retargets_the_name_and_removes_the_old_holder_in_one_commit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let m = Meta::open(&path, opts()).unwrap();
    let (old, new) = pair(&m);
    let held = m.snapshot("staged").unwrap();
    let reap = m.pending_reap().unwrap();

    m.replace_snapshot(new, "target").unwrap();

    assert_eq!(names(&m), [("target".to_string(), new.0)]);
    assert_eq!(held.id(), new, "the id must not move");
    assert_eq!(
        held.info().unwrap().name,
        "target",
        "an open handle follows"
    );
    let now = m.snapshot("target").unwrap();
    assert_eq!(now.id(), new);
    assert!(has(&now, "n") && !has(&now, "o"));
    assert!(matches!(m.snapshot_by_id(old), Err(Error::NoSuchSnapshot)));
    assert_eq!(
        m.pending_reap().unwrap(),
        reap + 1,
        "the old root is queued"
    );
    m.reap_all().unwrap();
    m.check().unwrap();

    drop((held, now, m));
    let m = Meta::open(&path, opts()).unwrap();
    assert_eq!(names(&m), [("target".to_string(), new.0)]);
    assert!(has(&m.snapshot("target").unwrap(), "n"));
    m.check().unwrap();
}

#[test]
fn an_untaken_name_is_a_rename_and_the_current_name_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let m = Meta::open(dir.path().join("m.redb"), opts()).unwrap();
    let (old, new) = pair(&m);
    m.replace_snapshot(new, "free").unwrap();
    assert_eq!(
        names(&m),
        [("free".to_string(), new.0), ("target".to_string(), old.0)]
    );
    let reap = m.pending_reap().unwrap();
    m.replace_snapshot(new, "free").unwrap();
    assert_eq!(m.pending_reap().unwrap(), reap);
    assert_eq!(names(&m).len(), 2);
}

#[test]
fn a_missing_id_and_a_bad_name_are_refused_and_change_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let m = Meta::open(dir.path().join("m.redb"), opts()).unwrap();
    let (old, new) = pair(&m);
    let before = names(&m);
    assert!(matches!(
        m.replace_snapshot(SnapshotId(9999), "target"),
        Err(Error::NoSuchSnapshot)
    ));
    assert!(matches!(
        m.replace_snapshot(new, ""),
        Err(Error::Invalid(_))
    ));
    let long = "n".repeat(70_000);
    assert!(matches!(
        m.replace_snapshot(new, &long),
        Err(Error::Invalid(_))
    ));
    assert_eq!(names(&m), before);
    assert_eq!(m.snapshot("target").unwrap().id(), old);
}

/// The commit is the only point at which the name moves: a failure at it leaves the old holder
/// under the name, in the live store and after a reopen.
#[test]
fn a_failed_replace_commit_leaves_the_old_holder_under_the_name() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let armed = Arc::new(AtomicBool::new(false));
    let a2 = armed.clone();
    let mut o = opts();
    o.before_sync = Some(Arc::new(move || {
        if a2.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("replace probe: sync refused"));
        }
        Ok(())
    }));
    let m = Meta::open(&path, o).unwrap();
    let (old, new) = pair(&m);
    let before = names(&m);
    armed.store(true, Ordering::SeqCst);
    assert!(m.replace_snapshot(new, "target").is_err());
    assert_eq!(names(&m), before, "no row moved");
    assert_eq!(m.snapshot("target").unwrap().id(), old);
    assert_eq!(m.pending_reap().unwrap(), 0);
    armed.store(false, Ordering::SeqCst);
    drop(m);
    let m = Meta::open(&path, opts()).unwrap();
    assert_eq!(names(&m), before);
    m.check().unwrap();
}

/// Writes still pending in the replacing snapshot are committed with the replace, not lost.
#[test]
fn pending_writes_in_the_replacing_snapshot_survive() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let m = Meta::open(
        &path,
        Options {
            node_size: 512,
            background: false,
            ..Options::default()
        },
    )
    .unwrap();
    let (_, new) = pair(&m);
    let s = m.snapshot("staged").unwrap();
    put(&s, "late");
    m.replace_snapshot(new, "target").unwrap();
    drop((s, m));
    let m = Meta::open(&path, opts()).unwrap();
    let t = m.snapshot("target").unwrap();
    assert!(has(&t, "n") && has(&t, "late"));
}
