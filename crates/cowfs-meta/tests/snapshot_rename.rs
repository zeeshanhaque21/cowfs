//! Issue #42 request 1: `Meta::rename_snapshot` moves a name in one transaction and keeps the id.
//!
//! The metadata API is what is under test here. No consumer is wired to it yet, and `cowfs-core`
//! still stages its own rename through `src/swap.rs`, so nothing outside `cowfs-meta` changes
//! behaviour. These tests drive `Meta` directly.
//!
//! Most cases compare content through the public content identity meta actually exposes, `chunks`
//! and `content_version`, because `Snapshot` has no read API. One case goes further and reads real
//! `cowfs-store` bytes back through those chunks, so the rename is checked against data.
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

/// Options for the pending-tree case only.
///
/// `sync_every_ops` is left at the default `256` with `Ack::Applied`, which is what
/// `cowfs_core::inner` opens meta with, so a single applied operation does not reach the commit
/// threshold and stays pending. Every other test in this file relies on `sync_every_ops: 1` to make
/// its setup deterministic, so this case cannot share `opts()`.
///
/// With `opts()` the pending case is untestable rather than hard: `s.create` is one applied op, so
/// `pending_ops` becomes `1`, `1 >= 1` satisfies the inline commit, and the tree is already clean
/// by the time the rename runs.
fn opts_pending() -> Options {
    Options {
        node_size: 512,
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
    let (id, root, _) = populated(&m);
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
fn a_rename_commits_a_pending_tree_change_and_leaves_the_row_readable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let m = Meta::open(&path, opts_pending()).unwrap();

    // Establish a durable root first, so the pending change below has an old root to differ from,
    // and so the row on disk before the rename is known good.
    let s = m.new_snapshot("snap").unwrap();
    let id = s.id();
    let base = write_files(&s, 3);
    m.sync().unwrap();
    let old_root = *s.info().unwrap().root.as_bytes();

    // A create that is NOT synced. With the default 256 ops per commit and Ack::Applied, one
    // applied operation does not reach the threshold, so the tree stays dirty and the rename is the
    // next durable commit. The entry is visible in the cached tree straight away, which is the
    // public evidence that the tree really is pending at this point.
    let pending = s.create(ROOT_INO, b"pending", 0o644).unwrap();
    let pending_ino = pending.ino;
    assert!(
        s.lookup(ROOT_INO, b"pending").is_ok(),
        "the pending entry is present in the cached tree before the rename"
    );

    m.rename_snapshot(id, "renamed").unwrap();

    assert_eq!(s.id(), id, "the id must not move");
    assert_eq!(s.info().unwrap().name, "renamed");

    drop(s);
    drop(m);

    // After a reopen the row must still describe a tree that exists, and the pending entry must be
    // in it.
    //
    // This is the discriminating step, and it is a read rather than a row comparison: the lookup
    // walks the tree from the row's root. If the rename left the row on the root the same
    // transaction freed, the walk fails with a missing tree node, which is the corruption the
    // review traced in the source.
    let again = Meta::open(&path, opts_pending()).unwrap();
    let infos = again.snapshots().unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].name, "renamed");
    assert_eq!(infos[0].id, id);
    assert_ne!(
        infos[0].root.as_bytes(),
        &old_root,
        "the committed row must carry the flushed root, not the pre-flush one"
    );

    let reopened = again
        .snapshot("renamed")
        .expect("the renamed snapshot must reopen with a readable root");
    let f = reopened
        .lookup(ROOT_INO, b"pending")
        .expect("the rename's commit must have made the pending create durable");
    assert_eq!(f.ino, pending_ino, "with the inode number it was handed");
    for (i, _) in base.iter().enumerate() {
        let name = format!("f{i}");
        assert!(
            reopened.lookup(ROOT_INO, name.as_bytes()).is_ok(),
            "{name} must still be in the renamed tree"
        );
    }
    again
        .check()
        .expect("check after a rename that committed a pending tree");
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

/// Real `cowfs-store` bytes, not a metadata fingerprint.
///
/// The other pending-tree case reads the row and the tree. This one reads data: bytes are ingested
/// into a real store, attached through the public `set_content`, and read back block by block after
/// the snapshot was renamed, dropped and reopened. Every block must still be in the store and every
/// byte must come back identical, so a rename that leaves the row on the wrong root cannot pass by
/// returning plausible metadata.
///
/// The body is deliberately larger than one maximum chunk so the survivor check is not a single
/// block in disguise.
#[test]
fn a_rename_keeps_real_file_bytes_readable_after_a_drop_and_reopen() {
    const BODY_LEN: usize = 512 * 1024;

    let dir = tempfile::tempdir().unwrap();
    let meta_path = dir.path().join("m.redb");
    let store_path = dir.path().join("store");

    // Deterministic and irregular, so content-defined chunking has real boundaries to find.
    let body: Vec<u8> = (0..BODY_LEN)
        .map(|i| ((i as u64).wrapping_mul(0x9E37_79B9) >> 24) as u8)
        .collect();

    let store =
        cowfs_store::Store::open(&store_path, cowfs_store::Options::default()).expect("open store");
    let m = Meta::open(&meta_path, opts_pending()).unwrap();

    let s = m.new_snapshot("snap").unwrap();
    let id = s.id();

    // Bytes land in the store before metadata references them, which is the order a writer uses.
    let chunks = store.ingest_bytes(&body).expect("ingest the body");
    assert!(
        chunks.len() > 1,
        "the body must chunk into more than one block, got {}",
        chunks.len()
    );
    store.sync().unwrap();

    s.batch(|tx: &mut Tx| {
        let f = tx.create(ROOT_INO, b"payload", 0o644)?;
        tx.set_content(f.ino, &chunks, body.len() as u64)?;
        Ok(())
    })
    .unwrap();

    // Durable root first, so the pending write below has an old root to differ from.
    m.sync().unwrap();
    let old_root = *s.info().unwrap().root.as_bytes();

    // One applied operation, not synced: below the default threshold, so the tree stays dirty and
    // the rename is the next durable commit.
    s.create(ROOT_INO, b"pending", 0o644).unwrap();

    m.rename_snapshot(id, "renamed").unwrap();

    drop(s);
    drop(m);

    let again = Meta::open(&meta_path, opts_pending()).unwrap();
    let reopened = again
        .snapshot("renamed")
        .expect("the renamed snapshot must reopen with a readable root");
    let f = reopened
        .lookup(ROOT_INO, b"payload")
        .expect("the file must survive the rename and the reopen");

    let after = reopened.chunks(f.ino).expect("chunks must still resolve");
    assert_eq!(
        after.len(),
        chunks.len(),
        "the rename must not change how many blocks the file is made of"
    );
    let covered: u32 = after.iter().map(|c| c.len).sum();
    assert_eq!(
        covered as usize,
        body.len(),
        "the chunk lengths must still cover the original body exactly"
    );

    let mut read_back = Vec::new();
    for (i, c) in after.iter().enumerate() {
        assert_ne!(
            c.id,
            cowfs_store::BlockId::of(b""),
            "chunk {i} must not be a placeholder id"
        );
        assert!(
            store.contains(c.id),
            "chunk {i} must still be in the store after the rename"
        );
        assert_eq!(
            c.id, chunks[i].id,
            "chunk {i} must be the same block, not a rewritten one"
        );
        read_back.extend_from_slice(&store.get(c.id).expect("the block must still read"));
    }

    assert_eq!(
        read_back, body,
        "every byte must come back identical through the renamed snapshot's root"
    );
    again
        .check()
        .expect("check after a rename that kept real bytes readable");
    assert_ne!(
        again.snapshots().unwrap()[0].root.as_bytes(),
        &old_root,
        "the committed row must carry the flushed root, not the pre-flush one"
    );
}

/// A rename whose old root is still shared with a fork.
///
/// Here the pre-flush root has a second referrer, so the transaction only decrements it and never
/// frees the tree. A stale row therefore does not surface as a missing node the way it does in the
/// unshared case; it surfaces as stale content, because the renamed snapshot would still be missing
/// the write that was pending when it was renamed. The fork also has to survive untouched.
#[test]
fn a_rename_of_a_dirty_snapshot_keeps_a_forked_old_root_live_and_the_renamed_one_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let m = Meta::open(&path, opts_pending()).unwrap();

    let base = m.new_snapshot("base").unwrap();
    let base_id = base.id();
    write_files(&base, 2);
    m.sync().unwrap();
    let base_root = *base.info().unwrap().root.as_bytes();

    // A fork is a second row on the same root, which is what keeps the old tree alive below.
    let forked = base.fork("forked").unwrap();
    assert_eq!(
        forked.info().unwrap().root.as_bytes(),
        &base_root,
        "the fork must start on the base root"
    );

    // Dirty only the base, so the fork stays on the old root and the base does not.
    base.create(ROOT_INO, b"pending", 0o644).unwrap();

    m.rename_snapshot(base_id, "renamed").unwrap();

    drop(base);
    drop(forked);
    drop(m);

    let again = Meta::open(&path, opts_pending()).unwrap();

    // The fork is not the rename target and must be exactly as it was.
    let f = again
        .snapshot("forked")
        .expect("the fork must still be readable after the base was renamed");
    assert_eq!(
        f.info().unwrap().root.as_bytes(),
        &base_root,
        "the fork keeps the old root, which the rename only decremented"
    );
    assert!(
        f.lookup(ROOT_INO, b"f0").is_ok(),
        "the fork's own content must be intact"
    );
    assert!(
        f.lookup(ROOT_INO, b"pending").is_err(),
        "the fork must not gain the write that was pending on the base"
    );

    // The renamed base carries the flushed root, and with it the write that was pending.
    let renamed = again
        .snapshot("renamed")
        .expect("the renamed base must reopen with a readable root");
    assert!(
        renamed.lookup(ROOT_INO, b"pending").is_ok(),
        "the renamed base must carry the write that was pending when it was renamed"
    );
    assert!(
        renamed.lookup(ROOT_INO, b"f0").is_ok(),
        "the renamed base must keep the content it already had"
    );
    again
        .check()
        .expect("check after a rename with a live forked root");
}
