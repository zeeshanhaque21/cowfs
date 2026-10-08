//! Load-bearing recovery tests for issue #40 M3.
//!
//! The fixture shape here is the only one in which the counter bump matters: the sync hook is armed
//! to fail after the snapshot exists, so every main commit fails while every `reserve_durable`
//! still commits (it runs no hook). The newest DURABLE commit is then a reservation, and one
//! rollback undoes exactly that reservation. In any other shape the rollback loses a commit that
//! never moved a counter, and the allocator's own headroom satisfies a floor assertion without the
//! bump doing anything.
//!
//! Assertions are on handed-out numbers and read-back state, never on floor headroom.
//!
//! Every fixture is a private `tempfile` store, closed before damage. Nothing else is touched.

use cowfs_meta::{Ino, Meta, Options, SyncHook, RECOVERY_FAILED, ROOT_INO};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;

const PAGE: usize = 4096;

/// Block the fixture is BUILT with. Every test here then recovers with a different, smaller block,
/// so the stored value has to be what bounds the floor.
const BUILT_WITH: u64 = 64;
/// Inode numbers the fixture hands out inside its single reservation.
const HANDED: u32 = 64;

fn opts(block: u64, hook: Option<SyncHook>) -> Options {
    Options {
        node_size: 512,
        sync_every_ops: 1,
        ino_block: block,
        background: false,
        before_sync: hook,
        ..Options::default()
    }
}

/// A hook that succeeds until armed, then always fails.
fn failing_hook() -> (Arc<AtomicBool>, SyncHook) {
    let armed = Arc::new(AtomicBool::new(false));
    let hook = {
        let armed = armed.clone();
        Arc::new(move || -> std::io::Result<()> {
            if armed.load(SeqCst) {
                Err(std::io::Error::other("hook refuses"))
            } else {
                Ok(())
            }
        }) as SyncHook
    };
    (armed, hook)
}

/// Damages the newest commit only. Searches on a scratch copy and never writes to `path` until one
/// page is known to produce a rollback, so a layout change fails loudly instead of silently
/// testing nothing.
fn damage_newest(path: &std::path::Path, scratch: &std::path::Path) -> usize {
    let full = std::fs::read(path).unwrap();
    for page in 1..full.len() / PAGE {
        let mut img = full.to_vec();
        for b in &mut img[page * PAGE..(page + 1) * PAGE] {
            *b = 0xA5;
        }
        std::fs::write(scratch, &img).unwrap();
        if Meta::open(scratch, opts(4, None)).is_ok() {
            continue;
        }
        match Meta::open_recover(scratch, opts(4, None)) {
            Ok((m, r)) if r.rolled_back => {
                drop(m);
                std::fs::write(path, &img).unwrap();
                let _ = std::fs::remove_file(scratch);
                let _ = std::fs::remove_file(scratch.with_extension("redb.pre-recover"));
                assert!(
                    Meta::open(path, opts(4, None)).is_err(),
                    "precondition: the damaged file must fail closed"
                );
                return page;
            }
            Ok(_) | Err(_) => continue,
        }
    }
    let _ = std::fs::remove_file(scratch);
    panic!("no page damage produced a rollback");
}

/// Builds a store whose newest durable commit is a reservation. Returns the work path, a scratch
/// path, and every inode number handed out.
fn build_reservation_latest(
    dir: &std::path::Path,
) -> (std::path::PathBuf, std::path::PathBuf, Vec<u64>) {
    let live = dir.join("live.redb");
    let work = dir.join("work.redb");
    let (armed, hook) = failing_hook();
    let m = Meta::open(&live, opts(BUILT_WITH, Some(hook))).unwrap();
    let s = m.new_snapshot("s0").unwrap();
    armed.store(true, SeqCst);

    // One batch, so a single mutate call allocates every inode and `pending_ops` only reaches 1,
    // which keeps the store under the backlog cap while the hook fails the batch's own commit.
    let mut handed = Vec::new();
    s.batch(|tx| {
        for i in 0..HANDED {
            let name = format!("f{i}");
            handed.push(tx.create(ROOT_INO, name.as_bytes(), 0o644)?.ino.0);
        }
        Ok(())
    })
    .expect("applied in memory even though the commit fails");
    // `close` fails on the hook too, so the last durable commit stays the reservation.
    let _ = m.close();
    drop(m);
    std::fs::copy(&live, &work).unwrap();
    (work, dir.join("scratch.redb"), handed)
}

/// The M3 blocker: recovering with a smaller `ino_block` than the store was built with used to
/// re-issue inode numbers the store had already handed out, silently and with `Health` reporting a
/// healthy store. The bound must come from the block the file was created with.
#[test]
fn a_smaller_block_at_recovery_does_not_re_issue_inode_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let (work, scratch, handed) = build_reservation_latest(dir.path());
    damage_newest(&work, &scratch);

    let (m, rec) = Meta::open_recover(&work, opts(4, None)).unwrap();
    assert!(rec.rolled_back, "{rec:?}");
    assert_eq!(rec.recoveries, 1, "{rec:?}");

    let max_handed = *handed.iter().max().unwrap();
    let name = m.snapshots().unwrap()[0].name.clone();
    let s = m.snapshot(&name).unwrap();

    // Allocate well past everything handed out before the crash.
    let mut fresh = Vec::new();
    for i in 0..200u32 {
        let f = s
            .create(ROOT_INO, format!("g{i}").as_bytes(), 0o644)
            .unwrap();
        fresh.push(f.ino.0);
    }
    m.sync().unwrap();

    let reused: Vec<u64> = fresh
        .iter()
        .copied()
        .filter(|n| handed.contains(n))
        .collect();
    assert!(
        reused.is_empty(),
        "{} inode numbers handed out twice, first {:?}; highest pre-crash was {max_handed}",
        reused.len(),
        &reused[..reused.len().min(8)]
    );

    // Read back through a fresh handle, so this is what the file says and not what memory holds.
    m.close().unwrap();
    drop(s);
    drop(m);
    let again = Meta::open(&work, opts(4, None)).unwrap();
    let name = again.snapshots().unwrap()[0].name.clone();
    let s2 = again.snapshot(&name).unwrap();
    for (n, f) in fresh.iter().take(20).enumerate() {
        let attr = s2
            .lookup(ROOT_INO, format!("g{n}").as_bytes())
            .unwrap_or_else(|e| {
                panic!("file g{n} is missing after reopen, so inode {f} was never durable: {e}")
            });
        assert_eq!(
            attr.ino,
            Ino(*f),
            "inode {f} does not name its own file after reopen"
        );
    }
    assert_eq!(again.health().recoveries, 1);
    again.check().unwrap();
}

/// A rollback that loses a snapshot creation must not hand that id out again. This is the case the
/// `+1` exists for, and it is reached by making the newest durable commit the snapshot itself.
#[test]
fn a_snapshot_id_lost_to_a_rollback_is_not_handed_out_again() {
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("live.redb");
    let work = dir.path().join("work.redb");
    let scratch = dir.path().join("scratch.redb");

    let m = Meta::open(&live, opts(4, None)).unwrap();
    m.new_snapshot("s0").unwrap();
    m.sync().unwrap();
    // `new_snapshot` is durable on return, so this is now the newest durable commit and the
    // rollback must lose it. Copied straight after: `close` writes the exact inode counter, and
    // that commit would become the newest one and survive instead.
    let lost = m.new_snapshot("s1").unwrap().id().0;
    std::fs::copy(&live, &work).unwrap();
    drop(m);

    damage_newest(&work, &scratch);
    let (m, rec) = Meta::open_recover(&work, opts(4, None)).unwrap();
    assert!(rec.rolled_back, "{rec:?}");

    // The rollback must actually have lost that id, or this test proves nothing.
    let present: Vec<u64> = m.snapshots().unwrap().iter().map(|s| s.id.0).collect();
    assert!(
        !present.contains(&lost),
        "fixture is wrong: snapshot {lost} survived, so nothing was lost to re-issue"
    );

    let fresh = m.new_snapshot("s2").unwrap().id().0;
    assert_ne!(fresh, lost, "snapshot id {lost} was handed out twice");
    m.check().unwrap();

    m.close().unwrap();
    drop(m);
    let again = Meta::open(&work, opts(4, None)).unwrap();
    let ids: Vec<u64> = again.snapshots().unwrap().iter().map(|s| s.id.0).collect();
    assert!(
        !ids.contains(&lost),
        "the file on disk re-issued the lost snapshot id {lost}: {ids:?}"
    );
    assert!(
        ids.contains(&fresh),
        "the new snapshot {fresh} is not on disk: {ids:?}"
    );
    again.check().unwrap();
}

/// A file written before the reservation block was persisted has no provable bound, so recovery
/// must refuse rather than guess. The lost commit's size is unknowable, and guessing would
/// re-issue numbers silently, which is the failure this whole change exists to prevent.
///
/// The key is removed with redb directly, which is what a file from an older build looks like.
#[test]
fn recovery_refuses_a_file_whose_reservation_block_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.redb");
    let m = Meta::open(&path, opts(4, None)).unwrap();
    m.new_snapshot("s0").unwrap();
    m.sync().unwrap();
    m.close().unwrap();
    drop(m);

    {
        let db = redb::Database::create(&path).unwrap();
        let wtx = db.begin_write().unwrap();
        let mut meta = wtx
            .open_table(redb::TableDefinition::<&str, u64>::new("meta"))
            .unwrap();
        assert!(
            meta.remove("ino_block").unwrap().is_some(),
            "the current build must persist the block, or this fixture is not a legacy one"
        );
        drop(meta);
        wtx.commit().unwrap();
    }

    // The file still opens and works: only recovery needs the bound.
    let m = Meta::open(&path, opts(4, None)).unwrap();
    let name = m.snapshots().unwrap()[0].name.clone();
    m.snapshot(&name)
        .unwrap()
        .create(ROOT_INO, b"still-writable", 0o644)
        .unwrap();
    m.sync().unwrap();
    m.close().unwrap();
    drop(m);

    // Damage it, and recovery must refuse with a reason rather than re-issue numbers.
    let work = dir.path().join("work.redb");
    std::fs::copy(&path, &work).unwrap();
    let scratch = dir.path().join("scratch.redb");
    let full = std::fs::read(&work).unwrap();
    let mut damaged = false;
    for page in 1..full.len() / PAGE {
        let mut img = full.to_vec();
        for b in &mut img[page * PAGE..(page + 1) * PAGE] {
            *b = 0xA5;
        }
        std::fs::write(&scratch, &img).unwrap();
        if Meta::open(&scratch, opts(4, None)).is_ok() {
            continue;
        }
        if Meta::open_recover(&scratch, opts(4, None)).is_err() {
            std::fs::write(&work, &img).unwrap();
            damaged = true;
            break;
        }
    }
    let _ = std::fs::remove_file(&scratch);
    let _ = std::fs::remove_file(scratch.with_extension("redb.pre-recover"));
    assert!(damaged, "could not produce a damaged fixture");

    let err = Meta::open_recover(&work, opts(4, None))
        .expect_err("recovery must refuse when the bound cannot be proven");
    let msg = err.to_string();
    // `Error`'s Display prefixes its own variant, so the marker is inside, not at the front.
    assert!(
        msg.contains(RECOVERY_FAILED),
        "the refusal is not marked as a recovery failure: {msg}"
    );
    assert!(
        msg.contains("predates the persisted inode reservation block"),
        "the refusal does not say why: {msg}"
    );
}

/// A plain reopen with a wildly different `ino_block` must not re-issue anything either: the
/// stored block governs the allocator too, not only recovery. This is the same failure the first
/// test covers, reached without any damage at all.
///
/// Two rollbacks of one file, and the count going 1 then 2, are covered in `health.rs`.
#[test]
fn the_stored_block_governs_a_plain_reopen_with_a_different_ino_block() {
    let dir = tempfile::tempdir().unwrap();
    let (work, scratch, handed) = build_reservation_latest(dir.path());
    damage_newest(&work, &scratch);
    let (m, rec) = Meta::open_recover(&work, opts(4, None)).unwrap();
    assert!(rec.rolled_back, "{rec:?}");
    m.close().unwrap();
    drop(m);

    // Reopened with a much larger block than the file was created with. If the caller's value
    // governed, the allocator would step in blocks of 4096 and hand out numbers from below the
    // recovered floor.
    let m = Meta::open(&work, opts(4096, None)).unwrap();
    let name = m.snapshots().unwrap()[0].name.clone();
    let s = m.snapshot(&name).unwrap();
    let mut fresh = Vec::new();
    for i in 0..80u32 {
        fresh.push(
            s.create(ROOT_INO, format!("h{i}").as_bytes(), 0o644)
                .unwrap()
                .ino
                .0,
        );
    }
    m.sync().unwrap();
    let reused: Vec<u64> = fresh
        .iter()
        .copied()
        .filter(|n| handed.contains(n))
        .collect();
    assert!(
        reused.is_empty(),
        "a reopen with ino_block 4096 re-issued {} pre-crash numbers",
        reused.len()
    );

    // Read back through a fresh handle, so this is what the file holds.
    m.close().unwrap();
    drop(s);
    drop(m);
    let again = Meta::open(&work, opts(4096, None)).unwrap();
    let name = again.snapshots().unwrap()[0].name.clone();
    let s2 = again.snapshot(&name).unwrap();
    for (n, f) in fresh.iter().take(20).enumerate() {
        let attr = s2
            .lookup(ROOT_INO, format!("h{n}").as_bytes())
            .unwrap_or_else(|e| {
                panic!("h{n} missing after reopen, so inode {f} was never durable: {e}")
            });
        assert_eq!(attr.ino, Ino(*f), "inode {f} does not name its own file");
    }
    assert_eq!(
        again.health().recoveries,
        1,
        "the count did not survive the reopen"
    );
    again.check().unwrap();
}
