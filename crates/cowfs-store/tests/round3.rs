//! Regression tests for the round-3 review findings. Each one reproduces a probe from the critic's
//! report and states the property that must hold.
mod common;

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{FileExt, PermissionsExt};
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, PoisonError};

use common::{opts, pack_ids, pack_path, parse_pack, random, PACK_HEADER, REC_HDR};
use cowfs_store::{oplog_start, oplog_take, BlockId, LogOp, Options, Store};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

fn small() -> Options {
    Options {
        max_pack_size: 20_000,
        ..opts()
    }
}

/// F1: a pack id is never reused, and a checkpoint written before the loss cannot hide new data.
#[test]
fn p13_a_reused_pack_id_cannot_lose_acked_blocks() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), small()).unwrap();
        for i in 0..14 {
            s.put(&random(i, 9000)).unwrap();
        }
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    let top = *pack_ids(dir.path()).last().unwrap();
    fs::remove_file(pack_path(dir.path(), top)).unwrap();
    let mut new = Vec::new();
    {
        let s = Store::open(dir.path(), small()).unwrap();
        assert!(
            !s.recovery().missing_synced.is_empty(),
            "the loss is reported"
        );
        s.acknowledge_corruption().unwrap();
        for i in 100..106 {
            let d = random(i, 9000);
            new.push((s.put(&d).unwrap(), d));
        }
        s.sync().unwrap();
    }
    assert!(
        !pack_ids(dir.path()).contains(&top),
        "the acknowledged pack id must never come back: {:?}",
        pack_ids(dir.path())
    );
    let s = Store::open(dir.path(), small()).unwrap();
    for (id, d) in &new {
        assert_eq!(
            s.get(*id).ok().as_ref(),
            Some(d),
            "acked block lost after reopen"
        );
    }
    assert!(!s.recovery().has_corruption(), "{:?}", s.recovery());
    assert!(
        !s.recovery().index_loaded,
        "the stale checkpoint must be dropped"
    );
}

/// F2: accepting a whole missing pack cannot hide corruption that happens later.
#[test]
fn p10_a_whole_pack_acknowledgement_does_not_swallow_later_corruption() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), small()).unwrap();
        for i in 0..12 {
            s.put(&random(i, 9000)).unwrap();
        }
        s.sync().unwrap();
    }
    let top = *pack_ids(dir.path()).last().unwrap();
    fs::remove_file(pack_path(dir.path(), top)).unwrap();
    {
        let s = Store::open(dir.path(), small()).unwrap();
        assert!(!s.recovery().missing_synced.is_empty());
        s.acknowledge_corruption().unwrap();
        s.sync().unwrap();
    }
    let live = *pack_ids(dir.path()).last().unwrap();
    assert_ne!(live, top);
    // Damage durable bytes of the pack that replaced the missing one.
    let p = pack_path(dir.path(), live);
    let len = fs::metadata(&p).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .write_all_at(&[0x5A; 64], len - 40)
        .unwrap();
    let s = Store::open(dir.path(), small()).unwrap();
    assert!(
        s.recovery().has_corruption(),
        "a different pack damaged after the acknowledgement must be reported: {:?}",
        s.recovery()
    );
}

/// F5: a loss stays reported on every open until somebody accepts it.
#[test]
fn p11_a_loss_keeps_being_reported_until_it_is_acknowledged() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), small()).unwrap();
        for i in 0..6 {
            s.put(&random(i, 4000)).unwrap();
        }
        s.sync().unwrap();
    }
    // Destroy the header of a record that a sync made durable, so no block can claim it.
    let p = pack_path(dir.path(), 0);
    OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .write_all_at(
            &[0xEE; 8],
            common::PACK_HEADER.len() as u64 + REC_HDR as u64 + 10,
        )
        .unwrap();
    for round in 0..3 {
        let s = Store::open(dir.path(), small()).unwrap();
        assert!(
            s.recovery().has_corruption(),
            "round {round} must still report the loss: {:?}",
            s.recovery()
        );
        s.checkpoint().unwrap();
    }
    let s = Store::open(dir.path(), small()).unwrap();
    s.acknowledge_corruption().unwrap();
    drop(s);
    for round in 0..2 {
        let s = Store::open(dir.path(), small()).unwrap();
        assert!(
            !s.recovery().has_corruption(),
            "round {round} after acknowledgement: {:?}",
            s.recovery()
        );
        assert!(!s.recovery().acknowledged.is_empty());
        assert!(!s.recovery().corrupt_synced.is_empty() || !s.recovery().acknowledged.is_empty());
    }
}

/// F5, the shape the critic hit: the damage is in bytes a cut removes, so the second open finds a
/// clean pack and a block that is gone. The loss must still be reported.
#[test]
fn p11_a_loss_hidden_by_a_cut_is_still_reported() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (random(1, 4000), random(2, 4000));
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        s.put(&a).unwrap();
        s.put(&b).unwrap();
        s.sync().unwrap();
    }
    // Without a watermark nothing after the pack header was ever promised, so the damaged region
    // is a torn tail and open cuts it away. Block `a` is inside it and is now gone for good.
    let _ = fs::remove_file(dir.path().join("SYNCED"));
    let p = pack_path(dir.path(), 0);
    OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .write_all_at(
            &[0xEE; 8],
            common::PACK_HEADER.len() as u64 + REC_HDR as u64 + 10,
        )
        .unwrap();
    for round in 0..3 {
        let s = Store::open(dir.path(), opts()).unwrap();
        assert!(
            s.recovery().has_corruption(),
            "round {round}: the loss must stay reported after the cut: {:?}",
            s.recovery()
        );
        assert!(s.get(BlockId::of(&a)).is_err());
        assert_eq!(s.get(BlockId::of(&b)).unwrap(), b);
        s.checkpoint().unwrap();
        let _ = fs::remove_file(dir.path().join("index.cix"));
    }
    let s = Store::open(dir.path(), opts()).unwrap();
    s.acknowledge_corruption().unwrap();
    drop(s);
    let s = Store::open(dir.path(), opts()).unwrap();
    assert!(!s.recovery().has_corruption(), "{:?}", s.recovery());
}

/// F5, second half: a repairing `put` clears the block from the list a caller works through.
#[test]
fn m09_putting_a_damaged_block_again_clears_it_from_the_damaged_list() {
    let dir = tempfile::tempdir().unwrap();
    let d = random(7, 5000);
    {
        let s = Store::open(dir.path(), small()).unwrap();
        s.put(&d).unwrap();
        s.put(&random(8, 5000)).unwrap();
        s.sync().unwrap();
    }
    let p = pack_path(dir.path(), 0);
    OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .write_all_at(
            &[0xEE; 8],
            common::PACK_HEADER.len() as u64 + REC_HDR as u64 + 10,
        )
        .unwrap();
    let s = Store::open(dir.path(), small()).unwrap();
    assert_eq!(s.damaged_blocks(), vec![BlockId::of(&d)]);
    assert!(s.get(BlockId::of(&d)).is_err());
    s.put(&d).unwrap();
    assert!(
        s.damaged_blocks().is_empty(),
        "a repaired block must leave the list: {:?}",
        s.damaged_blocks()
    );
    assert_eq!(s.get(BlockId::of(&d)).unwrap(), d);
}

fn sidecars(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir.join("packs"))
        .unwrap()
        .filter_map(|e| {
            let e = e.ok()?;
            let n = e.file_name().into_string().ok()?;
            n.contains(".torn-").then_some(n)
        })
        .collect();
    v.sort();
    v
}

/// F6: torn-tail sidecars are bounded store-wide, and the newest one survives.
#[test]
fn p1_torn_sidecars_are_bounded_and_pruned_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let o = Options {
        max_torn_sidecars: 3,
        max_torn_sidecar_bytes: 1 << 20,
        ..small()
    };
    let mut written = 0u32;
    let mut pruned = 0u32;
    let mut newest = String::new();
    for round in 0..8u64 {
        {
            let s = Store::open(dir.path(), o).unwrap();
            for i in 0..3 {
                s.put(&random(round * 10 + i, 5000)).unwrap();
            }
            s.sync().unwrap();
        }
        // Append junk past the watermark: the next open cuts it and keeps a sidecar.
        let live = *pack_ids(dir.path()).last().unwrap();
        let p = pack_path(dir.path(), live);
        let len = fs::metadata(&p).unwrap().len();
        OpenOptions::new()
            .write(true)
            .open(&p)
            .unwrap()
            .write_all_at(&vec![0x77; 900], len)
            .unwrap();
        let s = Store::open(dir.path(), o).unwrap();
        written += s.recovery().torn_sidecars.len() as u32;
        pruned += s.recovery().sidecars_pruned;
        if let Some(last) = s.recovery().torn_sidecars.last() {
            newest = last.name.clone();
        }
        assert!(
            s.recovery().torn_tail_discarded > 0,
            "round {round} must cut the junk"
        );
    }
    let on_disk: Vec<String> = sidecars(dir.path());
    assert!(
        on_disk.len() <= 3,
        "at most three sidecars, found {on_disk:?}"
    );
    assert_eq!(
        on_disk.len() + pruned as usize,
        written as usize,
        "every sidecar is either kept or pruned"
    );
    assert!(
        on_disk.contains(&newest),
        "the newest sidecar must be kept: {on_disk:?} lacks {newest}"
    );
    let total: u64 = fs::read_dir(dir.path().join("packs"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".torn-"))
        .map(|e| e.metadata().unwrap().len())
        .sum();
    assert!(total <= (1 << 20), "sidecar bytes are bounded too: {total}");
}

/// F6: if the discarded bytes cannot be preserved, nothing is cut and open says so.
#[test]
fn p2_a_store_that_cannot_keep_a_sidecar_refuses_to_truncate() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), small()).unwrap();
        s.put(&random(1, 5000)).unwrap();
        s.sync().unwrap();
    }
    let p = pack_path(dir.path(), 0);
    let len = fs::metadata(&p).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .write_all_at(&vec![0x77; 900], len)
        .unwrap();
    let packs = dir.path().join("packs");
    fs::set_permissions(&packs, fs::Permissions::from_mode(0o500)).unwrap();
    let out = match Store::open(dir.path(), small()) {
        Ok(_) => panic!("open truncated bytes it could not preserve"),
        Err(e) => format!("{e}"),
    };
    fs::set_permissions(&packs, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(out.contains("torn"), "unhelpful error: {out}");
    assert_eq!(
        fs::metadata(&p).unwrap().len(),
        len + 900,
        "the pack must be untouched"
    );
    assert!(
        !common::index_path(dir.path()).exists(),
        "a refused open must not leave a checkpoint"
    );
    let s = Store::open(dir.path(), small()).unwrap();
    assert_eq!(s.recovery().torn_tail_discarded, 900);
    assert_eq!(s.recovery().torn_sidecars.len(), 1);
}

/// F3: a last pack whose file size survived but whose data did not is an empty torn pack.
#[test]
fn p3_a_zero_filled_last_pack_is_reset_rather_than_refused() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), small()).unwrap();
        for i in 0..4 {
            s.put(&random(i, 4000)).unwrap();
        }
        s.sync().unwrap();
    }
    // Roll so the last pack is a fresh one, then blank it and give it a length, which is the shape
    // a crash between extending the file and writing into it leaves.
    {
        let s = Store::open(
            dir.path(),
            Options {
                max_pack_size: 1,
                ..small()
            },
        )
        .unwrap();
        s.put(&random(99, 100)).unwrap();
    }
    let top = *pack_ids(dir.path()).last().unwrap();
    let p = pack_path(dir.path(), top);
    fs::write(&p, vec![0u8; common::PACK_HEADER.len()]).unwrap();
    let f = OpenOptions::new().write(true).open(&p).unwrap();
    f.set_len(common::PACK_HEADER.len() as u64 + 4096).unwrap();
    drop(f);
    assert!(
        fs::read(&p).unwrap().iter().all(|&b| b == 0),
        "the shape is a file whose size survived and whose data did not"
    );
    let s = Store::open(dir.path(), small()).unwrap();
    assert!(!s.recovery().has_corruption(), "{:?}", s.recovery());
    let d = random(5, 4000);
    assert_eq!(s.put(&d).unwrap(), BlockId::of(&d));
    s.sync().unwrap();
    drop(s);
    let s = Store::open(dir.path(), small()).unwrap();
    assert_eq!(s.get(BlockId::of(&d)).unwrap(), d);
}

/// F8: a pack made for compaction does not collide with the writer's next rollover.
#[test]
fn f8_a_compaction_pack_does_not_collide_with_the_next_roll() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(
        dir.path(),
        Options {
            max_pack_size: 20_000,
            ..opts()
        },
    )
    .unwrap();
    for i in 0..6 {
        s.put(&random(i, 9000)).unwrap();
    }
    s.sync().unwrap();
    let (id, file) = s.new_pack().unwrap();
    assert!(file.metadata().unwrap().len() >= common::PACK_HEADER.len() as u64);
    s.finish_pack(id, common::PACK_HEADER.len() as u64).unwrap();
    for i in 6..20 {
        s.put(&random(i, 9000)).unwrap();
    }
    s.sync().unwrap();
    drop(s);
    let s = Store::open(
        dir.path(),
        Options {
            max_pack_size: 20_000,
            ..opts()
        },
    )
    .unwrap();
    for i in 0..20 {
        let d = random(i, 9000);
        assert_eq!(s.get(BlockId::of(&d)).unwrap(), d, "block {i} lost");
    }
}

/// Rebuild a store image from `base` after the first `k` logged operations, where a write that was
/// never fsynced may have landed in whole or in part.
fn image_after(base: &[(String, Vec<u8>)], ops: &[LogOp], k: usize) -> Vec<(String, Vec<u8>)> {
    let mut img: std::collections::BTreeMap<String, Vec<u8>> = base.iter().cloned().collect();
    let mut last_sync: std::collections::HashMap<String, usize> = Default::default();
    for (p, op) in ops.iter().enumerate().take(k) {
        if let LogOp::Sync { file } = op {
            last_sync.insert(file.clone(), p);
        }
    }
    for (p, op) in ops.iter().enumerate().take(k) {
        let Some(file) = (match op {
            LogOp::Write { file, .. }
            | LogOp::SetLen { file, .. }
            | LogOp::Sync { file }
            | LogOp::Create { file }
            | LogOp::Whole { file, .. } => Some(file.clone()),
            _ => None,
        }) else {
            continue;
        };
        let durable = last_sync.get(&file).is_some_and(|&s| p < s);
        match op {
            LogOp::Create { .. } => {
                img.entry(file).or_default();
            }
            LogOp::Whole { data, .. } => {
                img.insert(file, data.clone());
            }
            LogOp::SetLen { len, .. } => {
                let b = img.entry(file).or_default();
                let old = b.len();
                b.resize(*len as usize, 0);
                if !durable && *len as usize > old {
                    for x in &mut b[old..] {
                        *x = x.wrapping_mul(31).wrapping_add(7);
                    }
                }
            }
            LogOp::Write { off, data, .. } => {
                let b = img.entry(file).or_default();
                let end = *off as usize + data.len();
                if b.len() < *off as usize {
                    b.resize(end, 0);
                    if !durable {
                        for x in &mut b[*off as usize..end] {
                            *x = x.wrapping_add(1);
                        }
                    }
                }
                if b.len() < end {
                    b.resize(end, 0);
                }
                if durable {
                    b[*off as usize..end].copy_from_slice(data);
                } else {
                    // A torn write lands whole or not at all, never half a record.
                    let take = data.len() - (p % data.len().max(1));
                    b[*off as usize..*off as usize + take].copy_from_slice(&data[..take]);
                }
            }
            _ => {}
        }
    }
    img.into_iter().collect()
}

/// F4: recovery of two torn regions in one pack survives a crash at every write it makes.
#[test]
fn f4_two_torn_regions_survive_a_crash_at_every_write() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let blocks: Vec<Vec<u8>> = (0..8).map(|i| random(i, 3000)).collect();
    let acked: Vec<(BlockId, Vec<u8>)> = blocks[..4]
        .iter()
        .map(|b| (BlockId::of(b), b.clone()))
        .collect();
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        for b in &blocks[..4] {
            s.put(b).unwrap();
        }
        s.sync().unwrap();
        for b in &blocks[4..] {
            s.put(b).unwrap();
        }
    }
    // Two damaged records past the watermark, with valid records after each.
    let p = pack_path(dir.path(), 0);
    let recs = parse_pack(&fs::read(&p).unwrap());
    let mut bytes = fs::read(&p).unwrap();
    for r in [recs[5], recs[7]] {
        bytes[r.1 + REC_HDR + 5] ^= 0xFF;
    }
    fs::write(&p, &bytes).unwrap();

    oplog_start();
    let s = Store::open_unsynced(dir.path(), opts()).unwrap();
    for (id, b) in &acked {
        assert_eq!(
            s.get(*id).ok().as_ref(),
            Some(b),
            "block lost before the crash"
        );
    }
    assert!(
        s.recovery().recovered_from_tail >= 1,
        "the record after a tear must be recovered: {:?}",
        s.recovery()
    );
    let ops = oplog_take();
    assert!(ops.len() > 4, "the recovery must have written something");

    let mut base: Vec<(String, Vec<u8>)> = Vec::new();
    for e in fs::read_dir(dir.path()).unwrap().flatten() {
        if e.path().is_file() {
            base.push((
                e.file_name().to_string_lossy().into_owned(),
                fs::read(e.path()).unwrap(),
            ));
        }
    }
    for e in fs::read_dir(dir.path().join("packs")).unwrap().flatten() {
        base.push((
            e.file_name().to_string_lossy().into_owned(),
            fs::read(e.path()).unwrap(),
        ));
    }
    for k in 0..=ops.len() {
        let img = image_after(&base, &ops, k);
        let fresh = tempfile::tempdir().unwrap();
        fs::create_dir_all(fresh.path().join("packs")).unwrap();
        for (n, b) in &img {
            if n.starts_with("pack-") {
                fs::write(fresh.path().join("packs").join(n), b).unwrap();
            } else {
                fs::write(fresh.path().join(n), b).unwrap();
            }
        }
        let s = match Store::open_unsynced(fresh.path(), opts()) {
            Ok(s) => s,
            Err(e) => panic!("crash after op {k} of {}: open failed: {e:?}", ops.len()),
        };
        for (i, (id, b)) in acked.iter().enumerate() {
            assert_eq!(
                s.get(*id).ok().as_ref(),
                Some(b),
                "crash after op {k} of {}: acked block {i} lost, recovery {:?}",
                ops.len(),
                s.recovery()
            );
        }
    }
}

/// M12: an acknowledgement is only durable once the data it accepts has been synced.
#[test]
fn m12_acknowledgement_syncs_before_it_records() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let o = opts();
    {
        let s = Store::open(dir.path(), o).unwrap();
        for i in 0..4 {
            s.put(&random(i, 4000)).unwrap();
        }
        s.sync().unwrap();
    }
    let p = pack_path(dir.path(), 0);
    OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .write_all_at(
            &[0xEE; 8],
            common::PACK_HEADER.len() as u64 + REC_HDR as u64 + 10,
        )
        .unwrap();
    oplog_start();
    {
        let s = Store::open_unsynced(dir.path(), o).unwrap();
        s.put(&random(50, 4000)).unwrap();
        assert!(s.recovery().has_corruption());
        s.acknowledge_corruption().unwrap();
        let ops = oplog_take();
        let put_at = ops
            .iter()
            .position(|op| matches!(op, LogOp::Write { file, off, .. } if file.starts_with("pack-") && *off >= common::PACK_HEADER.len() as u64 && *off < 20000))
            .unwrap_or_else(|| panic!("a put write: {ops:#?}"));
        let pack_sync = ops[put_at..]
            .iter()
            .position(|op| matches!(op, LogOp::Sync { file } if file.starts_with("pack-")))
            .map(|i| put_at + i);
        let ack_at = ops
            .iter()
            .skip(put_at)
            .position(|op| matches!(op, LogOp::Whole { file, .. } if file == "ACKED"))
            .map(|i| put_at + i)
            .expect("the acknowledgement is recorded");
        assert!(
            pack_sync.is_some_and(|i| i > put_at && i < ack_at),
            "the pack must be synced before the acknowledgement is written: {ops:?}"
        );
    }
}

const CHILD_ENV: &str = "COWFS_LOCK_CHILD_DIR";

/// The store lock is held across processes: a second process cannot open a held store.
#[test]
fn the_lock_is_held_across_processes() {
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let script = format!(
        "exec '{}' --exact lock_child --ignored --nocapture",
        exe.display()
    );
    let s = Store::open(dir.path(), opts()).unwrap();
    s.put(b"held by the parent").unwrap();
    let out = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .env(CHILD_ENV, dir.path())
        .output()
        .unwrap();
    assert!(!out.status.success(), "a child must not open a held store");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("LOCKED"),
        "child said: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    drop(s);
    let out = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .env(CHILD_ENV, dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "after the lock is gone the child must open: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore]
fn lock_child() {
    let Some(dir) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    match Store::open(Path::new(&dir), opts()) {
        Err(e) => {
            println!("LOCKED {e}");
            std::process::exit(1);
        }
        Ok(_) => println!("opened"),
    }
}

/// F7: a flood of forged headers with valid checksums must not hide the real records. Salvage
/// trusts the payload hash, not the header, so it gets them back.
#[test]
fn p4_salvage_recovers_records_behind_forged_headers() {
    let dir = tempfile::tempdir().unwrap();
    let blocks: Vec<Vec<u8>> = (0..20).map(|i| random(i, 3000)).collect();
    let ids: Vec<BlockId> = blocks.iter().map(|b| BlockId::of(b)).collect();
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        for b in &blocks {
            s.put(b).unwrap();
        }
        s.sync().unwrap();
    }
    let p = pack_path(dir.path(), 0);
    let bytes = fs::read(&p).unwrap();

    // 300 forged headers in front of the real records, each with a valid header checksum and a
    // payload that reaches the end of the file, so each one costs a read of everything behind it. A
    // scanner with a work bound spends the bound here and never reaches the records.
    let len = bytes.len() + 300 * REC_HDR;
    let mut forged = PACK_HEADER.to_vec();
    for i in 0..300usize {
        let at = PACK_HEADER.len() + i * REC_HDR;
        let slen = (len - at - REC_HDR) as u32;
        let mut head = [0u8; REC_HDR];
        head[..4].copy_from_slice(b"CWRB");
        head[8..12].copy_from_slice(&slen.to_le_bytes());
        head[12..16].copy_from_slice(&slen.to_le_bytes());
        head[16..20].copy_from_slice(&(i as u32 ^ 7).to_le_bytes());
        let hcrc = crc32c::crc32c(&head[..48]);
        head[48..52].copy_from_slice(&hcrc.to_le_bytes());
        forged.extend_from_slice(&head);
    }
    forged.extend_from_slice(&bytes[PACK_HEADER.len()..]);
    assert_eq!(forged.len(), len);
    fs::write(&p, &forged).unwrap();
    let _ = fs::remove_file(common::index_path(dir.path()));

    let s = Store::open(dir.path(), opts()).unwrap();
    let missing: Vec<usize> = ids
        .iter()
        .enumerate()
        .filter(|(_, id)| s.get(**id).is_err())
        .map(|(i, _)| i)
        .collect();
    let report = s.salvage().unwrap();
    assert!(
        report.records >= 20,
        "salvage must see every record: {report:?}"
    );
    assert!(
        missing.is_empty() || report.newly_indexed + report.repaired >= 20,
        "salvage must re-index what open dropped: {missing:?} {report:?}"
    );
    for (i, b) in blocks.iter().enumerate() {
        assert_eq!(s.get(ids[i]).unwrap(), *b, "block {i} not recovered");
    }
    s.checkpoint().unwrap();
    drop(s);
    let s = Store::open(dir.path(), opts()).unwrap();
    assert!(
        s.recovery().index_loaded,
        "the salvaged index must be reused"
    );
    for (i, b) in blocks.iter().enumerate() {
        assert_eq!(s.get(ids[i]).unwrap(), *b, "block {i} lost after reopen");
    }
}

/// M05: two tears of one pack must not overwrite the same sidecar.
#[test]
fn m05_two_torn_tails_keep_two_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    let o = opts();
    for round in 0..2u64 {
        {
            let s = Store::open(dir.path(), o).unwrap();
            for i in 0..2 {
                s.put(&random(round * 10 + i, 5000)).unwrap();
            }
            s.sync().unwrap();
        }
        let p = pack_path(dir.path(), 0);
        let len = fs::metadata(&p).unwrap().len();
        OpenOptions::new()
            .write(true)
            .open(&p)
            .unwrap()
            .write_all_at(&vec![round as u8 + 1; 3000], len)
            .unwrap();
        let s = Store::open(dir.path(), o).unwrap();
        assert_eq!(s.recovery().torn_tail_discarded, 3000, "round {round}");
    }
    let names = sidecars(dir.path());
    assert_eq!(names.len(), 2, "{names:?}");
    let a = fs::read(dir.path().join("packs").join(&names[0])).unwrap();
    let b = fs::read(dir.path().join("packs").join(&names[1])).unwrap();
    assert_eq!(a.len(), 3000);
    assert_eq!(b.len(), 3000);
    assert_ne!(a, b, "the second tail must be kept next to the first");
}

/// Random sequences of put, sync, acknowledge, crash and reopen, with the checkpoint written at
/// random points. Every block that a sync made durable must be readable after every reopen.
#[test]
fn f1_random_sequences_keep_every_acked_block_readable() {
    for seed in 0..24u64 {
        let dir = tempfile::tempdir().unwrap();
        let o = Options {
            max_pack_size: 8_000,
            checkpoint_on_drop: false,
            ..opts()
        };
        let mut live: Vec<(BlockId, Vec<u8>)> = Vec::new();
        let mut acked: Vec<(BlockId, Vec<u8>)> = Vec::new();
        let mut r = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let next = |r: &mut u64| {
            *r ^= *r << 13;
            *r ^= *r >> 7;
            *r ^= *r << 17;
            *r
        };
        for round in 0..8 {
            let s = Store::open(dir.path(), o).unwrap();
            let steps = 2 + next(&mut r) % 5;
            for _ in 0..steps {
                let c = next(&mut r) % 10;
                if c == 0 && !acked.is_empty() {
                    // Accept whatever open reported, then put it back.
                    s.acknowledge_corruption().unwrap();
                    let d = random(next(&mut r), 4000);
                    let id = s.put(&d).unwrap();
                    live.push((id, d));
                } else if c < 4 {
                    let d = random(next(&mut r), 4000);
                    let id = s.put(&d).unwrap();
                    if !live.iter().any(|(i, _)| *i == id) {
                        live.push((id, d));
                    }
                } else if c < 6 {
                    s.sync().unwrap();
                    for b in &live {
                        if !acked.iter().any(|(i, _)| i == &b.0) {
                            acked.push(b.clone());
                        }
                    }
                } else {
                    s.checkpoint().unwrap();
                }
            }
            drop(s);
            // A crash: lose the index checkpoint now and then, which open must survive.
            if round % 3 == 2 {
                let _ = fs::remove_file(common::index_path(dir.path()));
            }
            let s = Store::open(dir.path(), o).unwrap();
            for (id, b) in &acked {
                assert_eq!(
                    s.get(*id).ok().as_ref(),
                    Some(b),
                    "seed {seed} round {round}: acked block lost: {:?}",
                    s.recovery()
                );
            }
            assert!(s.fsck().unwrap().is_clean(), "seed {seed} round {round}");
        }
    }
}

/// F1, second half: with the watermark gone the allocator falls back to the pack files, so an id can
/// be used again. The checkpoint that named the old pack must not be trusted for the new one.
#[test]
fn f1_a_stale_checkpoint_never_validates_against_a_recreated_pack() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), small()).unwrap();
        for i in 0..14 {
            s.put(&random(i, 9000)).unwrap();
        }
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    let top = *pack_ids(dir.path()).last().unwrap();
    fs::remove_file(pack_path(dir.path(), top)).unwrap();
    // No acknowledgement: the loss is still open, and losing the watermark drops the durable
    // high-water, so the allocator may hand the same id out again.
    let _ = fs::remove_file(dir.path().join("SYNCED"));
    {
        let s = Store::open(dir.path(), small()).unwrap();
        s.put(&random(200, 4000)).unwrap();
        s.sync().unwrap();
    }
    assert!(
        pack_ids(dir.path()).contains(&top),
        "without a watermark the id is reused, which is what this test needs: {:?}",
        pack_ids(dir.path())
    );
    let s = Store::open(dir.path(), small()).unwrap();
    assert!(
        !s.recovery().index_loaded,
        "a checkpoint that names a pack id must not survive that pack being recreated: {:?}",
        s.recovery()
    );
    let d = random(200, 4000);
    assert_eq!(
        s.get(BlockId::of(&d)).unwrap(),
        d,
        "the new block must be readable"
    );
    s.checkpoint().unwrap();
    drop(s);
    let s = Store::open(dir.path(), small()).unwrap();
    assert_eq!(
        s.get(BlockId::of(&d)).unwrap(),
        d,
        "and after a fresh checkpoint"
    );
}
