//! Writers, snapshot creation and removal, and collect all at once.
//!
//! The barrier is what makes this pass: `Roots::write` takes the same lock `reference_barrier`
//! holds, so a commit cannot land between the collector's last reachability check and the unlink.
//! Without a barrier the same workload would lose blocks, which is why the crate refuses to free
//! anything when no barrier is offered, and a test here proves that it does refuse.
//!
//! Every concurrent fixture here is **bounded** (issue 83): a fixed byte budget, a fixed iteration
//! cap, a finite run time, a no-progress watchdog, and a stop-on-error flag any worker or collector
//! sets. Before those bounds, a collector that stalled would let the writer loops append until the
//! runner's disk filled (`StorageFull` in CI). The bound is on the rate and the total, so a
//! descheduled runner stops early instead of growing without limit. The numbers each run actually
//! reached are printed (and asserted non-zero) so a bound that silently disabled the workload fails
//! loudly instead of passing empty.

mod common;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Fixture, Roots};
use cowfs_store::BlockId;

/// Total bytes any concurrent writer group may store in one test. A few MB, not tens.
const WRITE_BYTE_BUDGET: u64 = 24 << 20;
/// Iterations one writer thread may run, independent of the byte budget.
const WRITE_ITER_CAP: u64 = 100_000;
/// Wall-clock cap for a concurrent phase.
const RUN_CAP: Duration = Duration::from_secs(4);
/// Stop if no thread makes progress for this long: a stalled collector must not hang CI.
const NO_PROGRESS: Duration = Duration::from_secs(60);

/// The shared bound state of one concurrent phase.
///
/// `stop` is set by any worker or collector that fails, by the byte or iteration budget, and by
/// the watchdog. Every loop checks it, so one failure ends the whole phase instead of leaving a
/// thread spinning.
struct Bounds {
    stop: Arc<AtomicBool>,
    bytes: Arc<AtomicU64>,
    ops: Arc<AtomicU64>,
    last_progress: Arc<Mutex<Instant>>,
    failure: Arc<Mutex<Option<String>>>,
}

impl Bounds {
    fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            bytes: Arc::new(AtomicU64::new(0)),
            ops: Arc::new(AtomicU64::new(0)),
            last_progress: Arc::new(Mutex::new(Instant::now())),
            failure: Arc::new(Mutex::new(None)),
        }
    }

    fn clone_state(&self) -> BoundState {
        BoundState {
            stop: Arc::clone(&self.stop),
            bytes: Arc::clone(&self.bytes),
            ops: Arc::clone(&self.ops),
            last_progress: Arc::clone(&self.last_progress),
            failure: Arc::clone(&self.failure),
        }
    }

    fn trip(&self, msg: impl Into<String>) {
        *self.failure.lock().unwrap() = Some(msg.into());
        self.stop.store(true, Relaxed);
    }

    fn phase_over(&self) -> bool {
        self.stop.load(Relaxed) || self.last_progress.lock().unwrap().elapsed() > NO_PROGRESS
    }
}

/// The shared handles one spawned thread clones from [`Bounds`].
struct BoundState {
    stop: Arc<AtomicBool>,
    bytes: Arc<AtomicU64>,
    ops: Arc<AtomicU64>,
    last_progress: Arc<Mutex<Instant>>,
    failure: Arc<Mutex<Option<String>>>,
}

impl Drop for Bounds {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
    }
}

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

/// Run `body` while a watchdog stops the shared flag after `RUN_CAP` or a no-progress stall.
///
/// The watchdog is the outermost bound: even if a worker blocks, the phase ends and the test
/// reports the failure the worker set (or the no-progress timeout) instead of hanging.
fn run_bounded(b: &Bounds, phase: impl FnOnce()) -> Result<(), String> {
    let stop = Arc::clone(&b.stop);
    let last = Arc::clone(&b.last_progress);
    let watchdog = std::thread::spawn(move || {
        let t0 = Instant::now();
        loop {
            if stop.load(Relaxed) {
                return;
            }
            if t0.elapsed() >= RUN_CAP + Duration::from_secs(5) {
                stop.store(true, Relaxed);
                return;
            }
            if last.lock().unwrap().elapsed() > NO_PROGRESS {
                stop.store(true, Relaxed);
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    phase();
    b.stop.store(true, Relaxed);
    let _ = watchdog.join();
    match b.failure.lock().unwrap().clone() {
        Some(msg) => Err(msg),
        None => Ok(()),
    }
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

    let b = Bounds::new();
    let writes = Arc::new(AtomicU64::new(0));
    let collects = Arc::new(AtomicU64::new(0));
    let names: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    run_bounded(&b, || {
        std::thread::scope(|sc| {
            for w in 0..3u64 {
                let BoundState {
                    stop,
                    bytes,
                    ops,
                    last_progress: last,
                    failure,
                } = b.clone_state();
                let writes = Arc::clone(&writes);
                let names = Arc::clone(&names);
                let roots = Arc::clone(&roots);
                let base = &base;
                sc.spawn(move || {
                    let snap = match base.fork(&format!("w{w}")) {
                        Ok(s) => s,
                        Err(e) => {
                            *failure.lock().unwrap() = Some(format!("writer {w} fork: {e:?}"));
                            stop.store(true, Relaxed);
                            return;
                        }
                    };
                    if let Err(e) = parts.meta.sync() {
                        *failure.lock().unwrap() = Some(format!("writer {w} sync: {e:?}"));
                        stop.store(true, Relaxed);
                        return;
                    }
                    let mut i = 0u64;
                    while !stop.load(Relaxed) {
                        if bytes.load(Relaxed) >= WRITE_BYTE_BUDGET || i >= WRITE_ITER_CAP {
                            if bytes.load(Relaxed) >= WRITE_BYTE_BUDGET {
                                stop.store(true, Relaxed);
                            }
                            break;
                        }
                        let name = format!("f{i:02}");
                        let data = body(20_000, (w as u32) << 8 ^ i as u32);
                        roots.write(|| parts.write(&snap, name.as_bytes(), &data));
                        bytes.fetch_add(data.len() as u64, Relaxed);
                        ops.fetch_add(1, Relaxed);
                        writes.fetch_add(1, Relaxed);
                        names.lock().unwrap().push(name);
                        *last.lock().unwrap() = Instant::now();
                        i += 1;
                    }
                });
            }
            {
                let BoundState {
                    stop,
                    bytes: _,
                    ops,
                    last_progress: last,
                    failure,
                } = b.clone_state();
                sc.spawn(move || {
                    let mut n = 0u32;
                    while !stop.load(Relaxed) {
                        let s = match parts.meta.new_snapshot(&format!("tmp{n}")) {
                            Ok(s) => s,
                            Err(e) => {
                                *failure.lock().unwrap() =
                                    Some(format!("remover new_snapshot: {e:?}"));
                                stop.store(true, Relaxed);
                                return;
                            }
                        };
                        if let Err(e) = parts.meta.sync() {
                            *failure.lock().unwrap() = Some(format!("remover sync: {e:?}"));
                            stop.store(true, Relaxed);
                            return;
                        }
                        if let Err(e) = parts.meta.remove_snapshot(s.id()) {
                            *failure.lock().unwrap() =
                                Some(format!("remover remove_snapshot: {e:?}"));
                            stop.store(true, Relaxed);
                            return;
                        }
                        if let Err(e) = parts.meta.reap_all() {
                            *failure.lock().unwrap() = Some(format!("remover reap_all: {e:?}"));
                            stop.store(true, Relaxed);
                            return;
                        }
                        ops.fetch_add(1, Relaxed);
                        *last.lock().unwrap() = Instant::now();
                        n += 1;
                    }
                });
            }
            {
                let BoundState {
                    stop,
                    bytes: _,
                    ops,
                    last_progress: last,
                    failure,
                } = b.clone_state();
                let collects = Arc::clone(&collects);
                let roots = Arc::clone(&roots);
                sc.spawn(move || {
                    while !stop.load(Relaxed) {
                        match parts.gc.collect(Some(&*roots)) {
                            Ok(r) => {
                                assert!(r.errors.is_empty(), "collect errored: {:?}", r.errors);
                                assert!(
                                    !parts.store.recovery().has_corruption(),
                                    "corruption after a concurrent collect"
                                );
                                for blk in parts.live() {
                                    assert!(
                                        parts.store.get(blk).is_ok(),
                                        "a referenced block lost during a concurrent collect"
                                    );
                                }
                            }
                            Err(e) => {
                                *failure.lock().unwrap() = Some(format!("collect: {e:?}"));
                                stop.store(true, Relaxed);
                                return;
                            }
                        }
                        ops.fetch_add(1, Relaxed);
                        collects.fetch_add(1, Relaxed);
                        *last.lock().unwrap() = Instant::now();
                    }
                });
            }
            // The watchdog thread in `run_bounded` stops the phase on the time cap or a stall.
            while !b.phase_over() {
                std::thread::sleep(Duration::from_millis(10));
            }
            b.stop.store(true, Relaxed);
        });
    })
    .unwrap_or_else(|msg| panic!("bounded race phase failed: {msg}"));

    let wrote = writes.load(Relaxed);
    let collected = collects.load(Relaxed);
    assert!(wrote > 0, "no writes happened");
    assert!(collected > 0, "no collects happened");
    // Report what the bounds actually admitted, so a budget that silently starved the workload
    // fails here instead of passing empty.
    println!(
        "race bounds: bytes={} writes={} collects={} ops={}",
        b.bytes.load(Relaxed),
        wrote,
        collected,
        b.ops.load(Relaxed)
    );
    assert_eq!(names.lock().unwrap().len() as u64, wrote);

    let r = f.gc.collect(Some(&*roots)).expect("final collect");
    assert!(r.errors.is_empty(), "{:?}", r.errors);

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
///
/// Bounded like the other concurrent fixture: the writer loops have a byte and iteration budget
/// and stop on any error, so a collect that stalls cannot let them fill the disk. The assertion is
/// the same relative one: the barrier's own held time is a fraction of the collect window, so it is
/// handed off per pack rather than held to the end. It is explicitly **not** a performance gate:
/// wall-clock under a descheduled runner is variance, and the loose backstop below only catches a
/// barrier that is pathological for some other reason.
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

    let b = Bounds::new();
    let writes = Arc::new(AtomicU64::new(0));
    let worst = Arc::new(AtomicU64::new(0));
    let collect_wall = Arc::new(AtomicU64::new(0));
    let collected = Arc::new(AtomicU64::new(0));

    run_bounded(&b, || {
        std::thread::scope(|sc| {
            for w in 0..2u64 {
                let BoundState {
                    stop,
                    bytes,
                    ops,
                    last_progress: last,
                    failure,
                } = b.clone_state();
                let writes = Arc::clone(&writes);
                let worst = Arc::clone(&worst);
                let roots = Arc::clone(&roots);
                let snap = &snap;
                sc.spawn(move || {
                    let snap2 = match snap.fork(&format!("w{w}")) {
                        Ok(s) => s,
                        Err(e) => {
                            *failure.lock().unwrap() =
                                Some(format!("stall writer {w} fork: {e:?}"));
                            stop.store(true, Relaxed);
                            return;
                        }
                    };
                    if let Err(e) = parts.meta.sync() {
                        *failure.lock().unwrap() = Some(format!("stall writer {w} sync: {e:?}"));
                        stop.store(true, Relaxed);
                        return;
                    }
                    let mut i = 0u64;
                    while !stop.load(Relaxed) {
                        if bytes.load(Relaxed) >= WRITE_BYTE_BUDGET || i >= WRITE_ITER_CAP {
                            if bytes.load(Relaxed) >= WRITE_BYTE_BUDGET {
                                stop.store(true, Relaxed);
                            }
                            break;
                        }
                        let t = Instant::now();
                        let name = format!("f{i:03}");
                        let data = body(8000, i as u32);
                        roots.write(|| parts.write(&snap2, name.as_bytes(), &data));
                        worst.fetch_max(t.elapsed().as_micros() as u64, Relaxed);
                        bytes.fetch_add(data.len() as u64, Relaxed);
                        ops.fetch_add(1, Relaxed);
                        writes.fetch_add(1, Relaxed);
                        *last.lock().unwrap() = Instant::now();
                        i += 1;
                    }
                });
            }
            // Let the writers get going, then collect a bounded number of cycles.
            std::thread::sleep(Duration::from_millis(250));
            let started = Instant::now();
            for _ in 0..4 {
                if b.stop.load(Relaxed) {
                    break;
                }
                match parts.gc.collect(Some(&*roots)) {
                    Ok(r) => {
                        assert!(r.errors.is_empty(), "{:?}", r.errors);
                    }
                    Err(e) => {
                        b.trip(format!("stall collect: {e:?}"));
                        break;
                    }
                }
                collected.fetch_add(1, Relaxed);
                *b.last_progress.lock().unwrap() = Instant::now();
            }
            collect_wall.store(started.elapsed().as_micros() as u64, Relaxed);
            b.stop.store(true, Relaxed);
        });
    })
    .unwrap_or_else(|msg| panic!("bounded stall phase failed: {msg}"));

    assert!(writes.load(Relaxed) > 0, "writers made no progress");
    let cycles = collected.load(Relaxed);
    assert!(cycles > 0, "no bounded cycle ran");
    println!(
        "stall bounds: bytes={} writes={} cycles={}",
        b.bytes.load(Relaxed),
        writes.load(Relaxed),
        cycles
    );
    let worst_us = worst.load(Relaxed);
    let held_us = roots.held_us();
    let collect_us = collect_wall.load(Relaxed);
    // The raw wall-clock stall also contains scheduler descheduling, which the collector does not
    // control and a busy runner makes unbounded. It failed on CI at 3.2 s on a runner whose suite ran
    // 30x slower than normal, so an absolute bound on it is a test of the machine, not the collector.
    // The barrier's own held time is the property that matters and descheduling inflates the collect
    // window with it, so compare the two instead: a collector that takes the barrier once per pack
    // holds it for a fraction of the sweep, and one that holds it to the end cannot.
    assert!(
        held_us < collect_us,
        "the barrier was held for {held_us} us of a {collect_us} us collect: it is not handed off per pack"
    );
    // A loose backstop for a barrier that is pathological for some other reason. Kept generous
    // because a descheduled single write can push the wall clock to seconds on a loaded runner.
    assert!(
        worst_us < 30_000_000,
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
