//! Full power-loss model (unsynced ops persist in any subset) over whole histories AND over
//! open-time recovery, with crash-reopen loops. Ported from the round-3 critic's harness.
//! `C7C_SEEDS=1000 C7C_START=...` runs the 4000-case sweep.
mod common;

use std::fs;
use std::sync::Mutex;

use common::{compressible, random};
use cowfs_store::crashmodel::{crash_image, read_image, write_image, Image, Rng};
use cowfs_store::{oplog_marker, oplog_start, oplog_take, BlockId, LogOp, Options, Store};

static SERIAL: Mutex<()> = Mutex::new(());

fn opts(rng: &mut Rng) -> Options {
    Options {
        max_pack_size: 20_000 + rng.below(120_000),
        checkpoint_on_drop: false,
        ..Options::default()
    }
}

struct Hist {
    base: Image,
    ops: Vec<LogOp>,
    puts: Vec<(BlockId, Vec<u8>)>,
}

fn history(seed: u64, o: Options) -> Hist {
    let mut rng = Rng(seed ^ 0xA5A5);
    let dir = tempfile::tempdir().unwrap();
    let mut puts: Vec<(BlockId, Vec<u8>)> = Vec::new();
    oplog_start();
    {
        let s = Store::open_unsynced(dir.path(), o).unwrap();
        let steps = 8 + rng.below(40);
        for _ in 0..steps {
            match rng.below(7) {
                0 | 1 => {
                    s.sync().unwrap();
                    oplog_marker(puts.len() as u64);
                }
                2 if rng.below(3) == 0 => {
                    s.checkpoint().unwrap();
                    oplog_marker(puts.len() as u64);
                }
                3 if !puts.is_empty() => {
                    // duplicate put
                    let i = rng.below(puts.len() as u64) as usize;
                    let d = puts[i].1.clone();
                    s.put(&d).unwrap();
                }
                _ => {
                    let len = 500 + rng.below(9000) as usize;
                    let d = if rng.below(3) == 0 {
                        compressible(rng.next_u64(), len)
                    } else {
                        random(rng.next_u64(), len)
                    };
                    let id = s.put(&d).unwrap();
                    puts.push((id, d));
                }
            }
        }
    }
    Hist {
        base: Image::new(),
        ops: oplog_take(),
        puts,
    }
}

fn acked_at(ops: &[LogOp], k: usize) -> usize {
    let mut a = 0;
    for op in &ops[..k.min(ops.len())] {
        if let LogOp::Marker(n) = op {
            a = *n as usize;
        }
    }
    a
}

fn check(s: &Store, puts: &[(BlockId, Vec<u8>)], acked: usize, tag: &str) -> Result<(), String> {
    for (i, (id, d)) in puts.iter().enumerate() {
        match s.get(*id) {
            Ok(got) if &got == d => {}
            Ok(_) => return Err(format!("{tag}: WRONG BYTES for put #{i}")),
            Err(e) if i < acked => {
                return Err(format!("{tag}: ACKED LOST put #{i}/{acked}: {e:?}"))
            }
            Err(_) => {}
        }
    }
    Ok(())
}

#[derive(Default, Debug)]
struct Tally {
    cases: u32,
    lost: u32,
    wrong: u32,
    false_corrupt: u32,
    fsck_dirty: u32,
    other: u32,
    recovery_ops: u64,
    sidecars_max: usize,
    examples: Vec<String>,
}

fn case(seed: u64, depth: u32, t: &mut Tally) {
    let mut rng = Rng(seed);
    let o = opts(&mut rng);
    let h = history(seed, o);
    let mode = rng.below(4);
    let k = if rng.below(2) == 0 {
        rng.below(h.ops.len() as u64 + 1) as usize
    } else {
        // just after / around a sync
        let syncs: Vec<usize> = h
            .ops
            .iter()
            .enumerate()
            .filter(|(_, o)| matches!(o, LogOp::Sync { .. }))
            .map(|(i, _)| i)
            .collect();
        if syncs.is_empty() {
            0
        } else {
            (syncs[rng.below(syncs.len() as u64) as usize] + rng.below(4) as usize).min(h.ops.len())
        }
    };
    let acked = acked_at(&h.ops, k);
    let mut img = crash_image(&h.base, &h.ops, k, &mut rng, mode);
    let tag = format!(
        "seed={seed} depth={depth} mode={mode} k={k}/{}",
        h.ops.len()
    );
    // crash-reopen loop: each round, open (recovery logged), crash mid-recovery
    for round in 0..depth {
        let dir = tempfile::tempdir().unwrap();
        write_image(&img, dir.path());
        oplog_start();
        let r = Store::open_unsynced(dir.path(), o);
        let ops = oplog_take();
        match r {
            Ok(s) => {
                if let Err(e) = check(&s, &h.puts, acked, &format!("{tag} r{round}")) {
                    record(t, e);
                }
            }
            Err(e) => {
                record(t, format!("{tag} r{round}: OPEN FAILED {e:?}"));
                return;
            }
        }
        t.recovery_ops += ops.len() as u64;
        let base = read_image(dir.path());
        let _ = base;
        let kk = rng.below(ops.len() as u64 + 1) as usize;
        let m2 = rng.below(4);
        img = crash_image(&img, &ops, kk, &mut rng, m2);
    }
    // final full recovery, then one more cycle
    let dir = tempfile::tempdir().unwrap();
    write_image(&img, dir.path());
    let mut extra: Vec<(BlockId, Vec<u8>)> = Vec::new();
    {
        let s = match Store::open_unsynced(dir.path(), o) {
            Ok(s) => s,
            Err(e) => {
                record(t, format!("{tag} final: OPEN FAILED {e:?}"));
                return;
            }
        };
        if let Err(e) = check(&s, &h.puts, acked, &format!("{tag} final")) {
            record(t, e);
        }
        let rep = s.recovery();
        if !rep.corrupt_synced.is_empty() || !rep.missing_synced.is_empty() {
            t.false_corrupt += 1;
            if t.examples.len() < 6 {
                t.examples
                    .push(format!("{tag} final FALSE CORRUPT: {rep:?}"));
            }
        }
        for i in 0..4 {
            let d = random(rng.next_u64() ^ i, 600 + rng.below(3000) as usize);
            extra.push((s.put(&d).unwrap(), d));
        }
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    let s = Store::open_unsynced(dir.path(), o).unwrap();
    if let Err(e) = check(&s, &h.puts, acked, &format!("{tag} open2")) {
        record(t, e);
    }
    for (id, d) in &extra {
        if s.get(*id).ok().as_ref() != Some(d) {
            record(t, format!("{tag} open2: second-cycle block lost"));
        }
    }
    let rep = s.recovery();
    if rep.has_corruption() {
        t.false_corrupt += 1;
        if t.examples.len() < 6 {
            t.examples
                .push(format!("{tag} open2 FALSE CORRUPT: {rep:?}"));
        }
    }
    match s.fsck() {
        Ok(f) if !f.is_clean() => {
            t.fsck_dirty += 1;
            if t.examples.len() < 6 {
                t.examples.push(format!("{tag} fsck dirty {:?}", f.damage));
            }
        }
        Err(e) => record(t, format!("{tag} fsck err {e:?}")),
        _ => {}
    }
    let sc = fs::read_dir(dir.path().join("packs"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".torn-")
        })
        .count();
    t.sidecars_max = t.sidecars_max.max(sc);
    t.cases += 1;
}

fn record(t: &mut Tally, e: String) {
    if e.contains("ACKED LOST") {
        t.lost += 1;
    } else if e.contains("WRONG BYTES") {
        t.wrong += 1;
    } else {
        t.other += 1;
    }
    if t.examples.len() < 6 {
        t.examples.push(e);
    }
}

#[test]
fn crash_loops() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let n: u64 = std::env::var("C7C_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let start: u64 = std::env::var("C7C_START")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut t = Tally::default();
    for seed in start..start + n {
        for depth in 0..4u32 {
            case(seed * 10 + u64::from(depth), depth, &mut t);
        }
    }
    println!("TALLY {t:#?}");
    assert!(
        t.lost + t.wrong + t.other + t.false_corrupt + t.fsck_dirty == 0,
        "{t:#?}"
    );
}
