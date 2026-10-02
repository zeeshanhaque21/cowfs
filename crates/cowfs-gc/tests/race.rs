//! Writers, snapshot creation and removal, and collect all at once.
//!
//! The barrier is what makes this pass: `Roots::write` takes the same lock `reference_barrier`
//! holds, so a commit cannot land between the collector's last reachability check and the unlink.
//! Without a barrier the same workload would lose blocks, which is why the crate refuses to free
//! anything when no barrier is offered, and a test here proves that it does refuse.

mod common;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Fixture, Roots};
use cowfs_store::BlockId;

fn body(n: usize, seed: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut h = seed.wrapping_mul(2654435761).wrapping_add(1);
    for _ in 0..n {
        h = h.wrapping_mul(1664525).wrapping_add(1013904223);
        out.push(if h >> 29 == 0 {
            b'a'.wrapping_add((h >> 8) as u8)
        } else {
            (h >> 16) as u8
        });
    }
    out
}

/// Writers, snapshot creates, snapshot removes and collects, all at once.
///
/// After every collect, and at the end, every block any live snapshot references reads back.
#[test]
fn writers_and_collects_at_once_lose_nothing() {
    let f = Fixture::eager(128 << 10);
    // The fixture owns a TempDir and a lock, so the threads take the parts that are Send + Sync.
    let parts = f.parts();
    let parts = &parts;
    let roots = Roots::new();
    let base = f.meta.new_snapshot("base").unwrap();
    for i in 0..8u8 {
        parts.write(
            &base,
            format!("base{i}").as_bytes(),
            &body(30_000, u32::from(i)),
        );
    }
    parts.meta.sync().unwrap();
    parts.store.sync().unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let writes = Arc::new(AtomicU64::new(0));
    let collects = Arc::new(AtomicU64::new(0));
    let names: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));

    std::thread::scope(|sc| {
        for w in 0..3u64 {
            let stop = Arc::clone(&stop);
            let writes = Arc::clone(&writes);
            let names = Arc::clone(&names);
            let roots = Arc::clone(&roots);
            let base = &base;
            sc.spawn(move || {
                let snap = base.fork(&format!("w{w}")).unwrap();
                parts.meta.sync().unwrap();
                for i in 0..40u64 {
                    if stop.load(Relaxed) {
                        break;
                    }
                    let name = format!("f{i:02}");
                    let data = body(20_000, (w as u32) << 8 ^ i as u32);
                    roots.write(|| parts.write(&snap, name.as_bytes(), &data));
                    names.lock().unwrap().push(name);
                    writes.fetch_add(1, Relaxed);
                }
            });
        }
        {
            let stop = Arc::clone(&stop);
            sc.spawn(move || {
                let mut n = 0u32;
                while !stop.load(Relaxed) {
                    let s = parts.meta.new_snapshot(&format!("tmp{n}")).unwrap();
                    parts.meta.sync().unwrap();
                    parts.meta.remove_snapshot(s.id()).unwrap();
                    parts.meta.reap_all().unwrap();
                    n += 1;
                }
            });
        }
        {
            let stop = Arc::clone(&stop);
            let collects = Arc::clone(&collects);
            let roots = Arc::clone(&roots);
            sc.spawn(move || {
                while !stop.load(Relaxed) {
                    let r = parts.gc.collect(Some(&*roots)).expect("collect");
                    assert!(r.errors.is_empty(), "{:?}", r.errors);
                    assert!(!parts.store.recovery().has_corruption(), "corruption");
                    for b in parts.live() {
                        assert!(
                            parts.store.get(b).is_ok(),
                            "a referenced block lost during a concurrent collect"
                        );
                    }
                    collects.fetch_add(1, Relaxed);
                }
            });
        }
        std::thread::sleep(Duration::from_millis(1500));
        stop.store(true, Relaxed);
    });

    let r = f.gc.collect(Some(&*roots)).expect("final collect");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let wrote = writes.load(Relaxed);
    assert!(wrote > 0, "no writes happened");
    assert!(collects.load(Relaxed) > 0, "no collects happened");
    assert_eq!(names.lock().unwrap().len() as u64, wrote);

    let live = parts.live();
    assert!(!live.is_empty(), "the base snapshot is still live");
    for b in &live {
        assert!(
            parts.store.get(*b).is_ok(),
            "a live block is gone at the end"
        );
    }
    assert!(parts.store.fsck().expect("fsck").is_clean());
    assert!(!parts.store.recovery().has_corruption());
}

/// A cycle with no barrier reports its candidates and frees nothing.
#[test]
fn without_a_barrier_nothing_is_freed() {
    let f = Fixture::eager(32 << 10);
    let parts = f.parts();
    let snap = f.meta.new_snapshot("s").unwrap();
    parts.write(&snap, b"keep", &body(60_000, 1));
    for i in 0..40u32 {
        parts.store.put(&body(4000, i)).unwrap();
    }
    parts.store.sync().unwrap();
    parts.meta.sync().unwrap();
    let before = parts.store.stats().pack_bytes;

    let roots = Roots::no_barrier();
    let r = f.gc.collect(Some(&*roots)).expect("collect");
    assert!(!r.barrier, "the report says no barrier was taken");
    assert_eq!(r.freed_bytes, 0, "nothing is freed without a barrier");
    assert_eq!(
        parts.store.stats().pack_bytes,
        before,
        "and nothing is copied"
    );
    assert!(
        r.skipped
            .iter()
            .any(|s| s.reason == cowfs_gc::SkipReason::NotReached),
        "the reason is reported: {:?}",
        r.skipped
    );
    assert!(r.candidates > 0, "the candidates are still reported");
    for b in parts.live() {
        assert!(parts.store.get(b).is_ok(), "every live block reads");
    }

    let with = Roots::new();
    let r2 = f.gc.collect(Some(&*with)).expect("collect");
    assert!(r2.barrier, "the barrier is taken");
    assert!(r2.freed_bytes > 0, "and bytes come back: {r2:?}");
}

/// The barrier is short: writers are not starved by a collect over many packs.
#[test]
fn the_barrier_costs_writers_a_bounded_stall() {
    let f = Fixture::eager(64 << 10);
    let parts = f.parts();
    let parts = &parts;
    let snap = f.meta.new_snapshot("s").unwrap();
    parts.write(&snap, b"keep", &body(120_000, 1));
    for i in 0..60u32 {
        parts.store.put(&body(4000, i)).unwrap();
    }
    parts.store.sync().unwrap();
    parts.meta.sync().unwrap();
    let roots = Roots::new();

    let writes = Arc::new(AtomicU64::new(0));
    let worst = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    std::thread::scope(|sc| {
        for w in 0..2u64 {
            let writes = Arc::clone(&writes);
            let worst = Arc::clone(&worst);
            let stop = Arc::clone(&stop);
            let roots = Arc::clone(&roots);
            let snap = &snap;
            sc.spawn(move || {
                let snap2 = snap.fork(&format!("w{w}")).unwrap();
                parts.meta.sync().unwrap();
                let mut i = 0u64;
                while !stop.load(Relaxed) {
                    let t = Instant::now();
                    let name = format!("f{i:03}");
                    roots.write(|| parts.write(&snap2, name.as_bytes(), &body(8000, i as u32)));
                    worst.fetch_max(t.elapsed().as_micros() as u64, Relaxed);
                    writes.fetch_add(1, Relaxed);
                    i += 1;
                }
            });
        }
        std::thread::sleep(Duration::from_millis(250));
        for _ in 0..4 {
            let r = parts.gc.collect(Some(&*roots)).expect("collect");
            assert!(r.errors.is_empty(), "{:?}", r.errors);
        }
        stop.store(true, Relaxed);
    });
    assert!(writes.load(Relaxed) > 0, "writers made no progress");
    let worst_us = worst.load(Relaxed);
    assert!(
        worst_us < 3_000_000,
        "a write stalled for {worst_us} us, the barrier is not short"
    );
}

/// Two collectors at once: neither loses a block and neither reports an error.
#[test]
fn two_collectors_on_one_store_are_safe() {
    let f = Fixture::eager(32 << 10);
    let parts = f.parts();
    let snap = f.meta.new_snapshot("s").unwrap();
    parts.write(&snap, b"keep", &body(60_000, 1));
    for i in 0..40u32 {
        parts.store.put(&body(4000, i)).unwrap();
    }
    parts.store.sync().unwrap();
    parts.meta.sync().unwrap();
    let roots = Roots::new();
    std::thread::scope(|sc| {
        for _ in 0..2 {
            let roots = Arc::clone(&roots);
            sc.spawn(move || {
                for _ in 0..3 {
                    let r = parts.gc.collect(Some(&*roots)).expect("collect");
                    assert!(r.errors.is_empty(), "{:?}", r.errors);
                }
            });
        }
    });
    for b in parts.live() {
        assert!(
            parts.store.get(b).is_ok(),
            "a live block is gone after two collectors"
        );
    }
    assert!(parts.store.fsck().expect("fsck").is_clean());
}

/// A pinned block survives a collect, and goes on a later one once it is unpinned.
#[test]
fn a_block_pinned_then_unpinned_survives_until_it_is_not() {
    let f = Fixture::eager(32 << 10);
    let parts = f.parts();
    let roots = Roots::new();
    let b: BlockId = parts.store.put(&body(4000, 7)).unwrap();
    let snap = f.meta.new_snapshot("s").unwrap();
    parts.write(&snap, b"keep", &body(60_000, 1));
    for i in 0..30u32 {
        parts.store.put(&body(4000, i)).unwrap();
    }
    parts.store.sync().unwrap();
    parts.meta.sync().unwrap();
    roots.pin(b);
    let r = f.gc.collect(Some(&*roots)).expect("collect");
    assert!(r.freed_bytes > 0, "other garbage went");
    assert!(
        parts.store.get(b).is_ok(),
        "the pinned block is still there"
    );
    let live: HashSet<BlockId> = parts.live();
    assert!(
        !live.contains(&b),
        "and it is not referenced by any snapshot"
    );
    roots.unpin_all();
    f.gc.collect(Some(&*roots)).expect("collect");
    for x in parts.live() {
        assert!(parts.store.get(x).is_ok());
    }
}
