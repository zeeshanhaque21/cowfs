//! Compaction in the store: what it copies, what it frees, and what a crash leaves behind.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use cowfs_store::{BlockId, LogOp, Options, Store};

fn store(dir: &Path) -> Store {
    Store::open(dir, Options::default()).expect("open")
}

/// A store that rolls every 32 KiB, so a test has a sealed pack to work on. `data` is highly
/// compressible, so the records are noise instead: otherwise nothing rolls.
fn small_store(dir: &Path) -> Store {
    Store::open(
        dir,
        Options {
            max_pack_size: 32 << 10,
            ..Options::default()
        },
    )
    .expect("open")
}

/// Bytes that do not compress, so a pack fills at the size it claims.
fn noisy(n: usize, seed: u32) -> Vec<u8> {
    let mut h = seed.wrapping_mul(2654435761).wrapping_add(1);
    (0..n)
        .map(|_| {
            h = h.wrapping_mul(1664525).wrapping_add(1013904223);
            (h >> 24) as u8
        })
        .collect()
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

    let plan = s
        .plan_pack(info[0].id, &|b| live.contains(&b), &mut Vec::new())
        .unwrap();
    assert_eq!(plan.records, 8);
    assert!(plan.dead_bytes > 0);
    assert!(plan.dead_ratio() > 0.0 && plan.dead_ratio() < 1.0);
    assert!(!plan.corrupt);
    assert_eq!(plan.gap_bytes, 0);
    assert_eq!(plan.quarantined_bytes, 0);
    assert_eq!(dir_bytes(d.path()), before, "plan wrote nothing");

    // The active pack is never offered: it is the one `put` appends to.
    let plan_all_live = s.plan_pack(info[0].id, &|_| true, &mut Vec::new()).unwrap();
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

    let plan = s
        .plan_pack(old, &|b| live.contains(&b), &mut Vec::new())
        .unwrap();
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
    let plan = s
        .plan_pack(old, &|b| live.contains(&b), &mut Vec::new())
        .unwrap();
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
    let plan = s
        .plan_pack(old, &|b| live.contains(&b), &mut Vec::new())
        .unwrap();
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
    let plan = s.plan_pack(old, &|_| true, &mut Vec::new()).unwrap();
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
    let plan = s.plan_pack(old, &|_| true, &mut Vec::new()).unwrap();
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
            let plan = s
                .plan_pack(info.id, &|b| live.contains(&b), &mut Vec::new())
                .unwrap();
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

/// Compaction records a whole-pack acceptance only after the pack file is really gone.
///
/// A zero nonce matches any nonce in `ack::find`, so a whole-pack entry written while the pack was
/// still on disk would swallow damage reported against that pack later. The pack id is never
/// reused, so nothing can legitimately come back to it.
#[test]
fn a_whole_pack_acceptance_is_only_written_after_the_file_is_gone() {
    let d = tempfile::tempdir().unwrap();
    let s = store(d.path());
    let live: Vec<BlockId> = (0..3u8).map(|i| s.put(&data(4096, i)).unwrap()).collect();
    for i in 20..24u8 {
        s.put(&data(4096, i)).unwrap();
    }
    s.sync().unwrap();
    let old = s.packs().unwrap()[0].id;

    let plan = s
        .plan_pack(old, &|b| live.contains(&b), &mut Vec::new())
        .unwrap();
    let mut c = s.begin_compaction(&plan, &|b| live.contains(&b)).unwrap();
    while !s.copy_batch(&mut c, 0).unwrap() {}
    let rw = s.finish_compaction(&c).unwrap();

    // Before the unlink the pack is still there, so the acceptance file must name no acceptance of
    // this pack as a whole. `discard_pack` is the only thing that writes one, and it has not run.
    let acked = fs::read(d.path().join("ACKED")).unwrap_or_default();
    let whole_pack_ack_before = covers_whole_pack(&acked, old);
    assert!(
        !whole_pack_ack_before,
        "a whole-pack acceptance exists while the pack file is still there"
    );

    s.discard_pack(old, &rw.condemned).unwrap();
    assert_eq!(s.pack_len(old), 0, "the pack file is gone");
    let acked = fs::read(d.path().join("ACKED")).expect("ACKED");
    assert!(
        covers_whole_pack(&acked, old),
        "and now the acceptance names it, so the gap is not a reported loss"
    );
    assert!(!s.recovery().has_corruption());
    drop(s);
    let s = store(d.path());
    for b in &live {
        assert!(s.get(*b).is_ok(), "a live block survives the discard");
    }
    assert!(
        !s.recovery().has_corruption(),
        "a reopen does not report the discarded pack as missing: {:?}",
        s.recovery()
    );
}

/// True when `ACKED` holds an accepted entry covering the whole of `pack`.
///
/// 68 byte records with no file header: pack u32, nonce u32, state u8, offset u64 at 16, len u64
/// at 24. Mirrors `ack::encode`, which is private.
fn covers_whole_pack(bytes: &[u8], pack: u32) -> bool {
    const ENTRY: usize = 68;
    for e in bytes.as_chunks::<ENTRY>().0 {
        let p = u32::from_le_bytes(e[0..4].try_into().unwrap());
        let nonce = u32::from_le_bytes(e[4..8].try_into().unwrap());
        let state = e[8];
        let off = u64::from_le_bytes(e[16..24].try_into().unwrap());
        let len = u64::from_le_bytes(e[24..32].try_into().unwrap());
        if p == pack && state == 1 && nonce == 0 && off == 0 && len == u64::MAX {
            return true;
        }
    }
    false
}

/// A crash at any step of `discard_pack` leaves the store readable and never reports a lost pack.
///
/// `remove_file` is not in the op log, so this cannot be replayed from the log. Instead a child
/// process dies at the Nth fault boundary inside the discard, which is the only place the ordering
/// can go wrong, and the parent inspects what survived.
#[test]
fn a_crash_at_every_step_of_a_discard_leaves_the_store_clean() {
    use std::process::Command;
    let d = tempfile::tempdir().unwrap();
    let s = store(d.path());
    let live: Vec<BlockId> = (0..3u8).map(|i| s.put(&data(4096, i)).unwrap()).collect();
    for i in 20..24u8 {
        s.put(&data(4096, i)).unwrap();
    }
    s.sync().unwrap();
    let old = s.packs().unwrap()[0].id;
    let plan = s
        .plan_pack(old, &|b| live.contains(&b), &mut Vec::new())
        .unwrap();
    let mut c = s.begin_compaction(&plan, &|b| live.contains(&b)).unwrap();
    while !s.copy_batch(&mut c, 0).unwrap() {}
    let rw = s.finish_compaction(&c).unwrap();
    let mut ids = String::new();
    for b in &rw.condemned {
        ids.push_str(&hex(b.as_bytes()));
        ids.push('\n');
    }
    drop(s);
    fs::write(d.path().join("CONDEMNED"), ids).unwrap();

    let mut cases = 0;
    for n in 1..=40u64 {
        let sub = d.path().join(format!("n{n}"));
        copy_tree(d.path(), &sub);
        fs::remove_file(sub.join("CONDEMNED")).ok();
        fs::write(
            sub.join("CONDEMNED"),
            rw.condemned
                .iter()
                .map(|b| format!("{}\n", hex(b.as_bytes())))
                .collect::<String>(),
        )
        .unwrap();
        let code = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "--nocapture", "discard_crash_child"])
            .env("C7D_DISCARD_DIR", &sub)
            .env("C7D_DISCARD_N", n.to_string())
            .output()
            .unwrap();
        let status = code.status.code().unwrap_or(-1);
        assert!(
            matches!(status, 0 | 77),
            "child n={n} failed: {status} {}",
            String::from_utf8_lossy(&code.stderr)
        );
        cases += 1;

        let acked = fs::read(sub.join("ACKED")).unwrap_or_default();
        for p in pack_ids(&sub) {
            assert!(
                !covers_whole_pack(&acked, p),
                "n={n}: pack {p} is on disk and accepted as a whole"
            );
        }
        let reopened = store(&sub);
        let rep = reopened.recovery();
        assert!(
            !rep.has_corruption(),
            "n={n}: a discard reported a loss: {rep:?}"
        );
        assert!(reopened.fsck().unwrap().is_clean(), "n={n}: fsck dirty");
        for (i, b) in live.iter().enumerate() {
            assert_eq!(
                reopened.get(*b).unwrap(),
                data(4096, i as u8),
                "n={n}: a live block lost"
            );
        }
    }
    assert_eq!(cases, 40, "every boundary tried");
}

#[test]
fn discard_crash_child() {
    let Ok(dir) = std::env::var("C7D_DISCARD_DIR") else {
        return;
    };
    let n: u64 = std::env::var("C7D_DISCARD_N").unwrap().parse().unwrap();
    let s = match Store::open(Path::new(&dir), Options::default()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("child open: {e:?}");
            std::process::exit(3);
        }
    };
    let condemned: Vec<BlockId> = fs::read_to_string(Path::new(&dir).join("CONDEMNED"))
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| BlockId::from_bytes(unhex(l).try_into().unwrap()))
        .collect();
    // Set after the open, so the boundaries that matter are the ones inside the discard.
    std::env::set_var("C7D_EXIT_BOUNDARY_N", n.to_string());
    cowfs_store::oplog_start();
    cowfs_store::oplog_marker(n);
    let _ = s.discard_pack(s.packs().unwrap()[0].id, &condemned);
    std::process::exit(0);
}

#[cfg(feature = "fault-injection")]
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap_or(0))
        .collect()
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to.join("packs")).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let n = e.file_name();
        if n == "LOCK" || n.to_string_lossy().ends_with(".tmp") {
            continue;
        }
        if e.path().is_dir() {
            continue;
        }
        fs::write(to.join(&n), fs::read(e.path()).unwrap()).unwrap();
    }
    if from.join("packs").is_dir() {
        for e in fs::read_dir(from.join("packs")).unwrap().flatten() {
            fs::write(
                to.join("packs").join(e.file_name()),
                fs::read(e.path()).unwrap(),
            )
            .unwrap();
        }
    }
}

/// A pack the collector creates gets a durably reserved id, so nothing reuses it.
///
/// Round 4-5 made every pack id durable before the file is created, because a pack id that comes
/// back means a stale index entry can name bytes that are not the bytes it meant.
#[test]
fn a_collector_pack_id_is_reserved_durably_and_never_reused() {
    let d = tempfile::tempdir().unwrap();
    let s = store(d.path());
    let live: Vec<BlockId> = (0..2u8).map(|i| s.put(&data(4096, i)).unwrap()).collect();
    for i in 20..24u8 {
        s.put(&data(4096, i)).unwrap();
    }
    s.sync().unwrap();
    let old = s.packs().unwrap()[0].id;

    let plan = s
        .plan_pack(old, &|b| live.contains(&b), &mut Vec::new())
        .unwrap();
    let mut c = s.begin_compaction(&plan, &|b| live.contains(&b)).unwrap();
    while !s.copy_batch(&mut c, 0).unwrap() {}
    let rw = s.finish_compaction(&c).unwrap();
    s.discard_pack(old, &rw.condemned).unwrap();
    assert_eq!(s.pack_len(old), 0, "the source is gone");

    // Now write enough to roll several packs. None may land on `old` or `rw.to`.
    for i in 0..400u16 {
        s.put(&data(2000, (i % 250) as u8)).unwrap();
    }
    s.sync().unwrap();
    // `rw.to` is the pack that now holds the live records, so it is meant to be there. The id that
    // must never come back is the discarded one.
    let ids = pack_ids(d.path());
    assert!(
        ids.contains(&rw.to),
        "the copy still holds the live records: {ids:?}"
    );
    assert!(
        !ids.contains(&old),
        "the discarded id {old} came back: {ids:?}"
    );

    // And the reservation is durable, not just in memory: a reopen still refuses those ids.
    drop(s);
    let s = store(d.path());
    for i in 0..400u16 {
        s.put(&data(2000, (i % 251) as u8)).unwrap();
    }
    s.sync().unwrap();
    let ids = pack_ids(d.path());
    assert!(!ids.contains(&old), "after a reopen: {ids:?}");
    for b in &live {
        assert!(s.get(*b).is_ok(), "a live block survived the whole thing");
    }
}

/// `discard_pack` drops an index entry only when it points into the pack being discarded.
///
/// After a compaction the index points at the new pack, so the copied ids are named in the source
/// pack's condemned list while their entries belong to the copy. Dropping those entries
/// unconditionally makes the copied blocks unreadable, which is data loss, not bookkeeping.
#[test]
fn discarding_a_pack_keeps_an_index_entry_that_points_at_the_copy() {
    let d = tempfile::tempdir().unwrap();
    // A small roll, so a sealed pack exists to compact and discard.
    let s = small_store(d.path());
    let live: Vec<BlockId> = (0..3u32).map(|i| s.put(&noisy(4096, i)).unwrap()).collect();
    for i in 20..24u32 {
        s.put(&noisy(4096, i)).unwrap();
    }
    for i in 100..140u32 {
        s.put(&noisy(4096, i)).unwrap();
    }
    s.sync().unwrap();
    let old = s
        .packs()
        .unwrap()
        .into_iter()
        .find(|p| !p.active)
        .expect("a sealed pack")
        .id;

    let plan = s
        .plan_pack(old, &|b| live.contains(&b), &mut Vec::new())
        .unwrap();
    let mut c = s.begin_compaction(&plan, &|b| live.contains(&b)).unwrap();
    while !s.copy_batch(&mut c, 0).unwrap() {}
    let rw = s.finish_compaction(&c).unwrap();
    assert_eq!(rw.records, 3, "the live records were copied");
    assert!(s.pack_len(old) > 0, "the source is still whole");
    for b in &live {
        assert!(s.contains(*b), "still indexed after the repoint");
    }

    // A caller that condemns the copied ids while discarding the source must not drop their
    // entries, because those entries name the copy.
    s.discard_pack(old, &live).expect("discard");
    for (i, b) in live.iter().enumerate() {
        assert!(
            s.contains(*b),
            "the index entry for a copied block was dropped: {b:?}"
        );
        assert_eq!(s.get(*b).unwrap(), noisy(4096, i as u32));
    }
    assert!(s.fsck().unwrap().is_clean());
}

/// A crash at every step of finishing a copy leaves a store that reads.
///
/// `finish_compaction` fsyncs the new pack before the index names it, so a crash anywhere in it
/// leaves either the old index or a new one that points at bytes that are already durable.
#[test]
fn a_crash_at_every_step_of_finishing_a_copy_leaves_the_store_readable() {
    use std::process::Command;
    let d = tempfile::tempdir().unwrap();
    let s = small_store(d.path());
    let live: Vec<BlockId> = (0..3u32).map(|i| s.put(&noisy(4096, i)).unwrap()).collect();
    for i in 20..24u32 {
        s.put(&noisy(4096, i)).unwrap();
    }
    for i in 100..140u32 {
        s.put(&noisy(4096, i)).unwrap();
    }
    s.sync().unwrap();
    // The pack is named in a file, so the child compacts exactly this one. It picks the first
    // sealed pack itself otherwise, and a different pack holds no live record, so there is no copy
    // to make durable and the test would assert nothing.
    let old = s
        .packs()
        .unwrap()
        .into_iter()
        .find(|p| !p.active)
        .expect("a sealed pack")
        .id;
    fs::write(d.path().join("COPY_PACK"), old.to_string()).unwrap();

    let plan = s
        .plan_pack(old, &|b| live.contains(&b), &mut Vec::new())
        .unwrap();
    assert!(
        plan.live_bytes > 0,
        "the pack holds live records, so a copy is made"
    );
    let mut c = s.begin_compaction(&plan, &|b| live.contains(&b)).unwrap();
    while !s.copy_batch(&mut c, 0).unwrap() {}
    drop(s);
    let mut ids = String::new();
    for i in 20..24u32 {
        ids.push_str(&hex(BlockId::of(&noisy(4096, i)).as_bytes()));
        ids.push('\n');
    }
    fs::write(d.path().join("CONDEMNED"), ids).unwrap();

    let mut cases = 0;
    for n in 1..=30u64 {
        let sub = d.path().join(format!("f{n}"));
        copy_tree(d.path(), &sub);
        let code = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "--nocapture", "finish_copy_crash_child"])
            .env("C7D_FINISH_DIR", &sub)
            .env("C7D_FINISH_N", n.to_string())
            .output()
            .unwrap();
        let status = code.status.code().unwrap_or(-1);
        assert!(
            matches!(status, 0 | 77),
            "child n={n} failed: {status} {}",
            String::from_utf8_lossy(&code.stderr)
        );
        cases += 1;
        let reopened = small_store(&sub);
        assert!(
            reopened.fsck().unwrap().is_clean(),
            "n={n}: fsck dirty after a crash finishing a copy"
        );
        assert!(
            !reopened.recovery().has_corruption(),
            "n={n}: corruption reported"
        );
        for (i, b) in live.iter().enumerate() {
            assert_eq!(
                reopened.get(*b).expect("a live block reads"),
                noisy(4096, i as u32),
                "n={n}: a live block lost"
            );
        }
    }
    let _ = old;
    assert_eq!(cases, 30, "every boundary tried");
}

#[test]
fn finish_copy_crash_child() {
    let Ok(dir) = std::env::var("C7D_FINISH_DIR") else {
        return;
    };
    let n: u64 = std::env::var("C7D_FINISH_N").unwrap().parse().unwrap();
    let s = small_store(Path::new(&dir));
    let live: std::collections::HashSet<BlockId> =
        (0..3u32).map(|i| BlockId::of(&noisy(4096, i))).collect();
    let sealed = fs::read_to_string(Path::new(&dir).join("COPY_PACK")).expect("COPY_PACK");
    let sealed = sealed.trim().parse::<u32>().expect("a pack id");
    let plan = s
        .plan_pack(sealed, &|b| live.contains(&b), &mut Vec::new())
        .unwrap();
    let mut c = s.begin_compaction(&plan, &|b| live.contains(&b)).unwrap();
    while !s.copy_batch(&mut c, 0).unwrap() {}
    std::env::set_var("C7D_EXIT_BOUNDARY_N", n.to_string());
    let _ = s.finish_compaction(&c);
    std::process::exit(0);
}

/// The two fsyncs the ordering rests on, asserted through the op log.
///
/// A process exit cannot show either one: the index entry names a pack that is already visible, and
/// an unlinked name is already gone as far as the next open is concerned. So the ordering is
/// asserted where it can be seen, in the op log, between the markers the two paths place.
#[test]
fn the_copy_is_fsynced_before_the_index_names_it() {
    let d = tempfile::tempdir().unwrap();
    let s = small_store(d.path());
    let live: Vec<BlockId> = (0..3u32).map(|i| s.put(&noisy(4096, i)).unwrap()).collect();
    for i in 20..24u32 {
        s.put(&noisy(4096, i)).unwrap();
    }
    for i in 100..140u32 {
        s.put(&noisy(4096, i)).unwrap();
    }
    s.sync().unwrap();
    let old = s
        .packs()
        .unwrap()
        .into_iter()
        .find(|p| !p.active)
        .expect("a sealed pack")
        .id;
    let plan = s
        .plan_pack(old, &|b| live.contains(&b), &mut Vec::new())
        .unwrap();
    assert!(plan.live_bytes > 0, "the pack holds live records");
    let mut c = s.begin_compaction(&plan, &|b| live.contains(&b)).unwrap();
    while !s.copy_batch(&mut c, 0).unwrap() {}

    cowfs_store::oplog_start();
    let rw = s.finish_compaction(&c).expect("finish");
    let ops = cowfs_store::oplog_take();
    assert_eq!(rw.records, 3, "the live records were copied");

    let (before, after) = window(&ops, 9_101, 9_102);
    assert!(
        ops[before..after]
            .iter()
            .any(|o| matches!(o, LogOp::Sync { file } if file.contains(".cpk"))),
        "the new pack was not fsynced between the markers, so the index can name bytes that are \
         not durable: {ops:?}"
    );
    assert!(
        !ops[after..]
            .iter()
            .any(|o| matches!(o, LogOp::Sync { file } if file.contains(".cpk"))),
        "the pack is fsynced again after the index names it, which is too late: {ops:?}"
    );
    for b in &live {
        assert!(s.contains(*b), "still indexed: {b:?}");
    }
}

/// An unlink is only durable once the packs directory is fsynced after it.
#[test]
fn the_packs_directory_is_fsynced_after_the_unlink() {
    let d = tempfile::tempdir().unwrap();
    let s = small_store(d.path());
    for i in 0..3u32 {
        s.put(&noisy(4096, i)).unwrap();
    }
    for i in 20..24u32 {
        s.put(&noisy(4096, i)).unwrap();
    }
    for i in 100..140u32 {
        s.put(&noisy(4096, i)).unwrap();
    }
    s.sync().unwrap();
    let victim = s
        .packs()
        .unwrap()
        .into_iter()
        .find(|p| !p.active)
        .expect("a sealed pack")
        .id;

    cowfs_store::oplog_start();
    let freed = s.discard_pack(victim, &[]).expect("discard");
    let ops = cowfs_store::oplog_take();
    assert!(freed > 0, "the pack was freed");

    let (before, after) = window(&ops, 9_001, 9_002);
    assert!(
        ops[before..after].iter().any(|o| matches!(o, LogOp::DirSync)),
        "the packs directory was not fsynced after the unlink, so the unlink can be lost while the \
         watermark already says the pack is gone: {ops:?}"
    );
}

/// The half-open slice between two markers.
fn window(ops: &[LogOp], lo: u64, hi: u64) -> (usize, usize) {
    let at = |v: u64| {
        ops.iter()
            .position(|o| matches!(o, LogOp::Marker(m) if *m == v))
            .unwrap_or_else(|| panic!("marker {v} was never placed: {ops:?}"))
    };
    let (a, b) = (at(lo), at(hi));
    assert!(a < b, "marker {hi} came before marker {lo}: {ops:?}");
    (a, b)
}
