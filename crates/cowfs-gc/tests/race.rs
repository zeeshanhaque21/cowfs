//! Writers, snapshot creation and removal, and collect all at once.
//!
//! The barrier is what makes this pass: `Roots::write` takes the same lock `reference_barrier`
//! holds, so a commit cannot land between the collector's last reachability check and the unlink.
//! Without a barrier the same workload would lose blocks, which is why the crate refuses to free
//! anything when no barrier is offered, and a test here proves that it does refuse.
//!
//! Every concurrent fixture here is **bounded** (issue 83): one shared byte budget across all
//! writers in a test (24 MiB of input, not per writer), a fixed iteration cap, a finite cooperative
//! run time, a no-progress watchdog, and a stop-on-error flag any worker or collector sets. Before
//! those bounds, a collector that stalled would let the writer loops append until the runner's disk
//! filled (`StorageFull` in CI). The bound is on the rate and the total, so a descheduled runner
//! stops early instead of growing without limit. The numbers each run actually reached are printed
//! (and asserted non-zero) so a bound that silently disabled the workload fails loudly instead of
//! passing empty.
//!
//! These in-process bounds are **cooperative**: they set a flag every loop checks, so they bound
//! what the writers do. They cannot end a test whose collector parks forever inside a blocking call,
//! because the collector runs inside `std::thread::scope` and the scope joins it. The three
//! concurrent fixtures below are therefore also wrapped in a **process-level watchdog**
//! (`common::child`): each named test runs its real body in a child of the same binary with a fixed
//! parent-owned deadline, and the parent fails if the child is still alive at the deadline or exits
//! non-zero. The parent requires the child's typed `race bounds:`/`stall bounds:` line with non-zero
//! work, so a filter typo or an empty run cannot pass. See `docs/gc-race-bounds.md` for the numbers.

mod common;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::child::{
    child_log, is_child, is_fail_child, is_park_child, run_child_fixture, FAIL_ENV, PARK_ENV,
};
use common::{Fixture, Roots};
use cowfs_store::BlockId;

/// Hard parent-owned deadline for one wrapped race fixture child. Explicit finite budget, not a
/// raised CI job timeout: the cooperative cap is `RUN_CAP + 5 s` (9 s), the bounded race phase then
/// does a final collect, a readback of every live block and `fsck`, and the child needs startup and
/// reopen; 120 s is a few minutes short of the runner limit and still ends a parked collector. It is
/// far longer than a passing run (seconds), so a pass never waits it out.
const CHILD_DEADLINE: Duration = Duration::from_secs(120);

/// Total input bytes one test's writers may store, **shared** across all of that test's writers
/// (`clone_state` shares one counter): 24 MiB per test, not per writer. Two bounded tests exist, so
/// the job's input bound is about 48 MiB plus at most one write of overshoot. This counts input
/// bytes only, not the stored bytes, which also include compaction copies and the metadata file.
const WRITE_BYTE_BUDGET: u64 = 24 << 20;
/// Iterations one writer thread may run, independent of the byte budget. Not the binding limit at
/// the default write size (100000 x 8000 B exceeds the byte budget); kept as a backstop.
const WRITE_ITER_CAP: u64 = 100_000;
/// Cooperative wall-clock cap for a concurrent phase, applied as `RUN_CAP + 5 s` by the in-process
/// watchdog: the phase is asked to stop at 9 s. A collector parked inside `thread::scope` is joined
/// regardless, so this is not a hard bound; the process-level deadline in `resource_watchdog.rs` is.
const RUN_CAP: Duration = Duration::from_secs(4);
/// Stop if no thread makes progress for this long: a stalled collector must not hang CI's *workers*.
/// Cooperative, like `RUN_CAP`; it cannot end a joined, parked collector.
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

/// Run `body` while an in-process watchdog stops the shared flag after `RUN_CAP + 5 s` or a stall.
///
/// This is a **cooperative** bound: it only sets a flag the workers poll. It ends the writers at the
/// time cap, but it cannot end `body` itself if a thread inside it is blocked in a join, so it is not
/// a hard runtime bound. The hard bound for the resource-sensitive fixture is the parent process
/// deadline in `tests/resource_watchdog.rs`. The failure a worker set, or a no-progress stall, is
/// still reported here.
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

/// Parse `key=value` unsigned fields from the child's typed evidence line (the line containing
/// `marker`). Returns `None` for a missing marker or any missing/unparsable field, so a fabricated
/// constant marker without real counts is rejected rather than accepted.
fn parse_fields(output: &str, marker: &str) -> Option<Vec<(String, u64)>> {
    let line = output.lines().find(|l| l.contains(marker))?;
    let tail = line.split_once(marker)?.1;
    let mut out = Vec::new();
    for tok in tail.split_whitespace() {
        let (k, v) = tok.split_once('=')?;
        out.push((k.to_string(), v.parse::<u64>().ok()?));
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

/// Fetch one parsed field by name.
fn field(fields: &[(String, u64)], key: &str) -> Option<u64> {
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| *v)
}

/// Whether every named field is present and strictly greater than zero.
fn all_nonzero(fields: &[(String, u64)], keys: &[&str]) -> bool {
    keys.iter().all(|k| field(fields, k).is_some_and(|v| v > 0))
}

/// Writers, snapshot creates, snapshot removes and collects, all at once.
///
/// After every collect, and at the end, every block any live snapshot references reads back.
///
/// Run under the process-level watchdog: this body runs in a child with a fixed parent deadline. When
/// `COWFS_GC_CHILD_PARK` is set it parks the collector forever *after real setup, before the walk*
/// through the existing `Gc` test seam, with the cycle lock held; the parent must kill it at the
/// deadline and FAIL. That negative control runs this same real writers fixture, so the guard is
/// exercised on the actual racing workload, not a demo.
#[test]
fn writers_and_collects_at_once_lose_nothing() {
    if is_child() {
        writers_and_collects_body(is_park_child(), is_fail_child());
        return;
    }
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("race-writers.log");
    let (outcome, _) = run_child_fixture(
        "writers_and_collects_at_once_lose_nothing",
        &[],
        CHILD_DEADLINE,
        &log,
    );
    assert!(
        !outcome.timed_out && outcome.code == Some(0),
        "the race child must complete under the deadline: code={:?} timed_out={} waited={}ms\n{}",
        outcome.code,
        outcome.timed_out,
        outcome.waited.as_millis(),
        outcome.output
    );
    let fields = parse_fields(&outcome.output, "race bounds:").unwrap_or_else(|| {
        panic!(
            "the child must print a typed `race bounds:` line:\n{}",
            outcome.output
        )
    });
    assert!(
        all_nonzero(&fields, &["writes", "collects", "ops"]),
        "the child must do real, non-zero concurrent work: {fields:?}\n{}",
        outcome.output
    );
}

/// Negative control: the same real writers fixture with the collector parked forever inside the
/// cycle-lock seam. The parent must kill *this* child at the deadline and FAIL; the fixture is not a
/// pass until the inner fixture is observed to time out. The child log proves the child reached the
/// park phase after real setup and never logged the collect completing.
#[test]
fn a_parked_writers_fixture_is_killed_by_the_parent_and_the_parent_fails() {
    if is_child() {
        writers_and_collects_body(is_park_child(), is_fail_child());
        return;
    }
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("race-writers-park.log");
    let deadline = Duration::from_secs(20);
    let (outcome, _) = run_child_fixture(
        "writers_and_collects_at_once_lose_nothing",
        &[(PARK_ENV, "1")],
        deadline,
        &log,
    );
    assert!(
        outcome.waited < deadline + Duration::from_secs(10),
        "the parent deadline must bound the job: waited={}ms",
        outcome.waited.as_millis()
    );
    // The seam must actually have run. `phase=parking-collector` is logged before the seam is
    // installed, so a slow child that never installed the seam would still show it. `phase=parked`
    // is logged only from inside the seam, so requiring it is what separates "parked" from "slow".
    let logtext = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        logtext.contains("phase=parked"),
        "the collector must actually reach the parked seam, not merely be slow: {logtext:?}"
    );
    assert!(
        outcome.timed_out,
        "the parent must kill the parked race child: code={:?} waited={}ms\n{}",
        outcome.code,
        outcome.waited.as_millis(),
        outcome.output
    );
    // No success evidence escaped: a parked child must never print real counts.
    assert!(
        parse_fields(&outcome.output, "race bounds:").is_none(),
        "a parked child must never print its success line: {}",
        outcome.output
    );
    assert!(
        logtext.contains("phase=race-setup-done"),
        "the child did real setup before parking: {logtext:?}"
    );
    assert!(
        !logtext.contains("phase=race-collected"),
        "the parked child was ended before the collect returned: {logtext:?}"
    );
}

/// Negative control for the park control's own discriminator: the same writers body with `park`
/// requested but the seam removed and a 30 s sleep instead. The child is slow, not parked, so it
/// never logs `phase=parked`; the parent's `parked` requirement must reject it even though it
/// times out. This is the mutant the previous control wrongly accepted (reviewer evidence), and it
/// is the reason the control requires `phase=parked`. Short deadline, one run.
#[test]
fn a_slow_writer_child_without_the_seam_is_not_a_parked_child() {
    if is_child() {
        // Simulate the mutant: log the pre-seam line as the real body does, but install no seam and
        // sleep past the deadline instead of running the fixture.
        if is_park_child() {
            child_log("phase=race-setup-done");
            child_log("phase=parking-collector");
            std::thread::sleep(Duration::from_secs(30));
        }
        return;
    }
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("race-writers-slow.log");
    let deadline = Duration::from_secs(8);
    let (outcome, _) = run_child_fixture(
        "a_slow_writer_child_without_the_seam_is_not_a_parked_child",
        &[(PARK_ENV, "1")],
        deadline,
        &log,
    );
    assert!(
        outcome.timed_out,
        "the slow child is killed at the deadline: code={:?} waited={}ms",
        outcome.code,
        outcome.waited.as_millis()
    );
    let logtext = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !logtext.contains("phase=parked"),
        "a seam-less slow child must never log the parked phase: {logtext:?}"
    );
    // The park control's discriminator rejects it: no `phase=parked` means it is not a parked child.
    assert!(
        !logtext.contains("phase=parked"),
        "and therefore the parked-child requirement would fail, which is the point of this control"
    );
}

/// A real writer-thread failure, not a main-thread panic. The child builds its fixture with a
/// metadata `before_sync` hook (`Fixture::with_hook`, existing test helper) and arms it after setup,
/// so a concurrent writer's `fork`/`sync` fails with a non-`NoSuchSnapshot` error. The writer must
/// record that failure and stop the phase, and the parent must see a prompt non-zero exit well under
/// the deadline, with the writer diagnostic in the output and no success counts. This uses no new
/// production surface.
#[test]
fn a_writer_thread_failure_stops_the_phase_and_fails_the_parent() {
    if is_child() {
        writers_and_collects_body(is_park_child(), is_fail_child());
        return;
    }
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("race-writers-fail.log");
    let (outcome, _) = run_child_fixture(
        "a_writer_thread_failure_stops_the_phase_and_fails_the_parent",
        &[(FAIL_ENV, "1")],
        CHILD_DEADLINE,
        &log,
    );
    assert!(
        !outcome.timed_out,
        "the writer failure must stop the child on its own, not need the deadline: waited={}ms",
        outcome.waited.as_millis()
    );
    assert!(
        outcome.waited < Duration::from_secs(60),
        "the writer failure must be prompt: waited={}ms",
        outcome.waited.as_millis()
    );
    assert_ne!(
        outcome.code,
        Some(0),
        "a writer failure must exit non-zero: {:?}\n{}",
        outcome.code,
        outcome.output
    );
    assert!(
        outcome.output.contains("writer") && outcome.output.contains("injected writer failure"),
        "the actual writer diagnostic must reach the parent: {}",
        outcome.output
    );
    // No success counts escaped: the phase stopped on the error, not after a clean sweep.
    assert!(
        parse_fields(&outcome.output, "race bounds:").is_none(),
        "a failed phase must not print success counts: {}",
        outcome.output
    );
    let logtext = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        logtext.contains("phase=race-setup-done"),
        "the child did real setup before the failure: {logtext:?}"
    );
}

fn writers_and_collects_body(park: bool, fail: bool) {
    // For the writer-failure control the metadata `before_sync` hook is armed only *after* the
    // fixture's own setup, so it fails a concurrent writer's `fork`/`sync`, not the setup itself.
    let armed = Arc::new(AtomicBool::new(false));
    let f = if fail {
        let armed_hook = Arc::clone(&armed);
        Fixture::with_hook(
            128 << 10,
            Arc::new(move || {
                if armed_hook.load(Relaxed) {
                    Err(std::io::Error::other("injected writer failure"))
                } else {
                    Ok(())
                }
            }),
        )
    } else {
        Fixture::eager(128 << 10)
    };
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
    child_log("phase=race-setup-done");

    if fail {
        // Arm the hook now that setup is done: the next writer `fork`/`sync` fails with a
        // non-`NoSuchSnapshot` metadata error, which the writer must turn into a recorded failure
        // that stops the phase. This exercises the real writer-thread error path, distinct from a
        // panic on the child's main thread.
        armed.store(true, Relaxed);
    }

    if park {
        // Park the collector forever inside the cycle-lock seam, after real setup. The in-process
        // scope would join this forever; only the parent deadline ends the job.
        child_log("phase=parking-collector");
        parts.gc.set_between_list_and_walk(Box::new(|| {
            child_log("phase=parked");
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }));
    }

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
    // fails here instead of passing empty. The parent parses these typed fields and rejects zeros.
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
    child_log("phase=race-collected");
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

/// The stall fixture: writers run beside a bounded number of collects, and no block is lost.
///
/// Bounded like the other concurrent fixture: the writer loops have a byte and iteration budget
/// and stop on any error, so a collect that stalls cannot let them fill the disk.
///
/// This is a **functional** concurrency check only: it asserts that the barrier is taken, that the
/// writers and the collector both make progress, and that a collect causes no loss. It is **not** a
/// latency or fairness gate. An earlier version compared the barrier's held time against the
/// collect window (`held_us < collect_us`) on the theory that a collector holding one barrier to the
/// end of the sweep could not pass it; that was reproduced false - a mutant that takes the barrier
/// once and holds it for the whole sweep, and one that holds it for the whole cycle, both pass it
/// (reviewer evidence, issue 83). The ratio does not discriminate the hand-off property, so it is
/// gone and no hand-off or latency claim is made here. The hand-off itself is covered by the gate
/// unit test in `cowfs-core`, not by this wall-clock fixture.
#[test]
fn the_barrier_costs_writers_a_bounded_stall() {
    if is_child() {
        the_barrier_costs_writers_a_bounded_stall_body();
        return;
    }
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("race-stall.log");
    let (outcome, _) = run_child_fixture(
        "the_barrier_costs_writers_a_bounded_stall",
        &[],
        CHILD_DEADLINE,
        &log,
    );
    assert!(
        !outcome.timed_out && outcome.code == Some(0),
        "the stall child must complete under the deadline: code={:?} timed_out={} waited={}ms\n{}",
        outcome.code,
        outcome.timed_out,
        outcome.waited.as_millis(),
        outcome.output
    );
    let fields = parse_fields(&outcome.output, "stall bounds:").unwrap_or_else(|| {
        panic!(
            "the child must print a typed `stall bounds:` line:\n{}",
            outcome.output
        )
    });
    assert!(
        all_nonzero(&fields, &["writes", "cycles", "barriers"]),
        "the stall child must take the barrier and make real progress: {fields:?}\n{}",
        outcome.output
    );
}

fn the_barrier_costs_writers_a_bounded_stall_body() {
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
                        let name = format!("f{i:03}");
                        let data = body(8000, i as u32);
                        roots.write(|| parts.write(&snap2, name.as_bytes(), &data));
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
            b.stop.store(true, Relaxed);
        });
    })
    .unwrap_or_else(|msg| panic!("bounded stall phase failed: {msg}"));

    assert!(writes.load(Relaxed) > 0, "writers made no progress");
    let cycles = collected.load(Relaxed);
    assert!(cycles > 0, "no bounded cycle ran");
    // The barrier must actually have been taken: a run that freed nothing and never closed the
    // gate would not exercise the concurrency this test exists for.
    assert!(
        roots.barrier_taken() > 0,
        "the barrier must be taken at least once"
    );
    println!(
        "stall bounds: bytes={} writes={} cycles={} barriers={}",
        b.bytes.load(Relaxed),
        writes.load(Relaxed),
        cycles,
        roots.barrier_taken()
    );
    // Every block a live snapshot still references reads back: the concurrency lost nothing.
    for blk in parts.live() {
        assert!(
            parts.store.get(blk).is_ok(),
            "a live block is gone after the stall fixture"
        );
    }
    child_log("phase=stall-collected");
}

/// Two collectors at once: neither loses a block and neither reports an error.
///
/// Wrapped like the other concurrent fixtures: the collectors run inside `thread::scope`, so a
/// collector that parked would hang the scope. The parent owns the deadline.
#[test]
fn two_collectors_on_one_store_are_safe() {
    if is_child() {
        two_collectors_on_one_store_are_safe_body();
        return;
    }
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("race-two-collectors.log");
    let (outcome, _) = run_child_fixture(
        "two_collectors_on_one_store_are_safe",
        &[],
        CHILD_DEADLINE,
        &log,
    );
    assert!(
        !outcome.timed_out && outcome.code == Some(0),
        "the two-collector child must complete under the deadline: code={:?} timed_out={} waited={}ms\n{}",
        outcome.code,
        outcome.timed_out,
        outcome.waited.as_millis(),
        outcome.output
    );
    assert!(
        outcome.output.contains("two-collectors done"),
        "the child must report its real completion line:\n{}",
        outcome.output
    );
}

fn two_collectors_on_one_store_are_safe_body() {
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
    println!("two-collectors done barriers={}", roots.barrier_taken());
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
