//! Benchmarks for cowfs-meta.
//!
//! Run: `cargo run --release -p cowfs-meta --example bench`
//!
//! `COWFS_BENCH_SIZES=1000,100000,1000000` sets the tree sizes (inode counts, rounded to whole
//! directories of 1000 files). `COWFS_CPU_LOCK` overrides the shared CPU lock directory and
//! `COWFS_NO_LOCK=1` skips it. Every timed batch runs while holding the lock.

use cowfs_meta::{BlockId, ChunkRef, Ino, Marker, Meta, Options, Snapshot, ROOT_INO};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const DEFAULT_LOCK: &str = "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/cpu.lock";
const PER_DIR: usize = 1000;

struct LockGuard(Option<std::path::PathBuf>);

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_dir_all(p);
        }
    }
}

fn take_lock() -> LockGuard {
    if std::env::var("COWFS_NO_LOCK").is_ok() {
        return LockGuard(None);
    }
    let lock = std::path::PathBuf::from(
        std::env::var("COWFS_CPU_LOCK").unwrap_or_else(|_| DEFAULT_LOCK.into()),
    );
    if let Some(parent) = lock.parent() {
        std::fs::create_dir_all(parent).expect("create lock parent");
    }
    let start = Instant::now();
    loop {
        match std::fs::create_dir(&lock) {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                assert!(
                    start.elapsed() < Duration::from_secs(15 * 60),
                    "cpu.lock busy for 15 minutes"
                );
                std::thread::sleep(Duration::from_secs(10));
            }
            Err(e) => panic!("cpu.lock: {e}"),
        }
    }
    let owner = format!(
        "cowfs-meta bench, pid {}, unix time {}\n",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    );
    let _ = std::fs::write(lock.join("owner"), owner);
    LockGuard(Some(lock))
}

fn load1() -> f64 {
    let out = Command::new("uptime").output().ok();
    let text = out
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    text.split("averages:")
        .nth(1)
        .or_else(|| text.split("average:").nth(1))
        .and_then(|s| s.trim().split([' ', ',']).next().map(str::to_string))
        .and_then(|s| s.parse().ok())
        .unwrap_or(f64::NAN)
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % n as u64) as usize
    }
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

/// Runs `reps` batches; each returns the time it measured for `ops` operations.
fn measure(
    metric: &str,
    inodes: usize,
    reps: usize,
    ops: usize,
    mut f: impl FnMut(usize) -> Duration,
) {
    let (mut per_op, mut loads) = (Vec::new(), Vec::new());
    for rep in 0..reps {
        let before = load1();
        let d = {
            let _lock = take_lock();
            f(rep)
        };
        let after = load1();
        loads.push(before.max(after));
        per_op.push(d.as_secs_f64() * 1e6 / ops as f64);
    }
    let mut sorted = per_op.clone();
    let med = median(&mut sorted);
    let load_max = loads.iter().copied().fold(0.0, f64::max);
    let flag = if load_max > 30.0 { " HIGH-LOAD" } else { "" };
    println!(
        "| {metric} | {inodes} | {reps}x{ops} | {med:.1} | {:.1} | {:.1} | {:.0} | {load_max:.0}{flag} |",
        sorted[0],
        sorted[sorted.len() - 1],
        1e6 / med,
    );
}

fn chunk(seed: u64) -> ChunkRef {
    ChunkRef {
        id: BlockId::of(&seed.to_le_bytes()),
        len: 4096,
    }
}

struct Tree {
    m: Meta,
    s: Snapshot,
    dirs: Vec<Ino>,
    files: usize,
}

impl Tree {
    fn grow_to(&mut self, files: usize) {
        while self.files < files {
            let k = self.dirs.len();
            let s = self.s.clone();
            let dir = s
                .batch(|tx| {
                    let d = tx.mkdir(ROOT_INO, format!("d{k:05}").as_bytes(), 0o755)?;
                    for i in 0..PER_DIR {
                        let f = tx.create(d.ino, format!("f{i:04}").as_bytes(), 0o644)?;
                        tx.set_content(f.ino, &[chunk((k * PER_DIR + i) as u64)], 4096)?;
                    }
                    Ok(d.ino)
                })
                .unwrap();
            self.dirs.push(dir);
            self.files += PER_DIR;
        }
        self.m.sync().unwrap();
    }

    fn inodes(&self) -> usize {
        self.files + self.dirs.len()
    }

    fn fill_dir(&self, name: &str, n: usize) -> Ino {
        let s = self.s.clone();
        s.batch(|tx| {
            let d = tx.mkdir(ROOT_INO, name.as_bytes(), 0o755)?;
            for i in 0..n {
                tx.create(d.ino, format!("f{i:06}").as_bytes(), 0o644)?;
            }
            Ok(d.ino)
        })
        .unwrap()
    }
}

fn raw_redb_baseline(dir: &Path) {
    use redb::{Database, Durability, TableDefinition};
    const T: TableDefinition<&[u8], &[u8]> = TableDefinition::new("t");
    let db = Database::create(dir.join("raw.redb")).unwrap();
    let inodes = 0;
    let n = 200_000usize;
    measure(
        "baseline: raw redb insert, 1000 per txn, no fsync",
        inodes,
        5,
        n / 5,
        |rep| {
            let start = Instant::now();
            for b in 0..(n / 5 / 1000) {
                let mut w = db.begin_write().unwrap();
                w.set_durability(Durability::None).unwrap();
                {
                    let mut t = w.open_table(T).unwrap();
                    for i in 0..1000 {
                        let k = ((rep * 1_000_000 + b * 1000 + i) as u64).to_be_bytes();
                        t.insert(k.as_slice(), [7u8; 36].as_slice()).unwrap();
                    }
                }
                w.commit().unwrap();
            }
            start.elapsed()
        },
    );
    measure(
        "baseline: raw redb 1-row txn, no fsync",
        inodes,
        5,
        500,
        |rep| {
            let start = Instant::now();
            for i in 0..500u64 {
                let mut w = db.begin_write().unwrap();
                w.set_durability(Durability::None).unwrap();
                w.open_table(T)
                    .unwrap()
                    .insert(
                        (u64::MAX - rep as u64 * 1000 - i).to_be_bytes().as_slice(),
                        [1u8].as_slice(),
                    )
                    .unwrap();
                w.commit().unwrap();
            }
            start.elapsed()
        },
    );
    measure(
        "baseline: raw redb 1-row txn, fsync (durable)",
        inodes,
        7,
        20,
        |rep| {
            let start = Instant::now();
            for i in 0..20u64 {
                let w = db.begin_write().unwrap();
                w.open_table(T)
                    .unwrap()
                    .insert(
                        (rep as u64 * 100 + i).to_be_bytes().as_slice(),
                        [1u8].as_slice(),
                    )
                    .unwrap();
                w.commit().unwrap();
            }
            start.elapsed()
        },
    );
}

fn main() {
    let sizes: Vec<usize> = std::env::var("COWFS_BENCH_SIZES")
        .unwrap_or_else(|_| "1000,100000,1000000".into())
        .split(',')
        .map(|s| s.parse().unwrap())
        .collect();
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bench-tmp");
    std::fs::create_dir_all(&base).unwrap();
    let tmp = tempfile::tempdir_in(base).unwrap();
    let path = tmp.path().join("meta.redb");

    println!("uptime before: load1 {:.1}", load1());
    println!(
        "| metric | inodes | reps x ops | median us/op | min | max | median ops/s | load1 max |"
    );
    println!("|---|---|---|---|---|---|---|---|");
    raw_redb_baseline(tmp.path());

    let m = Meta::open(&path, Options::default()).unwrap();
    let s = m.new_snapshot("main").unwrap();
    let mut t = Tree {
        m: m.clone(),
        s,
        dirs: Vec::new(),
        files: 0,
    };
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    let mut fork_id = 0u64;

    for &size in &sizes {
        t.grow_to(size.div_ceil(PER_DIR) * PER_DIR);
        let inodes = t.inodes();
        let rand_file = |rng: &mut Rng, t: &Tree| {
            let k = rng.below(t.dirs.len());
            let i = rng.below(PER_DIR);
            (t.dirs[k], format!("f{i:04}"))
        };

        measure("snapshot create (durable)", inodes, 9, 1, |_| {
            fork_id += 1;
            let start = Instant::now();
            let c = t.s.fork(&format!("bench-fork-{fork_id}")).unwrap();
            let d = start.elapsed();
            m.remove_snapshot(c.id()).unwrap();
            d
        });
        measure("snapshot remove, unchanged (durable)", inodes, 9, 1, |_| {
            fork_id += 1;
            let c = t.s.fork(&format!("bench-fork-{fork_id}")).unwrap();
            let start = Instant::now();
            m.remove_snapshot(c.id()).unwrap();
            start.elapsed()
        });
        measure("lookup(dir, name)", inodes, 7, 20_000, |_| {
            let start = Instant::now();
            for _ in 0..20_000 {
                let (d, n) = rand_file(&mut rng, &t);
                t.s.lookup(d, n.as_bytes()).unwrap();
            }
            start.elapsed()
        });
        measure(
            "write one file: set_content (default policy)",
            inodes,
            7,
            200,
            |rep| {
                let picks: Vec<_> = (0..200)
                    .map(|_| {
                        let (d, n) = rand_file(&mut rng, &t);
                        t.s.lookup(d, n.as_bytes()).unwrap().ino
                    })
                    .collect();
                let start = Instant::now();
                for (j, ino) in picks.into_iter().enumerate() {
                    t.s.set_content(ino, &[chunk(9_000_000 + (rep * 200 + j) as u64)], 4096)
                        .unwrap();
                }
                start.elapsed()
            },
        );
        measure(
            "write one file: set_content + sync (fsync)",
            inodes,
            7,
            20,
            |rep| {
                let picks: Vec<_> = (0..20)
                    .map(|_| {
                        let (d, n) = rand_file(&mut rng, &t);
                        t.s.lookup(d, n.as_bytes()).unwrap().ino
                    })
                    .collect();
                let start = Instant::now();
                for (j, ino) in picks.into_iter().enumerate() {
                    t.s.set_content(ino, &[chunk(8_000_000 + (rep * 20 + j) as u64)], 4096)
                        .unwrap();
                    m.sync().unwrap();
                }
                start.elapsed()
            },
        );
    }

    let inodes = t.inodes();
    let big = t.fill_dir("big", 100_000);
    m.sync().unwrap();
    for (page, label) in [
        (1000, "readdir 100000 entries, 1000/page"),
        (128, "readdir 100000 entries, 128/page"),
    ] {
        measure(label, inodes, 5, 100_000, |_| {
            let start = Instant::now();
            let (mut cookie, mut seen) = (0, 0);
            loop {
                let r = t.s.readdir(big, cookie, page).unwrap();
                seen += r.entries.len();
                cookie = r.next_cookie;
                if r.end {
                    break;
                }
            }
            assert_eq!(seen, 100_000);
            start.elapsed()
        });
    }

    let inodes = t.inodes();
    measure(
        "create, one txn per file (default policy)",
        inodes,
        5,
        2000,
        |rep| {
            let d =
                t.s.mkdir(ROOT_INO, format!("cr{rep}").as_bytes(), 0o755)
                    .unwrap()
                    .ino;
            let start = Instant::now();
            for i in 0..2000 {
                t.s.create(d, format!("f{i}").as_bytes(), 0o644).unwrap();
            }
            start.elapsed()
        },
    );
    measure(
        "create, 1000 per txn (default policy)",
        inodes,
        5,
        10_000,
        |rep| {
            let d =
                t.s.mkdir(ROOT_INO, format!("cb{rep}").as_bytes(), 0o755)
                    .unwrap()
                    .ino;
            let start = Instant::now();
            for b in 0..10 {
                t.s.batch(|tx| {
                    for i in 0..1000 {
                        tx.create(d, format!("f{b}-{i}").as_bytes(), 0o644)?;
                    }
                    Ok(())
                })
                .unwrap();
            }
            start.elapsed()
        },
    );
    measure(
        "rename in one directory (default policy)",
        inodes,
        5,
        2000,
        |rep| {
            let d = t.fill_dir(&format!("rn{rep}"), 2000);
            let start = Instant::now();
            for i in 0..2000 {
                t.s.rename(
                    d,
                    format!("f{i:06}").as_bytes(),
                    d,
                    format!("g{i:06}").as_bytes(),
                )
                .unwrap();
            }
            start.elapsed()
        },
    );
    measure(
        "rename across directories (default policy)",
        inodes,
        5,
        2000,
        |rep| {
            let a = t.fill_dir(&format!("rx{rep}"), 2000);
            let b =
                t.s.mkdir(ROOT_INO, format!("rxb{rep}").as_bytes(), 0o755)
                    .unwrap()
                    .ino;
            let start = Instant::now();
            for i in 0..2000 {
                let n = format!("f{i:06}");
                t.s.rename(a, n.as_bytes(), b, n.as_bytes()).unwrap();
            }
            start.elapsed()
        },
    );
    measure("hardlink create (default policy)", inodes, 5, 2000, |rep| {
        let a = t.fill_dir(&format!("lk{rep}"), 2000);
        let b =
            t.s.mkdir(ROOT_INO, format!("lkb{rep}").as_bytes(), 0o755)
                .unwrap()
                .ino;
        let inos: Vec<Ino> = (0..2000)
            .map(|i| t.s.lookup(a, format!("f{i:06}").as_bytes()).unwrap().ino)
            .collect();
        let start = Instant::now();
        for (i, ino) in inos.into_iter().enumerate() {
            t.s.link(ino, b, format!("l{i:06}").as_bytes()).unwrap();
        }
        start.elapsed()
    });

    m.sync().unwrap();
    let inodes = t.inodes();
    let mut marker = Marker::new();
    let mut full_count = 0;
    measure("live_blocks full walk (no skipping)", inodes, 5, 1, |_| {
        marker = Marker::new();
        let start = Instant::now();
        full_count =
            t.s.live_blocks(&mut marker)
                .unwrap()
                .map(|r| r.map(|_| 1usize).unwrap())
                .sum::<usize>();
        start.elapsed()
    });
    let mut skip_count = 0;
    measure(
        "live_blocks after 1 file changed, marker reused",
        inodes,
        5,
        1,
        |rep| {
            fork_id += 1;
            let c = t.s.fork(&format!("bench-fork-{fork_id}")).unwrap();
            let ino = c.lookup(t.dirs[rep], b"f0000").unwrap().ino;
            c.set_content(ino, &[chunk(7_000_000 + rep as u64)], 4096)
                .unwrap();
            let start = Instant::now();
            skip_count = c
                .live_blocks(&mut marker)
                .unwrap()
                .map(|r| r.map(|_| 1usize).unwrap())
                .sum::<usize>();
            let d = start.elapsed();
            m.remove_snapshot(c.id()).unwrap();
            d
        },
    );
    println!(
        "live_blocks yielded {full_count} blocks on the full walk and {skip_count} with skipping"
    );

    drop(t);
    drop(m);
    let size = std::fs::metadata(&path).map_or(0, |m| m.len());
    println!(
        "database file: {:.1} MiB for {} inodes",
        size as f64 / 1048576.0,
        inodes
    );
    println!("uptime after: load1 {:.1}", load1());
}
