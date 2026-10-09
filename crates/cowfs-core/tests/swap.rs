//! Snapshot replacement: failure injection at every step of the swap (F2) and the intent record's
//! recovery on the next open.
//!
//! The swap stages a fork, writes an intent file, removes the old target, then forks into the
//! target name. The states a failure may leave are:
//!
//! | fault at | before reopen | after reopen |
//! |---|---|---|
//! | none | base is the new content | same |
//! | 1 (staging fork) | base is the old content | same |
//! | 2 (intent file) | base is the old content | same |
//! | 3 (remove old) | base is the old content | same |
//! | 4 (final fork) | `base` is absent, intent file present | base is the new content |
//!
//! So the only window in which a name is missing is the one the intent record explains, and a
//! failed swap never destroys the old base.

mod common;

use common::*;
use cowfs_core::{ControlError, Core};
use cowfs_vfs::{Error, Vfs, ROOT_INO};

fn content(c: &Core, snap: &str, name: &str) -> String {
    let fs = c.snapshot_view(snap).expect("view");
    let a = fs.lookup(ROOT_INO, name.as_bytes()).expect("lookup");
    String::from_utf8(read_all(&fs, a.ino)).expect("utf8")
}

fn damage(dir: &std::path::Path) {
    let pack = one_pack(dir);
    let mut bytes = std::fs::read(&pack).unwrap();
    bytes[16 + 52 + 70_000] ^= 0x55;
    std::fs::write(&pack, &bytes).unwrap();
}

fn one_pack(dir: &std::path::Path) -> std::path::PathBuf {
    let mut v: Vec<_> = std::fs::read_dir(dir.join("store/packs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    v.sort();
    v.remove(0)
}

fn leftovers(dir: &std::path::Path, c: &Core) -> Vec<String> {
    let mut v: Vec<String> = c
        .list_snapshots()
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .filter(|n| n.contains("cowfs-swap"))
        .collect();
    v.extend(
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("swap-") || n.starts_with("tmp-swap-")),
    );
    v
}

/// F2: a failure at any step keeps the old base, and no step leaves a staging snapshot or an
/// intent file behind once the next open has run.
#[test]
fn promote_base_survives_a_failure_at_every_step() {
    for step in 1..=5u8 {
        let dir = tempfile::tempdir().unwrap();
        {
            let c = Core::open(dir.path(), test_opts()).unwrap();
            c.create_snapshot("base").unwrap();
            let rb = root_entry(&c, "base").ino;
            mkfile(&c, rb, "f", b"old base");
            c.create_snapshot("src").unwrap();
            let rs = root_entry(&c, "src").ino;
            mkfile(&c, rs, "f", b"new content");
            c.sync().unwrap();
            c.set_swap_fault(step);
            let res = c.promote_base("src", "base");
            if step <= 3 {
                assert!(res.is_err(), "step {step} was not injected: {res:?}");
                assert_eq!(
                    content(&c, "base", "f"),
                    "old base",
                    "step {step} lost the old base"
                );
            } else {
                // past the point of no return the swap is rolled forward and reported as done
                assert!(res.is_ok(), "step {step} must roll forward: {res:?}");
                assert_eq!(content(&c, "base", "f"), "new content", "step {step}");
            }
        }
        let c = Core::open(dir.path(), test_opts()).expect("reopen");
        let want = if step >= 4 { "new content" } else { "old base" };
        assert_eq!(content(&c, "base", "f"), want, "step {step} after reopen");
        assert!(
            leftovers(dir.path(), &c).is_empty(),
            "step {step}: {:?}",
            leftovers(dir.path(), &c)
        );
        assert!(
            c.snapshot_view("src").is_ok(),
            "step {step}: the source was lost"
        );
        c.check().unwrap();
    }
}

/// The critic's a3 repro, after F3: the write over the damage is EIO at once, so the queue never
/// carries it, and a promote of a snapshot with one damaged file succeeds (the damaged file keeps
/// its last committed content in the new snapshot).
#[test]
fn a_damaged_block_does_not_stop_a_promote() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("base").unwrap();
    let rb = root_entry(&c, "base").ino;
    mkfile(&c, rb, "f", b"old base");
    c.create_snapshot("src").unwrap();
    let rs = root_entry(&c, "src").ino;
    let big = mkfile(&c, rs, "big", &pattern(600_000, 7));
    let small = mkfile(&c, rs, "small", b"intact");
    c.sync().unwrap();
    c.drop_caches();
    damage(dir.path());
    assert!(matches!(
        c.write(big.ino, 70_000, b"over"),
        Err(Error::Corrupt(_))
    ));
    let e = c
        .promote_base("src", "base")
        .expect("the swap is not blocked by one damaged file");
    assert_eq!(e.name, "base");
    let r2 = root_entry(&c, "base").ino;
    assert_eq!(read_all(&c, c.lookup(r2, b"small").unwrap().ino), b"intact");
    assert!(matches!(
        c.read(c.lookup(r2, b"big").unwrap().ino, 60_000, 40_000),
        Err(Error::Corrupt(_))
    ));
    let _ = small;
    c.check().unwrap();
    drop(c);
    // the metadata is intact and the damaged chunk is still never served
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let r3 = root_entry(&c, "base").ino;
    assert_eq!(read_all(&c, c.lookup(r3, b"small").unwrap().ino), b"intact");
    c.check().unwrap();
}

/// F2: a rename that fails leaves both names as they were, and one that succeeds changes no
/// content.
#[test]
fn rename_snapshot_is_failure_safe() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("a").unwrap();
    let ra = root_entry(&c, "a").ino;
    mkfile(&c, ra, "f", b"payload");
    c.create_snapshot("taken").unwrap();
    c.sync().unwrap();
    assert_eq!(c.rename_snapshot("a", "taken"), Err(ControlError::Exists));
    assert_eq!(content(&c, "a", "f"), "payload");
    assert!(c.snapshot_view("taken").is_ok());
    let e = c.rename_snapshot("a", "b").unwrap();
    assert_eq!(e.name, "b");
    assert_eq!(content(&c, "b", "f"), "payload");
    assert_eq!(c.snapshot_view("a").err(), Some(ControlError::NotFound));
    assert!(matches!(
        c.promote_base("b", "b"),
        Err(ControlError::InvalidName(_))
    ));
    let mut names: Vec<String> = c
        .list_snapshots()
        .unwrap()
        .into_iter()
        .map(|x| x.name)
        .collect();
    names.sort();
    assert_eq!(names, ["b".to_string(), "taken".to_string()]);
    c.check().unwrap();
}

// ---- the replacing ingest (issues 176 and 177) -------------------------------------------------

use cowfs_core::{ingest_replacing, Hooks, ImportError};

/// A source directory holding one file `f`.
fn source(root: &std::path::Path, tag: &str, body: &str) -> std::path::PathBuf {
    let d = root.join(format!("src-{tag}"));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("f"), body).unwrap();
    d
}

fn replace(c: &Core, from: &std::path::Path) -> Result<(), ImportError> {
    let mut hooks = Hooks {
        progress: &mut |_, _| true,
    };
    ingest_replacing(c, from, "base", &mut hooks).map(|_| ())
}

/// A retry that fails or is cancelled after its own staging started.
fn replace_cancelled(c: &Core, from: &std::path::Path) -> Result<(), ImportError> {
    let mut hooks = Hooks {
        progress: &mut |_, _| false,
    };
    ingest_replacing(c, from, "base", &mut hooks).map(|_| ())
}

/// Every snapshot name the metadata holds, staging ones included, plus the swap intent files.
fn raw_leftovers(dir: &std::path::Path, c: &Core) -> Vec<String> {
    let mut v: Vec<String> = c
        .meta()
        .snapshots()
        .unwrap()
        .into_iter()
        .map(|i| i.name)
        .filter(|n| n.contains("cowfs-swap"))
        .collect();
    v.extend(
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("swap-") || n.starts_with("tmp-swap-")),
    );
    v
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &p);
        } else {
            std::fs::copy(e.path(), &p).unwrap();
        }
    }
}

/// F2 for the replacing ingest: a failure before the old target goes keeps it, one after leaves the
/// name missing only while the intent file explains it, and the next open finishes the swap.
#[test]
fn ingest_replacing_survives_a_failure_at_every_step() {
    for step in 2..=4u8 {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let v1 = source(scratch.path(), "v1", "old base");
        let v2 = source(scratch.path(), "v2", "new content");
        {
            let c = Core::open(dir.path(), test_opts()).unwrap();
            replace(&c, &v1).unwrap();
            c.set_swap_fault(step);
            let res = replace(&c, &v2);
            assert!(res.is_err(), "step {step} was not injected: {res:?}");
            if step <= 3 {
                assert_eq!(content(&c, "base", "f"), "old base", "step {step}");
                assert!(raw_leftovers(dir.path(), &c).is_empty(), "step {step}");
            } else {
                assert!(c.snapshot_view("base").is_err(), "step {step}");
            }
        }
        let c = Core::open(dir.path(), test_opts()).expect("reopen");
        let want = if step >= 4 { "new content" } else { "old base" };
        assert_eq!(content(&c, "base", "f"), want, "step {step} after reopen");
        assert!(
            raw_leftovers(dir.path(), &c).is_empty(),
            "step {step}: {:?}",
            raw_leftovers(dir.path(), &c)
        );
        c.check().unwrap();
    }
}

/// Issue 177: `finish_swap` failed after the old target was removed, so the staged tree and the
/// intent are all that is left of the new content. A retry of the same name that then fails must
/// not delete them: the pending intent is rolled forward before the retry starts.
#[test]
fn a_failed_same_name_retry_does_not_destroy_a_pending_swap() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let v1 = source(scratch.path(), "v1", "old base");
    let v2 = source(scratch.path(), "v2", "second");
    let v3 = source(scratch.path(), "v3", "third");
    let c = Core::open(dir.path(), test_opts()).unwrap();
    replace(&c, &v1).unwrap();
    c.set_swap_fault(4);
    assert!(replace(&c, &v2).is_err());
    c.set_swap_fault(0);
    assert!(replace_cancelled(&c, &v3).is_err());
    assert_eq!(
        content(&c, "base", "f"),
        "second",
        "the pending swap was lost"
    );
    assert!(
        raw_leftovers(dir.path(), &c).is_empty(),
        "{:?}",
        raw_leftovers(dir.path(), &c)
    );
    drop(c);
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert_eq!(content(&c, "base", "f"), "second", "after reopen");
    c.check().unwrap();
}

/// Issue 177, the promote side: the same pending intent met by `promote_base` of the same name.
#[test]
fn a_failed_promote_does_not_destroy_a_pending_swap() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let v1 = source(scratch.path(), "v1", "old base");
    let v2 = source(scratch.path(), "v2", "second");
    let c = Core::open(dir.path(), test_opts()).unwrap();
    replace(&c, &v1).unwrap();
    c.create_snapshot("src").unwrap();
    let rs = root_entry(&c, "src").ino;
    mkfile(&c, rs, "f", b"promoted");
    c.sync().unwrap();
    c.set_swap_fault(4);
    assert!(replace(&c, &v2).is_err());
    c.set_swap_fault(1);
    assert!(c.promote_base("src", "base").is_err());
    c.set_swap_fault(0);
    assert_eq!(
        content(&c, "base", "f"),
        "second",
        "the pending swap was lost"
    );
    assert!(raw_leftovers(dir.path(), &c).is_empty());
    c.check().unwrap();
}

/// Issue 177, the success path: a retry that works replaces the rolled-forward tree.
#[test]
fn a_same_name_retry_after_a_pending_swap_lands_the_new_content() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let v1 = source(scratch.path(), "v1", "old base");
    let v2 = source(scratch.path(), "v2", "second");
    let v3 = source(scratch.path(), "v3", "third");
    let c = Core::open(dir.path(), test_opts()).unwrap();
    replace(&c, &v1).unwrap();
    c.set_swap_fault(4);
    assert!(replace(&c, &v2).is_err());
    c.set_swap_fault(0);
    replace(&c, &v3).unwrap();
    assert_eq!(content(&c, "base", "f"), "third");
    assert!(raw_leftovers(dir.path(), &c).is_empty());
    c.check().unwrap();
}

/// Issue 176: a crash during the long staging write leaves a hidden staging snapshot and no intent.
/// The crash image is the store directory copied while the ingest is mid-write.
#[test]
fn open_removes_a_staging_snapshot_that_has_no_intent() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let image = scratch.path().join("image");
    let v1 = source(scratch.path(), "v1", "kept");
    std::fs::write(v1.join("g"), vec![7u8; 100_000]).unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    {
        let mut taken = false;
        let mut hooks = Hooks {
            progress: &mut |done, _| {
                if done > 0 && !taken {
                    taken = true;
                    copy_dir(dir.path(), &image);
                }
                true
            },
        };
        ingest_replacing(&c, &v1, "fresh", &mut hooks).unwrap();
    }
    drop(c);
    let c = Core::open(&image, test_opts()).unwrap();
    assert!(
        c.meta()
            .snapshots()
            .unwrap()
            .iter()
            .all(|i| !i.name.contains("cowfs-swap")),
        "an orphan staging snapshot survived open: {:?}",
        raw_leftovers(&image, &c)
    );
    assert!(c.snapshot_view("fresh").is_err());
    c.check().unwrap();
}

/// Issue 176, the other side: the sweep must not take what a pending intent needs, and a second
/// open is a no-op.
#[test]
fn open_recovers_a_pending_swap_and_a_second_open_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let v1 = source(scratch.path(), "v1", "old base");
    let v2 = source(scratch.path(), "v2", "second");
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        replace(&c, &v1).unwrap();
        c.set_swap_fault(4);
        assert!(replace(&c, &v2).is_err());
    }
    // open twice: the first finishes the swap, the sweep must not have touched what it needed
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert_eq!(content(&c, "base", "f"), "second");
    drop(c);
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert_eq!(content(&c, "base", "f"), "second");
    assert!(raw_leftovers(dir.path(), &c).is_empty());
}

/// Critic of PR 220: the intent file of a target named `base.tmp` is `swap-base.tmp`, which the
/// intent scan once mistook for the writer's temp file of `base`. Open then swept the staging
/// snapshot of a swap past its point of no return. Names that end like a temp file, or that equal
/// another target plus a suffix, must recover like any other.
#[test]
fn a_target_named_like_a_temp_file_is_rolled_forward() {
    for target in ["base.tmp", "tmp-swap-x", "swap-y"] {
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let v1 = source(scratch.path(), "v1", "old base");
        let v2 = source(scratch.path(), "v2", "new content");
        let run = |c: &Core, from: &std::path::Path| {
            let mut hooks = Hooks {
                progress: &mut |_, _| true,
            };
            ingest_replacing(c, from, target, &mut hooks).map(|_| ())
        };
        {
            let c = Core::open(dir.path(), test_opts()).unwrap();
            run(&c, &v1).unwrap();
            c.set_swap_fault(4);
            assert!(run(&c, &v2).is_err());
        }
        let c = Core::open(dir.path(), test_opts()).unwrap();
        assert_eq!(content(&c, target, "f"), "new content", "{target}");
        assert!(raw_leftovers(dir.path(), &c).is_empty(), "{target}");
        c.check().unwrap();
    }
}

/// An intent and the temp file of another swap never share a file name: `base` and `base.tmp`
/// pending together both recover.
#[test]
fn two_pending_swaps_whose_names_differ_by_a_suffix_both_recover() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let v1 = source(scratch.path(), "v1", "old");
    let v2 = source(scratch.path(), "v2", "new");
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        for t in ["base", "base.tmp"] {
            let mut h = Hooks {
                progress: &mut |_, _| true,
            };
            ingest_replacing(&c, &v1, t, &mut h).unwrap();
            c.set_swap_fault(4);
            let mut h = Hooks {
                progress: &mut |_, _| true,
            };
            assert!(ingest_replacing(&c, &v2, t, &mut h).is_err());
            c.set_swap_fault(0);
        }
    }
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert_eq!(content(&c, "base", "f"), "new");
    assert_eq!(content(&c, "base.tmp", "f"), "new");
    assert!(raw_leftovers(dir.path(), &c).is_empty());
}

/// A temp file the older release named `swap-<target>.tmp` is dropped on open, not read as the
/// intent of a target called `<target>.tmp`.
#[test]
fn an_older_temp_file_is_dropped_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let v1 = source(scratch.path(), "v1", "kept");
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        replace(&c, &v1).unwrap();
    }
    let stale = dir.path().join("swap-base.tmp");
    std::fs::write(&stale, "base.cowfs-swap0\nbase\n").unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert!(!stale.exists());
    assert_eq!(content(&c, "base", "f"), "kept");
    drop(c);
    // a crash before the rename leaves the new-style temp, which open removes too
    std::fs::write(dir.path().join("tmp-swap-base"), "x").unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert!(raw_leftovers(dir.path(), &c).is_empty());
    c.check().unwrap();
}

fn replace_as(c: &Core, from: &std::path::Path, target: &str) -> Result<(), ImportError> {
    let mut hooks = Hooks {
        progress: &mut |_, _| true,
    };
    ingest_replacing(c, from, target, &mut hooks).map(|_| ())
}

/// Round-2 critic: staging names kept 200 characters of the target, so these two valid names shared
/// one staging snapshot. A pending swap of A was destroyed by a plain ingest of B.
#[test]
fn long_targets_sharing_a_prefix_do_not_share_a_staging_snapshot() {
    let a = format!("{}1", "a".repeat(200));
    let b = format!("{}2", "a".repeat(200));
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let v1 = source(scratch.path(), "v1", "old");
    let va = source(scratch.path(), "va", "content of A");
    let vb = source(scratch.path(), "vb", "content of B");
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        replace_as(&c, &v1, &a).unwrap();
        c.set_swap_fault(4);
        assert!(replace_as(&c, &va, &a).is_err());
        c.set_swap_fault(0);
        // a plain ingest of B must leave A's pending swap alone
        replace_as(&c, &vb, &b).unwrap();
    }
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert_eq!(content(&c, &a, "f"), "content of A");
    assert_eq!(content(&c, &b, "f"), "content of B");
    assert!(raw_leftovers(dir.path(), &c).is_empty());
}

#[test]
fn two_long_pending_swaps_with_a_shared_prefix_both_recover() {
    let a = format!("{}1", "a".repeat(200));
    let b = format!("{}2", "a".repeat(200));
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let v1 = source(scratch.path(), "v1", "old");
    let va = source(scratch.path(), "va", "content of A");
    let vb = source(scratch.path(), "vb", "content of B");
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        for t in [&a, &b] {
            replace_as(&c, &v1, t).unwrap();
        }
        c.set_swap_fault(4);
        assert!(replace_as(&c, &va, &a).is_err());
        assert!(replace_as(&c, &vb, &b).is_err());
    }
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert_eq!(content(&c, &a, "f"), "content of A");
    assert_eq!(content(&c, &b, "f"), "content of B");
}

/// A record cut off inside the target name must not be read as an intent for a shorter name.
/// The old target was already removed, so the staged tree is the only copy: it is kept and rolled
/// forward under the name the intent file carries (N2 of the PR 220 round 3 review).
#[test]
fn a_torn_intent_is_not_read_as_a_shorter_name() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let v1 = source(scratch.path(), "v1", "old");
    let v2 = source(scratch.path(), "v2", "new");
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        replace_as(&c, &v1, "abc").unwrap();
        c.set_swap_fault(4);
        assert!(replace_as(&c, &v2, "abc").is_err());
    }
    let p = dir.path().join("swap-abc");
    let text = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, text.trim_end_matches(['\n', 'c'])).unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert!(c.snapshot_view("ab").is_err(), "a stray snapshot ab");
    assert_eq!(content(&c, "abc", "f"), "new", "the only copy is kept");
    assert!(raw_leftovers(dir.path(), &c).is_empty());
}

/// A torn intent whose target still exists names a swap that had not removed it: the staging tree
/// is garbage and the old target is untouched.
#[test]
fn a_torn_intent_with_the_target_still_present_keeps_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let v1 = source(scratch.path(), "v1", "old");
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        replace_as(&c, &v1, "abc").unwrap();
    }
    std::fs::write(dir.path().join("swap-abc"), "xyz").unwrap(); // torn: no final newline
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert_eq!(content(&c, "abc", "f"), "old");
    assert!(raw_leftovers(dir.path(), &c).is_empty());
}

fn max_id(c: &Core) -> u64 {
    c.list_snapshots()
        .unwrap()
        .iter()
        .map(|e| e.id)
        .max()
        .unwrap()
}

/// Issue 42 (a): a promotion forks once. The staged snapshot is renamed into the target name and
/// keeps its id, so the call consumes exactly one snapshot id, with or without a victim.
#[test]
fn promote_base_forks_once_and_keeps_the_staged_id() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("base").unwrap();
    let rb = root_entry(&c, "base").ino;
    mkfile(&c, rb, "f", b"old base");
    c.create_snapshot("src").unwrap();
    let rs = root_entry(&c, "src").ino;
    mkfile(&c, rs, "f", b"new content");
    c.sync().unwrap();
    for (target, tag) in [("base", "replacing"), ("fresh", "no victim")] {
        let before = max_id(&c);
        let e = c.promote_base("src", target).unwrap();
        assert_eq!(e.id, before + 1, "{tag}: one fork, one id");
        assert_eq!(max_id(&c), before + 1, "{tag}: no second id was spent");
        assert_eq!(content(&c, target, "f"), "new content", "{tag}");
    }
    c.check().unwrap();
    drop(c);
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert_eq!(content(&c, "base", "f"), "new content");
    assert!(leftovers(dir.path(), &c).is_empty());
    c.check().unwrap();
}

/// A crash between the rename into the target name and the removal of the intent file leaves the
/// target in place, no staging snapshot and a stale intent; the next open drops the intent and
/// keeps the target.
#[test]
fn a_stale_intent_after_the_rename_is_dropped_on_open() {
    let dir = tempfile::tempdir().unwrap();
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.create_snapshot("base").unwrap();
        c.create_snapshot("src").unwrap();
        let rs = root_entry(&c, "src").ino;
        mkfile(&c, rs, "f", b"new content");
        c.sync().unwrap();
        c.promote_base("src", "base").unwrap();
    }
    // the intent the finished swap removed, put back as a crash before that removal would leave it
    let staged = format!("base~0000000000000000{}0", cowfs_snapname::RESERVED);
    std::fs::write(dir.path().join("swap-base"), format!("{staged}\nbase\n")).unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert_eq!(content(&c, "base", "f"), "new content");
    assert!(leftovers(dir.path(), &c).is_empty());
    c.check().unwrap();
}
