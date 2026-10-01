//! Round-5 regression tests. Every one is a port of an independent reviewer repro, so the
//! failure it pins came from outside this suite.
#![allow(unsafe_code)]
mod common;
use common::*;
use cowfs_store::{BlockId, Error, Op, Options, Store};
use std::os::unix::fs::FileExt;
use std::{fs, process::Command, time::Duration};

fn newest_slot(w: &[u8]) -> usize {
    let seq = |at: usize| u64::from_le_bytes(w[at..at + 8].try_into().unwrap());
    if seq(0) > seq(32) {
        0
    } else {
        32
    }
}

/// Tear the newest `SYNCED` slot, so the mark falls back to an older, lower one.
fn tear_newest_slot(dir: &std::path::Path) {
    let p = dir.join("SYNCED");
    let mut b = fs::read(&p).unwrap();
    let at = newest_slot(&b);
    b[at + 24] ^= 0xff;
    fs::write(&p, b).unwrap();
}

/// F1a: a torn watermark slot plus bit rot, with a valid checkpoint present. The checkpoint used
/// to be trusted, so the pack was never read and a synced block was gone with a clean report.
#[test]
fn a_torn_watermark_slot_plus_a_valid_checkpoint_reports_clean_after_a_loss() {
    let dir = tempfile::tempdir().unwrap();
    let a = random(1, 3000);
    let b = random(2, 3000);
    let (ia, ib) = {
        let s = Store::open(dir.path(), opts()).unwrap();
        let ia = s.put(&a).unwrap();
        s.sync().unwrap();
        let ib = s.put(&b).unwrap();
        s.sync().unwrap();
        s.checkpoint().unwrap();
        (ia, ib)
    };
    let p = pack_path(dir.path(), 0);
    let bytes = fs::read(&p).unwrap();
    let recs = parse_pack(&bytes);
    assert_eq!(recs.len(), 2);
    let mut bad = bytes.clone();
    bad[recs[1].1 + REC_HDR] ^= 1;
    fs::write(&p, bad).unwrap();
    tear_newest_slot(dir.path());
    assert!(fs::metadata(index_path(dir.path())).unwrap().len() > 0);

    let s = Store::open(dir.path(), opts()).unwrap();
    println!(
        "a_ok={} b_err={:?} has_corruption={} index_loaded={} records_scanned={} report={:?}",
        s.get(ia).is_ok(),
        s.get(ib).as_ref().err(),
        s.recovery().has_corruption(),
        s.recovery().index_loaded,
        s.recovery().records_scanned,
        s.recovery()
    );
    assert_eq!(s.get(ia).unwrap(), a, "the first record must survive");
    assert!(
        s.get(ib).is_err(),
        "setup: the flipped record must be unreadable"
    );
    assert!(
        s.recovery().has_corruption(),
        "a synced block was lost, the checkpoint was trusted, and the store reported clean"
    );
}

/// F1b: the same tear with no checkpoint, so the scan runs. The bytes past the rolled-back mark
/// used to be cut as a torn tail with no loss recorded.
#[test]
fn a_torn_watermark_slot_never_cuts_a_synced_record_silently() {
    let dir = tempfile::tempdir().unwrap();
    let a = random(3, 3000);
    let b = random(4, 3000);
    let ib = {
        let s = Store::open(dir.path(), opts()).unwrap();
        s.put(&a).unwrap();
        s.sync().unwrap();
        let ib = s.put(&b).unwrap();
        s.sync().unwrap();
        ib
    };
    let _ = fs::remove_file(index_path(dir.path()));
    let p = pack_path(dir.path(), 0);
    let bytes = fs::read(&p).unwrap();
    let recs = parse_pack(&bytes);
    let mut bad = bytes.clone();
    bad[recs[1].1 + REC_HDR] ^= 1;
    fs::write(&p, bad).unwrap();
    tear_newest_slot(dir.path());

    for round in 0..2 {
        let s = Store::open(dir.path(), opts()).unwrap();
        let lost = s.get(ib).is_err();
        println!(
            "round {round}: lost={lost} truncated={} has_corruption={} report={:?}",
            s.recovery().truncated_bytes,
            s.recovery().has_corruption(),
            s.recovery()
        );
        assert!(lost, "setup: the flipped record must be unreadable");
        assert!(
            s.recovery().has_corruption(),
            "round {round}: a synced record was cut with no loss reported"
        );
        drop(s);
    }
}

/// A tear at every byte offset of the newest slot, with damage present, must never report clean.
#[test]
fn a_torn_watermark_slot_at_every_offset_with_damage_is_loud() {
    let base = tempfile::tempdir().unwrap();
    let a = random(5, 3000);
    let b = random(6, 3000);
    let ib;
    {
        let s = Store::open(base.path(), opts()).unwrap();
        s.put(&a).unwrap();
        s.sync().unwrap();
        ib = s.put(&b).unwrap();
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    let p = pack_path(base.path(), 0);
    let bytes = fs::read(&p).unwrap();
    let recs = parse_pack(&bytes);
    let mut bad = bytes.clone();
    bad[recs[1].1 + REC_HDR] ^= 1;
    let work = tempfile::tempdir().unwrap();
    let mut quiet = Vec::new();
    let mut high_water_only = Vec::new();
    for byte in 0..32u32 {
        let sub = work.path().join(format!("b{byte}"));
        fs::create_dir_all(sub.join("packs")).unwrap();
        fs::write(pack_path(&sub, 0), &bad).unwrap();
        let mut wm = fs::read(base.path().join("SYNCED")).unwrap();
        wm[32 + byte as usize] ^= 0xff;
        fs::write(sub.join("SYNCED"), wm).unwrap();
        fs::copy(base.path().join("index.cix"), sub.join("index.cix")).unwrap();
        let Ok(s) = Store::open(&sub, opts()) else {
            continue;
        };
        assert!(
            s.get(ib).is_err(),
            "byte {byte}: wrong data must never be served"
        );
        if s.get(ib).is_err() && !s.recovery().has_corruption() {
            // Bytes 28..32 carry the pack-id high-water and sit outside the slot CRC, so a flip
            // there does not tear the slot: the mark still holds, the checkpoint is still trusted,
            // and open does not re-read the bytes it covers. That is the documented limit, so it
            // needs `verify_all` rather than an open-time report.
            if (28..32).contains(&byte) {
                let fsck = s.verify_all().unwrap();
                assert!(
                    !fsck.is_clean(),
                    "byte {byte}: rotted high-water hid the damage from verify_all too"
                );
                high_water_only.push(byte);
            } else {
                quiet.push(byte);
            }
        }
    }
    println!("torn-slot offsets reported clean: {quiet:?}");
    println!("offsets outside the slot CRC, needing verify_all: {high_water_only:?}");
    assert!(
        quiet.is_empty(),
        "lost a synced block with a clean report at {quiet:?}"
    );
}

/// F3: an acknowledged whole-pack entry used to hide damage in a pack that came back with the
/// same id, because the entry was a nonce-0 wildcard.
#[test]
fn an_acked_whole_pack_entry_does_not_hide_a_loss_in_a_restored_pack() {
    let dir = tempfile::tempdir().unwrap();
    let o = Options {
        max_pack_size: 4000,
        ..opts()
    };
    {
        let s = Store::open(dir.path(), o).unwrap();
        for i in 0..3u64 {
            s.put(&random(20 + i, 3000)).unwrap();
        }
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    let top = *pack_ids(dir.path()).last().unwrap();
    let raw = fs::read(pack_path(dir.path(), top)).unwrap();
    fs::remove_file(pack_path(dir.path(), top)).unwrap();
    {
        let s = Store::open(dir.path(), o).unwrap();
        let n = s.acknowledge_corruption().unwrap();
        println!("acknowledged {n} for the missing pack {top}");
        assert!(n >= 1);
    }
    // A different pack file at the same id, with real damage inside its one record.
    let p = pack_path(dir.path(), top);
    fs::write(&p, &raw).unwrap();
    let recs = parse_pack(&raw);
    let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
    f.write_all_at(&[0x5A; 16], (recs[0].1 + 40) as u64)
        .unwrap();
    f.sync_all().unwrap();
    drop(f);

    let s = Store::open(dir.path(), o).unwrap();
    println!(
        "restored pack {top}: corruption={} corrupt={:?} missing={:?} acknowledged={:?}",
        s.recovery().has_corruption(),
        s.recovery().corrupt_synced,
        s.recovery().missing_synced,
        s.recovery().acknowledged
    );
    assert!(
        s.recovery().has_corruption() && !s.recovery().corrupt_synced.is_empty(),
        "damage in a restored pack was swallowed by the acknowledgement: {:?}",
        s.recovery()
    );
}

/// B, case A: after acknowledging a missing pack the watermark is rewound, and restoring that pack
/// used to make open cut it to 16 bytes and sidecar it with a clean report.
#[test]
fn a_restored_pack_above_the_watermark_is_never_cut_silently() {
    let dir = tempfile::tempdir().unwrap();
    let blocks: Vec<Vec<u8>> = (0..8).map(|i| random(70 + i, 3000)).collect();
    let o = Options {
        max_pack_size: 6000,
        ..opts()
    };
    {
        let s = Store::open(dir.path(), o).unwrap();
        for b in &blocks {
            s.put(b).unwrap();
        }
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    let p = pack_path(dir.path(), 7);
    let raw = fs::read(&p).unwrap();
    let recs = parse_pack(&raw);
    fs::remove_file(&p).unwrap();
    {
        let s = Store::open(dir.path(), o).unwrap();
        assert!(s.recovery().missing_synced.contains(&7));
        s.acknowledge_corruption().unwrap();
    }
    fs::write(&p, &raw).unwrap();
    let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
    f.write_all_at(&[0x5A; 16], (recs[0].1 + 40) as u64)
        .unwrap();
    f.sync_all().unwrap();
    drop(f);

    let s = Store::open(dir.path(), o).unwrap();
    let after = fs::metadata(&p).unwrap().len();
    println!(
        "restored pack 7: {after} of {} readable={} has_corruption={} discarded={}",
        raw.len(),
        s.get(BlockId::of(&blocks[7])).is_ok(),
        s.recovery().has_corruption(),
        s.recovery().torn_tail_discarded
    );
    assert_eq!(after, raw.len() as u64, "a restored pack was cut away");
    assert!(
        s.get(BlockId::of(&blocks[7])).is_err(),
        "setup: the record is damaged"
    );
    assert!(
        s.recovery().has_corruption(),
        "a whole restored pack was cut and nothing reported: {:?}",
        s.recovery()
    );
}

/// c7d6 and D18: an unacknowledged loss must stay reported across a watermark rollback, and
/// acknowledging it must actually record something.
#[test]
fn an_unacknowledged_loss_survives_a_watermark_rollback_and_stays_acknowledgeable() {
    let dir = tempfile::tempdir().unwrap();
    let o = Options {
        max_pack_size: 4000,
        ..opts()
    };
    let old_watermark;
    let ids;
    {
        let s = Store::open(dir.path(), o).unwrap();
        for i in 0..12u64 {
            s.put(&random(100 + i, 3000)).unwrap();
        }
        s.sync().unwrap();
        s.checkpoint().unwrap();
        old_watermark = fs::read(dir.path().join("SYNCED")).unwrap();
        ids = pack_ids(dir.path());
    }
    assert!(ids.len() >= 3, "setup: {ids:?}");
    for &id in ids.iter().skip(1) {
        fs::remove_file(pack_path(dir.path(), id)).unwrap();
    }
    // The operator restores the older SYNCED from backup, so the mark no longer reaches those packs.
    fs::write(dir.path().join("SYNCED"), &old_watermark).unwrap();
    let first = {
        let s = Store::open(dir.path(), o).unwrap();
        s.recovery().missing_synced.clone()
    };
    println!("first open missing: {first:?}");
    assert!(!first.is_empty(), "the lost packs were not reported");
    // Restoring the same backup again must not forget the loss.
    fs::write(dir.path().join("SYNCED"), &old_watermark).unwrap();
    let s = Store::open(dir.path(), o).unwrap();
    println!(
        "after a second restore missing: {:?}",
        s.recovery().missing_synced
    );
    assert!(
        !s.recovery().missing_synced.is_empty(),
        "a rolled-back watermark forgot the loss"
    );
    let n = s.acknowledge_corruption().unwrap();
    println!("acknowledged {n}");
    assert!(n >= 1, "a reported loss could not be acknowledged");
    drop(s);
    // An accepted loss is silent, which is the documented contract, and it stays that way.
    let s = Store::open(dir.path(), o).unwrap();
    assert!(
        !s.recovery().has_corruption(),
        "an accepted loss is still reported: {:?}",
        s.recovery()
    );
}

/// D: `close` ran the shutdown, released the lock, and then `Drop` ran it again with no lock,
/// so another process could be writing `index.cix` and `SYNCED` at the same time. The trace
/// counts the attempts, so one flush is one set of ops.
#[test]
fn close_never_writes_after_it_has_released_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let mut o = opts();
    o.checkpoint_on_drop = true;
    let trace = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let s = Store::open_traced(dir.path(), o, trace.clone()).unwrap();
    let d = random(81, 3000);
    s.put(&d).unwrap();
    s.sync().unwrap();
    // The index is a directory, so the flush fails and reports, which is what used to make `Drop`
    // run a second, unlocked attempt.
    fs::create_dir_all(dir.path().join("index.cix")).unwrap();
    let r = s.close();
    println!("close with a failing checkpoint: {r:?}");
    assert!(r.is_err(), "setup: close must report the failed flush");
    let ops = std::mem::take(&mut *trace.lock().unwrap());
    let attempts = ops
        .iter()
        .filter(|op| matches!(op, Op::Create(n) if n == "index.cix.tmp"))
        .count();
    println!("index.cix.tmp attempts: {attempts} of {} ops", ops.len());
    assert_eq!(
        attempts, 1,
        "the store wrote the index again with no lock held"
    );
    let _ = fs::remove_dir(dir.path().join("index.cix"));
    let s = Store::open(dir.path(), opts()).unwrap();
    assert_eq!(
        s.get(BlockId::of(&d)).unwrap(),
        d,
        "a failed close lost data"
    );
}

/// F, nested records: a record inside another record's payload must not become a block, so
/// the block set cannot grow from bytes the store never put.
///
/// A header that still checksums states its record's length truthfully, so a record found inside
/// that span is nested and is not indexed. A header that does not checksum says nothing about its
/// length, so the store cannot tell a nested record from a real one there; that case is reported
/// as damage and documented rather than guessed at.
#[test]
fn a_record_inside_a_block_payload_is_not_a_block() {
    let dir = tempfile::tempdir().unwrap();
    let inner = tempfile::tempdir().unwrap();
    let (inner_id, inner_data) = {
        let s = Store::open(inner.path(), opts()).unwrap();
        let d = random(900, 2500);
        let id = s.put(&d).unwrap();
        s.sync().unwrap();
        (id, d)
    };
    let inner_bytes = fs::read(pack_path(inner.path(), 0)).unwrap();
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        s.put(&inner_bytes).unwrap();
        s.put(&random(901, 2500)).unwrap();
        s.sync().unwrap();
    }
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        assert!(
            !s.contains(inner_id),
            "a nested record became a block on a clean open"
        );
    }
    let p = pack_path(dir.path(), 0);
    let raw = fs::read(&p).unwrap();
    let recs = parse_pack(&raw);

    // The outer header stays intact and its payload is damaged, so its length is still known and
    // the scanner can tell the inner record is inside a payload.
    let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
    f.write_all_at(&[0x77; 8], (recs[0].1 + REC_HDR + 40) as u64)
        .unwrap();
    f.sync_all().unwrap();
    drop(f);
    let s = Store::open(dir.path(), opts()).unwrap();
    let after_open = s.contains(inner_id);
    let _ = s.salvage().unwrap();
    let after_salvage = s.contains(inner_id);
    println!(
        "intact outer header: after_open={after_open} after_salvage={after_salvage} has_corruption={} inner_data={}",
        s.recovery().has_corruption(),
        inner_data.len()
    );
    assert!(
        !after_open,
        "a record inside a damaged payload was resurrected by open"
    );
    assert!(
        !after_salvage,
        "salvage resurrected a record inside a block payload"
    );
}

/// H: a pack whose header is half written must be scanned, not refused, so the store is openable.
#[test]
fn a_half_written_pack_header_is_scanned_rather_than_refused() {
    let dir = tempfile::tempdir().unwrap();
    let a = random(120, 3000);
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        s.put(&a).unwrap();
        s.sync().unwrap();
    }
    let p = pack_path(dir.path(), 0);
    let mut bytes = fs::read(&p).unwrap();
    // A torn 16 byte header write: the magic landed, the version byte did not.
    bytes[8] = 0;
    fs::write(&p, &bytes).unwrap();
    let s = Store::open(dir.path(), opts()).unwrap();
    let report = s.salvage().unwrap();
    println!(
        "half header: salvage={report:?} readable={}",
        s.get(BlockId::of(&a)).is_ok()
    );
    assert_eq!(
        s.get(BlockId::of(&a)).unwrap(),
        a,
        "a half-written header hid live data"
    );
}

/// E: the lock belongs to the open file description, so a bare `fork` keeps it. A refusal must
/// name the holder, and a second process must still be refused.
#[test]
fn a_refused_open_names_the_holder_and_still_refuses_a_second_process() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), opts()).unwrap();
    let e = Store::open(dir.path(), opts()).unwrap_err();
    println!("refused: {e}");
    match e {
        Error::Locked { holder, .. } => assert_eq!(holder, Some(std::process::id())),
        other => panic!("expected Locked, got {other:?}"),
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_holder_child"])
        .env("C7D_LOCK_DIR", dir.path())
        .output()
        .unwrap();
    println!("child: {}", String::from_utf8_lossy(&out.stdout));
    assert!(out.status.success(), "a second process was not refused");
    drop(s);
    assert!(Store::open(dir.path(), opts()).is_ok());
}

#[test]
fn lock_holder_child() {
    let Ok(dir) = std::env::var("C7D_LOCK_DIR") else {
        return;
    };
    match Store::open(&dir, opts()) {
        Err(Error::Locked { .. }) => {}
        other => panic!("a second process must be refused, got {other:?}"),
    }
    // The refusal must be quick now that the wait only covers the in-process release.
    let start = std::time::Instant::now();
    let _ = Store::open(&dir, opts());
    assert!(
        start.elapsed() < Duration::from_millis(400),
        "{:?}",
        start.elapsed()
    );
}

/// The watermark-slot sweep the crash model never had: tear the newest `SYNCED` slot while damage
/// exists, at every offset that can change what the store believes, and require a loud report.
#[test]
#[cfg(feature = "fault-injection")]
fn crash_at_every_boundary_of_a_torn_watermark_recovery() {
    let mut cases = 0;
    for seed in 0..4u64 {
        let build = || {
            let d = tempfile::tempdir().unwrap();
            let x = random(300 + seed, 3000);
            let y = random(400 + seed, 3000);
            {
                let s = Store::open(d.path(), opts()).unwrap();
                s.put(&x).unwrap();
                s.sync().unwrap();
                s.put(&y).unwrap();
                s.sync().unwrap();
                s.checkpoint().unwrap();
            }
            let p = pack_path(d.path(), 0);
            let mut bytes = fs::read(&p).unwrap();
            let recs = parse_pack(&bytes);
            bytes[recs[1].1 + REC_HDR] ^= 1;
            fs::write(&p, bytes).unwrap();
            let mut wm = fs::read(d.path().join("SYNCED")).unwrap();
            let at = newest_slot(&wm);
            wm[at + 24] ^= 0xff;
            fs::write(d.path().join("SYNCED"), wm).unwrap();
            (d, x, y)
        };
        let (dir, a, b) = build();
        for key in ["C7D_EXIT_SYNC_N", "C7D_EXIT_BOUNDARY_N"] {
            for n in 1..=40 {
                let sub = dir.path().join(format!("s{seed}-{key}-{n}"));
                let _ = fs::remove_dir_all(&sub);
                cp_store(dir.path(), &sub);
                let code = child_open(&sub, &[(key, n.to_string())]);
                assert!(matches!(code, 0 | 77), "child failed: {code} {key}={n}");
                cases += 1;
                for _ in 0..2 {
                    let s = Store::open(&sub, opts()).unwrap();
                    assert_eq!(s.get(BlockId::of(&a)).unwrap(), a, "{key}={n}");
                    assert!(s.get(BlockId::of(&b)).is_err(), "{key}={n}");
                    assert!(
                        s.recovery().has_corruption(),
                        "false clean after a crash at {key}={n}: {:?}",
                        s.recovery()
                    );
                    drop(s);
                }
                let s = Store::open(&sub, opts()).unwrap();
                s.acknowledge_corruption().unwrap();
                drop(s);
                if code == 0 {
                    break;
                }
            }
        }
    }
    println!("torn-watermark recovery cases={cases}");
}

/// Copy a store's files, without the lock, so a recovery can be crashed in a fresh directory.
fn cp_store(from: &std::path::Path, to: &std::path::Path) {
    fs::create_dir_all(to.join("packs")).unwrap();
    for id in pack_ids(from) {
        fs::copy(pack_path(from, id), pack_path(to, id)).unwrap();
    }
    for name in ["SYNCED", "ACKED", "index.cix"] {
        let p = from.join(name);
        if p.exists() {
            fs::copy(&p, to.join(name)).unwrap();
        }
    }
}

#[test]
fn torn_watermark_child() {
    let Ok(p) = std::env::var("C7D_TORN_DIR") else {
        return;
    };
    let key = std::env::var("C7D_TORN_KEY").unwrap();
    let n = std::env::var("C7D_TORN_N").unwrap();
    unsafe {
        set_fault(&key, &n);
    }
    let _ = Store::open(&p, opts());
}
#[allow(unsafe_code)]
extern "C" {
    fn setenv(name: *const std::ffi::c_char, value: *const std::ffi::c_char, overwrite: i32)
        -> i32;
}

unsafe fn set_fault(key: &str, n: &str) {
    let (k, v) = (
        std::ffi::CString::new(key).unwrap(),
        std::ffi::CString::new(n).unwrap(),
    );
    unsafe { setenv(k.as_ptr(), v.as_ptr(), 1) };
}

fn child_open(dir: &std::path::Path, extra: &[(&str, String)]) -> i32 {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "torn_watermark_child"])
        .env("C7D_TORN_DIR", dir)
        .env("C7D_TORN_KEY", extra[0].0)
        .env("C7D_TORN_N", extra[0].1.clone())
        .status()
        .unwrap()
        .code()
        .unwrap_or(-1)
}
