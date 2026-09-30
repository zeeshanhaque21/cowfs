mod common;

use std::collections::HashMap;

use common::{fixture, install_wm, opts, parse_pack, random, Fixture, PACK_HEADER};
use cowfs_store::{BlockId, Error, Store};

const SMALL: &[(usize, bool)] = &[
    (0, false),
    (40, false),
    (90, true),
    (60, false),
    (130, true),
];
const BIG_PACK: u64 = 1 << 20;

fn open(dir: &std::path::Path) -> Store {
    Store::open_unsynced(dir, opts()).unwrap()
}

fn data_of(fx: &Fixture) -> HashMap<BlockId, &Vec<u8>> {
    fx.blocks.iter().map(|(id, d)| (*id, d)).collect()
}

fn crash_at_every_offset(fx: &Fixture) {
    let recs: Vec<_> = fx.packs.iter().map(|p| parse_pack(p)).collect();
    let last = fx.packs.len() - 1;
    let data = data_of(fx);
    let dir = tempfile::tempdir().unwrap();
    for cut in 0..=fx.packs[last].len() {
        let mut packs: Vec<(u32, &[u8])> = fx
            .packs
            .iter()
            .enumerate()
            .map(|(i, p)| (i as u32, &p[..]))
            .collect();
        packs[last].1 = &fx.packs[last][..cut];
        let kept_end = recs[last]
            .iter()
            .filter(|r| r.2 <= cut)
            .map(|r| r.2)
            .max()
            .unwrap_or(PACK_HEADER.len());
        let mark = if cut >= PACK_HEADER.len() {
            Some((last as u32, kept_end as u64))
        } else if last > 0 {
            Some((last as u32 - 1, fx.packs[last - 1].len() as u64))
        } else {
            None
        };
        install_wm(dir.path(), &packs, None, mark);

        let check = |s: &Store| {
            for (p, rs) in recs.iter().enumerate() {
                for (id, _, end) in rs {
                    if p < last || *end <= cut {
                        assert_eq!(&s.get(*id).unwrap(), data[id], "cut {cut}");
                    } else {
                        assert!(
                            matches!(s.get(*id), Err(Error::NotFound(_))),
                            "torn record served at cut {cut}"
                        );
                    }
                }
            }
        };
        let s = open(dir.path());
        check(&s);
        if cut >= PACK_HEADER.len() {
            assert_eq!(
                s.recovery().truncated_bytes,
                (cut - kept_end) as u64,
                "cut {cut}"
            );
        }
        assert!(s.fsck().unwrap().is_clean(), "cut {cut}");
        let extra = random(cut as u64, 77);
        let nid = s.put(&extra).unwrap();
        s.sync().unwrap();
        drop(s);

        let s = open(dir.path());
        check(&s);
        assert_eq!(s.recovery().truncated_bytes, 0, "cut {cut}");
        assert_eq!(s.get(nid).unwrap(), extra, "cut {cut}");
        assert!(s.fsck().unwrap().is_clean(), "cut {cut}");
    }
}

#[test]
fn crash_truncating_single_pack_at_every_offset() {
    crash_at_every_offset(&fixture(SMALL, BIG_PACK));
}

#[test]
fn crash_truncating_last_of_several_packs_at_every_offset() {
    let specs: Vec<_> = (0..8).map(|i| (100 + 7 * i, i % 2 == 0)).collect();
    let fx = fixture(&specs, 420);
    assert!(fx.packs.len() >= 3, "{} packs", fx.packs.len());
    crash_at_every_offset(&fx);
}

#[test]
fn torn_middle_record_is_skipped_and_neighbours_survive() {
    let fx = fixture(SMALL, BIG_PACK);
    let pack = &fx.packs[0];
    let recs = parse_pack(pack);
    let m = 2;
    let dir = tempfile::tempdir().unwrap();
    for c in recs[m].1..recs[m].2 {
        let mut b = pack.clone();
        b[c..recs[m].2].fill(0);
        let changed = b != *pack;
        install_wm(dir.path(), &[(0, &b)], None, Some((0, b.len() as u64)));
        let s = open(dir.path());
        assert_eq!(s.recovery().truncated_bytes, 0, "zeroed from {c}");
        assert_eq!(s.recovery().has_corruption(), changed, "zeroed from {c}");
        for (i, (id, d)) in fx.blocks.iter().enumerate() {
            if i == m && changed {
                assert!(s.get(*id).is_err(), "zeroed from {c}");
            } else {
                assert_eq!(&s.get(*id).unwrap(), d, "zeroed from {c}");
            }
        }
        assert_eq!(s.fsck().unwrap().is_clean(), !changed, "zeroed from {c}");
        assert_eq!(!s.recovery().gaps.is_empty(), changed, "zeroed from {c}");
    }
}

#[test]
fn garbage_after_the_last_record_is_cut_off() {
    let fx = fixture(SMALL, BIG_PACK);
    let dir = tempfile::tempdir().unwrap();
    for glen in [1usize, 2, 3, 7, 55, 56, 57, 200, 1000] {
        for garbage in [vec![0u8; glen], random(glen as u64, glen)] {
            let mut b = fx.packs[0].clone();
            b.extend_from_slice(&garbage);
            let durable = fx.packs[0].len() as u64;
            install_wm(dir.path(), &[(0, &b)], None, Some((0, durable)));
            let s = open(dir.path());
            for (id, d) in &fx.blocks {
                assert_eq!(&s.get(*id).unwrap(), d);
            }
            assert!(!s.recovery().has_corruption());
            assert_eq!(s.recovery().truncated_bytes, glen as u64);
            assert!(s.fsck().unwrap().is_clean());
        }
    }
}

/// Flip each `(byte, bit)` in the pack and check that wrong data is never served and the damage is seen.
fn flip_check(fx: &Fixture, positions: impl Iterator<Item = (usize, u8)>, with_index: bool) {
    let recs = parse_pack(&fx.packs[0]);
    let dir = tempfile::tempdir().unwrap();
    for (byte, bit) in positions {
        let mut b = fx.packs[0].clone();
        b[byte] ^= 1 << bit;
        let mark = Some((0, b.len() as u64));
        install_wm(
            dir.path(),
            &[(0, &b)],
            with_index.then_some(&fx.index[..]),
            mark,
        );
        let hit = recs.iter().position(|r| (r.1..r.2).contains(&byte));
        let ctx = format!("byte {byte} bit {bit}");
        let s = match Store::open_unsynced(dir.path(), opts()) {
            Err(_) => {
                assert!(hit.is_none(), "open failed for a record flip, {ctx}");
                continue;
            }
            Ok(s) => s,
        };
        // A flip inside a record makes that record unreadable. A flip in the 16 byte pack header
        // (magic, version or creation nonce) leaves every record intact, so nothing is lost and the
        // checkpoint, which names the nonce, is dropped instead.
        if byte < 16 {
            assert!(!s.recovery().index_loaded, "stale index kept, {ctx}");
            for (id, d) in &fx.blocks {
                assert_eq!(&s.get(*id).unwrap(), d, "{ctx}");
            }
            assert_eq!(s.recovery().truncated_bytes, 0, "{ctx}");
            assert!(s.fsck().unwrap().is_clean(), "{ctx}");
            continue;
        }
        assert!(hit.is_some(), "record flip missed, {ctx}");
        assert_eq!(s.recovery().index_loaded, with_index, "{ctx}");
        for (i, (id, d)) in fx.blocks.iter().enumerate() {
            if Some(i) == hit {
                assert!(s.get(*id).is_err(), "flipped record served, {ctx}");
            } else {
                assert_eq!(&s.get(*id).unwrap(), d, "{ctx}");
            }
        }
        assert_eq!(s.recovery().truncated_bytes, 0, "synced bytes cut, {ctx}");
        let on_disk = std::fs::metadata(common::pack_path(dir.path(), 0))
            .unwrap()
            .len();
        assert_eq!(on_disk, b.len() as u64, "synced bytes deleted, {ctx}");
        if !with_index {
            assert!(s.recovery().has_corruption(), "flip not reported, {ctx}");
            assert!(!s.fsck().unwrap().is_clean(), "flip not detected, {ctx}");
        }
    }
}

#[test]
fn without_a_watermark_damage_is_cut_but_kept_in_a_sidecar_and_reported() {
    let fx = fixture(SMALL, BIG_PACK);
    let recs = parse_pack(&fx.packs[0]);
    let last = recs[recs.len() - 1];
    let dir = tempfile::tempdir().unwrap();
    let mut b = fx.packs[0].clone();
    b[last.2 - 3] ^= 0x40;
    b.extend_from_slice(&[9u8; 70]);
    common::install(dir.path(), &[(0, &b)], None);
    let s = open(dir.path());
    assert!(s.recovery().watermark_missing);
    assert!(
        s.recovery().has_corruption(),
        "unclassifiable damage must be loud"
    );
    assert_eq!(s.recovery().torn_tail_discarded, (b.len() - last.1) as u64);
    let on_disk = std::fs::read(common::pack_path(dir.path(), 0)).unwrap();
    assert_eq!(on_disk, b[..last.1], "only the damaged tail is cut");
    let side = std::fs::read(format!(
        "{}.torn-0",
        common::pack_path(dir.path(), 0).display()
    ))
    .unwrap();
    assert_eq!(side, b[last.1..], "cut bytes are preserved");
    for (id, d) in &fx.blocks[..fx.blocks.len() - 1] {
        assert_eq!(&s.get(*id).unwrap(), d);
    }
    drop(s);
    let s = open(dir.path());
    assert!(!s.recovery().has_corruption(), "reported once, not forever");
}

fn every_bit(len: usize) -> impl Iterator<Item = (usize, u8)> {
    (0..len).flat_map(|b| (0..8).map(move |k| (b, k)))
}

#[test]
fn every_bit_flip_in_a_small_pack_is_detected_after_a_rebuild() {
    let fx = fixture(SMALL, BIG_PACK);
    flip_check(&fx, every_bit(fx.packs[0].len()), false);
}

#[test]
fn every_bit_flip_in_a_small_pack_is_detected_with_a_valid_index() {
    let fx = fixture(SMALL, BIG_PACK);
    flip_check(&fx, every_bit(fx.packs[0].len()), true);
}

#[test]
fn sampled_bit_flips_in_a_large_pack_are_detected() {
    let fx = fixture(
        &[(30_000, false), (30_000, true), (50_000, false)],
        BIG_PACK,
    );
    let len = fx.packs[0].len();
    let rnd = random(99, 8 * 600);
    let picks: Vec<(usize, u8)> = rnd
        .chunks(8)
        .map(|c| {
            let v = u64::from_le_bytes(c.try_into().unwrap());
            ((v >> 3) as usize % len, (v & 7) as u8)
        })
        .collect();
    flip_check(&fx, picks.iter().copied(), false);
    flip_check(&fx, picks.iter().copied(), true);
}
