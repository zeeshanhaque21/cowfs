//! Compaction in the store: what it copies, what it frees, and what a crash leaves behind.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use cowfs_store::{BlockId, Options, Store};

fn store(dir: &Path) -> Store {
    Store::open(dir, Options::default()).expect("open")
}

fn data(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8) ^ seed ^ (i >> 8) as u8).collect()
}

fn pack_ids(dir: &Path) -> Vec<u32> {
    let mut v: Vec<u32> = fs::read_dir(dir.join("packs"))
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            n.strip_prefix("pack-")?.strip_suffix(".cpk")?.parse().ok()
        })
        .collect();
    v.sort_unstable();
    v
}

fn dir_bytes(dir: &Path) -> u64 {
    fn walk(p: &Path) -> u64 {
        fs::read_dir(p).map_or(0, |d| {
            d.flatten()
                .map(|e| {
                    let m = e.metadata();
                    if m.as_ref().is_ok_and(|m| m.is_dir()) {
                        walk(&e.path())
                    } else {
                        m.map_or(0, |m| m.len())
                    }
                })
                .sum()
        })
    }
    walk(dir)
}

#[test]
fn plan_counts_live_and_dead_without_writing() {
    let d = tempfile::tempdir().unwrap();
    let s = store(d.path());
    let live: Vec<BlockId> = (0..4u8).map(|i| s.put(&data(1024, i)).unwrap()).collect();
    let mut dead = HashSet::new();
    for i in 10..14u8 {
        dead.insert(s.put(&data(1024, i)).unwrap());
    }
    s.sync().unwrap();
    let before = dir_bytes(d.path());
    let info = s.packs().unwrap();
    assert_eq!(info.len(), 1, "one pack so far");
    assert!(info[0].active);

    let plan = s.plan_pack(info[0].id, &|b| live.contains(&b)).unwrap();
    assert_eq!(plan.records, 8);
    assert!(plan.dead_bytes > 0);
    assert!(plan.dead_ratio() > 0.0 && plan.dead_ratio() < 1.0);
    assert!(!plan.corrupt);
    assert_eq!(plan.gap_bytes, 0);
    assert_eq!(plan.quarantined_bytes, 0);
    assert_eq!(dir_bytes(d.path()), before, "plan wrote nothing");

    // The active pack is never offered: it is the one `put` appends to.
    let plan_all_live = s.plan_pack(info[0].id, &|_| true).unwrap();
    assert_eq!(plan_all_live.dead_bytes, 0);
    assert_eq!(plan_all_live.live_bytes, plan.record_bytes());
}

#[test]
fn rewriting_copies_only_live_records_and_frees_the_rest() {
    let d = tempfile::tempdir().unwrap();
    let s = store(d.path());
    let live: Vec<BlockId> = (0..3u8).map(|i| s.put(&data(4096, i)).unwrap()).collect();
    for i in 20..24u8 {
        s.put(&data(4096, i)).unwrap();
    }
    s.sync().unwrap();
    let before = dir_bytes(d.path());
    let old = s.packs().unwrap()[0].id;
    let old_len = s.pack_len(old);

    let plan = s.plan_pack(old, &|b| live.contains(&b)).unwrap();
    let mut c = s.begin_compaction(&plan, &|b| live.contains(&b)).unwrap();
    assert!(!c.is_complete());
    while !s.copy_batch(&mut c, 0).unwrap() {}
    assert!(c.is_complete());
    assert_eq!(c.outstanding_bytes(), 0);
    let rw = s.finish_compaction(&c).unwrap();
    assert_eq!(rw.records, 3);
    assert_eq!(rw.condemned.len(), 4);
    assert_ne!(rw.to, old);
    assert_eq!(
        s.pack_len(old),
        old_len,
        "source untouched before the unlink"
    );

    for b in &live {
        assert!(s.contains(*b), "live block indexed");
    }
    for b in &rw.condemned {
        assert!(
            s.get(*b).is_ok(),
            "a condemned block still reads from the old pack"
        );
    }

    let freed = s.discard_pack(old, &rw.condemned).unwrap();
    assert_eq!(freed, old_len);
    assert_eq!(s.pack_len(old), 0);
    for b in &live {
        assert_eq!(
            s.get(*b).unwrap(),
            data(4096, live.iter().position(|x| x == b).unwrap() as u8)
        );
    }
    for b in &rw.condemned {
        assert!(!s.contains(*b), "condemned entry dropped from the index");
        assert!(s.get(*b).is_err());
    }
    assert!(
        dir_bytes(d.path()) < before,
        "bytes came back: {} -> {}",
        before,
        dir_bytes(d.path())
    );
    assert!(s.fsck().unwrap().is_clean(), "fsck clean after gc");
}

#[test]
fn copy_batch_is_resumable_and_never_splits_a_record() {
    let d = tempfile::tempdir().unwrap();
    let s = store(d.path());
    let live: Vec<BlockId> = (0..6u8).map(|i| s.put(&data(3000, i)).unwrap()).collect();
    for i in 30..36u8 {
        s.put(&data(3000, i)).unwrap();
    }
    s.sync().unwrap();
    let old = s.packs().unwrap()[0].id;
    let plan = s.plan_pack(old, &|b| live.contains(&b)).unwrap();
    let mut c = s.begin_compaction(&plan, &|b| live.contains(&b)).unwrap();

    let total = c.outstanding_bytes();
    let mut rounds = 0;
    while !c.is_complete() {
        let owed = c.outstanding_bytes();
        assert!(owed > 0, "not complete but nothing owed");
        s.copy_batch(&mut c, 1).unwrap();
        assert!(
            c.outstanding_bytes() < owed,
            "a batch of 1 byte still moves a record"
        );
        rounds += 1;
        assert!(rounds <= total / 10 + 8, "batch loop not making progress");
    }
    assert_eq!(c.outstanding_bytes(), 0);
    let rw = s.finish_compaction(&c).unwrap();
    assert_eq!(rw.records, 6);
    s.discard_pack(old, &rw.condemned).unwrap();
    for (i, b) in live.iter().enumerate() {
        assert_eq!(s.get(*b).unwrap(), data(3000, i as u8));
    }
}

#[test]
fn dropping_a_compaction_mid_copy_leaves_the_store_readable() {
    let d = tempfile::tempdir().unwrap();
    let s = store(d.path());
    let live: Vec<BlockId> = (0..5u8).map(|i| s.put(&data(2048, i)).unwrap()).collect();
    let dead: Vec<BlockId> = (20..25u8).map(|i| s.put(&data(2048, i)).unwrap()).collect();
    s.sync().unwrap();
    let old = s.packs().unwrap()[0].id;
    let plan = s.plan_pack(old, &|b| live.contains(&b)).unwrap();
    let mut c = s.begin_compaction(&plan, &|b| live.contains(&b)).unwrap();
    s.copy_batch(&mut c, 0).unwrap();
    drop(c);
    drop(s);

    let s = store(d.path());
    for (i, b) in live.iter().chain(dead.iter()).enumerate() {
        assert_eq!(
            s.get(*b).unwrap(),
            data(2048, (i % 5) as u8 + if i < 5 { 0 } else { 20 })
        );
    }
    assert!(s.fsck().unwrap().is_clean());
}

#[test]
fn a_pack_with_durable_corruption_is_never_rewritten() {
    let d = tempfile::tempdir().unwrap();
    let s = store(d.path());
    for i in 0..4u8 {
        s.put(&data(1024, i)).unwrap();
    }
    s.sync().unwrap();
    drop(s);
    // A checkpointed pack is not re-read at open, so the damage is only found by a rescan. This
    // is the same state a crash leaves when index.cix never reached the disk.
    fs::remove_file(d.path().join("index.cix")).unwrap();
    let pack = d.path().join("packs/pack-00000000.cpk");
    let mut bytes = fs::read(&pack).unwrap();
    bytes[100] ^= 0xff;
    fs::write(&pack, &bytes).unwrap();

    let s = store(d.path());
    assert!(
        s.recovery().has_corruption(),
        "damage below the watermark is corruption"
    );
    let old = pack_ids(d.path())[0];
    let plan = s.plan_pack(old, &|_| true).unwrap();
    assert!(plan.corrupt, "plan reports the pack as corrupt");
    let e = s.begin_compaction(&plan, &|_| true).unwrap_err();
    assert!(e.to_string().contains("damage"), "{e}");
    assert!(s.pack_len(old) > 0, "the damaged pack is still on disk");
}

#[test]
fn gap_bytes_are_counted_and_reported() {
    let d = tempfile::tempdir().unwrap();
    let s = store(d.path());
    for i in 0..3u8 {
        s.put(&data(1024, i)).unwrap();
    }
    s.sync().unwrap();
    drop(s);
    // Corrupt the middle of the last record so a gap appears, without touching the header CRC.
    let pack = d.path().join("packs/pack-00000000.cpk");
    let mut bytes = fs::read(&pack).unwrap();
    let n = bytes.len();
    bytes[n - 20] ^= 0xff;
    fs::write(&pack, &bytes).unwrap();

    let s = store(d.path());
    let old = pack_ids(d.path())[0];
    let plan = s.plan_pack(old, &|_| true).unwrap();
    assert!(plan.gap_bytes > 0, "gap counted");
    assert!(
        plan.records >= 2,
        "records after the damage are still found"
    );
    assert!(!plan.corrupt || plan.gap_bytes > 0);
}

#[test]
fn two_compaction_cycles_reclaim_a_two_pack_store() {
    let d = tempfile::tempdir().unwrap();
    let opts = Options {
        max_pack_size: 4096,
        ..Options::default()
    };
    let s = Store::open(d.path(), opts).unwrap();
    let live: Vec<BlockId> = (0..3u8).map(|i| s.put(&data(900, i)).unwrap()).collect();
    for i in 40..50u8 {
        s.put(&data(900, i)).unwrap();
    }
    s.sync().unwrap();
    let before = dir_bytes(d.path());
    assert!(s.packs().unwrap().len() >= 3, "sealed packs to reclaim");

    let mut freed_total = 0u64;
    for _ in 0..3 {
        for info in s.packs().unwrap() {
            if info.active {
                continue;
            }
            let plan = s.plan_pack(info.id, &|b| live.contains(&b)).unwrap();
            if plan.dead_ratio() < 0.5 {
                continue;
            }
            let mut c = s.begin_compaction(&plan, &|b| live.contains(&b)).unwrap();
            while !s.copy_batch(&mut c, 0).unwrap() {}
            let rw = s.finish_compaction(&c).unwrap();
            freed_total += s.discard_pack(info.id, &rw.condemned).unwrap();
        }
    }
    for (i, b) in live.iter().enumerate() {
        assert_eq!(s.get(*b).unwrap(), data(900, i as u8));
    }
    assert!(freed_total > 0, "some bytes were freed");
    assert!(dir_bytes(d.path()) < before / 2, "most bytes reclaimed");
    assert!(s.fsck().unwrap().is_clean());
    assert!(!s.recovery().has_corruption());
    drop(s);
    let s = store(d.path());
    for (i, b) in live.iter().enumerate() {
        assert_eq!(s.get(*b).unwrap(), data(900, i as u8));
    }
    assert!(
        !s.recovery().has_corruption(),
        "no missing synced pack after reopen"
    );
    let left = s.stats().blocks;
    assert!(
        (3..=5).contains(&left),
        "only the live blocks and whatever stayed below the ratio remain, got {left}"
    );
}

#[test]
fn epoch_is_the_durable_watermark() {
    let d = tempfile::tempdir().unwrap();
    let s = store(d.path());
    let e = s.epoch().unwrap();
    assert_eq!(e.0, 0, "first pack");
    s.put(&data(512, 1)).unwrap();
    let before = s.epoch().unwrap();
    assert_eq!(before, e, "an unsynced put does not move the epoch");
    s.put(&data(512, 2)).unwrap();
    s.sync().unwrap();
    assert!(s.epoch().unwrap().1 > before.1, "a sync moves it");
    drop(s);
    let s = store(d.path());
    assert!(s.epoch().is_some(), "a reopened store has an epoch");
}
