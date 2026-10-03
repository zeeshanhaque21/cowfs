//! Process-level watchdog around a resource-sensitive fixture (issue 83).
//!
//! The other concurrent fixtures bound themselves in-process. That bounds the *writers* (they stop
//! at a byte/time budget), but it cannot end a test whose collector parks forever: those fixtures
//! call the collector inside `std::thread::scope`, so the scope joins the parked thread and the test
//! never returns. The only bound that survives that is a separate process with a deadline held by
//! its parent.
//!
//! This file proves the process-level bound, end to end, on a real `cowfs-core` GC fixture:
//! - a happy child runs a real collect to a real reclaim, and the parent accepts it;
//! - a child that parks its collector forever is killed by the parent at the deadline, and the
//!   parent FAILS (it never returns PASS or SKIP);
//! - a child whose writer fails exits non-zero promptly, and the parent fails with its log.
//!
//! The parent owns the child's deadline and polls `try_wait`, so it detects a prompt non-zero exit
//! and otherwise ends at a fixed bound. It kills only the child it spawned, after re-checking the
//! child's pid and command, and never signals a process group.

mod common;

use std::time::Duration;

use common::child::{
    child_log, is_child, is_fail_child, is_park_child, run_child_fixture, FAIL_ENV, PARK_ENV,
};
use cowfs_core::{Core, Options as CoreOptions, SnapshotView};
use cowfs_gc::Options as GcOptions;
use cowfs_vfs::{Vfs, ROOT_INO};

const FIXTURE: &str = "a_real_collect_reclaims_and_reads_back_under_a_parent_deadline";

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
        // The writer-failure path: fail before the collect, so writers would stop promptly.
        child_log("phase=arming-failure");
        panic!("injected writer failure (fail child)");
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
    // The single line the parent requires. A child that skipped its work never prints it.
    println!(
        "CHILD_FIXTURE_OK unlinked={} freed={} survivor_hash_ok=1 fsck_clean=1",
        r.packs_unlinked, r.freed_bytes
    );
}

/// Happy path under the parent watchdog: a real reclaim, a survivor read after reopen, and the
/// parent accepts it. Also the child body when `CHILD_ENV` is set.
#[test]
fn a_real_collect_reclaims_and_reads_back_under_a_parent_deadline() {
    if is_child() {
        child_fixture_body(is_park_child(), is_fail_child());
        return;
    }
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("child.log");
    let outcome = run_child_fixture(FIXTURE, &[], Duration::from_secs(120), &log);
    assert!(
        outcome.succeeded("CHILD_FIXTURE_OK"),
        "happy child must complete its fixture: code={:?} timed_out={} waited={}ms\n{}",
        outcome.code,
        outcome.timed_out,
        outcome.waited.as_millis(),
        outcome.output
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
    let outcome = run_child_fixture(FIXTURE, &[(PARK_ENV, "1")], deadline, &log);
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
        !outcome.succeeded("CHILD_FIXTURE_OK"),
        "a parked child must never pass"
    );
    // The child log proves the child actually executed the fixture and reached the park phase, so
    // this is not a child that failed to start.
    let logtext = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        logtext.contains("phase=parking-collector"),
        "the child reached the park phase (not a spawn/filter failure): {logtext:?}"
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

/// Negative control: a child whose writer fails must exit non-zero promptly, and the parent must
/// fail with the child's log rather than wait out the deadline.
#[test]
fn a_child_writer_error_fails_promptly_and_reaches_the_parent() {
    if is_child() {
        child_fixture_body(is_park_child(), is_fail_child());
        return;
    }
    let keep = tempfile::tempdir().expect("parent tempdir");
    let log = keep.path().join("fail.log");
    let deadline = Duration::from_secs(60);
    let outcome = run_child_fixture(FIXTURE, &[(FAIL_ENV, "1")], deadline, &log);
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
        outcome.output.contains("injected writer failure"),
        "the child's failure is visible to the parent: {}",
        outcome.output
    );
    assert!(
        !outcome.succeeded("CHILD_FIXTURE_OK"),
        "a failing child must never pass"
    );
    let logtext = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        logtext.contains("phase=setup-done"),
        "the failing child did real setup first: {logtext:?}"
    );
}
