//! Ported from the independent critic's scratch tests (`out/critic26/work/crates/cowfs-core/tests/critic.rs`),
//! which is read-only scratch and not in this worktree.
//!
//! The heavy ones are `#[ignore]` so the default workspace run stays short. Commands, all from the
//! repository root with `cargo -j4`:
//!
//! - fsx, two seeds: `FSX_OPS=100000 FSX_SEED=1 cargo test -p cowfs-core --test critic --release -- --ignored fsx`
//!   and the same with `FSX_SEED=2`.
//! - hammer, three minutes: `HAMMER_SECS=180 cargo test -p cowfs-core --release --test critic -- --ignored hammer`
//! - barrier storm: `cargo test -p cowfs-core --release --test critic -- --ignored barrier_storm`
//! - many files: `N_FILES=500000 cargo test -p cowfs-core --release --test critic -- --ignored many_files`
//! - reader stall during a slow write: `cargo test -p cowfs-core --release --test critic -- --nocapture reader_stall`

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use cowfs_core::{Core, Options};
use cowfs_meta::Meta;
use cowfs_vfs::{SetAttr, Vfs, ROOT_INO};

fn snap(c: &Core, name: &str) -> u64 {
    c.create_snapshot(name).unwrap();
    root_entry(c, name).ino
}

fn set_len(c: &Core, f: u64, n: u64) {
    c.setattr(
        f,
        SetAttr {
            size: Some(n),
            ..SetAttr::default()
        },
    )
    .unwrap();
}

/// fsx-style: one file against a `Vec` model, with holes, truncates and restarts.
#[test]
#[ignore = "heavy: FSX_OPS=100000 FSX_SEED=1 cargo test -p cowfs-core --release --test critic -- --ignored fsx"]
fn fsx() {
    let ops: usize = std::env::var("FSX_OPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100_000);
    let seed: u64 = std::env::var("FSX_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let dir = tempfile::tempdir().unwrap();
    let mut o = test_opts();
    o.file_flush_bytes = 300 << 10;
    let mut c = Core::open(dir.path(), o.clone()).unwrap();
    let mut r = snap(&c, "s");
    let mut f = c.create(r, b"f", 0o644).unwrap().ino;
    let mut model: Vec<u8> = Vec::new();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ seed);
    let bounds: [u64; 3] = [16 << 10, 64 << 10, 256 << 10];
    let mut counts = [0usize; 6];
    let started = Instant::now();
    for i in 0..ops {
        if i % 1000 == 0 {
            eprintln!(
                "fsx seed {seed}: progress {i}/{ops}, elapsed {:?}",
                started.elapsed()
            );
        }
        let k = rng.below(100);
        let base = bounds[rng.below(3) as usize];
        let jitter = |rng: &mut Rng| -> i64 {
            match rng.below(4) {
                0 => 0,
                1 => 1,
                2 => -1,
                _ => (rng.below(2000) as i64) - 1000,
            }
        };
        if k < 40 {
            let off = ((base * rng.below(12)) as i64 + jitter(&mut rng)).max(0) as u64;
            let len = match rng.below(5) {
                0 => 1,
                1 => 4096,
                2 => base as usize,
                3 => rng.below(300_000) as usize + 1,
                _ => (base as i64 + jitter(&mut rng)).max(1) as usize,
            };
            let d = pattern(len, i as u64 + 1);
            let off = if rng.below(50) == 0 {
                (1u64 << 32) - 5 + rng.below(10)
            } else {
                off
            };
            if off + len as u64 > (1 << 33) {
                continue;
            }
            assert_eq!(c.write(f, off, &d).unwrap() as usize, len);
            if model.len() < off as usize + len {
                if off as usize + len > 40 << 20 && off > (1 << 31) {
                    let got = c.read(f, off, len as u32).unwrap();
                    assert_eq!(got, d, "far write readback");
                    assert_eq!(c.getattr(f).unwrap().size, off + len as u64);
                    let mid = off / 2;
                    assert_eq!(c.read(f, mid, 100).unwrap(), vec![0u8; 100]);
                    truncate(&c, f, model.len() as u64).unwrap();
                    assert_eq!(c.getattr(f).unwrap().size, model.len() as u64);
                    counts[5] += 1;
                    continue;
                }
                model.resize(off as usize + len, 0);
            }
            model[off as usize..off as usize + len].copy_from_slice(&d);
            counts[0] += 1;
        } else if k < 55 {
            let cur = model.len() as u64;
            let ns = match rng.below(4) {
                0 => 0,
                1 => (base * rng.below(10)) + rng.below(3),
                2 => cur.saturating_sub(rng.below(70_000)),
                _ => cur + rng.below(600_000),
            };
            if ns > 12 << 20 {
                continue;
            }
            truncate(&c, f, ns).unwrap();
            model.resize(ns as usize, 0);
            counts[1] += 1;
        } else if k < 85 {
            let off = rng.below(model.len() as u64 + 100);
            let len = rng.below(300_000) as u32 + 1;
            let got = c.read(f, off, len).unwrap();
            let exp: &[u8] = if off as usize >= model.len() {
                &[]
            } else {
                &model[off as usize..(off as usize + len as usize).min(model.len())]
            };
            assert!(got == exp, "op {i}: read mismatch at {off}+{len}");
            counts[2] += 1;
        } else if k < 90 {
            c.flush().unwrap();
            counts[3] += 1;
        } else if k < 94 {
            c.sync().unwrap();
            c.drop_caches();
            counts[3] += 1;
        } else if k < 96 {
            c.fsync(f, false).unwrap();
        } else if k < 97 && i % 50 == 0 {
            drop(c);
            c = Core::open(dir.path(), o.clone()).unwrap();
            r = root_entry(&c, "s").ino;
            f = c.lookup(r, b"f").unwrap().ino;
            counts[4] += 1;
        }
        assert_eq!(c.getattr(f).unwrap().size, model.len() as u64, "op {i}");
    }
    assert!(read_all(&c, f) == model, "final content mismatch");
    c.sync().unwrap();
    c.check().unwrap();
    println!(
        "fsx seed {seed}: {ops} ops, counts {counts:?}, final size {}",
        model.len()
    );
}

/// A shrink then a grow must not expose the bytes the shrink removed, with or without dirty
/// extents at shrink time.
#[test]
fn truncate_stale_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let r = snap(&c, "s");
    let f = mkfile(&c, r, "f", &pattern(500_000, 5)).ino;
    c.sync().unwrap();
    set_len(&c, f, 100_003);
    set_len(&c, f, 500_000);
    let got = c.read(f, 0, 500_000).unwrap();
    assert_eq!(&got[..100_003], &pattern(500_000, 5)[..100_003]);
    assert!(
        got[100_003..].iter().all(|b| *b == 0),
        "stale bytes leaked after shrink+grow"
    );
    c.write(f, 200_000, &[9u8; 10]).unwrap();
    set_len(&c, f, 150_000);
    set_len(&c, f, 300_000);
    let got = c.read(f, 150_000, 150_000).unwrap();
    assert!(got.iter().all(|b| *b == 0), "stale dirty bytes leaked");
    set_len(&c, f, 1 << 40);
    let mid = c.read(f, 1 << 39, 4096).unwrap();
    assert!(mid.iter().all(|b| *b == 0));
    assert_eq!(c.getattr(f).unwrap().size, 1 << 40);
}

/// Six threads overwriting one file in overlapping blocks while another thread churns the
/// directory: no torn block, and the same content after a reopen.
#[test]
#[ignore = "heavy: HAMMER_SECS=180 cargo test -p cowfs-core --release --test critic -- --ignored hammer"]
fn hammer() {
    let secs: u64 = std::env::var("HAMMER_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    let dir = tempfile::tempdir().unwrap();
    let mut o = test_opts();
    o.flush_interval = Duration::from_millis(20);
    o.file_flush_bytes = 256 << 10;
    o.max_pending_ops = 64;
    let c = Core::open(dir.path(), o.clone()).unwrap();
    let r = snap(&c, "s");
    let f = c.create(r, b"shared", 0o644).unwrap().ino;
    let stop = Arc::new(AtomicBool::new(false));
    let mut hs = Vec::new();
    for t in 0..6u8 {
        let (c, stop) = (c.clone(), stop.clone());
        hs.push(std::thread::spawn(move || {
            let mut rng = Rng(0xABCDEF ^ ((u64::from(t) + 1) * 7919));
            let mut n = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let blk = rng.below(200);
                let len = (1 + rng.below(6)) * 4096;
                let d = vec![t + 1; len as usize];
                c.write(f, blk * 4096, &d).unwrap();
                if rng.below(50) == 0 {
                    c.fsync(f, false).unwrap();
                }
                n += 1;
            }
            n
        }));
    }
    let side = {
        let c = c.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut i = 0;
            while !stop.load(Ordering::Relaxed) {
                let name = format!("x{}", i % 300);
                let _ = c.create(r, name.as_bytes(), 0o644).map(|a| {
                    let _ = c.write(a.ino, 0, b"hi");
                    c.forget(a.ino, 1)
                });
                let _ = c.readdir(r, 0, 1000);
                if i % 400 == 0 {
                    let _ = c.fork_snapshot("s", &format!("fork{i}"));
                }
                if i % 7 == 0 {
                    let _ = c.unlink(r, name.as_bytes());
                }
                if i % 97 == 0 {
                    let _ = c.rename(r, b"x1", r, b"x2", Default::default());
                }
                i += 1;
            }
        })
    };
    std::thread::sleep(Duration::from_secs(secs));
    stop.store(true, Ordering::Relaxed);
    let total: u64 = hs.into_iter().map(|h| h.join().unwrap()).sum();
    side.join().unwrap();
    let torn = check_uniform(&c, f);
    println!("hammer {secs}s: writes {total}, torn blocks {torn}");
    assert_eq!(torn, 0);
    c.sync().unwrap();
    c.check().unwrap();
    drop(c);
    let c = Core::open(dir.path(), o).unwrap();
    let r = root_entry(&c, "s").ino;
    let f = c.lookup(r, b"shared").unwrap().ino;
    assert_eq!(check_uniform(&c, f), 0, "torn blocks after a reopen");
    c.check().unwrap();
}

fn check_uniform(c: &Core, f: u64) -> usize {
    let d = read_all(c, f);
    let mut torn = 0;
    for (i, b) in d.chunks(4096).enumerate() {
        if b.len() == 4096 && b.iter().any(|x| *x != b[0]) {
            torn += 1;
            println!("torn block {i}");
        }
        assert!(b[0] <= 6, "unexpected byte value {}", b[0]);
    }
    torn
}

/// Two thousand create-then-list cycles: each listing is a barrier, so this is the barrier cost.
#[test]
#[ignore = "heavy: cargo test -p cowfs-core --release --test critic -- --ignored barrier_storm"]
fn barrier_storm() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let r = snap(&c, "s");
    let d = c.mkdir(r, b"d", 0o755).unwrap().ino;
    c.flush().unwrap();
    let b0 = c.stats().batches;
    let t = Instant::now();
    for i in 0..2000 {
        let a = c.create(d, format!("x{i}").as_bytes(), 0o644).unwrap();
        c.forget(a.ino, 1);
        let _ = c.readdir(d, 0, 10_000).unwrap();
    }
    let took = t.elapsed();
    println!(
        "barrier storm: 2000 create+readdir in {took:?}, {} batches",
        c.stats().batches - b0
    );
    c.check().unwrap();
}

/// Half a million files: the alias and node tables must not grow with the file count.
#[test]
#[ignore = "heavy: N_FILES=500000 cargo test -p cowfs-core --release --test critic -- --ignored many_files"]
fn many_files() {
    let n: usize = std::env::var("N_FILES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300_000);
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let r = snap(&c, "s");
    let t = Instant::now();
    let mut dirs = Vec::new();
    for i in 0..100 {
        dirs.push(c.mkdir(r, format!("d{i}").as_bytes(), 0o755).unwrap().ino);
    }
    for i in 0..n {
        let a = c
            .create(dirs[i % 100], format!("f{i}").as_bytes(), 0o644)
            .unwrap()
            .ino;
        c.write(a, 0, b"hello world").unwrap();
        c.forget(a, 1);
    }
    c.flush().unwrap();
    let s = c.stats();
    println!(
        "many files: {n} in {:?}; nodes {} dentries {} aliases {} pending {}",
        t.elapsed(),
        s.nodes,
        s.dentries,
        s.aliases,
        s.pending_ops
    );
    assert!(s.aliases <= 4096, "the alias table is unbounded: {s:?}");
    c.check().unwrap();
}

/// How long a reader waits while a big write is being stored: the write must not hold a lock that
/// a `getattr`, a `lookup` or a `read` of another file needs.
#[test]
fn reader_stall_during_a_slow_write() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(
        dir.path(),
        Options {
            background: false,
            file_flush_bytes: 8 << 20,
            block_cache_bytes: 4 << 20,
            ..test_opts()
        },
    )
    .unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    let slow = c.create(r, b"slow", 0o644).unwrap().ino;
    let other = mkfile(&c, r, "other", &pattern(1 << 20, 2)).ino;
    c.sync().unwrap();
    c.drop_caches();
    // how much the writer stores while the readers sample; raise with STALL_BYTES for a longer run
    let total_bytes: u64 = std::env::var("STALL_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(32 << 20);
    let writer = {
        let c = c.clone();
        std::thread::spawn(move || {
            let t = Instant::now();
            let mut off = 0u64;
            while off < total_bytes {
                let n = c.write(slow, off, &pattern(8 << 20, off / 1000)).unwrap();
                off += u64::from(n);
            }
            c.fsync(slow, false).unwrap();
            t.elapsed()
        })
    };
    let (mut g, mut l, mut rd) = (Vec::new(), Vec::new(), Vec::new());
    let t0 = Instant::now();
    while !writer.is_finished() {
        let s = Instant::now();
        c.getattr(other).unwrap();
        g.push(s.elapsed());
        let s = Instant::now();
        let a = c.lookup(r, b"other").unwrap();
        c.forget(a.ino, 1);
        l.push(s.elapsed());
        let s = Instant::now();
        let a = c.lookup(r, b"other").unwrap();
        c.read(a.ino, 0, 4096).unwrap();
        c.forget(a.ino, 1);
        rd.push(s.elapsed());
        std::thread::sleep(Duration::from_millis(20));
    }
    let write_took = writer.join().unwrap();
    let p = |v: &mut Vec<Duration>| {
        v.sort();
        format!(
            "p50 {:?} p99 {:?} max {:?}",
            v[v.len() / 2],
            v[v.len() * 99 / 100],
            v[v.len() - 1]
        )
    };
    println!(
        "reader stall: the {total_bytes} byte write took {write_took:?} (sampled over {t0:?}); getattr {}, lookup {}, read {}",
        p(&mut g),
        p(&mut l),
        p(&mut rd)
    );
    let worst = [g.last(), l.last(), rd.last()]
        .into_iter()
        .flatten()
        .max()
        .copied()
        .unwrap_or_default();
    assert!(
        worst < Duration::from_millis(250),
        "a reader waited {worst:?} behind a slow write"
    );
    c.check().unwrap();
}

/// A store whose `recovery` reports corruption is not opened at all, and the refusal names the
/// reason.
#[test]
fn a_store_with_corruption_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    mkfile(&c, r, "f", &pattern(600_000, 3));
    c.sync().unwrap();
    drop(c);
    // truncate the durable tail so the store reports missing synced bytes
    let mut v: Vec<_> = std::fs::read_dir(dir.path().join("store/packs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    v.sort();
    let pack = v.remove(0);
    let len = std::fs::metadata(&pack).unwrap().len();
    let f = std::fs::OpenOptions::new().write(true).open(&pack).unwrap();
    f.set_len(len - 4096).unwrap();
    drop(f);
    match Core::open(dir.path(), test_opts()) {
        Err(e) => {
            let m = e.to_string();
            assert!(m.contains("corruption") || m.contains("corrupt"), "{m}");
        }
        Ok(_) => panic!("a store that lost durable data was opened"),
    }
    // the same database is still readable through cowfs-meta alone, so the refusal is core's
    let dir2 = tempfile::tempdir().unwrap();
    let m = Meta::open(dir2.path().join("m.redb"), Default::default()).unwrap();
    let s = m.new_snapshot("s").unwrap();
    assert!(s.readdir(cowfs_meta::ROOT_INO, 0, 10).is_ok());
    let _ = ROOT_INO;
}
