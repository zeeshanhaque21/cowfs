//! Benchmarks for the review findings (F7, F10, F11, F13). One mode per process.
//!
//! `cargo run --release -p cowfs-meta --example review_bench -- <mode> [size]` with mode one of
//! `lookup`, `create`, `group`, `splice`, `rm`, `check`. Timed batches hold the shared CPU lock.

use cowfs_meta::{Ack, BlockId, ChunkRef, Ino, Meta, Options, Snapshot, ROOT_INO};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[path = "benchutil/mod.rs"]
mod util;
use util::{load1, locked, measure, row, Rng};

const PER_DIR: usize = 1000;

fn chunk(n: u64, len: u32) -> ChunkRef {
    ChunkRef::block(BlockId::of(&n.to_le_bytes()), len)
}

fn rss_mb() -> u64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok();
    out.and_then(|o| {
        String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse::<u64>()
            .ok()
    })
    .unwrap_or(0)
        / 1024
}

/// Adds directories of 1000 files (one chunk each) until the snapshot holds `files` files.
fn fill(s: &Snapshot, from_dir: usize, files: usize) -> Vec<Ino> {
    let mut dirs = Vec::new();
    for k in from_dir..from_dir + files.div_ceil(PER_DIR) {
        let d = s
            .batch(|tx| {
                let d = tx.mkdir(ROOT_INO, format!("d{k:05}").as_bytes(), 0o755)?;
                for i in 0..PER_DIR {
                    let f = tx.create(d.ino, format!("f{i:04}").as_bytes(), 0o644)?;
                    tx.set_content(f.ino, &[chunk((k * PER_DIR + i) as u64, 4096)], 4096)?;
                }
                Ok(d.ino)
            })
            .unwrap();
        dirs.push(d);
    }
    dirs
}

fn open(dir: &Path, opts: Options) -> Meta {
    Meta::open(dir.join("m.redb"), opts).unwrap()
}

fn lookup(dir: &Path, files: usize) {
    let m = open(dir, Options::default());
    let s = m.new_snapshot("main").unwrap();
    let dirs = fill(&s, 0, files);
    m.sync().unwrap();
    let inodes = dirs.len() * (PER_DIR + 1);
    drop((s, m));
    let mut rng = Rng(0xABCD_EF12_3456_789B);
    let pick = |rng: &mut Rng, dirs: &[Ino]| {
        (
            dirs[rng.below(dirs.len())],
            format!("f{:04}", rng.below(PER_DIR)),
        )
    };

    let m = open(dir, Options::default());
    let s = m.snapshot("main").unwrap();
    let hot: Vec<(Ino, String)> = (0..1000).map(|_| pick(&mut rng, &dirs)).collect();
    for (d, n) in &hot {
        s.lookup(*d, n.as_bytes()).unwrap();
    }
    measure("lookup, 1000 fixed names (hot)", inodes, 7, 20_000, |_| {
        let t = Instant::now();
        for i in 0..20_000 {
            let (d, n) = &hot[i % hot.len()];
            s.lookup(*d, n.as_bytes()).unwrap();
        }
        t.elapsed()
    });
    measure(
        "lookup, random file (node cache warmed by earlier runs)",
        inodes,
        7,
        20_000,
        |_| {
            let t = Instant::now();
            for _ in 0..20_000 {
                let (d, n) = pick(&mut rng, &dirs);
                s.lookup(d, n.as_bytes()).unwrap();
            }
            t.elapsed()
        },
    );
    measure("lookup, absent name (miss)", inodes, 7, 20_000, |_| {
        let t = Instant::now();
        for _ in 0..20_000 {
            let (d, n) = pick(&mut rng, &dirs);
            assert!(s.lookup(d, format!("{n}x").as_bytes()).is_err());
        }
        t.elapsed()
    });
    drop((s, m));

    measure(
        "lookup, random file, cold node cache (fresh open, redb page cache warm)",
        inodes,
        5,
        2_000,
        |_| {
            let m = open(dir, Options::default());
            let s = m.snapshot("main").unwrap();
            let t = Instant::now();
            for _ in 0..2_000 {
                let (d, n) = pick(&mut rng, &dirs);
                s.lookup(d, n.as_bytes()).unwrap();
            }
            t.elapsed()
        },
    );
    measure(
        "lookup, random file, node cache off (every node read and hashed)",
        inodes,
        5,
        2_000,
        |_| {
            let m = open(
                dir,
                Options {
                    node_cache: 0,
                    ..Options::default()
                },
            );
            let s = m.snapshot("main").unwrap();
            let t = Instant::now();
            for _ in 0..2_000 {
                let (d, n) = pick(&mut rng, &dirs);
                s.lookup(d, n.as_bytes()).unwrap();
            }
            t.elapsed()
        },
    );
    println!("rss {} MB", rss_mb());
}

fn create(dir: &Path) {
    let m = open(dir, Options::default());
    let s = m.new_snapshot("main").unwrap();
    measure(
        "create, one call per file, default policy (10,000 files)",
        0,
        5,
        10_000,
        |rep| {
            let d = s
                .mkdir(ROOT_INO, format!("c{rep}").as_bytes(), 0o755)
                .unwrap()
                .ino;
            let t = Instant::now();
            for i in 0..10_000 {
                s.create(d, format!("f{i}").as_bytes(), 0o644).unwrap();
            }
            t.elapsed()
        },
    );
    measure("create, 1000 per batch", 0, 5, 10_000, |rep| {
        let d = s
            .mkdir(ROOT_INO, format!("b{rep}").as_bytes(), 0o755)
            .unwrap()
            .ino;
        let t = Instant::now();
        for b in 0..10 {
            s.batch(|tx| {
                for i in 0..1000 {
                    tx.create(d, format!("f{b}-{i}").as_bytes(), 0o644)?;
                }
                Ok(())
            })
            .unwrap();
        }
        t.elapsed()
    });
    m.close().unwrap();
}

fn group(dir: &Path) {
    let hooks = Arc::new(AtomicUsize::new(0));
    let h2 = hooks.clone();
    let m = open(
        dir,
        Options {
            ack: Ack::Durable,
            before_sync: Some(Arc::new(move || {
                h2.fetch_add(1, SeqCst);
                Ok(())
            })),
            ..Options::default()
        },
    );
    let s = m.new_snapshot("main").unwrap();
    let d = s.mkdir(ROOT_INO, b"w", 0o755).unwrap().ino;
    measure(
        "durable create, 1 thread (each op waits for its own commit)",
        0,
        5,
        60,
        |rep| {
            let t = Instant::now();
            for i in 0..60 {
                s.create(d, format!("one{rep}-{i}").as_bytes(), 0o644)
                    .unwrap();
            }
            t.elapsed()
        },
    );
    for threads in [2usize, 8, 32] {
        let per = 800 / threads.min(8) / (threads / threads.min(8)).max(1);
        let per = per.max(20);
        let label =
            format!("durable create, {threads} threads x {per}, group commit (per op, aggregate)");
        let mut commits = Vec::new();
        measure(&label, 0, 5, threads * per, |rep| {
            let before = hooks.load(SeqCst);
            let t = Instant::now();
            std::thread::scope(|sc| {
                for th in 0..threads {
                    let s = s.clone();
                    sc.spawn(move || {
                        for i in 0..per {
                            s.create(d, format!("g{threads}-{rep}-{th}-{i}").as_bytes(), 0o644)
                                .unwrap();
                        }
                    });
                }
            });
            let e = t.elapsed();
            commits.push(hooks.load(SeqCst) - before);
            e
        });
        println!("  hook runs (= fsyncs) per rep for {threads} threads: {commits:?}");
    }
    m.close().unwrap();
}

fn splice(dir: &Path) {
    let m = open(dir, Options::default());
    let s = m.new_snapshot("main").unwrap();
    let mut rng = Rng(77);
    for n in [1_000u32, 10_000, 100_000, 1_000_000] {
        let f = s
            .create(ROOT_INO, format!("f{n}").as_bytes(), 0o644)
            .unwrap()
            .ino;
        let len = 65536u32;
        let mut done = 0u32;
        while done < n {
            let upto = (done + 100_000).min(n);
            let list: Vec<ChunkRef> = (done..upto).map(|i| chunk(u64::from(i), len)).collect();
            s.batch(|tx| {
                let a = tx.getattr(f)?;
                let v = tx.content_version(f)?;
                let covered = u64::from(done) * u64::from(len);
                debug_assert_eq!(a.size, covered);
                tx.splice_content(
                    f,
                    v,
                    covered,
                    covered,
                    &list,
                    u64::from(upto) * u64::from(len),
                )?;
                Ok(())
            })
            .unwrap();
            done = upto;
        }
        m.sync().unwrap();
        let mut covered = u64::from(n) * u64::from(len);
        measure(
            "append 1 chunk via splice_content (default policy)",
            n as usize,
            5,
            20,
            |rep| {
                let t = Instant::now();
                for j in 0..20u64 {
                    let v = s.content_version(f).unwrap();
                    s.splice_content(
                        f,
                        v,
                        covered,
                        covered,
                        &[chunk(9_000_000 + rep as u64 * 100 + j, len)],
                        covered + u64::from(len),
                    )
                    .unwrap();
                    covered += u64::from(len);
                }
                t.elapsed()
            },
        );
        measure(
            "replace 1 mid-file chunk via splice_content",
            n as usize,
            5,
            20,
            |_| {
                let t = Instant::now();
                for _ in 0..20 {
                    let v = s.content_version(f).unwrap();
                    let at = rng.below((n / 2) as usize) as u64 * u64::from(len)
                        + u64::from(len) * u64::from(n / 4);
                    s.splice_content(f, v, at, at + u64::from(len), &[chunk(at, len)], covered)
                        .unwrap();
                }
                t.elapsed()
            },
        );
        measure(
            "append via chunks() + set_content (whole-list rewrite, for comparison)",
            n as usize,
            5,
            2,
            |rep| {
                let t = Instant::now();
                for j in 0..2u64 {
                    let mut l = s.chunks(f).unwrap();
                    l.push(chunk(8_000_000 + rep as u64 * 10 + j, len));
                    let size = l.iter().map(|c| u64::from(c.len)).sum::<u64>();
                    s.set_content(f, &l, size).unwrap();
                    covered = size;
                }
                t.elapsed()
            },
        );
    }
    m.close().unwrap();
}

fn rm(dir: &Path, files: usize) {
    let m = open(
        dir,
        Options {
            sync_every_ops: u32::MAX,
            sync_interval: Duration::from_secs(3600),
            ..Options::default()
        },
    );
    let other = m.new_snapshot("other").unwrap();
    let (mut removes, mut worst, mut worst_reap, mut drains, mut load_max) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), 0.0f64);
    for rep in 0..5 {
        let big = m.new_snapshot(&format!("big{rep}")).unwrap();
        fill(&big, 0, files);
        m.sync().unwrap();
        let ((rm_us, worst_us, reap_us, drain_us), load) = locked(|| {
            let stop = Arc::new(AtomicBool::new(false));
            let reaping = Arc::new(AtomicBool::new(false));
            let (st, rp, o) = (stop.clone(), reaping.clone(), other.clone());
            let h = std::thread::spawn(move || {
                let (mut n, mut max, mut max_reap) = (0u64, Duration::ZERO, Duration::ZERO);
                while !st.load(SeqCst) {
                    let t = Instant::now();
                    let in_reap = rp.load(SeqCst);
                    o.create(ROOT_INO, format!("x{rep}-{n}").as_bytes(), 0o644)
                        .unwrap();
                    max = max.max(t.elapsed());
                    if in_reap {
                        max_reap = max_reap.max(t.elapsed());
                    }
                    n += 1;
                    std::thread::sleep(Duration::from_micros(500));
                }
                (max, max_reap)
            });
            std::thread::sleep(Duration::from_millis(100));
            let t = Instant::now();
            m.remove_snapshot(big.id()).unwrap();
            let rm = t.elapsed();
            reaping.store(true, SeqCst);
            while m.pending_reap().unwrap() > 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
            let drain = t.elapsed();
            stop.store(true, SeqCst);
            let (worst, worst_reap) = h.join().unwrap();
            let us = |d: Duration| d.as_secs_f64() * 1e6;
            (us(rm), us(worst), us(worst_reap), us(drain))
        });
        load_max = load_max.max(load);
        removes.push(rm_us);
        worst.push(worst_us);
        worst_reap.push(reap_us);
        drains.push(drain_us);
    }
    let inodes = files + files / PER_DIR;
    for (label, v) in [
        ("remove_snapshot call returns after (durable)", removes),
        ("worst latency of a concurrent create, whole window", worst),
        (
            "worst latency of a concurrent create started after remove returned (reaping only)",
            worst_reap,
        ),
        (
            "time until every node of the removed snapshot is freed",
            drains,
        ),
    ] {
        row(label, inodes, "5x1", v, load_max);
    }
    let t = Instant::now();
    m.check().unwrap();
    println!("check() after: {:?}, rss {} MB", t.elapsed(), rss_mb());
}

fn check(dir: &Path, files: usize) {
    let m = open(dir, Options::default());
    let s = m.new_snapshot("main").unwrap();
    fill(&s, 0, files);
    m.sync().unwrap();
    let base = rss_mb();
    let mut samples = Vec::new();
    let mut load_max = 0.0f64;
    for _ in 0..5 {
        let (d, load) = locked(|| {
            let t = Instant::now();
            m.check().unwrap();
            t.elapsed()
        });
        load_max = load_max.max(load);
        samples.push(d.as_secs_f64() * 1e6);
    }
    row("check()", files + files / PER_DIR, "5x1", samples, load_max);
    println!("rss before check {base} MB, after {} MB", rss_mb());
}

fn main() {
    let mode = std::env::args().nth(1).expect("mode");
    let size: usize = std::env::args()
        .nth(2)
        .map_or(1_000_000, |v| v.parse().unwrap());
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench-tmp");
    std::fs::create_dir_all(&base).unwrap();
    let tmp = tempfile::tempdir_in(base).unwrap();
    println!("mode {mode}, uptime before: load1 {:.1}", load1());
    println!(
        "| metric | inodes | reps x ops | median us/op | min | max | median ops/s | load1 max |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    match mode.as_str() {
        "lookup" => lookup(tmp.path(), size),
        "create" => create(tmp.path()),
        "group" => group(tmp.path()),
        "splice" => splice(tmp.path()),
        "rm" => rm(tmp.path(), size),
        "check" => check(tmp.path(), size),
        other => panic!("unknown mode {other}"),
    }
    println!("uptime after: load1 {:.1}", load1());
}
