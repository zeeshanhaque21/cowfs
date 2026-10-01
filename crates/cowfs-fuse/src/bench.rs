//! Per-operation latency harness for any mounted directory, so it can be pointed at a mount of
//! any `Vfs` (the real core included). It uses only `std::fs`, so what it measures is the
//! whole path: syscall, kernel, FUSE round trip, adapter, `Vfs`.
//!
//! Numbers from a `Vfs` that is only an in-memory model are a floor for the adapter, not a result.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Instant;

/// What to measure and how often.
#[derive(Clone, Debug)]
pub struct Config {
    /// Repetitions of each timed batch. Use at least 5.
    pub reps: usize,
    /// Operations per batch for every measurement except the directory listing.
    pub ops: usize,
    /// Entries in the directory listed by the readdir measurement.
    pub dir_entries: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            reps: 5,
            ops: 2000,
            dir_entries: 10_000,
        }
    }
}

/// Microseconds per operation for each repetition of one measurement.
#[derive(Clone, Debug)]
pub struct Stat {
    /// What was measured.
    pub name: &'static str,
    /// Microseconds per operation, one value per repetition.
    pub us_per_op: Vec<f64>,
}

impl Stat {
    /// Median over the repetitions.
    pub fn median(&self) -> f64 {
        let mut v = self.us_per_op.clone();
        v.sort_by(f64::total_cmp);
        match v.len() {
            0 => f64::NAN,
            n if n % 2 == 1 => v[n / 2],
            n => (v[n / 2 - 1] + v[n / 2]) / 2.0,
        }
    }

    /// Smallest repetition.
    pub fn min(&self) -> f64 {
        self.us_per_op.iter().copied().fold(f64::NAN, f64::min)
    }

    /// Largest repetition.
    pub fn max(&self) -> f64 {
        self.us_per_op.iter().copied().fold(f64::NAN, f64::max)
    }
}

/// All measurements plus the machine load around them.
#[derive(Clone, Debug)]
pub struct Report {
    /// One-minute load average before the run, where `/proc/loadavg` exists.
    pub load1_before: Option<f64>,
    /// One-minute load average after the run.
    pub load1_after: Option<f64>,
    /// Repetitions per measurement.
    pub reps: usize,
    /// The measurements.
    pub stats: Vec<Stat>,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "n={} load1 before={:?} after={:?} (microseconds per operation)",
            self.reps, self.load1_before, self.load1_after
        )?;
        for s in &self.stats {
            writeln!(
                f,
                "{:<48} median {:>9.2}  min {:>9.2}  max {:>9.2}",
                s.name,
                s.median(),
                s.min(),
                s.max()
            )?;
        }
        Ok(())
    }
}

fn load1() -> Option<f64> {
    fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

fn time_batch(ops: usize, mut f: impl FnMut(usize) -> io::Result<()>) -> io::Result<f64> {
    let t = Instant::now();
    for i in 0..ops {
        f(i)?;
    }
    Ok(t.elapsed().as_secs_f64() * 1e6 / ops as f64)
}

fn missing(p: &Path) -> io::Result<()> {
    match fs::metadata(p) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(_) => Err(io::Error::other("name unexpectedly exists")),
    }
}

/// Reads with `O_DIRECT` so the page cache cannot answer. `None` where that is unavailable.
#[cfg(target_os = "linux")]
fn direct_read(file: &Path, reps: usize, ops: usize) -> io::Result<Option<Vec<f64>>> {
    let Ok(d) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECT)
        .open(file)
    else {
        return Ok(None);
    };
    let mut aligned = vec![0u8; 8192];
    let start = (4096 - aligned.as_ptr() as usize % 4096) % 4096;
    let buf = &mut aligned[start..start + 4096];
    if d.read_exact_at(buf, 0).is_err() {
        return Ok(None);
    }
    (0..reps)
        .map(|_| time_batch(ops, |i| d.read_exact_at(buf, (i * 4096) as u64)))
        .collect::<io::Result<_>>()
        .map(Some)
}

#[cfg(not(target_os = "linux"))]
fn direct_read(_file: &Path, _reps: usize, _ops: usize) -> io::Result<Option<Vec<f64>>> {
    Ok(None)
}

/// Runs every measurement in a fresh directory under `dir` and removes it afterwards.
/// `dir` must be on the mount under test and writable.
pub fn measure(dir: &Path, cfg: &Config) -> io::Result<Report> {
    let work = dir.join(format!("bench-{}", std::process::id()));
    fs::create_dir(&work)?;
    let r = run(&work, cfg);
    let cleanup = fs::remove_dir_all(&work);
    let report = r?;
    cleanup?;
    Ok(report)
}

fn run(work: &Path, cfg: &Config) -> io::Result<Report> {
    let load1_before = load1();
    let mut stats: Vec<Stat> = Vec::new();
    let mut add = |name: &'static str, v: Vec<f64>| stats.push(Stat { name, us_per_op: v });
    let reps = cfg.reps;
    let ops = cfg.ops;

    add(
        "lookup missing name, unique names",
        (0..reps)
            .map(|r| time_batch(ops, |i| missing(&work.join(format!("miss-{r}-{i}")))))
            .collect::<io::Result<_>>()?,
    );

    let same = work.join("never-created");
    missing(&same)?;
    add(
        "lookup missing name, same name repeated",
        (0..reps)
            .map(|_| time_batch(ops, |_| missing(&same)))
            .collect::<io::Result<_>>()?,
    );

    let file = work.join("data");
    let page = [0xa5u8; 4096];
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&file)?;
    let span = ops.max(1) * page.len();
    f.write_all_at(&vec![7u8; span], 0)?;

    add(
        "lookup existing name, repeated",
        (0..reps)
            .map(|_| time_batch(ops, |_| fs::metadata(&file).map(|_| ())))
            .collect::<io::Result<_>>()?,
    );

    let mut buf = [0u8; 4096];
    add(
        "read 4 KiB (page cache)",
        (0..reps)
            .map(|_| time_batch(ops, |i| f.read_exact_at(&mut buf, (i * page.len()) as u64)))
            .collect::<io::Result<_>>()?,
    );

    if let Some(v) = direct_read(&file, reps, ops)? {
        add("read 4 KiB (O_DIRECT, one round trip)", v);
    }

    add(
        "write 4 KiB",
        (0..reps)
            .map(|_| time_batch(ops, |i| f.write_all_at(&page, (i * page.len()) as u64)))
            .collect::<io::Result<_>>()?,
    );

    add(
        "create empty file and close",
        (0..reps)
            .map(|r| {
                time_batch(ops, |i| {
                    File::create(work.join(format!("new-{r}-{i}"))).map(|_| ())
                })
            })
            .collect::<io::Result<_>>()?,
    );

    let big = work.join("big");
    fs::create_dir(&big)?;
    for i in 0..cfg.dir_entries {
        File::create(big.join(format!("e{i:06}")))?;
    }
    let mut listing = Vec::with_capacity(reps);
    for _ in 0..reps {
        let t = Instant::now();
        let mut n = 0usize;
        for e in fs::read_dir(&big)? {
            e?;
            n += 1;
        }
        if n != cfg.dir_entries {
            return Err(io::Error::other(format!(
                "listed {n} of {}",
                cfg.dir_entries
            )));
        }
        listing.push(t.elapsed().as_secs_f64() * 1e6 / n.max(1) as f64);
    }
    add("readdir, per entry of the listing", listing);

    Ok(Report {
        load1_before,
        load1_after: load1(),
        reps,
        stats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_summaries() {
        let s = Stat {
            name: "x",
            us_per_op: vec![3.0, 1.0, 2.0, 10.0],
        };
        assert_eq!((s.median(), s.min(), s.max()), (2.5, 1.0, 10.0));
        let s = Stat {
            name: "x",
            us_per_op: vec![5.0, 1.0, 9.0],
        };
        assert_eq!(s.median(), 5.0);
    }

    #[test]
    fn runs_on_a_plain_directory_and_cleans_up() {
        let d = tempfile::tempdir().unwrap();
        let cfg = Config {
            reps: 2,
            ops: 20,
            dir_entries: 30,
        };
        let r = measure(d.path(), &cfg).unwrap();
        assert!(r.stats.len() >= 6);
        assert!(r.stats.iter().all(|s| s.us_per_op.len() == 2));
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 0);
        assert!(r.to_string().contains("readdir"));
    }
}
