//! Metadata health and recovery accounting (issue #40).
//!
//! Three claims, each proved against a real on-disk fixture rather than a model store:
//!
//! 1. A panic in the `before_sync` hook must not kill the background timer thread. Before the fix
//!    it did: idle changes were never flushed again and nothing reported why.
//! 2. A rollback through `open_recover` must move the inode and snapshot counters past everything
//!    the lost commit handed out, so no number is ever handed out twice.
//! 3. A recovery that fails must say so and leave the file byte for byte as it found it.
//!    `check()` passing says nothing about any of this, so each is asserted on its own.
//!
//! Every store here is a private `tempfile` fixture, built and closed in-process: no daemon, no
//! live store, no mount and no signal, so there is no PID to verify and nothing to kill. Damage is
//! applied only after the store is closed, its bytes hashed, and a pristine copy kept beside it.
//! Each test asserts the pristine copy's hash is unchanged at the end.

use cowfs_meta::{Ack, Error, Meta, Options, SyncHook, RECOVERY_FAILED, ROOT_INO};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PAGE: usize = 4096;

/// Same knobs everywhere: a small inode block so the counter floors have to move more than once,
/// and no background thread except where a test is about the background thread.
fn opts() -> Options {
    Options {
        node_size: 512,
        sync_every_ops: 1,
        ino_block: 4,
        background: false,
        ..Options::default()
    }
}

/// Bounded wait. A dead timer thread looks exactly like a slow one, so the deadline is the only
/// thing that tells them apart.
fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    let limit = Duration::from_secs(20);
    while start.elapsed() < limit {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out after {limit:?} waiting for {what}");
}

fn sha(b: &[u8]) -> [u64; 2] {
    let mut h = [0xcbf2_9ce4_8422_2325u64, 0x9e37_79b9_7f4a_7c15u64];
    for &x in b {
        h[0] ^= u64::from(x);
        h[0] = h[0].wrapping_mul(0x0000_0100_0000_01b3);
        h[1] = (h[1] ^ h[0])
            .rotate_left(27)
            .wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
    h
}

fn sha_file(p: &Path) -> [u64; 2] {
    sha(&std::fs::read(p).unwrap())
}

/// The root the file records for its first snapshot. A durable commit of a file change moves it,
/// which is how a test tells "the background timer ran" from "nothing happened". Counting
/// snapshots would not: creating a file does not add one.
fn durable_root(m: &Meta) -> [u8; 32] {
    *m.durable_snapshots().unwrap()[0].root.as_bytes()
}

/// A built, closed store plus every number it had already handed out.
struct Fixture {
    path: PathBuf,
    pristine: PathBuf,
    scratch: PathBuf,
    /// The last recorded health, re-sampled when a second round adds writes.
    before: cowfs_meta::Health,
    inodes: Vec<u64>,
    snapshots: Vec<u64>,
}

/// Builds a store, records every inode and snapshot number it hands out, closes it, and keeps a
/// pristine copy. Nothing is damaged here.
///
/// The final commit is one large batch, which is what makes a rollback reachable at all.
/// redb keeps two commit slots and shares every page the newer commit did not rewrite, so when the
/// last commit is a single small write almost every page belongs to both slots: damaging one
/// breaks the previous commit too and no rollback is possible. Measured on a 78-page fixture
/// built with a one-write final commit, all 155 single- and two-page candidates gave either
/// "still opens" or "unrepairable" and none gave a rollback. A final batch of `TAIL` files gives
/// the newest slot pages of its own, and then a rollback is reachable.
fn build(path: &Path, files: u32, snapshots: &[&str]) -> Fixture {
    let m = Meta::open(path, opts()).unwrap();
    let (mut inodes, mut snaps) = (Vec::new(), Vec::new());
    for name in snapshots {
        let s = m.new_snapshot(name).unwrap();
        snaps.push(s.id().0);
        for i in 0..files {
            let f = s
                .create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)
                .unwrap();
            inodes.push(f.ino.0);
            m.sync().unwrap();
        }
    }
    let last = m.snapshot(snapshots.last().unwrap()).unwrap();
    inodes.extend(write_tail_batch(&last, 0));
    m.sync().unwrap();
    // Sampled after the last sync and before any close: the snapshot being taken is the newest
    // commit, and a rollback has to lose that one.
    let before = m.health();
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let sibling = |suffix: &str| path.with_file_name(format!("{name}.{suffix}"));
    let pristine = sibling("pristine");
    let work = sibling("work");
    std::fs::copy(path, &pristine).unwrap();
    std::fs::copy(path, &work).unwrap();
    // Damage only the copy. Dropping `m` commits once more, which would otherwise put a small
    // newest commit on top of the batch and make every page shared again.
    drop(m);
    Fixture {
        path: work,
        pristine,
        scratch: sibling("scratch"),
        before,
        inodes,
        snapshots: snaps,
    }
}

/// How many files the final batch writes.
const TAIL: u32 = 400;

/// One batch of `TAIL` files, synced by the caller, returning the inode numbers it handed out.
fn write_tail_batch(s: &cowfs_meta::Snapshot, tag: u32) -> Vec<u64> {
    let mut inodes = Vec::new();
    s.batch(|tx| {
        for i in 0..TAIL {
            let name = format!("t{tag}-{i}");
            inodes.push(tx.create(ROOT_INO, name.as_bytes(), 0o644)?.ino.0);
        }
        Ok(())
    })
    .unwrap();
    inodes
}

/// What one damaged page did to the file.
#[derive(PartialEq, Eq, Debug)]
enum Verdict {
    /// The file still opens, so nothing was lost.
    Opened,
    /// `Meta::open` refused it and `open_recover` rolled it back to an earlier commit.
    RolledBack,
    /// `Meta::open` refused it and no earlier commit could be used.
    Unrepairable,
}

/// Damages `pages` consecutive pages and reports what that did. Never touches the fixture, only
/// `scratch`.
fn probe_page(fx: &Fixture, full: &[u8], page: usize, pages: usize) -> Verdict {
    let mut img = full.to_vec();
    for b in &mut img[page * PAGE..(page + pages) * PAGE] {
        *b = 0xA5;
    }
    std::fs::write(&fx.scratch, &img).unwrap();
    let refused = match Meta::open(&fx.scratch, opts()) {
        Ok(_) => return Verdict::Opened,
        Err(e) => e,
    };
    assert!(
        matches!(refused, Error::Corrupt(_)),
        "page {page}+{pages}: damage must read as corruption, got {refused}"
    );
    match Meta::open_recover(&fx.scratch, opts()) {
        Ok((_, r)) if r.rolled_back => Verdict::RolledBack,
        // `Format` means the magic was destroyed, so recovery refused to start rather than
        // reporting a failed repair. That is a different case and not what these tests want.
        Ok(_) => Verdict::Opened,
        Err(_) => Verdict::Unrepairable,
    }
}

fn clean_scratch(fx: &Fixture) {
    let _ = std::fs::remove_file(&fx.scratch);
    let _ = std::fs::remove_file(fx.path.with_file_name(format!(
        "{}.scratch.pre-recover",
        fx.path.file_name().unwrap().to_string_lossy()
    )));
}

/// Finds the first damage, in a fixed order, that leaves the fixture in `want`, and applies it.
///
/// The search runs entirely on scratch copies, and the fixture is written once with damage already
/// known to reach `want`. A redb layout change therefore fails the test loudly rather than
/// quietly testing nothing. `windows` are tried narrowest first, then widest, so a single page is
/// preferred over a pair when either works.
fn damage(fx: &Fixture, want: Verdict) {
    let full = std::fs::read(&fx.path).unwrap();
    let pages_total = full.len() / PAGE;
    let mut tried = Vec::new();
    for pages in [1usize, 2] {
        if pages_total <= pages {
            break;
        }
        for page in 1..=pages_total - pages {
            let v = probe_page(fx, &full, page, pages);
            if v == want {
                let mut img = full.clone();
                for b in &mut img[page * PAGE..(page + pages) * PAGE] {
                    *b = 0xA5;
                }
                std::fs::write(&fx.path, &img).unwrap();
                clean_scratch(fx);
                assert!(
                    Meta::open(&fx.path, opts()).is_err(),
                    "precondition: the damaged fixture must fail closed"
                );
                return;
            }
            tried.push((page, pages, v));
        }
    }
    clean_scratch(fx);
    panic!("no {pages_total}-page fixture yielded {want:?}; tried {tried:?}");
}

/// Damages the fixture so exactly the newest commit is lost.
fn damage_newest_commit(fx: &Fixture) {
    damage(fx, Verdict::RolledBack);
}

/// Damages the fixture so no rollback can work: the newest commit is unreadable and the earlier
/// commit is unreachable too.
fn damage_unrepairable(fx: &Fixture) {
    damage(fx, Verdict::Unrepairable);
}

/// Asserts the untouched reference copy is byte-identical, so a test that damaged the wrong file
/// fails instead of passing quietly.
fn assert_pristine_intact(fx: &Fixture, hash: [u64; 2]) {
    assert_eq!(
        sha_file(&fx.pristine),
        hash,
        "the pristine copy was modified"
    );
    clean_scratch(fx);
}

/// M1: a panicking sync hook used to unwind out of the background loop. The thread died, so no
/// idle change was ever flushed again and the caller was never told.
#[test]
fn background_flush_survives_a_panicking_sync_hook() {
    let dir = tempfile::tempdir().unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicU64::new(0));
    let hook = {
        let armed = armed.clone();
        let calls = calls.clone();
        Arc::new(move || -> io::Result<()> {
            calls.fetch_add(1, SeqCst);
            if armed.swap(false, SeqCst) {
                panic!("sync hook exploded");
            }
            Ok(())
        }) as SyncHook
    };
    let m = Meta::open(
        dir.path().join("hook.redb"),
        Options {
            // Only the background timer may flush, so a moved durable root proves it ran.
            sync_every_ops: u32::MAX,
            sync_interval: Duration::from_millis(30),
            background: true,
            before_sync: Some(hook),
            ..opts()
        },
    )
    .unwrap();

    let s = m.new_snapshot("s0").unwrap();
    let root0 = durable_root(&m);
    armed.store(true, SeqCst);
    s.create(ROOT_INO, b"after-the-panic", 0o644).unwrap();
    assert_eq!(
        durable_root(&m),
        root0,
        "the change must not be durable before the timer runs, or the timer was never needed"
    );

    // The panic is caught, the timer re-armed, and the retry makes the change durable.
    wait_until("the background commit after the panicking hook", || {
        durable_root(&m) != root0
    });

    let h = m.health();
    assert!(
        h.background_panics >= 1,
        "the panicking hook was not recorded: {h:?}"
    );
    assert!(
        h.flush_failures >= 1,
        "a durable commit that panicked is not a success: {h:?}"
    );
    assert!(
        h.last_flush_error
            .as_deref()
            .is_some_and(|e| e.contains("panicked")),
        "the reason a background flush failed was not reported: {h:?}"
    );
    assert!(
        calls.load(SeqCst) >= 3,
        "the timer did not retry: the hook ran {} times",
        calls.load(SeqCst)
    );
    // The retry succeeded, so the failure run is cleared, but the reason must stay visible.
    assert_eq!(h.consecutive_flush_failures, 0, "{h:?}");
    assert!(
        m.health().last_flush_error.is_some(),
        "a later success hid the reason a background flush failed"
    );

    // The thread is still alive, which is the whole point: the store keeps taking writes.
    m.check()
        .expect("check after the recovered background flush");
    let root1 = durable_root(&m);
    s.create(ROOT_INO, b"still-working", 0o644).unwrap();
    m.sync().unwrap();
    assert_ne!(
        durable_root(&m),
        root1,
        "the store stopped accepting writes"
    );
}

/// The do-nothing control for the test above: same store, no faulty hook, so every counter is zero
/// and there is no reason to report.
#[test]
fn a_store_that_never_failed_reports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let m = Meta::open(
        dir.path().join("clean.redb"),
        Options {
            sync_every_ops: u32::MAX,
            sync_interval: Duration::from_millis(20),
            background: true,
            before_sync: Some(Arc::new(|| Ok(())) as SyncHook),
            ..opts()
        },
    )
    .unwrap();
    let s = m.new_snapshot("s0").unwrap();
    let root0 = durable_root(&m);
    s.create(ROOT_INO, b"f", 0o644).unwrap();
    wait_until("the idle change to be flushed", || {
        durable_root(&m) != root0
    });

    let h = m.health();
    assert_eq!(
        (
            h.last_flush_error.is_none(),
            h.flush_failures,
            h.consecutive_flush_failures,
            h.background_panics,
            h.poisoned,
            h.recoveries
        ),
        (true, 0, 0, 0, false, 0),
        "a healthy store must report zeros, got {h:?}"
    );
}

/// M1, second half: repeated failing flushes under `Ack::Applied` were only visible through private
/// session state, so a caller had no way to see a growing backlog before it was refused.
#[test]
fn repeated_flush_failures_are_counted_reported_and_eventually_refused() {
    let dir = tempfile::tempdir().unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let hook = {
        let armed = armed.clone();
        Arc::new(move || -> io::Result<()> {
            if armed.load(SeqCst) {
                Err(io::Error::other("disk is not flushing"))
            } else {
                Ok(())
            }
        }) as SyncHook
    };
    let m = Meta::open(
        dir.path().join("failing.redb"),
        Options {
            sync_every_ops: 1,
            ack: Ack::Applied,
            before_sync: Some(hook),
            ..opts()
        },
    )
    .unwrap();
    let s = m.new_snapshot("s0").unwrap();
    armed.store(true, SeqCst);

    let mut refused = None;
    let mut applied = 0u32;
    for i in 0..64u32 {
        match s.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644) {
            Ok(_) => applied += 1,
            Err(e) => {
                refused = Some(e.to_string());
                break;
            }
        }
    }
    let why = refused.expect("a store that cannot flush must stop accepting changes");
    assert!(
        why.contains("durable commits keep failing"),
        "the refusal did not explain itself: {why}"
    );
    assert_eq!(
        applied, 9,
        "with sync_every_ops 1 the backlog cap is 8 pending operations, so 9 may be applied"
    );

    let h = m.health();
    assert!(h.flush_failures >= 8, "failures were not counted: {h:?}");
    assert_eq!(
        h.consecutive_flush_failures, h.flush_failures,
        "nothing succeeded, so the run must equal the total: {h:?}"
    );
    assert!(
        h.last_flush_error
            .as_deref()
            .is_some_and(|e| e.contains("disk is not flushing")),
        "the hook's own reason was not reported: {h:?}"
    );
    assert_eq!(
        h.background_panics, 0,
        "the hook returned errors, it did not panic: {h:?}"
    );
}

/// M3: a rollback returns the counters to the previous commit, so numbers the lost commit had
/// already handed out would come back. They must not.
#[test]
fn a_rollback_never_hands_out_a_number_it_already_handed_out() {
    let dir = tempfile::tempdir().unwrap();
    let fx = build(&dir.path().join("rollback.redb"), 24, &["s0", "s1"]);
    let pristine_hash = sha_file(&fx.pristine);
    let max_ino = *fx.inodes.iter().max().unwrap();
    let max_snap = *fx.snapshots.iter().max().unwrap();
    assert!(
        fx.before.ino_floor > max_ino,
        "fixture is wrong: the floor must already exceed every number handed out"
    );

    damage_newest_commit(&fx);
    let (m, rec) = Meta::open_recover(&fx.path, opts()).unwrap();
    assert!(
        rec.rolled_back,
        "the damaged file was not rolled back: {rec:?}"
    );
    assert_eq!(rec.recoveries, 1, "the rollback was not counted: {rec:?}");
    assert!(
        rec.backup.as_deref().is_some_and(|b| b.exists()),
        "the pre-recovery copy is gone: {rec:?}"
    );

    let ino_floor = rec.ino_floor.expect("a rollback must move the inode floor");
    let snap_floor = rec
        .snapshot_floor
        .expect("a rollback must move the snapshot floor");
    assert!(
        ino_floor > max_ino,
        "the new inode floor {ino_floor} does not clear {max_ino}, the highest number already \
         handed out"
    );
    assert!(
        snap_floor > max_snap,
        "the new snapshot floor {snap_floor} does not clear {max_snap}"
    );

    let h = m.health();
    assert_eq!(h.recoveries, 1, "{h:?}");
    assert_eq!(h.ino_floor, ino_floor, "{h:?}");
    assert_eq!(h.snapshot_floor, snap_floor, "{h:?}");
    assert_eq!(
        h.last_flush_error, None,
        "a rollback is not a flush failure: {h:?}"
    );

    // The real test of a floor: keep using the recovered store.
    let s = m.new_snapshot("s2").unwrap();
    let mut fresh = Vec::new();
    for i in 0..40u32 {
        let f = s
            .create(ROOT_INO, format!("g{i}").as_bytes(), 0o644)
            .unwrap();
        fresh.push(f.ino.0);
    }
    m.sync().unwrap();
    let reused: Vec<u64> = fresh
        .iter()
        .copied()
        .filter(|n| fx.inodes.contains(n))
        .collect();
    assert!(
        reused.is_empty(),
        "the recovered store re-handed out {} inode number(s) it had already handed out: {reused:?}",
        reused.len()
    );
    assert!(
        s.id().0 > max_snap,
        "snapshot id {} was already handed out before the crash",
        s.id().0
    );
    m.check()
        .expect("check after the recovered store took new writes");

    // The count and the floors are durable: a plain reopen, not a recovery, still reports them.
    // The snapshot handle must go too, or the file is still locked and redb refuses the reopen.
    m.close().unwrap();
    drop(s);
    drop(m);
    let again = Meta::open(&fx.path, opts()).unwrap();
    let h = again.health();
    assert_eq!(h.recoveries, 1, "the count did not survive a reopen: {h:?}");
    // Monotonic, not equal: the 40 writes above advanced the floor further, and `close` lowered it
    // only to the next unused number. What must never happen is it going backwards, which would
    // mean the recovery record was lost and a number could be reused.
    assert!(
        h.ino_floor >= ino_floor,
        "the inode floor went backwards across a reopen: {ino_floor} then {}",
        h.ino_floor
    );
    assert!(
        h.snapshot_floor >= snap_floor,
        "the snapshot floor went backwards across a reopen: {snap_floor} then {}",
        h.snapshot_floor
    );
    assert!(
        h.ino_floor > max_ino,
        "the reopened floor {} does not clear {max_ino}",
        h.ino_floor
    );
    again
        .check()
        .expect("check after a plain reopen of a recovered store");

    assert_pristine_intact(&fx, pristine_hash);
}

/// The do-nothing control for the test above: nothing was lost, so nothing is counted and no floor
/// moves.
#[test]
fn opening_a_healthy_file_reports_no_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let fx = build(&dir.path().join("healthy.redb"), 8, &["s0"]);
    let (m, rec) = Meta::open_recover(&fx.path, opts()).unwrap();
    assert!(
        !rec.rolled_back,
        "a healthy file must not report a rollback: {rec:?}"
    );
    assert_eq!(rec.recoveries, 0, "{rec:?}");
    assert_eq!(
        rec.ino_floor, None,
        "no floor may move when nothing was lost: {rec:?}"
    );
    assert_eq!(rec.snapshot_floor, None, "{rec:?}");
    assert!(
        rec.backup.is_none(),
        "a healthy file needs no backup: {rec:?}"
    );
    let h = m.health();
    assert_eq!(h.recoveries, 0, "{h:?}");
    assert_eq!(
        h.ino_floor, fx.before.ino_floor,
        "the floor must not move: {h:?}"
    );
    assert_eq!(h.snapshot_floor, fx.before.snapshot_floor, "{h:?}");
    clean_scratch(&fx);
}

/// A failed recovery must be marked, not swallowed into a clean-looking result, and must put the
/// file back byte for byte. `check()` cannot show any of this, which is why it is asserted here.
#[test]
fn a_failed_recovery_is_reported_and_restores_the_file_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let fx = build(&dir.path().join("unrepairable.redb"), 12, &["s0"]);
    let pristine_hash = sha_file(&fx.pristine);

    damage_unrepairable(&fx);
    let damaged_hash = sha_file(&fx.path);
    assert_ne!(damaged_hash, pristine_hash, "the fixture was not damaged");

    let err = Meta::open_recover(&fx.path, opts())
        .expect_err("an unrepairable file must not produce a store");
    match &err {
        Error::Storage(m) => assert!(
            m.starts_with(RECOVERY_FAILED),
            "a failed recovery is not marked as one: {m}"
        ),
        other => panic!("expected a marked storage error, got {other:?}"),
    }
    assert_eq!(
        sha_file(&fx.path),
        damaged_hash,
        "the file was not restored byte for byte"
    );
    assert!(
        Meta::open(&fx.path, opts()).is_err(),
        "after a failed recovery the file must still fail closed, never look clean"
    );
    assert_pristine_intact(&fx, pristine_hash);
}

/// Two rollbacks of the same file: the durable count is monotonic and each rollback moves the
/// floors forward again, so the second recovery cannot reuse what the first one protected.
#[test]
fn repeated_rollbacks_keep_counting_and_keep_moving_the_floors() {
    let dir = tempfile::tempdir().unwrap();
    let mut fx = build(&dir.path().join("twice.redb"), 20, &["s0"]);
    let pristine_hash = sha_file(&fx.pristine);
    let max_ino = *fx.inodes.iter().max().unwrap();

    let mut floors = Vec::new();
    let mut handed_out = max_ino;
    for round in 1..=2u64 {
        // Each round needs a large batch as the newest commit, or a rollback is unreachable:
        // see `build`. The recovered store is reopened, given fresh files, and re-snapshotted.
        if round > 1 {
            let m = Meta::open(&fx.path, opts()).unwrap();
            let s = m.snapshots().unwrap()[0].name.clone();
            let s = m.snapshot(&s).unwrap();
            let fresh = write_tail_batch(&s, round as u32);
            handed_out = handed_out.max(*fresh.iter().max().unwrap());
            m.sync().unwrap();
            let before = m.health();
            std::fs::copy(&fx.path, &fx.scratch).unwrap();
            drop(m);
            // The store is closed now, so its newest commit is the one the copy captured.
            std::fs::copy(&fx.scratch, &fx.path).unwrap();
            fx.before = before;
            let _ = std::fs::remove_file(&fx.scratch);
        }
        damage_newest_commit(&fx);
        let (m, rec) = Meta::open_recover(&fx.path, opts()).unwrap();
        assert!(rec.rolled_back, "round {round}: {rec:?}");
        assert_eq!(rec.recoveries, round, "round {round}: {rec:?}");
        let f = (
            rec.ino_floor.expect("ino floor"),
            rec.snapshot_floor.expect("snapshot floor"),
        );
        assert!(
            f.0 > handed_out,
            "round {round}: the floor {} does not clear {handed_out}",
            f.0
        );
        assert_eq!(m.health().recoveries, round, "round {round}");
        m.check().unwrap();
        m.close().unwrap();
        drop(m);
        floors.push(f);
    }
    assert!(
        floors[1].0 > floors[0].0,
        "the inode floor did not move on the second rollback: {floors:?}"
    );
    assert!(
        floors[1].1 > floors[0].1,
        "the snapshot floor did not move on the second rollback: {floors:?}"
    );
    assert_pristine_intact(&fx, pristine_hash);
}
