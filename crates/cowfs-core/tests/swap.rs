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
            .filter(|n| n.starts_with("swap-")),
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
