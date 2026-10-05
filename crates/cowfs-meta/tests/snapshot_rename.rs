//! Issue #42 request 1: `Meta::rename_snapshot` moves a name in one transaction and keeps the id.
//!
//! The metadata API is what is under test here. No consumer is wired to it yet, and `cowfs-core`
//! still stages its own rename through `src/swap.rs`, so nothing outside `cowfs-meta` changes
//! behaviour. These tests drive `Meta` directly.
//!
//! Content is compared through the public content identity meta actually exposes, `chunks` and
//! `content_version`, rather than through file bytes, because `Snapshot` has no read API: reading is
//! `chunks` plus the store, and `chunks` is the fingerprint a rename must not disturb.
//!
//! What has to hold, and is asserted rather than assumed:
//!
//! - the [`SnapshotId`] is unchanged, so the tree, its Merkle root and every inode number in it are
//!   the same object before and after,
//! - a [`Snapshot`] handle taken before the rename stays usable and reports the new name,
//! - the old name stops resolving and the new one resolves to the same id,
//! - after dropping everything and reopening the store, the new name, id, root, content identity
//!   and inode numbers are all still there,
//! - a name held by a different snapshot is refused and nothing about either snapshot changes,
//! - a missing id and an invalid name are refused, and renaming to the current name is a no-op,
//! - a rename of a snapshot with uncommitted writes does not lose them, and
//! - a `before_sync` failure at the rename's own commit leaves the rows, the handles and the
//!   reopened store exactly as they were.

use cowfs_meta::{Error, Ino, Meta, Options, Snapshot, SnapshotId, Tx, ROOT_INO};
use std::collections::BTreeMap;

fn opts() -> Options {
    Options {
        node_size: 512,
        sync_every_ops: 1,
        background: false,
        ..Options::default()
    }
}

fn write_files(s: &Snapshot, n: u32) -> Vec<u64> {
    let mut inos = Vec::new();
    s.batch(|tx: &mut Tx| {
        for i in 0..n {
            inos.push(
                tx.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)?
                    .ino
                    .0,
            );
        }
        Ok(())
    })
    .unwrap();
    inos
}

/// A store with one populated, synced snapshot.
fn populated(m: &Meta) -> (SnapshotId, [u8; 32], Vec<u64>) {
    let s = m.new_snapshot("snap").unwrap();
    let id = s.id();
    let inos = write_files(&s, 3);
    m.sync().unwrap();
    (id, *s.info().unwrap().root.as_bytes(), inos)
}

/// The content identity of a snapshot: every file's inode number, its chunk list and its content
/// version. Two of these that are equal are the same bytes, which is what a rename must preserve.
fn fingerprint(s: &Snapshot, names: &[&str], inos: &[u64]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, ino) in names.iter().zip(inos) {
        let ino = Ino(*ino);
        let chunks: Vec<String> = s
            .chunks(ino)
            .unwrap()
            .iter()
            .map(|c| format!("{c:?}"))
            .collect();
        out.insert(
            (*name).to_owned(),
            format!("{ino}/{:?}/{:?}", s.content_version(ino).unwrap(), chunks),
        );
    }
    out
}

/// The three files the fixtures create: their names, and the inode numbers handed out for them.
fn file_names() -> [&'static str; 3] {
    ["f0", "f1", "f2"]
}

/// The whole point: the same snapshot, under a new name, with nothing about it changed.
#[test]
fn a_rename_keeps_the_id_the_root_the_numbers_and_an_open_handle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let m = Meta::open(&path, opts()).unwrap();
    let (id, root, inos) = populated(&m);

    // A handle taken before the rename.
    let held = m.snapshot("snap").unwrap();
    assert_eq!(held.id(), id);
    assert_eq!(held.info().unwrap().name, "snap");
    let before_fp = fingerprint(&held, &file_names(), &inos);

    m.rename_snapshot(id, "renamed").unwrap();

    // The handle is still the same snapshot, and now answers to the new name.
    assert_eq!(held.id(), id, "the id must not move");
    assert_eq!(
        held.info().unwrap().name,
        "renamed",
        "the open handle reports the new name"
    );
    assert_eq!(
        *held.info().unwrap().root.as_bytes(),
        root,
        "the root must not move"
    );
    assert_eq!(
        fingerprint(&held, &file_names(), &inos),
        before_fp,
        "the open handle still serves the same content"
    );
    assert!(
        m.snapshot("snap").is_err(),
        "the old name must stop resolving"
    );
    assert_eq!(m.snapshot("renamed").unwrap().id(), id);

    // The metadata agrees, and no snapshot was added or lost.
    let info = m.snapshots().unwrap();
    assert_eq!(info.len(), 1);
    assert_eq!(info[0].id, id);
    assert_eq!(info[0].name, "renamed");
    assert_eq!(*info[0].root.as_bytes(), root);
    assert!(
        !info.iter().any(|i| i.name == "snap"),
        "the old row is gone"
    );

    drop(held);
    drop(m);

    // After a reopen the same snapshot is there, under the new name.
    let again = Meta::open(&path, opts()).unwrap();
    let infos = again.snapshots().unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].name, "renamed");
    assert_eq!(infos[0].id, id);
    assert_eq!(*infos[0].root.as_bytes(), root);
    assert!(again.snapshot("snap").is_err());
    assert_eq!(
        fingerprint(&again.snapshot("renamed").unwrap(), &file_names(), &inos),
        before_fp,
        "the reopened store serves the same content"
    );
}

/// The id high-water mark and the inode floor are untouched by a rename.
#[test]
fn a_rename_does_not_move_the_next_id_or_the_inode_floor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let m = Meta::open(&path, opts()).unwrap();
    let (id, _, inos) = populated(&m);
    let _other = m.new_snapshot("other").unwrap();
    m.sync().unwrap();
    let highest_ino = *inos.iter().max().unwrap();

    m.rename_snapshot(id, "renamed").unwrap();

    // A snapshot created after the rename must not reuse the id the renamed one holds.
    let fresh = m.new_snapshot("later").unwrap();
    assert_ne!(fresh.id(), id, "a rename must not free the id for reuse");
    let fresh_inos = write_files(&fresh, 1);
    m.sync().unwrap();
    assert!(
        *fresh_inos.iter().max().unwrap() > highest_ino,
        "the inode floor must still clear everything handed out before the rename"
    );
    assert_eq!(m.snapshots().unwrap().len(), 3);
}

/// Renaming to the name the snapshot already has changes nothing and is not an error.
#[test]
fn renaming_to_the_current_name_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let m = Meta::open(&path, opts()).unwrap();
    let (id, root, inos) = populated(&m);
    let before = fingerprint(&m.snapshot("snap").unwrap(), &file_names(), &inos);

    m.rename_snapshot(id, "snap").unwrap();
    m.rename_snapshot(id, "snap").unwrap();

    let info = m.snapshots().unwrap();
    assert_eq!(info.len(), 1, "a no-op must not add a snapshot");
    assert_eq!(info[0].id, id);
    assert_eq!(*info[0].root.as_bytes(), root);
    assert_eq!(
        fingerprint(&m.snapshot("snap").unwrap(), &file_names(), &inos),
        before
    );

    drop(m);
    let again = Meta::open(&path, opts()).unwrap();
    let infos = again.snapshots().unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].name, "snap");
    assert_eq!(infos[0].id, id);
}

/// A name another snapshot holds is refused, and neither snapshot is touched.
#[test]
fn a_name_held_by_another_snapshot_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let m = Meta::open(&path, opts()).unwrap();
    let (id, root, inos) = populated(&m);
    let other = m.new_snapshot("taken").unwrap();
    let other_id = other.id();
    let other_root = *other.info().unwrap().root.as_bytes();
    m.sync().unwrap();

    let e = m.rename_snapshot(id, "taken").unwrap_err();
    assert!(
        matches!(e, Error::SnapshotExists),
        "a held name is refused, not replaced: {e:?}"
    );

    // Nothing moved: both snapshots still answer to their own names, with their own roots.
    let infos = m.snapshots().unwrap();
    assert_eq!(infos.len(), 2);
    let a = infos
        .iter()
        .find(|i| i.id == id)
        .expect("the renamed one is still here");
    assert_eq!(
        a.name, "snap",
        "the refused rename must not have renamed it"
    );
    assert_eq!(*a.root.as_bytes(), root);
    let b = infos
        .iter()
        .find(|i| i.id == other_id)
        .expect("the other is still here");
    assert_eq!(b.name, "taken");
    assert_eq!(
        *b.root.as_bytes(),
        other_root,
        "the other must not be replaced or removed"
    );
    assert_eq!(m.snapshot("snap").unwrap().id(), id);
    assert_eq!(m.snapshot("taken").unwrap().id(), other_id);
    assert_eq!(
        m.snapshot("snap").unwrap().info().unwrap().root.as_bytes(),
        &root,
        "the refused rename left the first snapshot's root alone"
    );
    assert_eq!(
        m.snapshot("taken").unwrap().info().unwrap().root.as_bytes(),
        &other_root,
        "and the second snapshot's root alone"
    );

    drop(other);
    drop(m);
    let again = Meta::open(&path, opts()).unwrap();
    let names: Vec<_> = again
        .snapshots()
        .unwrap()
        .into_iter()
        .map(|i| (i.name, i.id))
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&("snap".to_owned(), id)));
    assert!(names.contains(&("taken".to_owned(), other_id)));
}

/// A missing id and an invalid name are refused, and the namespace is unchanged.
#[test]
fn a_missing_id_and_an_invalid_name_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let m = Meta::open(&path, opts()).unwrap();
    let (id, _, _) = populated(&m);
    m.sync().unwrap();
    let before = m.snapshots().unwrap();

    let e = m.rename_snapshot(SnapshotId(9999), "whatever").unwrap_err();
    assert!(matches!(e, Error::NoSuchSnapshot), "missing id: {e:?}");

    let e = m.rename_snapshot(id, "").unwrap_err();
    assert!(matches!(e, Error::Invalid(_)), "empty name: {e:?}");

    let long = "x".repeat(usize::from(u16::MAX) + 1);
    let e = m.rename_snapshot(id, &long).unwrap_err();
    assert!(matches!(e, Error::Invalid(_)), "over-long name: {e:?}");

    assert_eq!(
        m.snapshots().unwrap(),
        before,
        "no refusal may change anything"
    );

    drop(m);
    let again = Meta::open(&path, opts()).unwrap();
    assert_eq!(again.snapshots().unwrap(), before);
}

/// A snapshot with uncommitted writes keeps them: the rename is a commit like any other, so the
/// data is flushed in the same transaction rather than dropped.
#[test]
fn a_rename_of_a_dirty_snapshot_keeps_its_uncommitted_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let m = Meta::open(&path, opts()).unwrap();
    let (id, _, _) = populated(&m);

    // Created but not synced: the snapshot's tree is dirty, and the rename's commit is the next
    // durable commit, so it is what makes the new inode durable.
    let s = m.snapshot("snap").unwrap();
    let f = s.create(ROOT_INO, b"dirty", 0o644).unwrap();
    let ino = f.ino;

    m.rename_snapshot(id, "renamed").unwrap();

    assert_eq!(s.id(), id, "the id must not move");
    assert_eq!(s.info().unwrap().name, "renamed");
    assert!(
        s.lookup(ROOT_INO, b"dirty").is_ok(),
        "the uncommitted create is visible on the open handle"
    );

    drop(s);
    drop(m);
    let again = Meta::open(&path, opts()).unwrap();
    let names = again.snapshot("renamed").unwrap();
    let df = names
        .lookup(ROOT_INO, b"dirty")
        .expect("the uncommitted create was committed by the rename, not dropped");
    assert_eq!(df.ino, ino, "with the inode number it was handed");
}

/// The existing `before_sync` hook, fired at the rename's own commit, must leave everything as it
/// was. The fault point is specified rather than assumed: the hook is armed only after the snapshot
/// exists and is synced, so nothing during the setup can trip it, and the next durable commit is
/// the rename's own transaction.
#[test]
fn a_before_sync_failure_at_the_rename_commit_changes_nothing() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let armed = Arc::new(AtomicBool::new(false));
    let tripped = Arc::new(AtomicBool::new(false));
    let (a2, t2) = (armed.clone(), tripped.clone());
    let mut o = opts();
    o.before_sync = Some(Arc::new(move || {
        if a2.load(Ordering::SeqCst) {
            t2.store(true, Ordering::SeqCst);
            return Err(std::io::Error::other("rename probe: sync refused"));
        }
        Ok(())
    }));
    let m = Meta::open(&path, o).unwrap();
    let (id, root, inos) = populated(&m);
    let before_rows = m.snapshots().unwrap();
    let held = m.snapshot("snap").unwrap();
    let before_fp = fingerprint(&held, &file_names(), &inos);

    // Arm the fault, then attempt the rename. The rename's commit is the next durable commit.
    armed.store(true, Ordering::SeqCst);
    let e = m.rename_snapshot(id, "renamed").unwrap_err();
    assert!(tripped.load(Ordering::SeqCst), "the hook must have fired");
    assert!(
        !e.to_string().is_empty(),
        "the failure is reported, not swallowed: {e:?}"
    );

    // The live namespace is unchanged.
    assert_eq!(m.snapshots().unwrap(), before_rows, "no row may move");
    assert_eq!(
        m.snapshot("snap").unwrap().id(),
        id,
        "the old name still resolves"
    );
    assert!(
        m.snapshot("renamed").is_err(),
        "the new name must not appear"
    );
    assert_eq!(held.id(), id, "the open handle still works");
    assert_eq!(fingerprint(&held, &file_names(), &inos), before_fp);

    drop(held);
    drop(m);

    // And so is the store on disk: the failed transaction wrote nothing.
    let again = Meta::open(&path, opts()).unwrap();
    assert_eq!(
        again.snapshots().unwrap(),
        before_rows,
        "the reopen must agree"
    );
    assert!(again.snapshot("renamed").is_err());
    assert_eq!(again.snapshot("snap").unwrap().id(), id);
    assert_eq!(
        fingerprint(&again.snapshot("snap").unwrap(), &file_names(), &inos),
        before_fp
    );
    let _ = (root, inos);
}

/// The other direction: renaming one snapshot frees its old name for a later create.
#[test]
fn a_rename_frees_the_old_name_and_keeps_the_new_one_busy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let m = Meta::open(&path, opts()).unwrap();
    let (id, _, _) = populated(&m);

    m.rename_snapshot(id, "renamed").unwrap();

    // The old name is genuinely free now.
    let reused = m.new_snapshot("snap").unwrap();
    assert_ne!(reused.id(), id, "a fresh create gets a fresh id");

    // The new name is still held by the renamed one.
    m.rename_snapshot(id, "renamed").unwrap();
    let e = m.rename_snapshot(reused.id(), "renamed").unwrap_err();
    assert!(matches!(e, Error::SnapshotExists), "{e:?}");
}
