//! Cold-read throughput of the core data path, alone in the process.
//!
//! `cargo run --release -p cowfs-core --example coldread` writes `COWFS_COLDREAD_BYTES`
//! (default 256 MiB) through `Core::write`, fsyncs, drops the `Core` so the verified-block cache
//! is empty, reopens, and times only the read loop. It prints the same `std::fs` read over an
//! equivalent file as the do-nothing baseline, and `uptime` load1 before and after every timed
//! batch. Every batch runs under the shared CPU lock (`COWFS_CPU_LOCK`, default the spikes lock).
//!
//! `COWFS_COLDREAD_PROFILE=<seconds>` keeps the process in the read loop for that long after
//! setup, so `sample` attributes the read path instead of the write path.
//!
//! Scratch data goes under `COWFS_BENCH_DIR` (default `target/bench-tmp`).

use std::fs;
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use cowfs_core::{Core, Options};
use cowfs_vfs::{Ino, Vfs, ROOT_INO};

const CALL: usize = 1 << 20;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
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

/// Runs `f` under the CPU lock. Returns seconds and the larger of the load1 values around it.
fn timed<T>(tag: &str, f: impl FnOnce() -> T) -> (f64, f64, T) {
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
        "coldread {tag} pid {} at {:?}\n",
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

fn scratch() -> tempfile::TempDir {
    let base = std::env::var("COWFS_BENCH_DIR").unwrap_or_else(|_| "target/bench-tmp".into());
    fs::create_dir_all(&base).expect("bench dir");
    tempfile::tempdir_in(base).expect("tempdir")
}

fn open_core(dir: &std::path::Path) -> Core {
    let c = Core::open(dir, Options::default()).expect("open");
    c.create_snapshot("s").expect("snapshot");
    c
}

/// A `Core` over a store that already holds snapshot `s`.
fn reopen(dir: &std::path::Path) -> (Core, Ino) {
    let c = Core::open(dir, Options::default()).expect("reopen");
    let f = c.lookup(ROOT_INO, b"s").expect("snapshot root").ino;
    let i = c.lookup(f, b"cold").expect("cold").ino;
    (c, i)
}

/// Reads the whole file through `Core::read` in `CALL`-byte calls, checking every byte.
fn core_read(c: &Core, ino: Ino, data: &[u8]) {
    let mut off = 0u64;
    while off < data.len() as u64 {
        let g = c.read(ino, off, CALL as u32).expect("read");
        assert!(!g.is_empty(), "read returned nothing at {off}");
        let at = off as usize;
        assert_eq!(
            g,
            data[at..at + g.len()],
            "read returned wrong data at {off}"
        );
        off += g.len() as u64;
    }
    assert_eq!(
        c.read(ino, data.len() as u64, CALL as u32)
            .expect("read past end"),
        Vec::<u8>::new()
    );
}

fn base_read(p: &std::path::Path, len: usize) {
    let f = fs::File::open(p).expect("open");
    let mut buf = vec![0u8; CALL];
    let mut left = len;
    while left > 0 {
        let want = CALL.min(left);
        f.read_exact_at(&mut buf[..want], (len - left) as u64)
            .expect("read");
        left -= want;
    }
}

fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    s[s.len() / 2]
}

/// Reads the file once per cache state and prints `pread` counts, which are a count rather than a
/// time and so hold at any machine load.
fn counts_only(c: &Core, ino: Ino, data: &[u8], label: &str) {
    let mut off = 0u64;
    while off < data.len() as u64 {
        let g = c.read(ino, off, CALL as u32).expect("read");
        assert_eq!(g, &data[off as usize..off as usize + g.len()]);
        off += g.len() as u64;
    }
    println!(
        "{label}: read {:.0} MiB, {} chunks of data",
        data.len() as f64 / 1048576.0,
        data.len().div_ceil(65536),
    );
}

fn main() {
    let bytes: usize = std::env::var("COWFS_COLDREAD_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(256 << 20);
    let reps: usize = std::env::var("COWFS_COLDREAD_REPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let profile: u64 = std::env::var("COWFS_COLDREAD_PROFILE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mib = bytes as f64 / 1048576.0;

    let d = scratch();
    let c = open_core(d.path());
    let root = c.lookup(ROOT_INO, b"s").expect("root").ino;
    let ino = c.create(root, b"cold", 0o644).expect("create").ino;
    let data = gen(bytes, 7);
    for (i, b) in data.chunks(CALL).enumerate() {
        c.write(ino, (i * CALL) as u64, b).expect("write");
    }
    c.fsync(ino, false).expect("fsync");
    let nd = scratch();
    let p = nd.path().join("cold");
    {
        let mut f = fs::File::create(&p).expect("create");
        for b in data.chunks(CALL) {
            f.write_all(b).expect("write");
        }
        f.sync_all().expect("sync");
    }
    let attrs = c.getattr(ino).expect("getattr");
    assert_eq!(attrs.size, bytes as u64, "core size");
    assert_eq!(
        fs::metadata(&p).expect("stat").len(),
        bytes as u64,
        "fs size"
    );
    println!(
        "wrote {mib:.0} MiB, {} chunks, core size {} ok",
        attrs.size.div_ceil(CALL as u64),
        attrs.size
    );
    drop(c);

    if std::env::var("COWFS_COLDREAD_COUNTS").is_ok() {
        let (c, i) = reopen(d.path());
        c.drop_caches();
        counts_only(&c, i, &data, "cold (empty cache)");
        counts_only(&c, i, &data, "warm (same core, cache filled)");
        drop(c);
        let (c, i) = reopen(d.path());
        c.drop_caches();
        counts_only(&c, i, &data, "cold again (fresh core)");
        return;
    }

    if profile > 0 {
        let deadline = Instant::now() + Duration::from_secs(profile);
        let mut rounds = 0u64;
        while Instant::now() < deadline {
            let (c, i) = reopen(d.path());
            c.drop_caches();
            core_read(&c, i, &data);
            drop(c);
            rounds += 1;
        }
        println!("profile: {rounds} read rounds in {profile}s");
        return;
    }

    println!("uptime at start: load1 {}", load1());
    let mut core_s = Vec::new();
    let mut base_s = Vec::new();
    let mut load = 0.0f64;
    for _ in 0..reps {
        let (c, i) = reopen(d.path());
        c.drop_caches();
        let (s, l, _) = timed("core", || core_read(&c, i, &data));
        core_s.push(s);
        load = load.max(l);
        drop(c);
        let (s, l, _) = timed("base", || base_read(&p, bytes));
        base_s.push(s);
        load = load.max(l);
    }
    let m = median(&core_s);
    let b = median(&base_s);
    let flag = if load > 30.0 { " HIGH" } else { "" };
    println!(
        "| metric | n | cowfs-core median | range | std::fs median | core / baseline | max load1 |"
    );
    println!("|---|---|---|---|---|---|---|");
    println!(
        "| cold read {mib:.0} MiB in 1 MiB calls | {reps} | {:.0} MiB/s | {:.3} - {:.3} s | {:.0} MiB/s | {:.2}x | {:.0}{flag} |",
        mib / m,
        core_s.iter().copied().fold(f64::INFINITY, f64::min),
        core_s.iter().copied().fold(0.0, f64::max),
        mib / b,
        m / b,
        load
    );
    println!(
        "floor: the read path must move each byte at least once; {:.0} MiB over {reps} reps",
        mib
    );
    println!("uptime at end: load1 {}", load1());
}
