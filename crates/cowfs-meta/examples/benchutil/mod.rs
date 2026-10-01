//! Shared by the benchmark examples: CPU lock, load, and the n-repetition measurement table.

use std::process::Command;
use std::time::{Duration, Instant};

const DEFAULT_LOCK: &str = "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/cpu.lock";

pub struct LockGuard(Option<std::path::PathBuf>);

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            let _ = std::fs::remove_dir_all(p);
        }
    }
}

pub fn take_lock() -> LockGuard {
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

pub fn load1() -> f64 {
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

pub struct Rng(pub u64);

impl Rng {
    pub fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % n as u64) as usize
    }
}

pub fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    v[v.len() / 2]
}

/// Prints one table row from per-operation samples in microseconds.
pub fn row(metric: &str, inodes: usize, label: &str, mut per_op: Vec<f64>, load_max: f64) {
    per_op.sort_by(|a, b| a.total_cmp(b));
    let med = median(&mut per_op);
    let flag = if load_max > 30.0 { " HIGH-LOAD" } else { "" };
    println!(
        "| {metric} | {inodes} | {label} | {med:.1} | {:.1} | {:.1} | {:.0} | {load_max:.0}{flag} |",
        per_op[0],
        per_op[per_op.len() - 1],
        1e6 / med,
    );
}

/// Runs `f` while holding the shared CPU lock; returns its result and the higher of the load
/// averages read just before and just after.
pub fn locked<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let before = load1();
    let out = {
        let _lock = take_lock();
        f()
    };
    (out, before.max(load1()))
}

/// Runs `reps` batches; each returns the time it measured for `ops` operations.
pub fn measure(
    metric: &str,
    inodes: usize,
    reps: usize,
    ops: usize,
    mut f: impl FnMut(usize) -> Duration,
) {
    let (mut per_op, mut load_max) = (Vec::new(), 0.0f64);
    for rep in 0..reps {
        let (d, load) = locked(|| f(rep));
        load_max = load_max.max(load);
        per_op.push(d.as_secs_f64() * 1e6 / ops as f64);
    }
    row(metric, inodes, &format!("{reps}x{ops}"), per_op, load_max);
}
