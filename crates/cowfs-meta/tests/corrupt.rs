//! Truncated and bit-flipped database files must yield an error or a valid tree, never a panic
//! or a corrupt tree that passes `check()`.
//!
//! `COWFS_CORRUPT_SCALE=5` multiplies the number of damaged images.

mod common;

use common::{digest, step, Rng, WState};
use cowfs_meta::{Meta, Options};
use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

fn opts() -> Options {
    Options {
        node_size: 512,
        sync_every_ops: 1,
        ..Options::default()
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Outcome {
    OpenError,
    CheckError,
    Valid,
    UnknownState,
}

fn build(path: &Path) -> HashSet<[u8; 32]> {
    let m = Meta::open(path, opts()).unwrap();
    m.new_snapshot("s0").unwrap();
    let mut states = HashSet::from([digest(&m)]);
    let (mut rng, mut st) = (Rng(99), WState::new());
    for _ in 0..80 {
        step(&m, &mut rng, &mut st);
        m.sync().unwrap();
        states.insert(digest(&m));
    }
    m.check().unwrap();
    states
}

fn outcome(path: &Path, states: &HashSet<[u8; 32]>) -> Outcome {
    let Ok(m) = Meta::open(path, opts()) else {
        return Outcome::OpenError;
    };
    let Ok(snaps) = m.snapshots() else {
        return Outcome::CheckError;
    };
    if m.check().is_err() {
        return Outcome::CheckError;
    }
    if snaps.is_empty() || states.contains(&digest(&m)) {
        Outcome::Valid
    } else {
        Outcome::UnknownState
    }
}

#[test]
fn damaged_files_never_panic_or_pass_check_with_wrong_content() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src.redb");
    let states = build(&src);
    let orig = std::fs::read(&src).unwrap();
    let scale: usize = std::env::var("COWFS_CORRUPT_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let mut rng = Rng(0xBAD5EED);

    let mut cases: Vec<(String, Vec<u8>)> = Vec::new();
    let len = orig.len();
    let mut cuts = vec![
        0,
        1,
        63,
        64,
        511,
        512,
        4095,
        4096,
        len / 2,
        len - 1,
        len - 4096,
    ];
    cuts.extend((0..30 * scale).map(|_| rng.below(len)));
    cuts.extend((0..10 * scale).map(|_| rng.below(len / 4096) * 4096));
    for c in cuts {
        cases.push((format!("truncate to {c}"), orig[..c].to_vec()));
    }
    let nonzero: Vec<usize> = (0..len).filter(|&i| orig[i] != 0).collect();
    let flip = |at: usize, rng: &mut Rng| {
        let mut b = orig.clone();
        let bit = rng.below(8);
        b[at] ^= 1 << bit;
        (format!("flip bit {bit} of byte {at}"), b)
    };
    for _ in 0..50 * scale {
        let at = rng.below(1024);
        cases.push(flip(at, &mut rng));
    }
    for _ in 0..150 * scale {
        let at = nonzero[rng.below(nonzero.len())];
        cases.push(flip(at, &mut rng));
    }

    let path = dir.path().join("damaged.redb");
    let mut counts = [0usize; 4];
    let mut bad = Vec::new();
    for (what, bytes) in &cases {
        std::fs::write(&path, bytes).unwrap();
        match catch_unwind(AssertUnwindSafe(|| outcome(&path, &states))) {
            Ok(o) => {
                counts[o as usize] += 1;
                if o == Outcome::UnknownState {
                    bad.push(format!("{what}: passed check() with unknown content"));
                }
            }
            Err(_) => bad.push(format!("{what}: panicked")),
        }
    }
    eprintln!(
        "{} damaged images: open error {}, check error {}, valid {}, unknown state {}, panics/bad {}",
        cases.len(),
        counts[0],
        counts[1],
        counts[2],
        counts[3],
        bad.len()
    );
    assert!(bad.is_empty(), "{}", bad[..bad.len().min(8)].join("\n"));
}
