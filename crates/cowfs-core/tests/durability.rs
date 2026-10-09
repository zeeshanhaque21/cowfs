//! Durability orderings and node-load contention: the three behaviours the surviving mutants
//! describe. Every test here fails with its mutant and passes without it.
//!
//! `cowfs_core::fsops` is a test-only seam: it records the `sync` calls the swap and the
//! swap makes, and can make one of them fail. Counter-only reservations do not run the store hook.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use cowfs_core::fsops::{self, Fault};
use cowfs_core::{Core, Options};
use cowfs_vfs::{Vfs, ROOT_INO};

/// The seam is process-wide, so only one of these tests may hold it at a time.
fn seam() -> std::sync::MutexGuard<'static, ()> {
    static L: std::sync::Mutex<()> = std::sync::Mutex::new(());
    L.lock().unwrap_or_else(|e| e.into_inner())
}

fn opts_tiny_reservation() -> Options {
    Options {
        background: false,
        ..Options::default()
    }
}

// ---------------------------------------------------------------- n02: the swap intent file

/// n02: the intent record must be on the medium before anything reads it. The victim snapshot is
/// removed only after the intent file and the directory entry that names it are both durable.
#[test]
fn the_intent_file_is_durable_before_the_victim_snapshot_is_removed() {
    let _seam = seam();
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), opts_tiny_reservation()).unwrap();
    c.create_snapshot("src").unwrap();
    c.create_snapshot("base").unwrap();
    mkfile(&c.snapshot_view("src").unwrap(), ROOT_INO, "f", b"NEW");
    mkfile(&c.snapshot_view("base").unwrap(), ROOT_INO, "f", b"OLD");
    c.sync().unwrap();

    fsops::arm();
    let r = c.promote_base("src", "base");
    let trace = fsops::trace_take();
    fsops::disarm();
    println!("promote -> {r:?}\ntrace: {trace:?}");
    r.unwrap();

    let pos = |what: &str| {
        trace
            .iter()
            .position(|e| e == what)
            .unwrap_or_else(|| panic!("{what} missing from {trace:?}"))
    };
    assert!(
        pos("sync_file:tmp-swap-base") < pos("intent_renamed"),
        "the intent record was renamed into place before its bytes were durable: {trace:?}"
    );
    let dir_sync = |e: &String| e.starts_with("sync_dir:");
    let renamed = pos("intent_renamed");
    assert!(
        trace[renamed + 1..].iter().any(dir_sync),
        "no directory sync after the intent rename: {trace:?}"
    );
    let victim = pos("victim_removed");
    assert!(
        trace[..victim].iter().any(dir_sync),
        "the victim snapshot was removed before the intent directory entry was durable: {trace:?}"
    );
}

/// n02 again, from the other side: if the intent file cannot be made durable the swap must refuse,
/// because a crash right after would find a removal with no record of it.
#[test]
fn a_swap_refuses_when_the_intent_file_cannot_be_made_durable() {
    let _seam = seam();
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), opts_tiny_reservation()).unwrap();
    c.create_snapshot("src").unwrap();
    c.create_snapshot("base").unwrap();
    mkfile(&c.snapshot_view("src").unwrap(), ROOT_INO, "f", b"NEW");
    mkfile(&c.snapshot_view("base").unwrap(), ROOT_INO, "f", b"OLD");
    c.sync().unwrap();

    fsops::arm();
    fsops::set_fault(Fault::FileSync, "tmp-swap-base", 1);
    let r = c.promote_base("src", "base");
    fsops::disarm();
    println!("promote with a failing intent sync -> {r:?}");
    assert!(
        r.is_err(),
        "the swap went ahead without a durable intent record"
    );
    let mut names: Vec<String> = c
        .list_snapshots()
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["base", "src"],
        "a refused swap changed the mount: {names:?}"
    );
    assert!(
        !dir.path().join("swap-base").exists(),
        "an unreadable intent was kept"
    );
}

#[test]
fn a_physical_reservation_is_durable_before_its_number_is_handed_out() {
    use std::sync::atomic::AtomicUsize;

    let dir = tempfile::tempdir().unwrap();
    let syncs = Arc::new(AtomicUsize::new(0));
    let observed = syncs.clone();
    let c = Core::open_with_meta(dir.path(), opts_tiny_reservation(), move |d, mut o| {
        let store_sync = o.before_sync.take().unwrap();
        o.before_sync = Some(Arc::new(move || {
            store_sync()?;
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }));
        cowfs_meta::Meta::open(d.join("meta.redb"), o)
    })
    .unwrap();
    c.create_snapshot("s").unwrap();
    let fs = c.snapshot_view("s").unwrap();
    let before = syncs.load(Ordering::SeqCst);
    let a = fs.create(ROOT_INO, b"f", 0o644).unwrap();
    assert_eq!(a.ino & (1 << 63), 0);
    assert_eq!(syncs.load(Ordering::SeqCst), before);
    let meta_ino = c.meta_inode(a.ino).unwrap();
    let floor = c.meta().health().ino_floor;
    assert!(floor > meta_ino);
    fs.write(a.ino, 0, b"durable physical identity").unwrap();
    c.sync().unwrap();
    assert!(syncs.load(Ordering::SeqCst) > before);
    drop(fs);
    drop(c);
    let c = Core::open(dir.path(), opts_tiny_reservation()).unwrap();
    let fs = c.snapshot_view("s").unwrap();
    let reopened = fs.lookup(ROOT_INO, b"f").unwrap();
    assert_eq!(reopened.ino, a.ino);
    assert_eq!(
        fs.read(a.ino, 0, 100).unwrap(),
        b"durable physical identity"
    );
    assert!(c.meta().health().ino_floor >= floor);
    assert_ne!(fs.create(ROOT_INO, b"g", 0o644).unwrap().ino, a.ino);
}

#[test]
fn a_reserved_create_keeps_its_identity_when_the_store_sync_fails_and_retries() {
    let dir = tempfile::tempdir().unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let flag = armed.clone();
    let c = Core::open_with_meta(dir.path(), opts_tiny_reservation(), move |d, mut o| {
        let store_sync = o.before_sync.take().unwrap();
        o.before_sync = Some(Arc::new(move || {
            if flag.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("store sync refused"));
            }
            store_sync()
        }));
        cowfs_meta::Meta::open(d.join("meta.redb"), o)
    })
    .unwrap();
    c.create_snapshot("s").unwrap();
    let fs = c.snapshot_view("s").unwrap();
    armed.store(true, Ordering::SeqCst);
    let a = fs.create(ROOT_INO, b"g", 0o644).unwrap();
    assert_eq!(a.ino & (1 << 63), 0);
    assert!(c.meta().health().ino_floor > c.meta_inode(a.ino).unwrap());
    fs.write(a.ino, 0, b"retry survived").unwrap();
    let r = c.sync();
    armed.store(false, Ordering::SeqCst);
    assert!(
        r.is_err(),
        "sync acknowledged despite the refused store hook"
    );
    assert_eq!(fs.lookup(ROOT_INO, b"g").unwrap().ino, a.ino);
    assert_eq!(fs.read(a.ino, 0, 100).unwrap(), b"retry survived");
    c.sync().unwrap();
    drop(fs);
    drop(c);
    let c = Core::open(dir.path(), opts_tiny_reservation()).unwrap();
    let fs = c.snapshot_view("s").unwrap();
    assert_eq!(fs.lookup(ROOT_INO, b"g").unwrap().ino, a.ino);
    assert_eq!(fs.read(a.ino, 0, 100).unwrap(), b"retry survived");
}

#[test]
fn a_physical_reservation_refuses_when_the_metadata_session_is_closed() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), opts_tiny_reservation()).unwrap();
    c.create_snapshot("s").unwrap();
    let fs = c.snapshot_view("s").unwrap();
    c.meta().close().unwrap();
    assert!(fs.create(ROOT_INO, b"g", 0o644).is_err());
    assert!(matches!(
        fs.lookup(ROOT_INO, b"g"),
        Err(cowfs_vfs::Error::NotFound)
    ));
}

// ---------------------------------------------------------------- b11: the node load retry

/// b11: `load_node` builds a node's state from meta. After its retry budget it must fail closed
/// rather than insert that state, because the table may hold a live node whose unflushed extents a
/// state read from meta does not have.
///
/// The seam arms the contention deterministically, so the budget is exhausted without a real race.
#[test]
fn a_node_load_that_exhausts_its_retry_budget_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.create_snapshot("s").unwrap();
        let fs = c.snapshot_view("s").unwrap();
        let a = fs.create(ROOT_INO, b"f", 0o644).unwrap().ino;
        fs.write(a, 0, b"committed").unwrap();
        c.sync().unwrap();
    }
    // a reopened core has an empty node table, so the first lookup builds a node from meta
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let fs = c.snapshot_view("s").unwrap();
    c.set_load_node_contention(0, 100);
    let r = fs.lookup(ROOT_INO, b"f");
    c.set_load_node_contention(0, 0);
    assert!(
        matches!(r, Err(cowfs_vfs::Error::Stale)),
        "a node load that could not win the table inserted a node built from meta instead of failing: {r:?}"
    );
}

// ---------------------------------------------------------------- b07: no lock across the meta commit

/// b07: `unregister` marks the snapshot removed and then waits for meta's writer lock, which another
/// commit can hold for as long as its store sync takes. It must not still be holding that snapshot's
/// namespace and flush locks across that wait: every other operation on the snapshot is then stuck for
/// the whole foreign store sync.
///
/// The slow sync hook is what makes the wait long enough to look at, and the probe keeps the handle on
/// the locks after the snapshot leaves the table.
#[test]
fn removing_a_snapshot_releases_its_locks_before_its_metadata_commit() {
    let dir = tempfile::tempdir().unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let hook_armed = armed.clone();
    let c = Core::open_with_meta(dir.path(), test_opts(), move |d, mut o| {
        let store_sync = o.before_sync.take().expect("the store sync hook");
        o.before_sync = Some(Arc::new(move || {
            if hook_armed.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1500));
            }
            store_sync()
        }));
        cowfs_meta::Meta::open(d.join("meta.redb"), o)
    })
    .unwrap();
    c.create_snapshot("s").unwrap();
    c.create_snapshot("other").unwrap();
    mkfile(
        &c.snapshot_view("other").unwrap(),
        ROOT_INO,
        "f",
        b"payload",
    );
    c.sync().unwrap();
    let probe = c.snapshot_lock_probe("s").expect("the snapshot");
    armed.store(true, Ordering::SeqCst);

    let remover = c.clone();
    let done = Arc::new(AtomicBool::new(false));
    let finished = done.clone();
    std::thread::spawn(move || {
        let _ = remover.remove_snapshot("s");
        finished.store(true, Ordering::SeqCst);
    });

    let start = Instant::now();
    let mut blocked_while_committing = false;
    while start.elapsed() < Duration::from_secs(20) && !done.load(Ordering::SeqCst) {
        if !probe.free() {
            blocked_while_committing = true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    armed.store(false, Ordering::SeqCst);
    println!("locks held at some point during the remove: {blocked_while_committing}");
    assert!(
        !blocked_while_committing,
        "the snapshot's namespace and flush locks were still held while its metadata commit waited"
    );
}
