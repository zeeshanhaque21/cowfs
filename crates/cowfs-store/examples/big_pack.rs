//! Open time and loss on a full-size pack. Usage: big_pack <scratch-dir>
//! Builds one 256 MiB pack of 4096 incompressible 64 KiB blocks, then times `open` on it clean
//! (with and without the index) and after damage, and checks that only records that overlap
//! damaged bytes are lost.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::time::Instant;

use cowfs_store::{BlockId, Options, Store};

const N: usize = 4096;
const LEN: usize = 65536;
const REC: u64 = 56 + LEN as u64;

fn rnd(seed: u64, len: usize) -> Vec<u8> {
    let mut s = seed;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        out.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    out.truncate(len);
    out
}

fn opts() -> Options {
    Options {
        max_pack_size: 1 << 31,
        checkpoint_on_drop: false,
        ..Options::default()
    }
}

fn copy(from: &Path, to: &Path, index: bool) {
    let _ = fs::remove_dir_all(to);
    fs::create_dir_all(to.join("packs")).unwrap();
    fs::copy(
        from.join("packs/pack-00000000.cpk"),
        to.join("packs/pack-00000000.cpk"),
    )
    .unwrap();
    fs::copy(from.join("SYNCED"), to.join("SYNCED")).unwrap();
    if index {
        fs::copy(from.join("index.cix"), to.join("index.cix")).unwrap();
    }
}

/// Damages a pack file and returns the indices of the records it destroyed.
type Damage = Box<dyn Fn(&std::fs::File) -> Vec<usize>>;

fn record_range(i: usize) -> (u64, u64) {
    let start = 16 + i as u64 * REC;
    (start, start + REC)
}

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("scratch dir"));
    let _ = fs::remove_dir_all(&dir);
    let base = dir.join("base");
    let mut ids: Vec<BlockId> = Vec::new();
    {
        let s = Store::open_unsynced(&base, opts()).unwrap();
        for i in 0..N {
            ids.push(s.put(&rnd(i as u64, LEN)).unwrap());
        }
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    let pack = base.join("packs/pack-00000000.cpk");
    println!("pack bytes={}", fs::metadata(&pack).unwrap().len());
    let work = dir.join("work");

    for (label, index) in [("clean, index loaded", true), ("clean, rebuild", false)] {
        copy(&base, &work, index);
        let t = Instant::now();
        let s = Store::open_unsynced(&work, opts()).unwrap();
        let dt = t.elapsed();
        let ok = ids.iter().filter(|id| s.get(**id).is_ok()).count();
        println!(
            "{label}: open={dt:?} readable={ok} scanned={} has_corruption={}",
            s.recovery().records_scanned,
            s.recovery().has_corruption()
        );
        assert_eq!(ok, N);
    }

    let scenarios: [(&str, Damage); 4] = [
        (
            "A zero 1MiB at offset 1MiB",
            Box::new(|f| {
                f.write_all_at(&vec![0u8; 1 << 20], 1 << 20).unwrap();
                (0..N)
                    .filter(|&i| {
                        let (a, b) = record_range(i);
                        a < (2 << 20) && b > (1 << 20)
                    })
                    .collect()
            }),
        ),
        (
            "B flip 2 bytes in the first 1100 records",
            Box::new(|f| {
                for i in 0..1100 {
                    f.write_all_at(&[0xEE, 0xEE], record_range(i).0 + 56 + 100)
                        .unwrap();
                }
                (0..1100).collect()
            }),
        ),
        (
            "C flip 2 bytes in the first 3000 records",
            Box::new(|f| {
                for i in 0..3000 {
                    f.write_all_at(&[0xEE, 0xEE], record_range(i).0 + 56 + 100)
                        .unwrap();
                }
                (0..3000).collect()
            }),
        ),
        (
            "D flip 2 bytes in every other record",
            Box::new(|f| {
                for i in (0..N).step_by(2) {
                    f.write_all_at(&[0xEE, 0xEE], record_range(i).0 + 56 + 100)
                        .unwrap();
                }
                (0..N).step_by(2).collect()
            }),
        ),
    ];
    for (label, damage) in scenarios {
        copy(&base, &work, false);
        let f = OpenOptions::new()
            .write(true)
            .open(work.join("packs/pack-00000000.cpk"))
            .unwrap();
        let lost = damage(&f);
        drop(f);
        let t = Instant::now();
        let s = Store::open_unsynced(&work, opts()).unwrap();
        let dt = t.elapsed();
        let readable: Vec<bool> = ids.iter().map(|id| s.get(*id).is_ok()).collect();
        let ok = readable.iter().filter(|r| **r).count();
        let wrongly_lost = (0..N)
            .filter(|i| !lost.contains(i) && !readable[*i])
            .count();
        println!(
            "{label}: open={dt:?} readable={ok} expected_lost={} wrongly_lost={wrongly_lost} gaps={}",
            lost.len(),
            s.recovery().gaps.len()
        );
        assert_eq!(wrongly_lost, 0);
        assert_eq!(ok, N - lost.len());
    }
}
