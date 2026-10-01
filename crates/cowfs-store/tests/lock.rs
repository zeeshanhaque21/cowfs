//! The store lock must be free the moment `drop` returns, because every caller reopens right
//! after: the mount layer, the collector, a daemon restart, and the tests themselves.
//!
//! The long loop is ignored by default. Run it with:
//!
//! ```text
//! cargo test -p cowfs-store --release --test lock -- --ignored --nocapture
//! COWFS_LOCK_ITERS=200000 COWFS_LOCK_CKP=1 COWFS_LOCK_FD=2 cargo test -p cowfs-store --release \
//!     --test lock -- --ignored --nocapture
//! ```
//!
//! `COWFS_LOCK_ITERS` iterations, `COWFS_LOCK_CKP=1` to checkpoint on drop, `COWFS_LOCK_FD` to set
//! the size of the descriptor cache, `COWFS_LOCK_BLOCKS` to hold the store open in several threads.
mod common;

use std::time::{Duration, Instant};

use cowfs_store::{BlockId, Options, Store};

fn opts() -> Options {
    Options {
        checkpoint_on_drop: std::env::var("COWFS_LOCK_CKP").is_ok(),
        max_open_packs: std::env::var("COWFS_LOCK_FD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(64),
        ..common::opts()
    }
}

fn iters() -> u64 {
    std::env::var("COWFS_LOCK_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000)
}

fn blocks() -> usize {
    std::env::var("COWFS_LOCK_BLOCKS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// One open, put, sync, drop, open, drop cycle. `Ok(())` means the lock was free on reopen.
/// With `threads` above zero, that many extra threads hold the same store while it is in use.
fn cycle(dir: &std::path::Path, o: Options, data: &[u8], threads: usize) -> Result<(), String> {
    {
        let s = Store::open(dir, o).map_err(|e| format!("first open: {e}"))?;
        s.put(data).map_err(|e| format!("put: {e}"))?;
        s.sync().map_err(|e| format!("sync: {e}"))?;
        if threads > 0 {
            std::thread::scope(|sc| {
                for _ in 0..threads {
                    sc.spawn(|| s.get(BlockId::of(data)).is_ok());
                }
            });
        }
    }
    let s = Store::open(dir, o).map_err(|e| format!("reopen: {e}"))?;
    s.get(BlockId::of(data)).map_err(|e| format!("get: {e}"))?;
    Ok(())
}

#[test]
fn drop_releases_the_lock_before_it_returns() {
    let dir = tempfile::tempdir().unwrap();
    let o = opts();
    let n = std::env::var("COWFS_LOCK_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(32);
    let blocks = blocks();
    let data = common::random(1, 9000);
    let start = Instant::now();
    let mut fails = 0u64;
    let mut first = String::new();
    for i in 0..n {
        let r = cycle(dir.path(), o, &data, blocks);
        if let Err(e) = r {
            fails += 1;
            if first.is_empty() {
                first = format!("iteration {i}: {e}");
            }
        }
    }
    println!(
        "LOCK-LOOP iters={n} blocks={blocks} ckp={} fd={} fails={fails} elapsed={:?} first={first}",
        o.checkpoint_on_drop,
        o.max_open_packs,
        start.elapsed()
    );
    assert_eq!(
        fails, 0,
        "{fails} of {n} reopens found the lock still held: {first}"
    );
}

#[test]
fn a_held_store_keeps_out_other_stores() {
    let dir = tempfile::tempdir().unwrap();
    let o = opts();
    let a = Store::open(dir.path(), o).unwrap();
    let b = Store::open(dir.path(), o);
    assert!(
        b.is_err(),
        "a second store must be refused while one is open"
    );
    let c = Store::open(dir.path(), o);
    assert!(c.is_err(), "and again after the failure");
    drop(a);
    let d = Store::open(dir.path(), o);
    assert!(
        d.is_ok(),
        "the lock must be free once the holder is gone: {d:?}"
    );
}

#[test]
#[ignore = "the long loop, see the module docs for the command"]
fn twenty_thousand_open_drop_reopen_cycles() {
    let dir = tempfile::tempdir().unwrap();
    let o = opts();
    let n = iters().max(20_000);
    let data = common::random(2, 9000);
    let start = Instant::now();
    for i in 0..n {
        if let Err(e) = cycle(dir.path(), o, &data, 0) {
            panic!("iteration {i} of {n}: {e}");
        }
    }
    println!("LOCK-LONG iters={n} elapsed={:?}", start.elapsed());
}

/// The CI symptom: a reopen that lands while the previous holder is on its way out must not be
/// told the store is busy. Before the fix `open` asked once and answered `Locked`.
///
/// The wait covers the in-process release, which is sub-millisecond, so a holder released inside
/// the bound is waited for and one held past it is refused. That is the proven behaviour: `flock`
/// belongs to the open file description, so a forked child keeps the store locked for as long as
/// it lives and no bound helps.
#[test]
fn a_reopen_that_races_a_release_in_flight_succeeds() {
    for delay_ms in [0u64, 1, 5, 20] {
        let dir = tempfile::tempdir().unwrap();
        let o = opts();
        {
            // A store to make the directory real, then a second holder that is let go late.
            let s = Store::open(dir.path(), o).unwrap();
            s.put(&common::random(6, 9000)).unwrap();
            s.sync().unwrap();
        }
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.path().join("LOCK"))
            .unwrap();
        holder.try_lock().unwrap();
        // flock is shared by a duplicated descriptor and released only when the last one goes, so
        // both descriptors move into the thread that lets the store go.
        let second = holder.try_clone().unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(delay_ms));
            drop(second);
            drop(holder);
        });
        let opened = Store::open(dir.path(), o);
        assert!(
            opened.is_ok(),
            "delay {delay_ms} ms: an open that overlaps a release must wait for it, got {:?}",
            opened.err()
        );
        drop(opened);
        releaser.join().unwrap();
    }
    // A holder that outlives the bound is a real owner, not a release in flight, so the open is
    // refused quickly and names the holder rather than waiting on a lock it cannot take.
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        s.put(&common::random(8, 9000)).unwrap();
        s.sync().unwrap();
    }
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.path().join("LOCK"))
        .unwrap();
    holder.try_lock().unwrap();
    let start = Instant::now();
    let e = Store::open(dir.path(), opts()).unwrap_err();
    let took = start.elapsed();
    assert!(took < Duration::from_millis(400), "{took:?}");
    match e {
        cowfs_store::Error::Locked { holder, .. } => {
            assert_eq!(
                holder,
                Some(std::process::id()),
                "the refusal must name the holder"
            )
        }
        other => panic!("expected Locked, got {other:?}"),
    }
    drop(holder);
    assert!(
        Store::open(dir.path(), opts()).is_ok(),
        "the lock must be free again"
    );
}

/// The lock is held exactly while a store is open, and free the instant it is gone.
#[test]
fn the_lock_is_held_while_a_store_is_open_and_free_once_it_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let data = common::random(3, 9000);
    let s = Store::open(dir.path(), opts()).unwrap();
    s.put(&data).unwrap();
    s.sync().unwrap();
    assert!(!lock_is_free(dir.path()), "the store must hold its lock");
    drop(s);
    assert!(lock_is_free(dir.path()), "the lock must be free after drop");
    let t = Store::open(dir.path(), opts()).unwrap();
    assert!(
        !lock_is_free(dir.path()),
        "and taken again by the next store"
    );
    assert_eq!(t.get(BlockId::of(&data)).unwrap(), data);
    t.close().unwrap();
    assert!(lock_is_free(dir.path()), "close must leave it free");
}

/// True when this process can take the store lock, which only a closed store allows.
fn lock_is_free(dir: &std::path::Path) -> bool {
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.join("LOCK"))
    {
        Ok(f) => f.try_lock().is_ok(),
        Err(e) => panic!("cannot open the lock file: {e}"),
    }
}

/// `close` flushes, releases and reports, and the lock is free the moment it returns.
#[test]
fn close_releases_the_lock_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let data = common::random(4, 9000);
    let s = Store::open(dir.path(), opts()).unwrap();
    s.put(&data).unwrap();
    s.close().expect("close must succeed");
    assert!(
        lock_is_free(dir.path()),
        "close must release the lock itself"
    );
    let t = Store::open(dir.path(), opts()).expect("the lock must be free after close");
    t.close().unwrap();
}
