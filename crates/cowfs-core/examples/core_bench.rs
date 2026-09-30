//! Benchmark of `Core` against raw `std::fs` on the same file system.
//!
//! `cargo run --release -p cowfs-core --example core_bench` prints a Markdown table. `COWFS_BENCH_QUICK=1`
//! shrinks every size for a smoke run. Each timed batch runs under the shared CPU lock directory
//! (`COWFS_CPU_LOCK`, default the spikes lock) and records `uptime` load before and after.
//! Scratch data goes under `COWFS_BENCH_DIR` (default `target/bench-tmp`).

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use cowfs_core::{Core, Options};
use cowfs_vfs::{Ino, Vfs, ROOT_INO};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn gen(len: usize, seed: u64) -> Vec<u8> {
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut v = vec![0u8; len];
    for c in v.chunks_mut(8) {
        let b = r.next().to_le_bytes();
        c.copy_from_slice(&b[..c.len()]);
    }
    v
}

fn load1() -> f64 {
    let out = Command::new("uptime")
        .output()
        .map(|o| o.stdout)
        .unwrap_or_default();
    let s = String::from_utf8_lossy(&out);
    s.split("load average")
        .nth(1)
        .and_then(|t| t.split(':').nth(1))
        .and_then(|t| t.split_whitespace().next())
        .and_then(|t| t.trim_end_matches(',').parse().ok())
        .unwrap_or(f64::NAN)
}

struct Held(PathBuf);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn lock_path() -> PathBuf {
    std::env::var("COWFS_CPU_LOCK").map_or_else(
        |_| PathBuf::from("/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/cpu.lock"),
        PathBuf::from,
    )
}

/// Runs `f` under the CPU lock. Returns seconds and the larger of the load1 values before and after.
fn timed<T>(f: impl FnOnce() -> T) -> (f64, f64, T) {
    let path = lock_path();
    let start = Instant::now();
    loop {
        match fs::create_dir(&path) {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                assert!(
                    start.elapsed() < Duration::from_secs(900),
                    "cpu lock busy for 15 minutes"
                );
                std::thread::sleep(Duration::from_secs(10));
            }
            Err(e) => panic!("cpu lock {path:?}: {e}"),
        }
    }
    let _held = Held(path.clone());
    let owner = format!(
        "cowfs-core-bench pid {} at {:?}\n",
        std::process::id(),
        std::time::SystemTime::now()
    );
    let _ = fs::write(path.join("owner"), owner);
    let l0 = load1();
    let t = Instant::now();
    let out = f();
    let secs = t.elapsed().as_secs_f64();
    let l1 = load1();
    (secs, l0.max(l1), out)
}

struct Row {
    metric: String,
    unit: &'static str,
    amount: f64,
    core: Vec<f64>,
    base: Vec<f64>,
    load: f64,
}

fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    s[s.len() / 2]
}

impl Row {
    fn new(metric: &str, unit: &'static str, amount: f64) -> Self {
        Self {
            metric: metric.into(),
            unit,
            amount,
            core: Vec::new(),
            base: Vec::new(),
            load: 0.0,
        }
    }

    fn core(&mut self, (s, l, ()): (f64, f64, ())) {
        self.core.push(s);
        self.load = self.load.max(l);
    }

    fn base(&mut self, (s, l, ()): (f64, f64, ())) {
        self.base.push(s);
        self.load = self.load.max(l);
    }

    fn print(&self) {
        let rate = |v: &[f64]| {
            if v.is_empty() {
                return "-".to_string();
            }
            let r = self.amount / median(v);
            if r >= 100.0 {
                format!("{r:.0} {}", self.unit)
            } else {
                format!("{r:.2} {}", self.unit)
            }
        };
        let lo = self.core.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = self.core.iter().copied().fold(0.0, f64::max);
        let ratio = if self.base.is_empty() {
            "-".to_string()
        } else {
            format!("{:.2}x", median(&self.core) / median(&self.base))
        };
        let flag = if self.load > 30.0 { " HIGH" } else { "" };
        println!(
            "| {} | {} | {} | {:.3} - {:.3} s | {} | {} | {:.0}{} |",
            self.metric,
            self.core.len(),
            rate(&self.core),
            lo,
            hi,
            rate(&self.base),
            ratio,
            self.load,
            flag
        );
    }
}

fn scratch() -> tempfile::TempDir {
    let base = std::env::var("COWFS_BENCH_DIR").unwrap_or_else(|_| "target/bench-tmp".into());
    fs::create_dir_all(&base).expect("bench dir");
    tempfile::tempdir_in(base).expect("tempdir")
}

fn open_core(dir: &Path) -> (Core, Ino) {
    let c = Core::open(dir, Options::default()).expect("open");
    c.create_snapshot("s").expect("snapshot");
    let r = c.lookup(ROOT_INO, b"s").expect("root").ino;
    (c, r)
}

fn size_mix(r: &mut Rng) -> usize {
    match r.below(100) {
        0..=59 => r.below(4096) as usize,
        60..=89 => 4096 + r.below(28 << 10) as usize,
        90..=98 => (32 << 10) + r.below(96 << 10) as usize,
        _ => (128 << 10) + r.below(384 << 10) as usize,
    }
}

struct Cfg {
    n: usize,
    files: usize,
    seq: usize,
    rnd_file: usize,
    rnd_ops: usize,
    tree: usize,
    lookups: usize,
}

fn create_rate(cfg: &Cfg, rows: &mut Vec<Row>) {
    let mut r = Rng(11);
    let data: Vec<Vec<u8>> = (0..cfg.files)
        .map(|i| gen(size_mix(&mut r), i as u64 + 1))
        .collect();
    let total: usize = data.iter().map(Vec::len).sum();
    let mut row = Row::new(
        &format!(
            "create {} files, {:.0} MiB total, then durable",
            cfg.files,
            total as f64 / 1048576.0
        ),
        "files/s",
        cfg.files as f64,
    );
    let mut batches = 0;
    for _ in 0..cfg.n {
        let d = scratch();
        let (c, root) = open_core(d.path());
        let dirs: Vec<Ino> = (0..100)
            .map(|i| {
                c.mkdir(root, format!("d{i}").as_bytes(), 0o755)
                    .expect("mkdir")
                    .ino
            })
            .collect();
        c.sync().expect("sync");
        let b0 = c.stats().batches;
        row.core(timed(|| {
            for (i, bytes) in data.iter().enumerate() {
                let a = c
                    .create(dirs[i % 100], format!("f{i}").as_bytes(), 0o644)
                    .expect("create");
                c.write(a.ino, 0, bytes).expect("write");
                c.forget(a.ino, 1);
            }
            c.sync().expect("sync");
        }));
        batches = c.stats().batches - b0;
        drop(c);
        let d = scratch();
        for i in 0..100 {
            fs::create_dir(d.path().join(format!("d{i}"))).expect("mkdir");
        }
        row.base(timed(|| {
            for (i, bytes) in data.iter().enumerate() {
                fs::write(d.path().join(format!("d{}/f{i}", i % 100)), bytes).expect("write");
            }
        }));
    }
    println!(
        "<!-- create: {batches} meta batches per run; baseline is page cache only, no fsync -->"
    );
    rows.push(row);
}

fn lookups(cfg: &Cfg, rows: &mut Vec<Row>) {
    let n = cfg.lookups;
    let d = scratch();
    let (c, root) = open_core(d.path());
    let dir = c.mkdir(root, b"d", 0o755).expect("mkdir").ino;
    for i in 0..n {
        let a = c
            .create(dir, format!("f{i}").as_bytes(), 0o644)
            .expect("create");
        c.write(a.ino, 0, b"0123456789").expect("write");
        c.forget(a.ino, 1);
    }
    c.sync().expect("sync");
    let nd = scratch();
    fs::create_dir(nd.path().join("d")).expect("mkdir");
    for i in 0..n {
        fs::write(nd.path().join(format!("d/f{i}")), b"0123456789").expect("write");
    }
    let hit = |c: &Core| {
        for i in 0..n {
            let a = c.lookup(dir, format!("f{i}").as_bytes()).expect("hit");
            c.forget(a.ino, 1);
        }
    };
    let miss = |c: &Core| {
        for i in 0..n {
            assert!(c.lookup(dir, format!("m{i}").as_bytes()).is_err());
        }
    };
    let native_hit = || {
        for i in 0..n {
            fs::metadata(nd.path().join(format!("d/f{i}"))).expect("hit");
        }
    };
    let native_miss = || {
        for i in 0..n {
            assert!(fs::metadata(nd.path().join(format!("d/m{i}"))).is_err());
        }
    };
    let mut r = [
        Row::new(
            &format!("lookup hit, cold caches, {n} names"),
            "lookups/s",
            n as f64,
        ),
        Row::new("lookup hit, warm caches", "lookups/s", n as f64),
        Row::new("lookup miss, cold caches", "lookups/s", n as f64),
        Row::new(
            "lookup miss, warm (negative entries)",
            "lookups/s",
            n as f64,
        ),
        Row::new("getattr of held inodes, warm", "stats/s", n as f64),
    ];
    for _ in 0..cfg.n {
        c.drop_caches();
        r[0].core(timed(|| hit(&c)));
        r[1].core(timed(|| hit(&c)));
        c.drop_caches();
        r[2].core(timed(|| miss(&c)));
        r[3].core(timed(|| miss(&c)));
        let held: Vec<Ino> = (0..n)
            .map(|i| c.lookup(dir, format!("f{i}").as_bytes()).expect("hit").ino)
            .collect();
        r[4].core(timed(|| {
            for i in &held {
                c.getattr(*i).expect("getattr");
            }
        }));
        for i in held {
            c.forget(i, 1);
        }
        r[0].base(timed(native_hit));
        r[1].base(timed(native_hit));
        r[2].base(timed(native_miss));
        r[3].base(timed(native_miss));
        r[4].base(timed(native_hit));
    }
    rows.extend(r);
}

fn cargo_replay(cfg: &Cfg, rows: &mut Vec<Row>) {
    let (dirs_n, per, miss_per) = (200usize, 10usize, 70usize);
    let _ = cfg;
    let d = scratch();
    let (c, root) = open_core(d.path());
    let nd = scratch();
    let mut dirs = Vec::new();
    for i in 0..dirs_n {
        let dn = c
            .mkdir(root, format!("crate{i}").as_bytes(), 0o755)
            .expect("mkdir")
            .ino;
        fs::create_dir(nd.path().join(format!("crate{i}"))).expect("mkdir");
        for j in 0..per {
            let a = c
                .create(dn, format!("u{j}.o").as_bytes(), 0o644)
                .expect("create");
            c.forget(a.ino, 1);
            fs::write(nd.path().join(format!("crate{i}/u{j}.o")), b"").expect("write");
        }
        dirs.push(dn);
    }
    c.sync().expect("sync");
    let mut ops: Vec<(usize, String)> = Vec::new();
    for i in 0..dirs_n {
        for j in 0..per {
            ops.push((i, format!("u{j}.o")));
        }
        for j in 0..miss_per {
            ops.push((i, format!("dep{j}.d")));
        }
    }
    let mut r = Rng(5);
    for i in (1..ops.len()).rev() {
        let j = r.below(i as u64 + 1) as usize;
        ops.swap(i, j);
    }
    let total = ops.len();
    let mut cold = Row::new(
        &format!(
            "cargo no-op lookup replay ({total} lookups: 2000 hit, 14000 miss, synthetic), cold"
        ),
        "lookups/s",
        total as f64,
    );
    let mut warm = Row::new("same replay, warm", "lookups/s", total as f64);
    let native = || {
        for (i, n) in &ops {
            let _ = fs::metadata(nd.path().join(format!("crate{i}/{n}")));
        }
    };
    let replay = || {
        for (i, n) in &ops {
            if let Ok(a) = c.lookup(dirs[*i], n.as_bytes()) {
                c.forget(a.ino, 1);
            }
        }
    };
    for _ in 0..cfg.n {
        c.drop_caches();
        cold.core(timed(replay));
        warm.core(timed(replay));
        cold.base(timed(native));
        warm.base(timed(native));
    }
    rows.push(cold);
    rows.push(warm);
}

fn seq_and_random(cfg: &Cfg, rows: &mut Vec<Row>) {
    let mib = cfg.seq as f64 / 1048576.0;
    let mut w = Row::new(
        &format!("sequential write {mib:.0} MiB + fsync (1 MiB calls)"),
        "MiB/s",
        mib,
    );
    let mut rd = Row::new(
        &format!("sequential read {mib:.0} MiB, caches dropped"),
        "MiB/s",
        mib,
    );
    for rep in 0..cfg.n {
        let data = gen(cfg.seq, rep as u64 + 100);
        let d = scratch();
        let (c, root) = open_core(d.path());
        let ino = c.create(root, b"big", 0o644).expect("create").ino;
        w.core(timed(|| {
            for (i, b) in data.chunks(1 << 20).enumerate() {
                c.write(ino, (i as u64) << 20, b).expect("write");
            }
            c.fsync(ino, false).expect("fsync");
        }));
        c.drop_caches();
        let mut got = Vec::new();
        rd.core(timed(|| {
            let mut off = 0u64;
            while off < cfg.seq as u64 {
                let g = c.read(ino, off, 1 << 20).expect("read");
                off += g.len() as u64;
                got.push(g);
            }
        }));
        assert!(
            got.iter().zip(data.chunks(1 << 20)).all(|(a, b)| a == b),
            "sequential read returned wrong data"
        );
        drop(got);
        drop(c);
        let nd = scratch();
        let p = nd.path().join("big");
        w.base(timed(|| {
            let mut f = fs::File::create(&p).expect("create");
            for b in data.chunks(1 << 20) {
                f.write_all(b).expect("write");
            }
            f.sync_all().expect("sync");
        }));
        rd.base(timed(|| {
            let mut f = fs::File::open(&p).expect("open");
            let mut buf = vec![0u8; 1 << 20];
            for _ in 0..cfg.seq >> 20 {
                f.read_exact(&mut buf).expect("read");
            }
        }));
    }
    rows.push(w);
    rows.push(rd);
    let mut floor = Row::new(
        &format!("floor: Store::ingest_bytes + sync of {mib:.0} MiB, no Core"),
        "MiB/s",
        mib,
    );
    let mut chunk_only = Row::new("floor: FastCDC + BLAKE3 only (no store)", "MiB/s", mib);
    for rep in 0..cfg.n {
        let data = gen(cfg.seq, rep as u64 + 100);
        let d = scratch();
        let st =
            cowfs_store::Store::open(d.path(), cowfs_store::Options::default()).expect("store");
        floor.core(timed(|| {
            st.ingest_bytes(&data).expect("ingest");
            st.sync().expect("sync");
        }));
        chunk_only.core(timed(|| {
            let mut n = 0usize;
            for c in cowfs_store::chunks(&data) {
                n += cowfs_store::BlockId::of(c).as_bytes()[0] as usize;
            }
            std::hint::black_box(n);
        }));
    }
    rows.push(floor);
    rows.push(chunk_only);

    let blocks = (cfg.rnd_file / 4096) as u64;
    let mib = cfg.rnd_file as f64 / 1048576.0;
    let mut rr = Row::new(
        &format!(
            "random 4 KiB reads, {mib:.0} MiB file, caches dropped, {} ops",
            cfg.rnd_ops
        ),
        "ops/s",
        cfg.rnd_ops as f64,
    );
    let mut rw = Row::new(
        &format!("random 4 KiB writes + fsync, {} ops", cfg.rnd_ops),
        "ops/s",
        cfg.rnd_ops as f64,
    );
    for rep in 0..cfg.n {
        let data = gen(cfg.rnd_file, rep as u64 + 200);
        let mut r = Rng(rep as u64 + 7);
        let pos: Vec<u64> = (0..cfg.rnd_ops).map(|_| r.below(blocks)).collect();
        let patch = gen(4096, 3);
        let d = scratch();
        let (c, root) = open_core(d.path());
        let ino = c.create(root, b"f", 0o644).expect("create").ino;
        for (i, b) in data.chunks(1 << 20).enumerate() {
            c.write(ino, (i as u64) << 20, b).expect("write");
        }
        c.fsync(ino, false).expect("fsync");
        c.drop_caches();
        rr.core(timed(|| {
            for p in &pos {
                c.read(ino, p * 4096, 4096).expect("read");
            }
        }));
        rw.core(timed(|| {
            for p in &pos {
                c.write(ino, p * 4096, &patch).expect("write");
            }
            c.fsync(ino, false).expect("fsync");
        }));
        drop(c);
        let nd = scratch();
        let f = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(nd.path().join("f"))
            .expect("create");
        f.write_all_at(&data, 0).expect("write");
        f.sync_all().expect("sync");
        let mut buf = vec![0u8; 4096];
        rr.base(timed(|| {
            for p in &pos {
                f.read_exact_at(&mut buf, p * 4096).expect("read");
            }
        }));
        rw.base(timed(|| {
            for p in &pos {
                f.write_all_at(&patch, p * 4096).expect("write");
            }
            f.sync_all().expect("sync");
        }));
    }
    rows.push(rr);
    rows.push(rw);
}

fn build_tree(c: &Core, root: Ino, files: usize) {
    let dirs = (files / 1000).max(1);
    for d in 0..dirs {
        let dn = c
            .mkdir(root, format!("d{d}").as_bytes(), 0o755)
            .expect("mkdir")
            .ino;
        for i in 0..files / dirs {
            let a = c
                .create(dn, format!("f{i}").as_bytes(), 0o644)
                .expect("create");
            c.write(a.ino, 0, b"0123456789").expect("write");
            c.forget(a.ino, 1);
        }
        c.forget(dn, 1);
    }
    c.sync().expect("sync");
}

fn snapshots(cfg: &Cfg, rows: &mut Vec<Row>) {
    for files in [1000, cfg.tree] {
        let d = scratch();
        let (c, root) = open_core(d.path());
        let t = Instant::now();
        build_tree(&c, root, files);
        println!(
            "<!-- built a {files}-file tree in {:.1} s ({:.0} files/s, durable) -->",
            t.elapsed().as_secs_f64(),
            files as f64 / t.elapsed().as_secs_f64()
        );
        let mut row = Row::new(
            &format!("snapshot create (durable), {files} files"),
            "snapshots/s",
            1.0,
        );
        for i in 0..cfg.n {
            row.core(timed(|| {
                c.fork_snapshot("s", &format!("f{i}")).expect("fork");
            }));
        }
        for i in 0..cfg.n {
            c.remove_snapshot(&format!("f{i}")).expect("remove");
        }
        if files == cfg.tree {
            let nd = scratch();
            let src = nd.path().join("src");
            for dd in 0..files / 1000 {
                fs::create_dir_all(src.join(format!("d{dd}"))).expect("mkdir");
                for i in 0..1000 {
                    fs::write(src.join(format!("d{dd}/f{i}")), b"0123456789").expect("write");
                }
            }
            for i in 0..cfg.n {
                let dst = nd.path().join(format!("clone{i}"));
                row.base(timed(|| {
                    let st = Command::new("cp")
                        .arg("-cR")
                        .arg(&src)
                        .arg(&dst)
                        .status()
                        .expect("cp");
                    assert!(st.success());
                }));
                let _ = fs::remove_dir_all(&dst);
            }
        }
        rows.push(row);
    }
}

fn main() {
    let quick = std::env::var("COWFS_BENCH_QUICK").is_ok_and(|v| v == "1");
    let cfg = if quick {
        Cfg {
            n: 2,
            files: 2000,
            seq: 64 << 20,
            rnd_file: 16 << 20,
            rnd_ops: 500,
            tree: 10_000,
            lookups: 1000,
        }
    } else {
        Cfg {
            n: 5,
            files: 20_000,
            seq: 1 << 30,
            rnd_file: 256 << 20,
            rnd_ops: 5000,
            tree: 100_000,
            lookups: 10_000,
        }
    };
    println!("uptime at start: load1 {}", load1());
    let mut rows = Vec::new();
    println!("| metric | n | cowfs-core median | range | std::fs median | core time / baseline time | max load1 |");
    println!("|---|---|---|---|---|---|---|");
    let sections: [fn(&Cfg, &mut Vec<Row>); 5] = [
        create_rate,
        lookups,
        cargo_replay,
        seq_and_random,
        snapshots,
    ];
    for s in sections {
        let before = rows.len();
        s(&cfg, &mut rows);
        for r in &rows[before..] {
            r.print();
        }
    }
    println!("uptime at end: load1 {}", load1());
}
