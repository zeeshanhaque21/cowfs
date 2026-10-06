//! An atomic rename moves the name and keeps everything the name pointed at.
//!
//! `cowfs-meta` can now move a snapshot's name in one transaction and keep its id (PR #137), so
//! `Core::rename_snapshot` no longer has to fork a staging snapshot and swap it in. That changes
//! what a caller can rely on: the snapshot id, the inode numbers inside it, and any handle already
//! open on it all survive, where the old staging swap replaced all three.
//!
//! These cases use only the public `Core` and `Vfs` surface, so the same source runs against the
//! tree before the change and the tree with it. The identity cases fail on the old tree, which is
//! the point: they are the claim being made.
//!
//! What this does not cover: the promotion and crash-recovery paths in `src/swap.rs`, which keep
//! using the staging swap because they replace a target rather than move a name.

mod common;

use common::{mkfile, read_all, root_entry, test_opts, write_all};
use cowfs_core::{ControlError, Core};
use cowfs_vfs::Vfs;

fn names(c: &Core) -> Vec<String> {
    let mut n: Vec<String> = c
        .list_snapshots()
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    n.sort();
    n
}

fn entry(c: &Core, name: &str) -> cowfs_core::SnapshotEntry {
    c.list_snapshots()
        .unwrap()
        .into_iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("{name} is listed"))
}

/// One snapshot with one file of known bytes, and the numbers a rename must not disturb.
struct Fixture2 {
    /// Held so the directory outlives the `Core` in it.
    _dir: tempfile::TempDir,
    c: Core,
    id: u64,
    ino: cowfs_vfs::Ino,
    file: cowfs_vfs::Ino,
}

fn with_file(name: &str, body: &[u8]) -> Fixture2 {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot(name).unwrap();
    let ino = root_entry(&c, name).ino;
    let v = c.snapshot_view(name).unwrap();
    let file = mkfile(&v, ino, "f", body).ino;
    c.sync().unwrap();
    let id = entry(&c, name).id;
    Fixture2 {
        _dir: dir,
        c,
        id,
        ino,
        file,
    }
}

/// The claim, in one case: the name moves and nothing else does.
#[test]
fn a_rename_keeps_the_id_the_root_and_the_file_numbers() {
    let f = with_file("work", b"payload");
    let before = entry(&f.c, "work");

    let after = f.c.rename_snapshot("work", "renamed").unwrap();

    assert_eq!(after.name, "renamed", "the new name comes back");
    assert_eq!(after.id, before.id, "the snapshot id must not change");
    assert_eq!(after.ino, f.ino, "the root inode must not change");
    let file_after =
        f.c.snapshot_view("renamed")
            .unwrap()
            .lookup(f.ino, b"f")
            .unwrap()
            .ino;
    assert_eq!(file_after, f.file, "the file inode must not change");
    assert_eq!(names(&f.c), ["renamed"]);
    assert_eq!(
        f.c.snapshot_view("work").err(),
        Some(ControlError::NotFound)
    );
    f.c.check().unwrap();
}

/// A handle opened before the rename still reads and writes the same file afterwards.
#[test]
fn a_handle_open_across_the_rename_still_reads_and_writes() {
    let f = with_file("work", b"payload");
    let v = f.c.snapshot_view("work").unwrap();
    let h = v.open(f.file).expect("open");

    let after = f.c.rename_snapshot("work", "renamed").unwrap();
    assert_eq!(after.id, f.id, "the snapshot id must not change");

    let v = f.c.snapshot_view("renamed").unwrap();
    assert_eq!(
        read_all(&v, f.file),
        b"payload",
        "the handle's inode must still read the same bytes"
    );
    write_all(&v, f.file, 0, b"PAYLOAD");
    assert_eq!(read_all(&v, f.file), b"PAYLOAD");
    v.release(h).expect("release");
    f.c.sync().unwrap();
    assert_eq!(
        read_all(&f.c.snapshot_view("renamed").unwrap(), f.file),
        b"PAYLOAD"
    );
    f.c.check().unwrap();
}

/// A dirty write made before the rename is still there after it, through the same inode.
#[test]
fn a_dirty_write_before_the_rename_is_visible_after_it() {
    let f = with_file("work", b"first");
    // no sync: this is queued work, not committed work
    let v = f.c.snapshot_view("work").unwrap();
    write_all(&v, f.file, 0, b"second");

    let after = f.c.rename_snapshot("work", "renamed").unwrap();
    assert_eq!(after.id, f.id, "the snapshot id must not change");

    assert_eq!(
        read_all(&f.c.snapshot_view("renamed").unwrap(), f.file),
        b"second",
        "the queued write must survive the rename"
    );
    f.c.sync().unwrap();
    assert_eq!(
        read_all(&f.c.snapshot_view("renamed").unwrap(), f.file),
        b"second"
    );
    f.c.check().unwrap();
}

/// A refused rename changes no name and no content, and leaves no staging snapshot behind.
#[test]
fn a_refused_rename_changes_no_name_and_leaves_no_leftovers() {
    let f = with_file("work", b"payload");
    f.c.create_snapshot("taken").unwrap();
    let taken_before = entry(&f.c, "taken");

    // a name that is held
    assert_eq!(
        f.c.rename_snapshot("work", "taken"),
        Err(ControlError::Exists),
        "a held name must be refused"
    );
    // a name that breaks the rules
    assert!(matches!(
        f.c.rename_snapshot("work", "a/b"),
        Err(ControlError::InvalidName(_))
    ));
    // a source that is not there
    assert_eq!(
        f.c.rename_snapshot("nope", "other"),
        Err(ControlError::NotFound)
    );
    // the same name twice is still refused, the existing contract
    assert!(matches!(
        f.c.rename_snapshot("work", "work"),
        Err(ControlError::InvalidName(_))
    ));

    assert_eq!(
        names(&f.c),
        ["taken".to_string(), "work".to_string()],
        "no name may appear or disappear"
    );
    assert_eq!(
        entry(&f.c, "taken").id,
        taken_before.id,
        "the refused target keeps its identity"
    );
    assert_eq!(entry(&f.c, "work").id, f.id);
    assert_eq!(
        read_all(&f.c.snapshot_view("work").unwrap(), f.file),
        b"payload",
        "content is untouched"
    );
    f.c.check().unwrap();
}

/// A missing source is reported before an invalid target name, which is the existing order.
#[test]
fn a_missing_source_is_reported_before_an_invalid_target_name() {
    let f = with_file("work", b"payload");
    assert_eq!(
        f.c.rename_snapshot("nope", "a/b"),
        Err(ControlError::NotFound),
        "the source is looked up first"
    );
    f.c.check().unwrap();
}

/// The exact stored bytes survive a sync, a drop and a reopen, under the same id.
#[test]
fn the_stored_bytes_survive_a_drop_and_a_reopen_under_the_same_id() {
    let dir = tempfile::tempdir().unwrap();
    let body = b"the exact bytes a rename must not touch";
    let (id, packed, file) = {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.create_snapshot("work").unwrap();
        let ino = root_entry(&c, "work").ino;
        let v = c.snapshot_view("work").unwrap();
        let file = mkfile(&v, ino, "f", body).ino;
        c.sync().unwrap();
        let e = c.rename_snapshot("work", "renamed").unwrap();
        assert_eq!(entry(&c, "renamed").id, e.id);
        (e.id, e.ino, file)
    };

    let c = Core::open(dir.path(), test_opts()).expect("reopen");
    let e = entry(&c, "renamed");
    assert_eq!(e.id, id, "the id survives a reopen");
    assert_eq!(e.ino, packed, "the packed root inode survives a reopen");
    // A fresh session hands out its own virtual inode numbers and never reuses an earlier
    // session's, so the file is resolved by name here. The rename is what has to be invisible:
    // same id, same packed root, same bytes under the new name.
    let v = c.snapshot_view("renamed").unwrap();
    let root = root_entry(&c, "renamed").ino;
    let reopened_file = v.lookup(root, b"f").expect("f is under the new name").ino;
    assert_ne!(
        reopened_file, file,
        "a new session must not reuse an earlier session's virtual inode"
    );
    assert_eq!(read_all(&v, reopened_file), body, "the exact bytes survive");
    assert_eq!(c.snapshot_view("work").err(), Some(ControlError::NotFound));
    c.check().unwrap();
}

/// A rename moves one name and leaves every other snapshot exactly as it was.
#[test]
fn a_rename_moves_one_name_and_changes_no_other_snapshot() {
    let f = with_file("work", b"payload");
    f.c.create_snapshot("other").unwrap();
    let other_ino = root_entry(&f.c, "other").ino;
    let v = f.c.snapshot_view("other").unwrap();
    let other_file = mkfile(&v, other_ino, "g", b"untouched").ino;
    f.c.sync().unwrap();
    let other_before = entry(&f.c, "other");

    let after = f.c.rename_snapshot("work", "renamed").unwrap();

    assert_eq!(after.id, f.id);
    assert_eq!(
        entry(&f.c, "other"),
        other_before,
        "another snapshot's entry is untouched"
    );
    let v = f.c.snapshot_view("other").unwrap();
    assert_eq!(
        read_all(&v, other_file),
        b"untouched",
        "another snapshot's content is untouched"
    );
    assert_eq!(names(&f.c), ["other".to_string(), "renamed".to_string()]);
    f.c.check().unwrap();
}

/// Promotion replaces its target with the source's content. Its recorded old-consumer
/// failure was an earlier body of this fixture reading the target's original inode, which
/// the replacement destroys, so nothing in this case depends on the rename change.
#[test]
fn promote_base_replaces_its_target_with_the_source_content() {
    let f = with_file("base", b"old base");
    f.c.create_snapshot("src").unwrap();
    let v = f.c.snapshot_view("src").unwrap();
    let src_root = root_entry(&f.c, "src").ino;
    mkfile(&v, src_root, "h", b"from src");
    f.c.sync().unwrap();
    let src_before = entry(&f.c, "src");

    f.c.promote_base("src", "base").expect("promote");

    assert_eq!(
        entry(&f.c, "src"),
        src_before,
        "promotion leaves the source snapshot alone"
    );
    let base = f.c.snapshot_view("base").unwrap();
    let base_root = root_entry(&f.c, "base").ino;
    assert_eq!(
        read_all(&base, base.lookup(base_root, b"h").unwrap().ino),
        b"from src",
        "the target now holds the source's content"
    );
    assert!(
        base.lookup(base_root, b"f").is_err(),
        "promotion replaces the target, so the target's own file is gone"
    );
    assert_eq!(names(&f.c), ["base".to_string(), "src".to_string()]);
    f.c.check().unwrap();
}

/// Renaming twice in a row keeps the same id, so the second rename starts from the first.
#[test]
fn two_renames_in_a_row_keep_the_same_id() {
    let f = with_file("a", b"payload");
    let first = f.c.rename_snapshot("a", "b").unwrap();
    let second = f.c.rename_snapshot("b", "c").unwrap();
    assert_eq!(second.id, f.id, "the id survives both renames");
    assert_eq!(second.id, first.id);
    assert_eq!(names(&f.c), ["c".to_string()]);
    assert_eq!(
        read_all(&f.c.snapshot_view("c").unwrap(), f.file),
        b"payload"
    );
    f.c.check().unwrap();
}

/// A rename that fails at the metadata commit changes nothing at all.
///
/// The fault is armed through the existing `Core::open_with_meta` seam: the metadata
/// `before_sync` hook `Core::open` already installs, wrapped so it can be told to fail. No new
/// public fault API is introduced for this.
#[test]
fn a_rename_that_fails_at_the_commit_changes_nothing() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let flag = armed.clone();
    let c = Core::open_with_meta(
        dir.path(),
        test_opts(),
        move |dir, mut o: cowfs_meta::Options| {
            let store_sync = o
                .before_sync
                .take()
                .expect("Core::open wires the store hook");
            o.before_sync = Some(Arc::new(move || {
                if flag.load(Ordering::Acquire) {
                    return Err(std::io::Error::other("armed before_sync failure"));
                }
                store_sync()
            }));
            cowfs_meta::Meta::open(dir.join("meta.redb"), o)
        },
    )
    .unwrap();
    c.create_snapshot("work").unwrap();
    let ino = root_entry(&c, "work").ino;
    let v = c.snapshot_view("work").unwrap();
    let file = mkfile(&v, ino, "f", b"payload").ino;
    c.sync().unwrap();
    let before = entry(&c, "work");

    armed.store(true, Ordering::Release);
    let err = c
        .rename_snapshot("work", "renamed")
        .expect_err("the commit is armed to fail");
    armed.store(false, Ordering::Release);

    assert!(
        matches!(err, ControlError::Fs(_)),
        "the hook failure must surface as a control error, got {err:?}"
    );
    assert_eq!(
        names(&c),
        ["work".to_string()],
        "a failed rename must not add, remove or move a name"
    );
    assert_eq!(
        entry(&c, "work"),
        before,
        "the snapshot is exactly as it was"
    );
    assert_eq!(
        read_all(&c.snapshot_view("work").unwrap(), file),
        b"payload",
        "content is untouched"
    );
    assert!(
        c.snapshot_view("renamed").is_err(),
        "the new name must not exist after a failed rename"
    );
    // the old name still works after the fault is disarmed, so nothing was left half done
    c.rename_snapshot("work", "renamed")
        .expect("the retry succeeds");
    assert_eq!(entry(&c, "renamed").id, before.id);
    c.check().unwrap();
}
