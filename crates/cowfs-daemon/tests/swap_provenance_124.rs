//! Issue #124: a swap that replaces a base's tree must not leave that base's record behind.
//!
//! `swap` replaces the tree under a name without touching the base record, so after a swap the
//! record still describes the tree that was replaced, and a caller that reconnects is told that
//! stale commit. These tests bind real snapshots on real private stores and assert the record,
//! the tree and a fresh reopen together, so "the record was cleared" and "the tree really changed"
//! are checked against the same swap rather than separately.
//!
//! What is asserted, on both backends:
//!
//! - a successful swap into a promoted base leaves the base promoted with no repo, ref or commit,
//!   so the record cannot describe a tree it did not produce,
//! - the provenance of the source snapshot is deliberately not adopted, because a swap installs a
//!   clone and says nothing about which build the clone came from,
//! - the source snapshot is untouched by the swap,
//! - a swap into a name that was never promoted does not invent a base record,
//! - a swap that cannot publish the record reports failure and leaves the old tree and the old
//!   record, rather than succeeding with stale metadata,
//! - the existing refusals keep their meaning and keep their record.
//!
//! The `promote` step matters: it is what makes a snapshot a base, and a base designation is
//! preserved by the swap. Only a promoted name has a record to invalidate.

use cowfs_daemon::{Backend, CoreBackend, PathBackend};
use cowfs_vfs::ROOT_INO;

fn meta(repo: &str, git_ref: &str, commit: &str) -> cowfs_ctl::BaseMeta {
    cowfs_ctl::BaseMeta {
        repo: Some(repo.to_owned()),
        git_ref: Some(git_ref.to_owned()),
        commit: Some(commit.to_owned()),
    }
}

fn commit_of(i: &cowfs_ctl::SnapshotInfo) -> Option<String> {
    i.base.as_ref().and_then(|b| b.commit.clone())
}

fn repo_of(i: &cowfs_ctl::SnapshotInfo) -> Option<String> {
    i.base.as_ref().and_then(|b| b.repo.clone())
}

fn ref_of(i: &cowfs_ctl::SnapshotInfo) -> Option<String> {
    i.base.as_ref().and_then(|b| b.git_ref.clone())
}

/// A base that has been proven stale: promoted, but no field of the record may survive.
fn assert_base_unknown(what: &str, i: &cowfs_ctl::SnapshotInfo) {
    assert!(
        i.base.is_some(),
        "{what}: the base designation must survive the swap: {i:?}"
    );
    assert_eq!(commit_of(i), None, "{what}: a stale commit survived: {i:?}");
    assert_eq!(repo_of(i), None, "{what}: a stale repo survived: {i:?}");
    assert_eq!(ref_of(i), None, "{what}: a stale ref survived: {i:?}");
}

fn body(vfs: &std::sync::Arc<dyn cowfs_vfs::Vfs>, text: &[u8]) {
    // A clone of a seeded snapshot already carries `only`, so the name is freed first: this writes
    // content, it does not assert the name is free.
    let _ = vfs.unlink(ROOT_INO, b"only");
    let f = vfs.create(ROOT_INO, b"only", 0o644).unwrap();
    let h = vfs.open(f.ino).unwrap();
    vfs.write(f.ino, 0, text).unwrap();
    vfs.fsync(f.ino, false).unwrap();
    vfs.release(h).unwrap();
}

/// The tree's real bytes, so a change of tree is decided by content and not by a name.
fn tree(vfs: &std::sync::Arc<dyn cowfs_vfs::Vfs>) -> String {
    let a = vfs.lookup(ROOT_INO, b"only").unwrap();
    let h = vfs.open(a.ino).unwrap();
    let n = vfs.read(a.ino, 0, 4096).unwrap();
    vfs.release(h).unwrap();
    String::from_utf8_lossy(&n).into_owned()
}

/// A base built from `src`, promoted, carrying `commit`'s provenance.
fn seeded_base(b: &dyn Backend, src: &str, base: &str, commit: &str) -> String {
    let s = b.snapshots();
    s.create(src, None).unwrap();
    body(
        &b.snapshot(src).unwrap(),
        format!("AAAA-from-{src}").as_bytes(),
    );
    s.create(base, Some(src)).unwrap();
    s.promote(base).unwrap();
    s.set_base_meta(base, &meta("/repoA", "refs/heads/main", commit))
        .unwrap();
    let before = tree(&b.snapshot(base).unwrap());
    assert_eq!(
        commit_of(&s.create_meta(base).unwrap()).as_deref(),
        Some(commit),
        "the fixture must start with its provenance recorded"
    );
    before
}

fn seeded_source(b: &dyn Backend, src: &str) -> String {
    let s = b.snapshots();
    s.create(src, None).unwrap();
    body(
        &b.snapshot(src).unwrap(),
        format!("BBBB-from-{src}").as_bytes(),
    );
    tree(&b.snapshot(src).unwrap())
}

/// The regression for #124 on the core backend: the primary counterexample, with the store
/// reopened so the answer a reconnecting caller would get is the one asserted.
#[test]
fn a_core_swap_clears_the_base_record_it_inherited() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().to_owned();
    let b = CoreBackend::open(&store, cowfs_core::Options::default()).unwrap();
    let s = b.snapshots();
    let before = seeded_base(&b, "srcA", "base", "commit-AAA");
    let src_before = seeded_source(&b, "srcB");

    let response = s.swap("base", "srcB").unwrap();
    let after = tree(&b.snapshot("base").unwrap());
    assert_ne!(
        before, after,
        "the swap must have installed a different tree"
    );
    assert_eq!(after, "BBBB-from-srcB", "the tree must be the source's");
    assert_eq!(
        tree(&b.snapshot("srcB").unwrap()),
        src_before,
        "the source must survive the swap intact"
    );
    // The immediate response is what the calling client is told, so it must not carry the old record.
    assert_base_unknown("core, immediate response", &response);
    assert_base_unknown("core, fresh info", &s.create_meta("base").unwrap());

    drop(b);
    let reopened = CoreBackend::open(&store, cowfs_core::Options::default()).unwrap();
    assert_base_unknown(
        "core, fresh backend",
        &reopened.snapshots().create_meta("base").unwrap(),
    );
    assert_eq!(
        tree(&reopened.snapshot("base").unwrap()),
        "BBBB-from-srcB",
        "the reopened store still has the source's tree"
    );
}

/// The same on the path backend, because `swap` is implemented twice and the defect is in both.
#[test]
fn a_path_swap_clears_the_base_record_it_inherited() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let b = PathBackend::open(&store).unwrap();
    let s = b.snapshots();
    let before = seeded_base(&b, "srcA", "base", "commit-AAA");
    let src_before = seeded_source(&b, "srcB");

    let response = s.swap("base", "srcB").unwrap();
    let after = tree(&b.snapshot("base").unwrap());
    assert_ne!(
        before, after,
        "the swap must have installed a different tree"
    );
    assert_eq!(after, "BBBB-from-srcB");
    assert_eq!(tree(&b.snapshot("srcB").unwrap()), src_before);
    assert_base_unknown("path, immediate response", &response);
    assert_base_unknown("path, fresh info", &s.create_meta("base").unwrap());

    drop(b);
    let reopened = PathBackend::open(&store).unwrap();
    assert_base_unknown(
        "path, fresh backend",
        &reopened.snapshots().create_meta("base").unwrap(),
    );
    assert_eq!(tree(&reopened.snapshot("base").unwrap()), "BBBB-from-srcB");
}

/// A source that is itself a published base has a correct record available, and the swap must
/// still not adopt it: the swap installs a clone, and nothing about the call says which build that
/// clone was taken from, so adopting would replace one unproven claim with another.
#[test]
fn a_swap_does_not_adopt_the_source_provenance() {
    let dir = tempfile::tempdir().unwrap();
    let b = CoreBackend::open(dir.path(), cowfs_core::Options::default()).unwrap();
    let s = b.snapshots();
    seeded_base(&b, "srcA", "base", "commit-AAA");

    s.create("srcB", None).unwrap();
    body(&b.snapshot("srcB").unwrap(), b"BBBB-from-srcB");
    s.promote("srcB").unwrap();
    s.set_base_meta("srcB", &meta("/repoB", "refs/heads/dev", "commit-BBB"))
        .unwrap();
    assert_eq!(
        commit_of(&s.create_meta("srcB").unwrap()).as_deref(),
        Some("commit-BBB"),
        "the source must start with a record of its own"
    );

    s.swap("base", "srcB").unwrap();
    assert_base_unknown(
        "source record deliberately discarded",
        &s.create_meta("base").unwrap(),
    );
    assert_eq!(
        commit_of(&s.create_meta("srcB").unwrap()).as_deref(),
        Some("commit-BBB"),
        "and the source keeps its own record"
    );
}

/// Do-nothing control: with no swap the base keeps its own commit, so the assertions above are
/// about the swap and not about a fixture that cannot hold provenance at all.
#[test]
fn a_base_keeps_its_own_provenance_when_nothing_is_swapped() {
    let dir = tempfile::tempdir().unwrap();
    let b = CoreBackend::open(dir.path(), cowfs_core::Options::default()).unwrap();
    let s = b.snapshots();
    let base_tree = seeded_base(&b, "srcA", "base", "commit-AAA");
    let src_tree = seeded_source(&b, "srcB");
    assert_ne!(base_tree, src_tree, "the two trees must differ");
    let info = s.create_meta("base").unwrap();
    assert_eq!(commit_of(&info).as_deref(), Some("commit-AAA"));
    assert_eq!(repo_of(&info).as_deref(), Some("/repoA"));
    assert_eq!(ref_of(&info).as_deref(), Some("refs/heads/main"));
}

/// A name that was never promoted is not a base, so a swap must not make it one. Inventing a
/// record here would give a plain snapshot a base designation nothing asked for.
#[test]
fn a_swap_into_a_name_that_was_never_promoted_invents_no_base() {
    for which in ["core", "path"] {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        let b: Box<dyn Backend> = if which == "core" {
            Box::new(CoreBackend::open(&store, cowfs_core::Options::default()).unwrap())
        } else {
            Box::new(PathBackend::open(&store).unwrap())
        };
        let s = b.snapshots();
        s.create("srcA", None).unwrap();
        body(&b.snapshot("srcA").unwrap(), b"AAAA-from-srcA");
        s.create("plain", Some("srcA")).unwrap();
        seeded_source(b.as_ref(), "srcB");

        s.swap("plain", "srcB").unwrap();
        assert_eq!(
            tree(&b.snapshot("plain").unwrap()),
            "BBBB-from-srcB",
            "{which}: the tree is still replaced"
        );
        assert!(
            s.create_meta("plain").unwrap().base.is_none(),
            "{which}: a plain snapshot must not acquire a base record: {:?}",
            s.create_meta("plain").unwrap()
        );
    }
}

/// The record is published before the swap commits, so a store whose record cannot be written
/// must report the failure and leave both the old tree and the old record in place. Reporting
/// success here would hand the caller a base that claims a commit its new tree did not produce.
#[test]
fn a_swap_that_cannot_publish_the_record_fails_and_keeps_the_old_base() {
    for which in ["core", "path"] {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        let b: Box<dyn Backend> = if which == "core" {
            Box::new(CoreBackend::open(&store, cowfs_core::Options::default()).unwrap())
        } else {
            Box::new(PathBackend::open(&store).unwrap())
        };
        let s = b.snapshots();
        let before_tree = seeded_base(b.as_ref(), "srcA", "base", "commit-AAA");
        seeded_source(b.as_ref(), "srcB");

        // The existing seam for this, used by the base_meta tests: publishing a record needs write
        // permission on the base's own record directory, so making that read-only stops the write
        // without touching anything else. The probe below proves the failure was really injected,
        // so a passing run cannot come from a store that was never actually read-only.
        let record_dir = store.join(".cowfs-base-meta").join("base");
        let readonly = |p: &std::path::Path, m: u32| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap()
        };
        readonly(&record_dir, 0o500);
        let injected = std::fs::write(record_dir.join("probe"), b"x").is_err();
        readonly(&record_dir, 0o700);
        assert!(
            injected,
            "{which}: the record directory was never read-only"
        );

        readonly(&record_dir, 0o500);
        let refused = s.swap("base", "srcB");
        readonly(&record_dir, 0o700);

        assert!(
            refused.is_err(),
            "{which}: a swap that cannot publish its record must not report success"
        );
        assert_eq!(
            tree(&b.snapshot("base").unwrap()),
            before_tree,
            "{which}: the old tree must survive a refused swap"
        );
        let kept = s.create_meta("base").unwrap();
        assert_eq!(
            commit_of(&kept).as_deref(),
            Some("commit-AAA"),
            "{which}: the old record must survive a refused swap: {kept:?}"
        );
        assert_eq!(repo_of(&kept).as_deref(), Some("/repoA"));
        assert_eq!(ref_of(&kept).as_deref(), Some("refs/heads/main"));
        assert_eq!(
            tree(&b.snapshot("srcB").unwrap()),
            "BBBB-from-srcB",
            "{which}: the source must be untouched by a refused swap"
        );

        // And the record a later reader sees agrees with the live one.
        drop(b);
        let reopened: Box<dyn Backend> = if which == "core" {
            Box::new(CoreBackend::open(&store, cowfs_core::Options::default()).unwrap())
        } else {
            Box::new(PathBackend::open(&store).unwrap())
        };
        assert_eq!(
            commit_of(&reopened.snapshots().create_meta("base").unwrap()).as_deref(),
            Some("commit-AAA"),
            "{which}: a reopened store must not report a different commit"
        );
    }
}

/// The refusals a swap already had keep their meaning, and a refused call leaves the record alone.
#[test]
fn the_existing_swap_refusals_keep_their_record() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let b = CoreBackend::open(&store, cowfs_core::Options::default()).unwrap();
    let s = b.snapshots();
    seeded_base(&b, "srcA", "base", "commit-AAA");
    let before_tree = tree(&b.snapshot("base").unwrap());
    seeded_source(&b, "srcB");

    // Missing source: refused, and nothing about the base changes.
    let e = s.swap("base", "nosuch").unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "{e}");

    // A snapshot with itself: refused by name.
    let e = s.swap("base", "base").unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput, "{e}");

    // A missing target: still not found, so a swap never quietly creates a base.
    let e = s.swap("nosuchbase", "srcB").unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "{e}");

    let kept = s.create_meta("base").unwrap();
    assert_eq!(
        commit_of(&kept).as_deref(),
        Some("commit-AAA"),
        "a refused swap keeps the record: {kept:?}"
    );
    assert_eq!(
        tree(&b.snapshot("base").unwrap()),
        before_tree,
        "a refused swap keeps the tree"
    );
    assert_eq!(
        tree(&b.snapshot("srcB").unwrap()),
        "BBBB-from-srcB",
        "a refused swap leaves the source alone"
    );
}

/// A target that is not a base stays not a base, even when the source is one. The swap moves a
/// tree; it does not promote.
#[test]
fn a_swap_does_not_promote_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let b = CoreBackend::open(&store, cowfs_core::Options::default()).unwrap();
    let s = b.snapshots();
    s.create("srcA", None).unwrap();
    body(&b.snapshot("srcA").unwrap(), b"AAAA-from-srcA");
    s.create("plain", Some("srcA")).unwrap();
    s.create("base", Some("srcA")).unwrap();
    s.promote("base").unwrap();
    s.set_base_meta("base", &meta("/repoA", "refs/heads/main", "commit-AAA"))
        .unwrap();

    s.swap("plain", "base").unwrap();
    assert!(
        s.create_meta("plain").unwrap().base.is_none(),
        "swapping from a base must not promote the target"
    );
    assert_eq!(
        commit_of(&s.create_meta("base").unwrap()).as_deref(),
        Some("commit-AAA"),
        "and must not disturb the base it was swapped from"
    );
}

/// Resetting a target from the source it already came from is a normal operation and must still
/// leave the base with an unknown provenance rather than reviving the old commit.
#[test]
fn a_swap_from_the_same_source_still_clears_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("store");
    let b = CoreBackend::open(&store, cowfs_core::Options::default()).unwrap();
    let s = b.snapshots();
    seeded_base(&b, "srcA", "base", "commit-AAA");
    s.create("clone", Some("srcA")).unwrap();
    body(&b.snapshot("clone").unwrap(), b"BBBB-from-clone");

    s.swap("base", "clone").unwrap();
    assert_base_unknown("same-source reset", &s.create_meta("base").unwrap());
    assert_eq!(tree(&b.snapshot("base").unwrap()), "BBBB-from-clone");
    assert_eq!(
        commit_of(&s.create_meta("clone").unwrap()),
        None,
        "a clone that was never promoted has no record to lose"
    );
}
