//! Regression tests for the round-2 critic findings N1 to N9. Names starting `d`, `q` and `r`
//! come from the critic's reproducers; the assertions are theirs unless a comment says otherwise.

mod common;

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::process::Command;

use common::{
    index_bytes, install_wm, opts, pack_ids, pack_path, parse_pack, random, record, PACK_HEADER,
    REC_HDR,
};
use cowfs_store::{BlockId, Error, Op, Options, Store};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn decode_slot(b: &[u8]) -> Option<(u64, u32, u64)> {
    let b: &[u8; 32] = b.try_into().ok()?;
    if crc32c::crc32c(&b[..24]).to_le_bytes() != b[24..28] {
        return None;
    }
    Some((
        u64::from_le_bytes(b[..8].try_into().unwrap()),
        u32::from_le_bytes(b[8..12].try_into().unwrap()),
        u64::from_le_bytes(b[16..24].try_into().unwrap()),
    ))
}

fn read_mark(dir: &Path) -> Option<(u32, u64)> {
    let b = fs::read(dir.join("SYNCED")).ok()?;
    let mut best: Option<(u64, u32, u64)> = None;
    for i in 0..2 {
        if b.len() >= (i + 1) * 32 {
            if let Some(s) = decode_slot(&b[i * 32..(i + 1) * 32]) {
                if best.is_none_or(|x| s.0 > x.0) {
                    best = Some(s);
                }
            }
        }
    }
    best.map(|(_, p, l)| (p, l))
}

fn copy_all(from: &Path, to: &Path) {
    fs::create_dir_all(to.join("packs")).unwrap();
    for id in pack_ids(from) {
        fs::copy(pack_path(from, id), pack_path(to, id)).unwrap();
    }
    for f in ["SYNCED", "index.cix", "ACKED"] {
        if from.join(f).exists() {
            fs::copy(from.join(f), to.join(f)).unwrap();
        }
    }
}

fn small_packs() -> Options {
    Options {
        max_pack_size: 20_000,
        ..opts()
    }
}

#[derive(Clone, Copy, Debug)]
enum WmMode {
    Intact,
    NewestTorn,
    BothTorn,
}

/// Damage the crash image: only bytes past the durable mark of the active pack may be lost.
fn damage(img: &Path, rng: &mut Rng, mode: WmMode, sector: u64) {
    let mark = read_mark(img);
    let last = *pack_ids(img).last().unwrap();
    let p = pack_path(img, last);
    let len = fs::metadata(&p).unwrap().len();
    let durable = match mark {
        Some((pk, l)) if pk == last => l,
        Some((pk, _)) if pk > last => len,
        _ => 16,
    };
    let f = OpenOptions::new().write(true).open(&p).unwrap();
    let keep_len = durable + rng.below(len - durable + 1);
    f.set_len(keep_len).unwrap();
    let mut pos = durable - durable % sector;
    while pos < keep_len {
        if rng.below(3) == 0 {
            let start = pos.max(durable);
            let end = (pos + sector).min(keep_len);
            if end > start {
                let junk = if rng.below(2) == 0 {
                    vec![0u8; (end - start) as usize]
                } else {
                    random(rng.next(), (end - start) as usize)
                };
                f.write_at(&junk, start).unwrap();
            }
        }
        pos += sector;
    }
    let sy = img.join("SYNCED");
    if sy.exists() {
        let mut b = fs::read(&sy).unwrap();
        let slots: Vec<usize> = (0..2)
            .filter(|i| b.len() >= (i + 1) * 32 && decode_slot(&b[i * 32..(i + 1) * 32]).is_some())
            .collect();
        let newest = slots
            .iter()
            .copied()
            .max_by_key(|&i| decode_slot(&b[i * 32..(i + 1) * 32]).unwrap().0);
        match (mode, newest) {
            (WmMode::NewestTorn, Some(i)) => b[i * 32 + 16] ^= 0xff,
            (WmMode::BothTorn, _) => {
                for i in slots {
                    b[i * 32 + 16] ^= 0xff;
                }
            }
            _ => {}
        }
        fs::write(&sy, b).unwrap();
    }
}

fn check_acked(s: &Store, model: &HashMap<BlockId, Vec<u8>>, acked: &[BlockId], ctx: &str) {
    for id in acked {
        match s.get(*id) {
            Ok(d) => assert_eq!(&d, &model[id], "{ctx}: served wrong bytes"),
            Err(e) => panic!("{ctx}: acked block lost: {e:?}"),
        }
    }
    for (id, d) in model {
        if let Ok(got) = s.get(*id) {
            assert_eq!(&got, d, "{ctx}: served wrong bytes (unacked)");
        }
    }
}

/// Returns `(open1_reported_corruption, open2_reported_corruption)` or a hard failure.
fn run_case(seed: u64, mode: WmMode, sector: u64) -> Result<(bool, bool), String> {
    let mut rng = Rng(seed);
    let dir = tempfile::tempdir().unwrap();
    let img = tempfile::tempdir().unwrap();
    let o = Options {
        max_pack_size: 60_000 + rng.below(100_000),
        checkpoint_on_drop: false,
        ..Options::default()
    };
    let mut model: HashMap<BlockId, Vec<u8>> = HashMap::new();
    let mut acked: Vec<BlockId> = Vec::new();
    let mut pending: Vec<BlockId> = Vec::new();
    {
        let s = Store::open_unsynced(dir.path(), o).unwrap();
        let steps = 10 + rng.below(40);
        for _ in 0..steps {
            match rng.below(6) {
                0 => {
                    s.sync().unwrap();
                    acked.append(&mut pending);
                }
                1 if rng.below(3) == 0 => {
                    s.checkpoint().unwrap();
                    acked.append(&mut pending);
                }
                _ => {
                    let d = random(rng.next(), 500 + rng.below(9000) as usize);
                    let id = s.put(&d).unwrap();
                    model.insert(id, d);
                    pending.push(id);
                }
            }
        }
        copy_all(dir.path(), img.path());
    }
    damage(img.path(), &mut rng, mode, sector);
    let tag = format!("seed={seed} mode={mode:?} sector={sector}");
    let mut second: Vec<(BlockId, Vec<u8>)> = Vec::new();
    let first_reported;
    {
        let s = Store::open_unsynced(img.path(), o).map_err(|e| format!("{tag}: open1 {e:?}"))?;
        let r = s.recovery().clone();
        // Damage the store could not classify (a cut made with no watermark) is reported until it
        // is accepted, so it is expected here; anything else is a false report.
        first_reported = r.has_corruption() && !r.corrupt_synced.iter().all(|c| c.unclassified);
        if r.has_corruption() && first_reported && !matches!(mode, WmMode::BothTorn) {
            return Err(format!("{tag}: FALSE corruption on open1 {r:?}"));
        }
        check_acked(&s, &model, &acked, &format!("{tag} open1"));
        for i in 0..5 {
            let d = random(rng.next() ^ i, 700 + rng.below(3000) as usize);
            let id = s.put(&d).unwrap();
            second.push((id, d));
        }
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    let s = Store::open_unsynced(img.path(), o).map_err(|e| format!("{tag}: open2 {e:?}"))?;
    let r = s.recovery().clone();
    check_acked(&s, &model, &acked, &format!("{tag} open2"));
    for (id, d) in &second {
        assert_eq!(&s.get(*id).unwrap(), d, "{tag}: second-cycle block lost");
    }
    if r.has_corruption() && !r.corrupt_synced.iter().all(|c| c.unclassified) {
        return Err(format!(
            "{tag}: FALSE corruption on open2 (after benign torn write + one sync): {:?}",
            r.corrupt_synced
        ));
    }
    Ok((first_reported, false))
}

/// N1: 1800 simulated power losses at full size (`COWFS_POWERLOSS_SEEDS=400`), 270 by default.
#[test]
fn power_loss_orderings() {
    let seeds: u64 = std::env::var("COWFS_POWERLOSS_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let mut fails: HashMap<String, (u32, String)> = HashMap::new();
    let (mut total, mut first_reports, mut intact_or_newest_reports) = (0, 0, 0);
    for seed in 0..seeds {
        for &mode in &[WmMode::Intact, WmMode::NewestTorn, WmMode::BothTorn] {
            for &sector in &[512u64, 4096] {
                if matches!(mode, WmMode::BothTorn) && seed % 4 != 0 {
                    continue;
                }
                total += 1;
                match run_case(seed, mode, sector) {
                    Ok((first, _)) => {
                        first_reports += u32::from(first);
                        intact_or_newest_reports +=
                            u32::from(first && !matches!(mode, WmMode::BothTorn));
                    }
                    Err(e) => {
                        let key = e
                            .split(": ")
                            .nth(1)
                            .unwrap_or("")
                            .chars()
                            .take(40)
                            .collect::<String>();
                        let ent = fails
                            .entry(format!("{mode:?}/{key}"))
                            .or_insert((0, e.clone()));
                        ent.0 += 1;
                    }
                }
            }
        }
    }
    println!(
        "cases={total} open1_reported={first_reports} of which intact_or_newest={intact_or_newest_reports}"
    );
    for (k, (n, ex)) in &fails {
        println!("FAIL-CLASS {k}: {n} cases, e.g. {ex}");
    }
    assert!(fails.is_empty(), "{} failure classes", fails.len());
}

fn open(dir: &Path) -> Store {
    Store::open(dir, opts()).unwrap()
}

/// N1 trigger 1: record B is torn, C persisted, both after the last sync.
#[test]
fn d1_middle_gap_beyond_watermark_becomes_corruption_after_next_sync() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b, c) = (random(1, 4000), random(2, 4000), random(3, 4000));
    {
        let s = open(dir.path());
        s.put(&a).unwrap();
        s.sync().unwrap();
        s.put(&b).unwrap();
        s.put(&c).unwrap();
    }
    let p = pack_path(dir.path(), 0);
    let f = OpenOptions::new().write(true).open(&p).unwrap();
    f.write_at(
        &[0u8; 512],
        (PACK_HEADER.len() + 2 * REC_HDR + 4000 + 100) as u64,
    )
    .unwrap();
    drop(f);
    {
        let s = open(dir.path());
        let r = s.recovery();
        assert!(!r.has_corruption(), "{r:?}");
        assert!(r.torn_tail_discarded > 0, "the torn record is discarded");
        assert_eq!(
            r.recovered_from_tail, 1,
            "the valid record after it moves down"
        );
        assert_eq!(s.get(BlockId::of(&a)).unwrap(), a);
        assert!(s.get(BlockId::of(&b)).is_err());
        assert_eq!(s.get(BlockId::of(&c)).unwrap(), c);
        assert!(s.fsck().unwrap().is_clean());
        s.put(&random(9, 3000)).unwrap();
        s.sync().unwrap();
    }
    let s = open(dir.path());
    assert!(
        !s.recovery().has_corruption(),
        "benign torn write became permanent corruption: {:?}",
        s.recovery()
    );
    assert_eq!(s.recovery().torn_tail_discarded, 0);
    assert_eq!(s.get(BlockId::of(&c)).unwrap(), c);
    assert!(s.fsck().unwrap().is_clean());
}

/// N1 trigger 2: SYNCED missing with a torn tail.
#[test]
fn d2_missing_watermark_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = open(dir.path());
        s.put(&random(1, 4000)).unwrap();
        s.sync().unwrap();
        s.put(&random(2, 4000)).unwrap();
    }
    let p = pack_path(dir.path(), 0);
    let len = fs::metadata(&p).unwrap().len();
    OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .set_len(len - 1000)
        .unwrap();
    fs::remove_file(dir.path().join("SYNCED")).unwrap();
    {
        let s = open(dir.path());
        assert!(s.recovery().watermark_missing);
        s.put(&random(3, 100)).unwrap();
        s.sync().unwrap();
    }
    let s = open(dir.path());
    assert!(
        s.recovery().has_corruption(),
        "a cut made without a watermark destroys unclassifiable bytes, so it is a pending loss"
    );
    assert_eq!(s.acknowledge_corruption().unwrap(), 1);
    drop(s);
    let s = open(dir.path());
    assert!(!s.recovery().has_corruption(), "accepted");
}

/// N2: the watermark names packs that are gone. It must be loud and `sync` must keep fsyncing.
#[test]
fn d3_watermark_ahead_of_all_packs_disables_fsync() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), small_packs()).unwrap();
        for i in 0..12 {
            s.put(&random(i, 9000)).unwrap();
        }
        s.sync().unwrap();
    }
    let ids = pack_ids(dir.path());
    let (mark_pack, _) = read_mark(dir.path()).unwrap();
    for id in &ids[2..] {
        fs::remove_file(pack_path(dir.path(), *id)).unwrap();
    }
    let _ = fs::remove_file(dir.path().join("index.cix"));
    let trace = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let s = Store::open_traced(dir.path(), opts(), trace.clone()).unwrap();
    assert!(s.recovery().has_corruption(), "lost packs 2.. not reported");
    assert_eq!(
        s.recovery().missing_synced,
        (2..=mark_pack).collect::<Vec<_>>()
    );
    trace.lock().unwrap().clear();
    s.put(&random(100, 5000)).unwrap();
    s.sync().unwrap();
    let t = trace.lock().unwrap().clone();
    assert!(
        t.iter()
            .any(|o| matches!(o, Op::Sync(n) if n.ends_with(".cpk"))),
        "sync() did not fsync any pack: {t:?}"
    );
    let n = s.acknowledge_corruption().unwrap();
    assert_eq!(n, s.recovery().missing_synced.len());
    drop(s);
    let s = open(dir.path());
    assert!(!s.recovery().has_corruption(), "{:?}", s.recovery());
    s.put(&random(101, 100)).unwrap();
    s.sync().unwrap();
}

/// N3: a deleted middle pack is reported, and acknowledging it makes later opens clean.
#[test]
fn d4_missing_middle_pack_is_reported_and_can_be_acknowledged() {
    let dir = tempfile::tempdir().unwrap();
    let mut blocks = Vec::new();
    {
        let s = Store::open(dir.path(), small_packs()).unwrap();
        for i in 0..12 {
            let d = random(i, 9000);
            blocks.push((s.put(&d).unwrap(), d));
        }
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    let ids = pack_ids(dir.path());
    fs::remove_file(pack_path(dir.path(), ids[1])).unwrap();
    {
        let s = Store::open(dir.path(), small_packs()).unwrap();
        let lost = blocks.iter().filter(|(id, _)| s.get(*id).is_err()).count();
        assert!(lost > 0);
        assert_eq!(s.recovery().missing_synced, vec![ids[1]]);
        assert!(s.recovery().has_corruption(), "deleted pack not reported");
        assert_eq!(s.acknowledge_corruption().unwrap(), 1);
    }
    let _ = fs::remove_file(dir.path().join("index.cix"));
    let s = Store::open(dir.path(), small_packs()).unwrap();
    assert!(!s.recovery().has_corruption(), "{:?}", s.recovery());
}

/// N6: open does not re-read the checkpointed region, so it cannot see bit rot there.
/// `verify_all` is the way to find it.
#[test]
fn d5_bit_rot_in_the_checkpointed_region_is_found_by_verify_all_not_by_open() {
    let dir = tempfile::tempdir().unwrap();
    let mut blocks = Vec::new();
    {
        let s = open(dir.path());
        for i in 0..6 {
            let d = random(i, 5000);
            blocks.push((s.put(&d).unwrap(), d));
        }
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    let f = OpenOptions::new()
        .write(true)
        .open(pack_path(dir.path(), 0))
        .unwrap();
    f.write_at(&[0xAB; 8], (PACK_HEADER.len() + REC_HDR + 3000) as u64)
        .unwrap();
    drop(f);
    let s = open(dir.path());
    assert!(s.recovery().index_loaded);
    assert_eq!(s.recovery().records_scanned, 0);
    assert!(!s.recovery().has_corruption(), "documented blind spot");
    assert!(blocks.iter().any(|(id, _)| s.get(*id).is_err()));
    let r = s.verify_all().unwrap();
    assert!(!r.is_clean(), "verify_all must find the rot");
}

/// A repaired block stops being reported: its damaged region is superseded by the new copy.
#[test]
fn q1_repair_by_put_makes_later_opens_clean_with_and_without_the_index() {
    let dir = tempfile::tempdir().unwrap();
    let data: Vec<Vec<u8>> = (0..6).map(|i| random(i, 5000)).collect();
    let mut ids = Vec::new();
    {
        let s = open(dir.path());
        for d in &data {
            ids.push(s.put(d).unwrap());
        }
        s.sync().unwrap();
    }
    let f = OpenOptions::new()
        .write(true)
        .open(pack_path(dir.path(), 0))
        .unwrap();
    f.write_at(
        &[0xAB; 8],
        PACK_HEADER.len() as u64 + ((REC_HDR + 5000) * 2 + REC_HDR + 100) as u64,
    )
    .unwrap();
    drop(f);
    let _ = fs::remove_file(dir.path().join("index.cix"));
    {
        let s = open(dir.path());
        assert!(s.recovery().has_corruption());
        assert!(matches!(s.get(ids[2]), Err(Error::Corrupt { .. })));
        for (i, id) in ids.iter().enumerate() {
            if i != 2 {
                assert_eq!(&s.get(*id).unwrap(), &data[i]);
            }
        }
        s.put(&data[2]).unwrap();
        assert_eq!(&s.get(ids[2]).unwrap(), &data[2]);
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    for round in 0..2 {
        if round == 1 {
            let _ = fs::remove_file(dir.path().join("index.cix"));
        }
        let s = open(dir.path());
        assert!(
            !s.recovery().has_corruption(),
            "round {round}: repaired corruption still reported: {:?}",
            s.recovery()
        );
        assert_eq!(s.recovery().superseded.len(), 1, "round {round}");
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(&s.get(*id).unwrap(), &data[i], "round {round} block {i}");
        }
        assert_eq!(s.stats().blocks, 6);
    }
}

/// A damaged region with a destroyed header cannot be matched to a block, so it needs an
/// explicit acknowledgement, which then survives index loss.
#[test]
fn unknown_damage_needs_an_acknowledgement_that_survives_index_loss() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = open(dir.path());
        for i in 0..4 {
            s.put(&random(i, 3000)).unwrap();
        }
        s.sync().unwrap();
    }
    let f = OpenOptions::new()
        .write(true)
        .open(pack_path(dir.path(), 0))
        .unwrap();
    f.write_at(
        &[0xEE; 10],
        PACK_HEADER.len() as u64 + REC_HDR as u64 + 3008,
    )
    .unwrap();
    drop(f);
    {
        let s = open(dir.path());
        assert!(s.recovery().has_corruption());
        assert_eq!(s.acknowledge_corruption().unwrap(), 1);
    }
    for round in 0..2 {
        let _ = fs::remove_file(dir.path().join("index.cix"));
        let s = open(dir.path());
        assert!(!s.recovery().has_corruption(), "round {round}");
        assert_eq!(s.recovery().acknowledged.len(), 1);
        assert!(!s.recovery().gaps.is_empty(), "the bytes are still there");
        s.checkpoint().unwrap();
    }
}

/// Q7: a pack cut at a record boundary below the watermark is not a torn tail.
#[test]
fn q3_pack_truncated_at_boundary_below_watermark() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = open(dir.path());
        for i in 0..6 {
            s.put(&random(i, 5000)).unwrap();
        }
        s.sync().unwrap();
    }
    let p = pack_path(dir.path(), 0);
    OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .set_len((PACK_HEADER.len() + 3 * (REC_HDR + 5000)) as u64)
        .unwrap();
    let s = open(dir.path());
    assert!(
        s.recovery().has_corruption(),
        "3 synced blocks vanished silently"
    );
}

/// Q9, S1, S2: an in-place repair over an index entry that came from a checkpoint.
#[test]
fn q2_repair_beats_stale_checkpoint_entry() {
    let dir = tempfile::tempdir().unwrap();
    let data: Vec<Vec<u8>> = (0..4).map(|i| random(i, 5000)).collect();
    let mut ids = Vec::new();
    {
        let s = open(dir.path());
        for d in &data {
            ids.push(s.put(d).unwrap());
        }
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    let f = OpenOptions::new()
        .write(true)
        .open(pack_path(dir.path(), 0))
        .unwrap();
    f.write_at(&[0xAB; 8], PACK_HEADER.len() as u64 + REC_HDR as u64 + 100)
        .unwrap();
    drop(f);
    {
        let s = open(dir.path());
        s.put(&data[0]).unwrap();
        s.sync().unwrap();
        assert_eq!(s.get(ids[0]).unwrap(), data[0]);
        let st = s.stats();
        assert_eq!(
            (st.blocks, st.uncompressed_bytes),
            (4, 20000),
            "stats drift after in-place repair: {st:?}"
        );
    }
    let s = open(dir.path());
    assert_eq!(
        s.get(ids[0]).unwrap(),
        data[0],
        "repaired block unreadable after reopen (stale checkpoint entry won)"
    );
}

/// N8: a block that rots after this session verified it: `put` still says Ok and `get` fails.
/// After a reopen the verified bit is gone, so `put` rewrites it.
#[test]
fn v1_bit_rot_after_verify_in_the_same_session_is_a_known_window() {
    let dir = tempfile::tempdir().unwrap();
    let d = random(1, 5000);
    let id;
    {
        let s = open(dir.path());
        id = s.put(&d).unwrap();
        s.sync().unwrap();
        s.checkpoint().unwrap();
        assert_eq!(s.get(id).unwrap(), d);
        let f = OpenOptions::new()
            .write(true)
            .open(pack_path(dir.path(), 0))
            .unwrap();
        f.write_at(&[0xAB; 8], PACK_HEADER.len() as u64 + REC_HDR as u64 + 100)
            .unwrap();
        drop(f);
        assert!(
            s.put(&d).is_ok(),
            "the verified bit is trusted within a session"
        );
        assert!(s.get(id).is_err());
    }
    let s = open(dir.path());
    assert!(s.put(&d).is_ok());
    assert_eq!(
        s.get(id).unwrap(),
        d,
        "a reopened store re-checks and repairs"
    );
}

/// N4: a leftover pack file where the next pack must go used to make every later put fail.
#[test]
fn r1_roll_survives_a_leftover_empty_pack_file() {
    let dir = tempfile::tempdir().unwrap();
    let o = Options {
        max_pack_size: 12_000,
        ..opts()
    };
    let s = Store::open(dir.path(), o).unwrap();
    s.put(&random(1, 9000)).unwrap();
    fs::write(pack_path(dir.path(), 1), PACK_HEADER).unwrap();
    let r1 = s.put(&random(2, 9000));
    let r2 = s.put(&random(3, 9000));
    assert!(r1.is_ok() && r2.is_ok(), "r1={r1:?} r2={r2:?}");
    assert_eq!(s.stats().packs, pack_ids(dir.path()).len() as u64);
    s.sync().unwrap();
    assert!(s.fsck().unwrap().is_clean());
}

/// N4: a leftover pack that holds data is not ours to overwrite: fail, and fail cleanly.
#[test]
fn a_leftover_pack_with_records_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let o = Options {
        max_pack_size: 12_000,
        ..opts()
    };
    let s = Store::open(dir.path(), o).unwrap();
    s.put(&random(1, 9000)).unwrap();
    let mut foreign = PACK_HEADER.to_vec();
    foreign.extend_from_slice(&random(5, 100));
    fs::write(pack_path(dir.path(), 1), &foreign).unwrap();
    assert!(s.put(&random(2, 9000)).is_err());
    assert_eq!(fs::read(pack_path(dir.path(), 1)).unwrap(), foreign);
}

const EMFILE_ENV: &str = "COWFS_EMFILE_CHILD_DIR";

/// N4 with a real EMFILE: the roll fails after creating the file, and the store recovers.
#[test]
fn emfile_during_a_roll_does_not_wedge_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "ulimit -n 48 && exec '{}' --exact emfile_child --ignored --nocapture",
            exe.display()
        ))
        .env(EMFILE_ENV, dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore]
fn emfile_child() {
    use std::fs::File;
    let Some(dir) = std::env::var_os(EMFILE_ENV) else {
        return;
    };
    let dir = Path::new(&dir);
    let o = Options {
        max_pack_size: 12_000,
        max_open_packs: 2,
        ..opts()
    };
    let s = Store::open_unsynced(dir, o).unwrap();
    let a = random(1, 9000);
    let ia = s.put(&a).unwrap();
    let mut hog = Vec::new();
    while let Ok(f) = File::open("/dev/null") {
        hog.push(f);
    }
    let b = random(2, 9000);
    assert!(s.put(&b).is_err(), "no fd at all: roll must fail");
    hog.pop();
    assert!(
        s.put(&b).is_err(),
        "one fd: publishing the skipped reservation or the pack must fail"
    );
    assert!(
        !pack_path(dir, 1).exists(),
        "a half-created pack must be removed"
    );
    drop(hog);
    let ib = s.put(&b).unwrap();
    assert_eq!(s.stats().packs, 2);
    let ids = pack_ids(dir);
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], 0);
    assert!(
        ids[1] > 1,
        "the failed reservation must stay consumed: {ids:?}"
    );
    assert_eq!(s.get(ia).unwrap(), a);
    assert_eq!(s.get(ib).unwrap(), b);
    s.sync().unwrap();
    assert!(s.fsck().unwrap().is_clean());
}

/// N5: a flood of headers that pass their own checksum still opens fast, and real records
/// before it stay readable.
#[test]
fn a_flood_of_forged_headers_with_valid_checksums_opens_fast() {
    let dir = tempfile::tempdir().unwrap();
    let d = random(1, 4000);
    let id;
    {
        let s = open(dir.path());
        id = s.put(&d).unwrap();
        s.sync().unwrap();
    }
    let p = pack_path(dir.path(), 0);
    let fake = record(0, 262_144, [7u8; 32], &[]);
    let mut hdr = fake[..REC_HDR].to_vec();
    hdr[12..16].copy_from_slice(&262_144u32.to_le_bytes());
    let hcrc = crc32c::crc32c(&hdr[..48]);
    hdr[48..52].copy_from_slice(&hcrc.to_le_bytes());
    let mut bytes = fs::read(&p).unwrap();
    for _ in 0..(4 << 20) / REC_HDR {
        bytes.extend_from_slice(&hdr);
    }
    bytes.extend_from_slice(&vec![0u8; 300_000]);
    fs::write(&p, &bytes).unwrap();
    let t = std::time::Instant::now();
    let s = Store::open(dir.path(), opts()).unwrap();
    assert!(
        t.elapsed().as_millis() < 3000,
        "open took {:?}",
        t.elapsed()
    );
    assert_eq!(s.get(id).unwrap(), d);
    assert_eq!(s.iter_ids().count(), 1);
    assert!(
        fs::metadata(&p).unwrap().len() >= bytes.len() as u64,
        "an exhausted search must not truncate what it did not examine"
    );
}

/// N5: many damaged records in one pack no longer hide the valid ones after them.
#[test]
fn many_damaged_records_do_not_hide_the_valid_ones() {
    let dir = tempfile::tempdir().unwrap();
    let n = 400usize;
    let mut ids = Vec::new();
    {
        let s = Store::open_unsynced(dir.path(), opts()).unwrap();
        for i in 0..n {
            ids.push(s.put(&random(i as u64, 3000)).unwrap());
        }
        s.sync().unwrap();
    }
    let p = pack_path(dir.path(), 0);
    let f = OpenOptions::new().write(true).open(&p).unwrap();
    for i in 0..n / 2 {
        f.write_all_at(
            &[0xEE; 2],
            (PACK_HEADER.len() + i * (REC_HDR + 3000) + REC_HDR + 100) as u64,
        )
        .unwrap();
    }
    drop(f);
    let _ = fs::remove_file(dir.path().join("index.cix"));
    let t = std::time::Instant::now();
    let s = Store::open_unsynced(dir.path(), opts()).unwrap();
    assert!(
        t.elapsed().as_millis() < 3000,
        "open took {:?}",
        t.elapsed()
    );
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(s.get(*id).is_ok(), i >= n / 2, "block {i}");
    }
    assert_eq!(s.recovery().records_scanned, (n / 2) as u64);
}

/// N5: the salvage path re-indexes verifiable records that the loaded index does not know.
#[test]
fn salvage_indexes_records_missing_from_a_loaded_index_and_repairs_bad_entries() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (random(1, 3000), random(2, 3000));
    let (ia, ib) = (BlockId::of(&a), BlockId::of(&b));
    let good_a = record(0, 3000, *ia.as_bytes(), &a);
    let bad_a = record(0, 3000, *ia.as_bytes(), &random(9, 3000));
    let rec_b = record(0, 3000, *ib.as_bytes(), &b);
    let mut pack = PACK_HEADER.to_vec();
    pack.extend_from_slice(&bad_a);
    pack.extend_from_slice(&good_a);
    pack.extend_from_slice(&rec_b);
    let slen = 3000u32;
    let ix = index_bytes(
        &[(0, pack.len() as u64)],
        &[(ia, [0, PACK_HEADER.len() as u32, slen, slen])],
    );
    install_wm(
        dir.path(),
        &[(0, &pack)],
        Some(&ix),
        Some((0, pack.len() as u64)),
    );
    let s = Store::open_unsynced(dir.path(), opts()).unwrap();
    assert!(s.recovery().index_loaded);
    assert!(matches!(s.get(ia), Err(Error::HashMismatch(_))));
    assert!(matches!(s.get(ib), Err(Error::NotFound(_))));
    let r = s.salvage().unwrap();
    assert_eq!((r.newly_indexed, r.repaired), (1, 1), "{r:?}");
    assert_eq!(r.damaged.len(), 1);
    assert_eq!(s.get(ia).unwrap(), a);
    assert_eq!(s.get(ib).unwrap(), b);
}

/// Multi-pack store: damage in a sealed pack is corruption even though the watermark names a later pack.
#[test]
fn damage_in_a_sealed_pack_is_reported_and_never_cut() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), small_packs()).unwrap();
        for i in 0..12 {
            s.put(&random(i, 9000)).unwrap();
        }
        s.sync().unwrap();
    }
    let p0 = pack_path(dir.path(), 0);
    let before = fs::metadata(&p0).unwrap().len();
    let recs = parse_pack(&fs::read(&p0).unwrap());
    let f = OpenOptions::new().write(true).open(&p0).unwrap();
    f.write_at(&[0xAB; 4], (recs[1].1 + REC_HDR + 50) as u64)
        .unwrap();
    drop(f);
    let _ = fs::remove_file(dir.path().join("index.cix"));
    let s = Store::open(dir.path(), small_packs()).unwrap();
    assert!(s.recovery().has_corruption());
    assert_eq!(s.recovery().corrupt_synced[0].pack, 0);
    assert_eq!(fs::metadata(&p0).unwrap().len(), before);
    assert_eq!(s.recovery().torn_tail_discarded, 0);
}

/// Mutant guard: a swapped (checksum-valid) index must fail on the id check, not just on the hash.
#[test]
fn a_swapped_index_is_rejected_as_a_mismatch_between_index_and_record() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (random(1, 4000), random(2, 4000));
    let (ia, ib) = (BlockId::of(&a), BlockId::of(&b));
    let mut pack = PACK_HEADER.to_vec();
    pack.extend_from_slice(&record(0, 4000, *ia.as_bytes(), &a));
    pack.extend_from_slice(&record(0, 4000, *ib.as_bytes(), &b));
    let oa = PACK_HEADER.len() as u32;
    let ob = (PACK_HEADER.len() + REC_HDR + 4000) as u32;
    let ix = index_bytes(
        &[(0, pack.len() as u64)],
        &[(ia, [0, ob, 4000, 4000]), (ib, [0, oa, 4000, 4000])],
    );
    install_wm(
        dir.path(),
        &[(0, &pack)],
        Some(&ix),
        Some((0, pack.len() as u64)),
    );
    let s = Store::open_unsynced(dir.path(), opts()).unwrap();
    assert!(s.recovery().index_loaded);
    for id in [ia, ib] {
        assert!(
            matches!(s.get(id), Err(Error::Corrupt { .. })),
            "{:?}",
            s.get(id)
        );
    }
}

/// Mutant guard: a zstd record that decodes to fewer bytes than its header says is corrupt.
#[test]
fn a_short_decode_is_corrupt_even_when_the_bytes_hash_to_the_id() {
    let dir = tempfile::tempdir().unwrap();
    let short = random(4, 500);
    let id = BlockId::of(&short);
    let payload = zstd::bulk::compress(&short, 3).unwrap();
    assert!(payload.len() < 1000);
    let mut pack = PACK_HEADER.to_vec();
    pack.extend_from_slice(&record(1, 1000, *id.as_bytes(), &payload));
    let ix = index_bytes(
        &[(0, pack.len() as u64)],
        &[(
            id,
            [0, PACK_HEADER.len() as u32, payload.len() as u32, 1000],
        )],
    );
    install_wm(
        dir.path(),
        &[(0, &pack)],
        Some(&ix),
        Some((0, pack.len() as u64)),
    );
    let s = Store::open_unsynced(dir.path(), opts()).unwrap();
    assert!(
        matches!(s.get(id), Err(Error::Corrupt { .. })),
        "{:?}",
        s.get(id).map(|v| v.len())
    );
}
