//! The control surface: dry run, progress, cancel, exact byte accounting, and the hint file.

mod common;

use std::sync::{Arc, Mutex};

use common::{eager, Fixture, Roots};
use cowfs_gc::{Options, SkipReason};

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

/// A store with one live file and enough garbage for several packs to be reclaimed.
fn garbage() -> Fixture {
    let f = Fixture::eager(32 << 10);
    let snap = f.meta.new_snapshot("s").expect("snapshot");
    f.write(&snap, b"keep", &body(60_000, 1));
    for i in 0..40u32 {
        f.store.put(&body(4000, i)).expect("put");
    }
    f.store.sync().expect("sync");
    f.meta.sync().expect("meta sync");
    f
}

/// The collector's recorded incremental set, or `None` when it has none yet.
fn marks(f: &Fixture) -> Option<Vec<u8>> {
    std::fs::read(f.gc_dir().join("mark.bin")).ok()
}

/// A dry run leaves the store directory byte-identical, and says what it would have freed.
#[test]
fn a_dry_run_changes_nothing_and_still_reports() {
    let f = garbage();
    let roots = Roots::new();
    let before = f.store_files();
    let packs_before = f.store.stats().packs;
    let marks_before = marks(&f);

    let g = cowfs_gc::Gc::open(
        f.gc_dir(),
        Arc::clone(&f.store),
        Arc::clone(&f.meta),
        Options {
            dry_run: true,
            ..eager()
        },
    )
    .expect("gc");
    let r = g.collect(Some(&*roots)).expect("collect");

    assert!(r.dry_run, "the report says it was a dry run");
    assert!(r.candidates > 0, "candidates are still found: {r:?}");
    assert!(r.candidate_bytes > 0, "and their bytes counted");
    assert_eq!(r.freed_bytes, 0, "a dry run frees nothing");
    assert_eq!(r.packs_unlinked, 0, "and unlinks nothing");
    assert_eq!(r.packs_rewritten, 0, "and copies nothing");
    assert_eq!(
        f.store_files(),
        before,
        "the store directory is byte-identical"
    );
    assert_eq!(f.store.stats().packs, packs_before);
    for b in f.live_blocks() {
        assert!(f.store.get(b).is_ok(), "every live block reads");
    }

    // The collector's own state is untouched too, so a dry run does not consume the incremental
    // set a real cycle would have built.
    assert_eq!(
        marks(&f),
        marks_before,
        "a dry run leaves the recorded set exactly as it found it"
    );
}

/// A dry run over a set an earlier cycle recorded leaves those exact bytes, so the set is read and
/// not consumed: the second cycle still skips the root the first one walked.
#[test]
fn a_dry_run_does_not_consume_a_recorded_set() {
    let f = garbage();
    let roots = Roots::new();

    // A real cycle records the set a following cycle can seed from.
    f.gc.collect(Some(&*roots)).expect("real collect");
    let recorded = marks(&f).expect("a real cycle records the set it walked");

    let g = cowfs_gc::Gc::open(
        f.gc_dir(),
        Arc::clone(&f.store),
        Arc::clone(&f.meta),
        Options {
            dry_run: true,
            ..eager()
        },
    )
    .expect("gc");
    let r = g.collect(Some(&*roots)).expect("collect");

    assert_eq!(
        r.marked_skipped_roots, 1,
        "the dry run seeded from the recorded set instead of walking again: {r:?}"
    );
    assert_eq!(
        marks(&f).as_deref(),
        Some(recorded.as_slice()),
        "the recorded set is byte-identical after a dry run read it"
    );
}

/// Removing a snapshot and collecting frees exactly the bytes the report says it does.
#[test]
fn a_removed_snapshot_reclaims_exactly_the_reported_bytes() {
    let f = Fixture::eager(32 << 10);
    let roots = Roots::new();
    let keep = f.meta.new_snapshot("keep").expect("snapshot");
    f.write(&keep, b"keep", &body(40_000, 1));
    f.meta.sync().unwrap();

    // A second snapshot that will be removed, holding a known number of blocks.
    let doomed = keep.fork("doomed").expect("fork");
    let mut data = Vec::new();
    for i in 0..20u32 {
        let d = body(9000, 200 + i);
        f.write(&doomed, format!("d{i:02}").as_bytes(), &d);
        data.push(d);
    }
    f.meta.sync().unwrap();
    f.store.sync().unwrap();
    let doomed_blocks: Vec<_> = data
        .iter()
        .flat_map(|d| f.store.ingest_bytes(d).expect("ingest"))
        .map(|c| c.id)
        .collect();
    let before = f.store.stats().pack_bytes;

    f.meta.remove_snapshot(doomed.id()).unwrap();
    f.meta.reap_all().unwrap();
    f.meta.sync().unwrap();

    // One cycle with a budget that cannot finish, then more, until it is done.
    let mut total = 0u64;
    let mut rounds = 0;
    loop {
        let r = f.gc.collect(Some(&*roots)).expect("collect");
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        total += r.freed_bytes;
        rounds += 1;
        if r.freed_bytes == 0 || rounds > 8 {
            break;
        }
    }

    let after = f.store.stats().pack_bytes;
    assert_eq!(
        before - after,
        total,
        "the bytes that came back are the bytes the report said"
    );
    assert!(total > 0, "removing a snapshot reclaimed nothing");
    for b in f.live_blocks() {
        assert!(f.store.get(b).is_ok(), "the kept snapshot is intact");
    }
    for b in &doomed_blocks {
        let _ = f.store.get(*b);
    }
    assert!(f.store.fsck().expect("fsck").is_clean());
    assert!(!f.store.recovery().has_corruption());
}

/// Progress is streamed and monotonic, and the total is the number of candidates.
#[test]
fn progress_is_streamed_and_ends_at_the_candidate_count() {
    let f = garbage();
    let roots = Roots::new();
    let seen: Arc<Mutex<Vec<cowfs_gc::Progress>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    f.gc.set_progress(move |p| sink.lock().unwrap().push(*p));

    let r = f.gc.collect(Some(&*roots)).expect("collect");
    let seen = seen.lock().unwrap().clone();
    assert!(!seen.is_empty(), "no progress was reported");
    assert_eq!(seen.last().unwrap().packs_done, r.candidates);
    assert_eq!(seen.last().unwrap().packs_total, r.candidates);
    assert!(
        seen.last().unwrap().sweeping,
        "the last report is from the sweep phase"
    );
    assert!(
        seen.last().unwrap().freed_bytes > 0,
        "and carries the freed bytes"
    );
    for w in seen.windows(2) {
        assert!(
            w[1].packs_done >= w[0].packs_done,
            "progress went backwards"
        );
    }
    // A second collector replaces the callback rather than adding to it.
    f.gc.set_progress(|_| panic!("the old callback is gone"));
    let _ = f.gc.collect(Some(&*roots)).expect("collect");
}

/// A cancel stops the cycle, is reported, and the next cycle after `resume` does the work.
#[test]
fn a_cancel_stops_the_cycle_and_is_reported() {
    let f = garbage();
    let roots = Roots::new();
    let before = f.store.stats().pack_bytes;
    f.gc.cancel();
    assert!(f.gc.is_cancelled(), "the cancel is visible");
    let r = f.gc.collect(Some(&*roots)).expect("collect");
    assert_eq!(r.freed_bytes, 0, "a cancelled cycle frees nothing");
    assert!(r.packs_rewritten == 0, "and copies nothing");
    assert!(
        r.skipped.iter().any(|s| s.reason == SkipReason::NotReached),
        "the packs are reported as not reached: {:?}",
        r.skipped
    );
    assert_eq!(
        f.store.stats().pack_bytes,
        before,
        "the packs are untouched"
    );
    f.gc.resume();
    assert!(!f.gc.is_cancelled());
    let r2 = f.gc.collect(Some(&*roots)).expect("collect");
    assert!(r2.freed_bytes > 0, "the resumed cycle reclaims: {r2:?}");
    for b in f.live_blocks() {
        assert!(f.store.get(b).is_ok());
    }
}

/// A cancel raised while a cycle runs stops it, and the store is left consistent.
#[test]
fn a_cancel_mid_cycle_leaves_the_store_consistent() {
    let f = garbage();
    let roots = Roots::new();
    // Cancel from inside the progress callback, which the collector calls from its own copy loop,
    // so the cancel lands inside the cycle deterministically. A cancel from a second thread races
    // the loop: on a fast machine the whole sweep finishes first, and then there is no mid-cycle
    // cancel to observe at all. The callback is 'static, so it holds an owned collector.
    let gc = Arc::new(
        cowfs_gc::Gc::open(
            f.gc_dir(),
            Arc::clone(&f.store),
            Arc::clone(&f.meta),
            eager(),
        )
        .expect("gc"),
    );
    let cancel = Arc::clone(&gc);
    gc.set_progress(move |p| {
        if p.packs_done == 1 {
            cancel.cancel();
        }
    });
    let r = gc.collect(Some(&*roots)).expect("collect");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    // The cancel landed mid-cycle, which is what this test is for: some packs were reached and the
    // rest were not. Without this the assertions below pass vacuously on a fast machine.
    assert!(
        r.packs_unlinked < r.candidates,
        "the cancel stopped the cycle part way: {r:?}"
    );
    for b in f.live_blocks() {
        assert!(f.store.get(b).is_ok(), "a cancel never loses a live block");
    }
    assert!(f.store.fsck().expect("fsck").is_clean());
    gc.resume();
    let r2 = gc.collect(Some(&*roots)).expect("collect");
    assert!(r2.errors.is_empty(), "{:?}", r2.errors);
    // The work the cancel stopped is still there, so the next cycle reclaims it.
    assert!(
        r2.freed_bytes > 0,
        "the cycle after the cancel reclaims what it stopped: {r2:?}"
    );
    for b in f.live_blocks() {
        assert!(f.store.get(b).is_ok());
    }
}

/// A cancel raised mid-cycle stops the copy loop, and every pack the cycle did copy is unlinked.
///
/// The cancel bounds how much work a cycle starts, not what it finishes: a pack that was copied and
/// indexed is unlinked even after the cancel, because leaving it leaves its source and its copy both
/// on disk and the next cycle redoes the copy.
#[test]
fn a_cancel_stops_the_copy_loop_but_finishes_what_it_copied() {
    let f = garbage();
    let roots = Roots::new();
    let gc = Arc::new(
        cowfs_gc::Gc::open(
            f.gc_dir(),
            Arc::clone(&f.store),
            Arc::clone(&f.meta),
            eager(),
        )
        .expect("gc"),
    );
    let cancel = Arc::clone(&gc);
    gc.set_progress(move |p| {
        if p.packs_done == 1 {
            cancel.cancel();
        }
    });
    let r = gc.collect(Some(&*roots)).expect("collect");

    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        r.candidates > 1,
        "the fixture must have more than one candidate, or this proves nothing: {r:?}"
    );
    // The cycle stopped before every candidate, so the packs it never reached are the ones that
    // stay, and the packs it did copy are the ones that went.
    assert_eq!(
        r.packs_rewritten, r.packs_unlinked,
        "every copy that was made was also unlinked, so no pack is left behind: {r:?}"
    );
    assert!(
        r.packs_rewritten < r.candidates,
        "the cancel stopped the cycle part way, so some candidates were never copied: {r:?}"
    );
    assert!(
        r.skipped.iter().any(|s| s.reason == SkipReason::NotReached),
        "and the packs it never reached say so: {:?}",
        r.skipped
    );
    assert!(f.store.fsck().expect("fsck").is_clean());
    for b in f.live_blocks() {
        assert!(f.store.get(b).is_ok(), "a cancel never loses a live block");
    }

    // And the resumed cycle finishes the rest.
    gc.resume();
    let r2 = gc.collect(Some(&*roots)).expect("collect");
    assert!(r2.errors.is_empty(), "{:?}", r2.errors);
    assert!(r2.freed_bytes > 0, "and it reclaims: {r2:?}");
    assert!(f.store.fsck().expect("fsck").is_clean());
    for b in f.live_blocks() {
        assert!(f.store.get(b).is_ok());
    }
}

/// An access hint never becomes a write, and the batch file is append-only.
#[test]
fn a_read_never_writes_and_hints_flush_in_batches() {
    let f = garbage();
    let b = f.store.put(&body(4000, 3)).unwrap();
    let hint_file = f.gc_dir().join("atime.bin");
    assert!(!hint_file.exists(), "a put does not create the hint file");
    let before: Vec<String> = f.store_files().iter().map(|(n, _)| n.clone()).collect();
    assert!(
        !before.iter().any(|n| n == "atime.bin"),
        "the store never gains one"
    );

    f.gc.note_access(b);
    f.gc.note_access(b);
    f.gc.note_access(cowfs_gc::HOLE);
    assert!(f.gc.last_access(&b) > 0, "the hint is in memory");
    assert!(!hint_file.exists(), "and a read does not write it");
    assert_eq!(
        f.gc.flush_hints().unwrap(),
        1,
        "one record, however many times it was noted"
    );
    assert!(hint_file.exists(), "the batch file is written by the flush");
    let len = std::fs::metadata(&hint_file).unwrap().len();

    // A second flush with nothing new does no I/O.
    assert_eq!(f.gc.flush_hints().unwrap(), 0);
    assert_eq!(std::fs::metadata(&hint_file).unwrap().len(), len);

    // The file is append-only: a later flush adds, never rewrites.
    let c = f.store.put(&body(4000, 4)).unwrap();
    f.gc.note_access(c);
    f.gc.flush_hints().unwrap();
    assert!(
        std::fs::metadata(&hint_file).unwrap().len() > len,
        "the file grew"
    );
    let r = f.gc.collect(Some(&*Roots::new())).expect("collect");
    assert!(
        r.hints_tracked >= 2,
        "hints are tracked: {}",
        r.hints_tracked
    );
    assert!(
        r.hints_flushed == 0,
        "the explicit flushes already wrote them, so the cycle writes nothing: {}",
        r.hints_flushed
    );
    // A hint noted after the last explicit flush is written by the cycle.
    let d = f.store.put(&body(4000, 9)).unwrap();
    f.gc.note_access(d);
    let r2 = f.gc.collect(Some(&*Roots::new())).expect("collect");
    assert!(
        r2.hints_flushed > 0,
        "the cycle flushes what is pending: {r2:?}"
    );
}

/// The hint cap drops hints and counts them, and losing a hint costs no block.
#[test]
fn the_hint_cap_drops_hints_and_says_so() {
    let d = tempfile::tempdir().unwrap();
    let _ = d;
    let f = Fixture::new(
        common::small_store_opts(1 << 20),
        Options {
            max_hints: 8,
            ..eager()
        },
    );
    for i in 0..40u32 {
        let b = f.store.put(&body(4000, i)).unwrap();
        f.gc.note_access(b);
    }
    let r = f.gc.collect(Some(&*Roots::new())).expect("collect");
    assert!(r.hints_tracked <= 8, "the cap holds: {}", r.hints_tracked);
    assert!(
        r.hints_dropped > 0,
        "and the drops are counted: {}",
        r.hints_dropped
    );
    for b in f.live_blocks() {
        assert!(f.store.get(b).is_ok(), "a dropped hint costs no block");
    }
}

/// The I/O budget bounds a cycle: it stops after the budget and the next cycle continues.
#[test]
fn the_io_budget_bounds_a_cycle() {
    let f = Fixture::new(
        common::small_store_opts(32 << 10),
        Options {
            io_budget_bytes: 1,
            ..eager()
        },
    );
    let snap = f.meta.new_snapshot("s").unwrap();
    // Garbage, then a big live file, then more garbage. The packs in the middle hold both, so
    // every candidate has real bytes to copy. A pack of pure garbage copies nothing, which is
    // correct and useless for measuring a budget.
    for i in 0..60u32 {
        f.store.put(&body(4000, i)).unwrap();
    }
    f.write(&snap, b"keep", &body(2_000_000, 1));
    for i in 60..200u32 {
        f.store.put(&body(4000, i)).unwrap();
    }
    f.store.sync().unwrap();
    f.meta.sync().unwrap();
    let roots = Roots::new();
    let r = f.gc.collect(Some(&*roots)).expect("collect");
    assert!(
        r.candidates > 2,
        "the fixture needs several candidates: {r:?}"
    );
    assert!(
        r.bytes_copied <= 4096,
        "the budget held: {}",
        r.bytes_copied
    );
    assert!(
        r.packs_rewritten < r.candidates
            || r.skipped.iter().any(|s| s.reason == SkipReason::NotReached),
        "the budget stopped the cycle before every candidate: {r:?}"
    );
    for b in f.live_blocks() {
        assert!(f.store.get(b).is_ok());
    }
    // A pack of pure garbage is a candidate and copies nothing, which is correct: a budget is
    // spent on copied bytes, not on packs looked at. So the pure-garbage packs are still freed
    // even with a budget of one byte, and only the packs with live records to copy are held back.
    assert!(
        r.packs_rewritten > 0,
        "the garbage packs are still reclaimed: {r:?}"
    );
    // A cycle with no budget finishes what the bounded one started.
    let g = cowfs_gc::Gc::open(
        f.gc_dir(),
        Arc::clone(&f.store),
        Arc::clone(&f.meta),
        eager(),
    )
    .expect("gc");
    let r2 = g.collect(Some(&*roots)).expect("collect");
    assert!(r2.freed_bytes > 0, "the unbounded cycle reclaims: {r2:?}");
    for b in f.live_blocks() {
        assert!(
            g.store().get(b).is_ok(),
            "every live block reads after a budgeted cycle"
        );
    }
}

/// A store whose recovery reported damage is refused before anything is written.
#[test]
fn a_store_with_corruption_is_refused() {
    let f = garbage();
    drop(f);
    // Rebuild on the same paths, damage a pack below the watermark, and try again.
    let d = tempfile::tempdir().unwrap();
    let store = cowfs_store::Store::open(
        d.path().join("store"),
        cowfs_store::Options {
            max_pack_size: 32 << 10,
            ..cowfs_store::Options::default()
        },
    )
    .expect("store");
    let meta = cowfs_meta::Meta::open(
        d.path().join("meta"),
        cowfs_meta::Options {
            background: false,
            ..cowfs_meta::Options::default()
        },
    )
    .expect("meta");
    let snap = meta.new_snapshot("s").unwrap();
    let chunks = store.ingest_bytes(&body(30_000, 1)).expect("ingest");
    let ino = snap
        .batch(|tx| tx.create(cowfs_meta::ROOT_INO, b"a", 0o644))
        .unwrap()
        .ino;
    snap.batch(|tx| tx.set_content(ino, &chunks, 30_000))
        .unwrap();
    for i in 0..10u32 {
        store.put(&body(4000, i)).unwrap();
    }
    store.sync().unwrap();
    meta.sync().unwrap();
    drop(snap);
    drop(store);
    drop(meta);

    // A checkpointed pack is not re-read at open, so the index goes too and the scan finds it.
    std::fs::remove_file(d.path().join("store/index.cix")).unwrap();
    let pack = d.path().join("store/packs/pack-00000000.cpk");
    let mut bytes = std::fs::read(&pack).unwrap();
    bytes[100] ^= 0xff;
    std::fs::write(&pack, &bytes).unwrap();

    let store = Arc::new(
        cowfs_store::Store::open(
            d.path().join("store"),
            cowfs_store::Options {
                max_pack_size: 32 << 10,
                ..cowfs_store::Options::default()
            },
        )
        .expect("reopen"),
    );
    assert!(store.recovery().has_corruption(), "the damage is reported");
    let meta = Arc::new(
        cowfs_meta::Meta::open(
            d.path().join("meta"),
            cowfs_meta::Options {
                background: false,
                ..cowfs_meta::Options::default()
            },
        )
        .expect("meta reopen"),
    );
    let gc =
        cowfs_gc::Gc::open(d.path().join("gc"), Arc::clone(&store), meta, eager()).expect("gc");
    let e = gc.collect(Some(&*Roots::new())).unwrap_err();
    assert!(
        matches!(e, cowfs_gc::Error::CorruptStore(n) if n > 0),
        "the collector refuses a store with known data loss: {e}"
    );
}
