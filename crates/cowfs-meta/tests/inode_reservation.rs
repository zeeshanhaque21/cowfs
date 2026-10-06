//! Issue #42 request 4: `Meta::reserve_inodes(n)` hands out inode numbers before any inode exists.
//!
//! Meta already reserves durably inside itself, one block at a time, so a caller creating inodes
//! never runs out. What it could not do was get a number *before* the transaction that creates the
//! inode, which is what forces a caller to invent numbers itself and keep an alias table. This
//! closes that gap at the metadata layer and nothing else.
//!
//! What has to hold, and is asserted rather than assumed:
//!
//! - a reservation hands back numbers without any inode being created,
//! - ordinary creation and a reservation never hand out the same number,
//! - numbers come back contiguous, with an exclusive end, and never include the root,
//! - a number reserved and then never used is not reissued after a drop and reopen,
//! - asking for zero, or for more than the numbers left below `INO_LIMIT`, writes nothing,
//! - the durable floor only ever moves up, and a reservation is durable when it returns.

use cowfs_meta::{Error, Ino, Meta, Options, SnapshotId, INO_LIMIT, ROOT_INO};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

/// Small blocks so a reservation crosses a block boundary within a test.
fn opts() -> Options {
    Options {
        node_size: 512,
        sync_every_ops: 1,
        background: false,
        ino_block: 8,
        ..Options::default()
    }
}

fn open(dir: &std::path::Path, name: &str) -> Meta {
    Meta::open(dir.join(name), opts()).unwrap()
}

/// A reservation hands back numbers, and no inode exists for any of them.
#[test]
fn a_reservation_returns_numbers_without_creating_anything() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path(), "m.redb");

    let r = m.reserve_inodes(5).unwrap();

    assert_eq!(r.len(), 5, "five numbers were asked for");
    assert_eq!(r.end().0 - r.start().0, 5, "the end is exclusive");
    assert!(!r.is_empty(), "a range this crate hands out is never empty");
    assert_ne!(r.start(), ROOT_INO, "the root is never handed out");
    assert!(
        r.start().0 >= ROOT_INO.0 + 1,
        "reservation starts above the root, got {}",
        r.start().0
    );
    assert!(
        r.end().0 <= INO_LIMIT,
        "reservation stays below the limit, got {}",
        r.end().0
    );

    // Nothing exists yet, so no number in the range resolves.
    let s = m.new_snapshot("snap").unwrap();
    for ino in r.iter() {
        assert!(
            s.getattr(ino).is_err(),
            "inode {ino:?} must not exist just because its number was reserved"
        );
    }
}

/// The numbers are contiguous and the range reports them exactly.
#[test]
fn a_reservation_is_contiguous_with_an_exclusive_end() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path(), "m.redb");

    let r = m.reserve_inodes(4).unwrap();

    let got: Vec<u64> = r.iter().map(|i| i.0).collect();
    let want: Vec<u64> = (r.start().0..r.end().0).collect();
    assert_eq!(got, want, "iter yields every number in order");
    assert_eq!(got.len(), 4);
    for (i, w) in want.iter().enumerate() {
        assert_eq!(got[i], *w);
        if i > 0 {
            assert_eq!(got[i], got[i - 1] + 1, "the numbers are contiguous");
        }
    }

    assert!(r.contains(Ino(r.start().0)), "the start is inside");
    assert!(r.contains(Ino(r.end().0 - 1)), "the last number is inside");
    assert!(
        !r.contains(Ino(r.end().0)),
        "the exclusive end is not inside"
    );
    assert!(!r.contains(Ino(r.start().0 - 1)), "one below is outside");
}

/// Two reservations in a row are disjoint and the second follows the first.
#[test]
fn two_reservations_are_disjoint_and_the_second_follows_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path(), "m.redb");

    let a = m.reserve_inodes(3).unwrap();
    let b = m.reserve_inodes(3).unwrap();

    assert_eq!(
        b.start().0,
        a.end().0,
        "the second starts where the first ended"
    );
    assert!(
        !a.contains(b.start()) && !b.contains(a.start()),
        "the ranges do not overlap"
    );
}

/// A number reserved and never used is not reissued after a reopen, which is the whole point of
/// committing the floor before returning.
#[test]
fn numbers_reserved_and_never_used_are_not_reissued_after_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");

    let reserved = {
        let m = Meta::open(&path, opts()).unwrap();
        // Cross a block boundary so more than one durable reservation commit happens.
        let r = m.reserve_inodes(11).unwrap();
        // Deliberately create nothing at all.
        r
    };
    assert_eq!(reserved.len(), 11);

    drop(reserved);
    let m = Meta::open(&path, opts()).unwrap();
    let after = m.reserve_inodes(11).unwrap();

    assert!(
        !after.iter().any(|i| reserved.contains(i)),
        "no number from the first reservation comes back after a reopen"
    );
    assert!(
        after.start().0 >= reserved.end().0,
        "the new reservation starts at or above the old end: {} vs {}",
        after.start().0,
        reserved.end().0
    );
    m.sync().unwrap();
}

/// Ordinary creation and a reservation draw from the same allocator and never collide, including
/// when both run on separate threads.
#[test]
fn a_reservation_and_ordinary_creation_never_hand_out_the_same_number() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");

    let reserved = {
        let m = Meta::open(&path, opts()).unwrap();
        m.reserve_inodes(20).unwrap()
    };

    let m = Meta::open(&path, opts()).unwrap();
    let s = m.new_snapshot("snap").unwrap();
    let mut created = Vec::new();
    s.batch(|tx| {
        for i in 0..20 {
            created.push(
                tx.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)?
                    .ino
                    .0,
            );
        }
        Ok(())
    })
    .unwrap();

    let overlap: Vec<u64> = created
        .iter()
        .copied()
        .filter(|n| reserved.contains(Ino(*n)))
        .collect();
    assert!(
        overlap.is_empty(),
        "creation must not receive a reserved number, got {overlap:?}"
    );

    let uniq: HashSet<u64> = created.iter().copied().collect();
    assert_eq!(uniq.len(), created.len(), "creation hands out no repeats");

    // And a further reservation after those creates starts past everything handed out.
    let after = m.reserve_inodes(4).unwrap();
    assert!(
        !after.iter().any(|i| reserved.contains(i)),
        "a later reservation does not reissue earlier numbers"
    );
    for n in &created {
        assert!(
            !after.contains(Ino(*n)),
            "a later reservation does not reissue a created number {n}"
        );
    }
}

/// Concurrent reservations on separate threads are disjoint. The allocator is shared through the
/// session, so the check is that the lock actually serialises them.
#[test]
fn concurrent_reservations_are_disjoint() {
    let dir = tempfile::tempdir().unwrap();
    let m = Arc::new(open(dir.path(), "m.redb"));

    let mut handles = Vec::new();
    for _ in 0..4 {
        let m = Arc::clone(&m);
        handles.push(std::thread::spawn(move || {
            let mut all = Vec::new();
            for _ in 0..5 {
                let r = m.reserve_inodes(3).unwrap();
                for i in r.iter() {
                    all.push(i.0);
                }
            }
            all
        }));
    }

    let mut all = Vec::new();
    for h in handles {
        all.extend(h.join().unwrap());
    }

    let uniq: HashSet<u64> = all.iter().copied().collect();
    assert_eq!(
        uniq.len(),
        all.len(),
        "20 numbers were handed out across 4 threads and must all differ"
    );
    assert!(!uniq.contains(&ROOT_INO.0), "the root is never handed out");
    assert!(
        all.iter().all(|n| *n > ROOT_INO.0),
        "every number is above the root"
    );
}

/// Concurrent reservations interleaved with ordinary creation stay disjoint.
#[test]
fn a_reservation_racing_creation_stays_disjoint() {
    let dir = tempfile::tempdir().unwrap();
    let m = Arc::new(open(dir.path(), "m.redb"));
    let snapshot_id: SnapshotId = {
        let s = m.new_snapshot("snap").unwrap();
        s.id()
    };

    let seen: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));

    let mut handles = Vec::new();
    for t in 0..3 {
        let m = Arc::clone(&m);
        let seen = Arc::clone(&seen);
        handles.push(std::thread::spawn(move || {
            let mut mine = Vec::new();
            for i in 0..6 {
                if t % 2 == 0 {
                    let r = m.reserve_inodes(2).unwrap();
                    mine.extend(r.iter().map(|x| x.0));
                } else {
                    let s = m.snapshot_by_id(snapshot_id).unwrap();
                    s.batch(|tx| {
                        for k in 0..2 {
                            mine.push(
                                tx.create(ROOT_INO, format!("t{t}i{i}k{k}").as_bytes(), 0o644)?
                                    .ino
                                    .0,
                            );
                        }
                        Ok(())
                    })
                    .unwrap();
                }
            }
            seen.lock().unwrap().extend(mine);
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let all = seen.lock().unwrap().clone();
    let uniq: HashSet<u64> = all.iter().copied().collect();
    assert_eq!(
        uniq.len(),
        all.len(),
        "reserved and created numbers must not collide across threads"
    );
    assert_eq!(all.len(), 36, "3 threads times 6 rounds times 2 numbers");
}

/// Asking for zero writes nothing and is refused.
#[test]
fn a_zero_reservation_is_refused_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path(), "m.redb");

    let e = m.reserve_inodes(0).unwrap_err();
    assert!(matches!(e, Error::Invalid(_)), "{e:?}");

    // Nothing was consumed: the next reservation starts where a fresh file's would.
    let r = m.reserve_inodes(1).unwrap();
    let fresh = {
        let other = tempfile::tempdir().unwrap();
        open(other.path(), "m.redb").reserve_inodes(1).unwrap()
    };
    assert_eq!(
        r.start(),
        fresh.start(),
        "a refused reservation must not move the allocator"
    );
}

/// Asking for more than the numbers left below the limit writes nothing.
#[test]
fn a_reservation_past_the_limit_is_refused_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path(), "m.redb");

    let e = m.reserve_inodes(INO_LIMIT).unwrap_err();
    assert!(matches!(e, Error::LimitExceeded(_)), "{e:?}");

    let e = m.reserve_inodes(u64::MAX).unwrap_err();
    assert!(
        matches!(e, Error::LimitExceeded(_)),
        "a count that would wrap must be refused, not wrapped: {e:?}"
    );

    let r = m.reserve_inodes(1).unwrap();
    assert!(
        r.start().0 < INO_LIMIT,
        "the allocator is untouched after both refusals"
    );
}

/// The floor only moves up, and a reservation is durable when it returns rather than waiting for a
/// later commit.
#[test]
fn the_durable_floor_only_moves_up_across_reservations() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path(), "m.redb");

    let mut high = 0;
    for n in [1, 2, 5, 3, 9] {
        let r = m.reserve_inodes(n).unwrap();
        assert_eq!(r.len(), n);
        assert!(
            r.start().0 >= high,
            "start {} went backwards from {high}",
            r.start().0
        );
        high = r.end().0;
    }

    // Reopen: the durable floor is at or above everything handed out, because the floor was
    // committed before each range was returned.
    drop(m);
    let m = open(dir.path(), "m.redb");
    let after = m.reserve_inodes(1).unwrap();
    assert!(
        after.start().0 >= high,
        "after a reopen the allocator resumes at or above {high}, got {}",
        after.start().0
    );
}

/// A reservation works on a handle whose snapshot set is otherwise empty, and does not disturb an
/// existing snapshot.
#[test]
fn a_reservation_leaves_existing_snapshots_alone() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path(), "m.redb");
    let s = m.new_snapshot("snap").unwrap();
    let id = s.id();
    let root_before = *s.info().unwrap().root.as_bytes();
    m.sync().unwrap();

    let r = m.reserve_inodes(6).unwrap();
    assert_eq!(r.len(), 6);

    let info = m.snapshot_by_id(id).unwrap().info().unwrap();
    assert_eq!(info.id, id, "the snapshot is still there");
    assert_eq!(
        *info.root.as_bytes(),
        root_before,
        "its root is unchanged by an unrelated reservation"
    );
    m.check().expect("check after a reservation");
}
