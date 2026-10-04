//! Process-level watchdog around a resource-sensitive fixture (issue 83).
//!
//! The actual race fixtures now run under this watchdog from `tests/race.rs` (the real
//! resource-sensitive entrypoints). This file proves the *helper's* mechanics on a smaller real
//! `cowfs-core` fixture and exercises its failure paths, so the guard itself is tested independently
//! of the racing workload:
//! - a happy child runs a real collect to a real reclaim, and the parent accepts it;
//! - a child that parks its collector forever is killed by the parent at the deadline, and the
//!   parent FAILS (it never returns PASS or SKIP);
//! - a child that fails generically exits non-zero promptly, and the parent fails with its log;
//! - the helper rejects a bad filter (zero tests), a preset inherited guard, and a nonce mismatch.
//!
//! The parent owns the child's deadline and polls `try_wait`, so it detects a prompt non-zero exit
//! and otherwise ends at a fixed bound. It kills only the child it spawned and never signals a
//! process group.

mod common;

use std::time::Duration;

use common::child::{
    child_log, descendant_secs, is_child, is_child_given, is_fail_child, is_park_child,
    run_child_fixture, run_guard_only_child, DESC_ENV, FAIL_ENV, NONCE_ENV, PARK_ENV,
};
use cowfs_core::{Core, Options as CoreOptions, SnapshotView};
use cowfs_gc::Options as GcOptions;
use cowfs_vfs::{Vfs, ROOT_INO};

const FIXTURE: &str = "a_real_collect_reclaims_and_reads_back_under_a_parent_deadline";
const EVIDENCE: &str = "CHILD_FIXTURE_OK";

fn core_opts() -> CoreOptions {
    CoreOptions {
        background: false,
        store: cowfs_store::Options {
            max_pack_size: 64 << 10,
            ..cowfs_store::Options::default()
        },
        file_flush_bytes: 32 << 10,
        flush_interval: Duration::from_millis(20),
        sync_interval: Duration::from_millis(50),
        ..CoreOptions::default()
    }
}

fn gc_opts() -> GcOptions {
    GcOptions {
        dead_ratio: 0.0,
        min_dead_bytes: 1,
        io_budget_bytes: 0,
        batch_bytes: 4096,
        ..GcOptions::default()
    }
}

fn body(n: usize, seed: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut h = seed.wrapping_mul(2654435761).wrapping_add(1);
    for _ in 0..n {
        h = h.wrapping_mul(1664525).wrapping_add(1013904223);
        out.push((h >> 16) as u8);
    }
    out
}

fn put_file(v: &SnapshotView, name: &str, data: &[u8]) {
    let a = v.create(ROOT_INO, name.as_bytes(), 0o644).expect("create");
    assert_eq!(v.write(a.ino, 0, data).expect("write") as usize, data.len());
    v.forget(a.ino, 1);
}

fn read_file(v: &SnapshotView, name: &str) -> Result<Vec<u8>, cowfs_vfs::Error> {
    let a = v.lookup(ROOT_INO, name.as_bytes())?;
    let mut out = Vec::new();
    let mut err = None;
    while (out.len() as u64) < a.size {
        match v.read(a.ino, out.len() as u64, 1 << 20) {
            Ok(part) if part.is_empty() => break,
            Ok(part) => out.extend(part),
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    v.forget(a.ino, 1);
    err.map_or(Ok(out), Err)
}

/// The child fixture body: a real store with real garbage, a real collect that reclaims real
/// packs, and a survivor read back after a reopen with `fsck` clean. Prints one evidence line the
/// parent requires, so a child that exits 0 without doing the work cannot pass.
fn child_fixture_body(park: bool, fail: bool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = Core::open(dir.path(), core_opts()).expect("open");
    child_log(&format!("phase=open park={park} fail={fail}"));

    // A keeper snapshot and a dropper snapshot whose files interleave in the packs.
    core.create_snapshot("keep").expect("keep");
    core.create_snapshot("drop").expect("drop");
    let kv = core.snapshot_view("keep").expect("view");
    let dv = core.snapshot_view("drop").expect("view");
    let survivor = body(40_000, 4242);
    put_file(&kv, "K", &survivor);
    for i in 0..20u32 {
        put_file(&kv, &format!("k{i:02}"), &body(40_000, i));
        put_file(&dv, &format!("d{i:02}"), &body(40_000, 1000 + i));
        core.sync().expect("sync");
    }
    core.remove_snapshot("drop").expect("remove drop");
    core.sync().expect("sync");
    drop(dv);
    drop(kv);
    child_log("phase=setup-done");

    let c = core.collector(gc_opts()).expect("collector");
    if fail {
        // A generic child failure (a panic on the child's main thread): proves exit-code
        // propagation. It is not a writer-thread failure; that is exercised inside the actual race
        // fixture in `race.rs`.
        child_log("phase=failing");
        panic!("injected child failure (generic failure control)");
    }
    if park {
        // Park the collector forever inside a seam that runs with the cycle lock held. The
        // in-process scope would join this forever; only the parent deadline ends the job.
        child_log("phase=parking-collector");
        c.gc().set_between_list_and_walk(Box::new(|| {
            child_log("phase=parked");
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }));
    }

    let r = c.collect().expect("collect");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        r.packs_unlinked >= 1 && r.freed_bytes > 0,
        "the fixture must reclaim real dead packs: {r:?}"
    );
    child_log(&format!(
        "phase=collected unlinked={} freed={}",
        r.packs_unlinked, r.freed_bytes
    ));
    let unlinked = r.packs_unlinked;
    let freed = r.freed_bytes;
    drop(c);
    core.close().expect("close");

    let core = Core::open(dir.path(), core_opts()).expect("reopen");
    core.drop_caches();
    let fv = core.snapshot_view("keep").expect("survivor view");
    let got = read_file(&fv, "K").expect("survivor K reads after reopen");
    assert_eq!(
        got, survivor,
        "the survivor's bytes are unchanged after reopen"
    );
    let fs = core.fsck().expect("fsck");
    assert!(fs.damage.is_empty(), "fsck damage: {:?}", fs.damage);
    drop(fv);
    core.close().expect("close");
    // The evidence line the parent requires, with the actual counts and the nonce it was started
    // with. A child that skipped its work, a stale line, or a filter typo never produces this.
    println!(
        "{EVIDENCE} unlinked={unlinked} freed={freed} nonce={}",
        common::child::child_nonce()
    );
}

/// Happy path under the parent watchdog: a real reclaim, a survivor read after reopen, and the
/// parent accepts it after parsing the child's nonce + counts. Also the child body when
/// `CHILD_ENV` is set.
#[test]
fn a_real_collect_reclaims_and_reads_back_under_a_parent_deadline() {
    if is_child() {
        child_fixture_body(is_park_child(), is_fail_child());
        return;
    }
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("child.log");
    let (outcome, nonce) = run_child_fixture(FIXTURE, &[], Duration::from_secs(120), &log);
    assert!(
        outcome.succeeded(EVIDENCE, &nonce),
        "happy child must complete its fixture: code={:?} timed_out={} drain_expired={} waited={}ms\n{}",
        outcome.code,
        outcome.timed_out,
        outcome.drain_expired,
        outcome.waited.as_millis(),
        outcome.output
    );
    // Parse the actual counts, not just the marker.
    let (unlinked, freed) = parse_counts(&outcome.output, EVIDENCE);
    assert!(
        unlinked >= 1 && freed > 0,
        "the child's reclaim must be real and non-zero: unlinked={unlinked} freed={freed}"
    );
}

/// Negative control: a child that parks its collector forever must be killed by the parent at the
/// deadline, and the parent must FAIL. A tiny deadline keeps this fast; the child's own cooperative
/// bounds never fire, so this is exactly the case the in-process watchdog cannot end.
#[test]
fn a_permanently_parked_collector_is_killed_by_the_parent_and_the_parent_fails() {
    if is_child() {
        child_fixture_body(is_park_child(), is_fail_child());
        return;
    }
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("park.log");
    let deadline = Duration::from_secs(20);
    let (outcome, nonce) = run_child_fixture(FIXTURE, &[(PARK_ENV, "1")], deadline, &log);
    // The seam must actually have run. `phase=parking-collector` is logged before the seam is
    // installed, so a slow child that never installed it would still show it. `phase=parked` is
    // logged only from inside the seam, so requiring it separates "parked" from "slow".
    let logtext = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        logtext.contains("phase=parked"),
        "the collector must actually reach the parked seam, not merely be slow: {logtext:?}"
    );
    // The parent must have ended the child itself, within the declared bound.
    assert!(
        outcome.timed_out,
        "the parent must kill the parked child: code={:?} waited={}ms\n{}",
        outcome.code,
        outcome.waited.as_millis(),
        outcome.output
    );
    assert!(
        outcome.waited < deadline + Duration::from_secs(10),
        "the parent's hard deadline must bound the job: waited={}ms",
        outcome.waited.as_millis()
    );
    // The child reached the park phase and never printed the success line.
    assert!(
        !outcome.succeeded(EVIDENCE, &nonce),
        "a parked child must never pass"
    );
    assert!(
        logtext.contains("phase=setup-done"),
        "the child did real setup before it parked: {logtext:?}"
    );
    // The child was killed mid-park: it never logged the collect finishing. This is the proof that
    // the parent deadline, not the child's own cooperative bounds, ended the job. Without the parent
    // deadline this call would never return (the child's main thread is blocked in the seam forever,
    // which is the reproduced old-wrapper behavior: an external 40 s alarm killed the in-process test
    // with rc=142).
    assert!(
        !logtext.contains("phase=collected"),
        "the parked child must never reach the post-collect phase: {logtext:?}"
    );
}

/// Negative control for the parked discriminator: the small fixture with `park` requested but the
/// seam removed and a 30 s sleep instead. It is slow, not parked, so it never logs `phase=parked`;
/// the parked requirement must reject it even though it times out. Short deadline.
#[test]
fn a_slow_child_without_the_seam_is_not_a_parked_child() {
    if is_child() {
        if is_park_child() {
            child_log("phase=setup-done");
            child_log("phase=parking-collector");
            std::thread::sleep(Duration::from_secs(30));
        }
        return;
    }
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("slow.log");
    let deadline = Duration::from_secs(8);
    let (outcome, nonce) = run_child_fixture(
        "a_slow_child_without_the_seam_is_not_a_parked_child",
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
    assert!(
        !outcome.succeeded(EVIDENCE, &nonce),
        "a slow, seam-less child must never pass"
    );
    let logtext = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !logtext.contains("phase=parked"),
        "a seam-less slow child must never log the parked phase: {logtext:?}"
    );
}

/// Negative control: a child that fails generically (a panic on its main thread) must exit non-zero
/// promptly, and the parent must fail with the child's log rather than wait out the deadline. This
/// proves exit-code propagation for a main-thread panic. The distinct real writer-thread failure
/// path is covered by `race.rs::a_writer_thread_failure_stops_the_phase_and_fails_the_parent`.
#[test]
fn a_generic_child_failure_exits_nonzero_and_reaches_the_parent() {
    if is_child() {
        child_fixture_body(is_park_child(), is_fail_child());
        return;
    }
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("fail.log");
    let deadline = Duration::from_secs(60);
    let (outcome, nonce) = run_child_fixture(FIXTURE, &[(FAIL_ENV, "1")], deadline, &log);
    assert!(
        !outcome.timed_out,
        "the failing child must exit on its own, not need the deadline: waited={}ms",
        outcome.waited.as_millis()
    );
    assert_ne!(
        outcome.code,
        Some(0),
        "a failing child must exit non-zero: {:?}\n{}",
        outcome.code,
        outcome.output
    );
    assert!(
        outcome.output.contains("injected child failure"),
        "the child's failure is visible to the parent: {}",
        outcome.output
    );
    assert!(
        !outcome.succeeded(EVIDENCE, &nonce),
        "a failing child must never pass"
    );
    let logtext = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        logtext.contains("phase=setup-done"),
        "the failing child did real setup first: {logtext:?}"
    );
}

/// The helper rejects a filter that matches no test: a bare name that runs zero tests exits 0 but
/// prints no evidence and carries no nonce, so the parent must not accept it.
#[test]
fn a_filter_that_matches_no_test_does_not_pass() {
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("badfilter.log");
    let (outcome, nonce) = run_child_fixture(
        "this_test_name_does_not_exist",
        &[],
        Duration::from_secs(30),
        &log,
    );
    assert!(
        !outcome.succeeded(EVIDENCE, &nonce),
        "a zero-test child must never pass: code={:?} timed_out={} output={}",
        outcome.code,
        outcome.timed_out,
        outcome.output
    );
    assert!(
        outcome.output.contains("0 tests") || !outcome.output.contains(EVIDENCE),
        "the child ran no tests: {}",
        outcome.output
    );
}

/// A preset inherited guard must not turn a process into the child body. The guard alone (no nonce)
/// must fail the dispatch predicate, and a real process started with `CHILD_ENV=1` but no nonce must
/// run the parent path, not the fixture body. No in-process `set_var`/`remove_var`, so concurrent
/// tests cannot race; the child is bounded by a short deadline and is verified before any kill.
#[test]
fn a_preset_inherited_guard_does_not_bypass_the_parent() {
    // The dispatch predicate: with the guard present and no nonce, this is not the child.
    assert!(
        !is_child_given((true, false)),
        "an inherited guard alone must not classify a process as the child"
    );
    assert!(
        is_child_given((true, true)),
        "the guard plus a nonce is the child"
    );
    // A real process with the guard set but no nonce must take the parent branch. The dedicated body
    // logs which branch it took, so the assertion is discriminating rather than an empty pass.
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("preset.log");
    let outcome = run_guard_only_child(
        "a_guard_only_process_takes_the_parent_branch",
        Duration::from_secs(20),
        &log,
    );
    assert!(
        !outcome.timed_out,
        "the guard-only process must return promptly, not park: code={:?} waited={}ms",
        outcome.code,
        outcome.waited.as_millis()
    );
    let logtext = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        logtext.contains("phase=ran-parent-path"),
        "the guard-only process must take the parent branch: {logtext:?}"
    );
    assert!(
        !logtext.contains("phase=wrongly-ran-body"),
        "an inherited guard must never dispatch into the body: {logtext:?}"
    );
}

/// The body `a_preset_inherited_guard_does_not_bypass_the_parent` spawns with the guard set and no
/// nonce. If the dispatch were wrong this would run `child_fixture_body` and park; taking the parent
/// branch logs the marker the control asserts.
#[test]
fn a_guard_only_process_takes_the_parent_branch() {
    if is_child() {
        child_log("phase=wrongly-ran-body");
        std::thread::sleep(Duration::from_secs(3600));
    }
    child_log("phase=ran-parent-path");
}

/// The nonce is required: a child line that carries the evidence marker but not this run's nonce is
/// not a success. Exercised directly on the outcome shape.
#[test]
fn an_evidence_line_without_the_run_nonce_is_rejected() {
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("nonce.log");
    let (outcome, nonce) = run_child_fixture(
        FIXTURE,
        &[(NONCE_ENV, "wrong-nonce")],
        Duration::from_secs(120),
        &log,
    );
    // The child echoes the nonce it was actually started with; the parent's own nonce differs, so
    // a forged or stale line cannot satisfy both.
    assert!(
        !outcome.succeeded(EVIDENCE, &nonce),
        "a mismatched nonce must not pass: nonce={nonce} output={}",
        outcome.output
    );
}

/// The finite drain budget must report an expiry when a descendant holds the inherited pipe open,
/// and the parent must FAIL rather than accept the child as succeeded. The child here prints its
/// evidence line and exits 0 while a bounded descendant holds stdout/stderr for longer than the
/// budget, so the only reason the drain ended is the budget, not a disconnect.
#[test]
fn a_descendant_holding_the_pipe_open_is_reported_as_a_drain_expiry() {
    if is_child() {
        if let Some(secs) = descendant_secs() {
            // Spawn a bounded descendant that inherits our stdout/stderr and holds them open. It
            // ends by itself; it is never killed by the parent. We do not wait on it.
            let _ = std::process::Command::new("sleep")
                .arg(secs.to_string())
                .spawn();
            // Print the evidence line and exit 0 promptly, leaving the descendant holding the pipe.
            let dir = tempfile::tempdir().expect("tempdir");
            let core = Core::open(dir.path(), core_opts()).expect("open");
            core.create_snapshot("keep").expect("keep");
            core.sync().expect("sync");
            println!(
                "{EVIDENCE} unlinked=1 freed=1 nonce={}",
                common::child::child_nonce()
            );
            return;
        }
        child_fixture_body(is_park_child(), is_fail_child());
        return;
    }
    // A descendant that outlives the 10 s drain budget but is itself bounded.
    let hold_secs = 15u64;
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("descendant.log");
    let started = std::time::Instant::now();
    let (outcome, nonce) = run_child_fixture(
        "a_descendant_holding_the_pipe_open_is_reported_as_a_drain_expiry",
        &[(DESC_ENV, &hold_secs.to_string())],
        Duration::from_secs(120),
        &log,
    );
    let elapsed = started.elapsed();
    assert!(
        !outcome.timed_out,
        "the child itself exits promptly: code={:?} waited={}ms",
        outcome.code,
        outcome.waited.as_millis()
    );
    assert!(
        outcome.drain_expired,
        "a descendant holding the pipe must be reported as a drain expiry, not a clean drain"
    );
    assert!(
        !outcome.succeeded(EVIDENCE, &nonce),
        "a drain expiry must fail the parent even though the child printed evidence and exited 0"
    );
    // Bounded: the parent waited about one drain budget, not the descendant's whole life or forever.
    assert!(
        elapsed < Duration::from_secs(hold_secs + 5),
        "the parent must bound the wait to its drain budget: elapsed={elapsed:?}"
    );
    // The child's nonzero result (here: 0 plus evidence) is preserved in the output regardless.
    assert!(
        outcome.output.contains(EVIDENCE),
        "the child's evidence is preserved even on expiry: {}",
        outcome.output
    );
}

/// Parse `unlinked=N freed=M` from the evidence line. Returns `(0, 0)` when the line or a field is
/// missing, so the caller's non-zero assertion fails instead of accepting a partial line.
fn parse_counts(output: &str, evidence: &str) -> (u64, u64) {
    let line = output
        .lines()
        .find(|l| l.contains(evidence))
        .unwrap_or_default();
    let get = |key: &str| -> u64 {
        line.split_whitespace()
            .find_map(|tok| tok.strip_prefix(key))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    };
    (get("unlinked="), get("freed="))
}
