//! Per-operation latency over the mount against native APFS, in the same run. `#[ignore]`d.
//!
//! `cargo test -p cowfs-nfs --release --test bench -j4 -- --ignored --nocapture`
//!
//! Setup happens outside the timed section and through the `Vfs` itself, so the NFS client has
//! never seen the files it times: every timed call is a cold round trip. Timed batches hold the
//! shared CPU lock (`COWFS_CPU_LOCK`, default the spike lock directory); the lock is never held
//! across setup. `COWFS_BENCH_REPS` sets the repetitions (default 5, minimum 5 for a report).
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cowfs_nfs::{mount_nfs_available, Mount, MountOptions};
use cowfs_vfs::{Vfs, ROOT_INO};
use cowfs_vfs_test::MemVfs;

const ITERS: usize = 300;
const DIR_ENTRIES: usize = 10_000;
const SMALL: usize = 4096;
const OPS: [&str; 6] = [
    "lookup missing name",
    "lookup existing (stat)",
    "read 4 KiB (open+read+close)",
    "write 4 KiB (open+write+close)",
    "create empty file",
    "readdir 10,000 entries (total)",
];

struct CpuLock(PathBuf);

impl CpuLock {
    fn acquire() -> CpuLock {
        let path = std::env::var("COWFS_CPU_LOCK").map_or_else(
            |_| {
                PathBuf::from("/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/cpu.lock")
            },
            PathBuf::from,
        );
        let deadline = Instant::now() + Duration::from_secs(15 * 60);
        loop {
            if fs::create_dir(&path).is_ok() {
                let stamp = format!(
                    "cowfs-nfs bench pid {} at {:?}\n",
                    std::process::id(),
                    std::time::SystemTime::now()
                );
                let _ = fs::write(path.join("owner"), stamp);
                return CpuLock(path);
            }
            assert!(Instant::now() < deadline, "cpu lock busy for 15 minutes");
            std::thread::sleep(Duration::from_secs(10));
        }
    }
}

impl Drop for CpuLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn load1() -> f64 {
    let out = Command::new("uptime")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    out.ok()
        .and_then(|s| {
            s.split("load averages:")
                .nth(1)
                .and_then(|t| t.split_whitespace().next().map(str::to_owned))
        })
        .and_then(|t| t.trim_end_matches(',').parse().ok())
        .unwrap_or(f64::NAN)
}

/// Where files are created: through the `Vfs` (never seen by the client) or on native APFS.
enum Side<'a> {
    Native,
    Nfs(&'a MemVfs),
}

impl Side<'_> {
    fn prep_dir(&self, root: &Path, name: &str, files: usize, len: usize) -> PathBuf {
        let dir = root.join(name);
        match self {
            Side::Native => {
                fs::create_dir(&dir).unwrap();
                for i in 0..files {
                    fs::write(dir.join(format!("f{i}")), vec![1u8; len]).unwrap();
                }
            }
            Side::Nfs(v) => {
                let d = v.mkdir(ROOT_INO, name.as_bytes(), 0o755).unwrap().ino;
                for i in 0..files {
                    let f = v.create(d, format!("f{i}").as_bytes(), 0o644).unwrap().ino;
                    if len > 0 {
                        v.write(f, 0, &vec![1u8; len]).unwrap();
                    }
                }
            }
        }
        dir
    }
}

struct Dirs {
    missing: PathBuf,
    existing: PathBuf,
    read: PathBuf,
    write: PathBuf,
    create: PathBuf,
    listing: PathBuf,
}

fn prep(side: &Side, root: &Path, rep: usize) -> Dirs {
    Dirs {
        missing: side.prep_dir(root, &format!("m{rep}"), 1, 0),
        existing: side.prep_dir(root, &format!("e{rep}"), ITERS, 0),
        read: side.prep_dir(root, &format!("r{rep}"), ITERS, SMALL),
        write: side.prep_dir(root, &format!("w{rep}"), ITERS, SMALL),
        create: side.prep_dir(root, &format!("c{rep}"), 1, 0),
        listing: side.prep_dir(root, &format!("l{rep}"), DIR_ENTRIES, 0),
    }
}

/// Mean microseconds per operation for each of `OPS`, for `ITERS` cold operations (readdir: one listing, in ms).
fn timed(d: &Dirs) -> [f64; 6] {
    let per = |t: Instant| t.elapsed().as_secs_f64() * 1e6 / ITERS as f64;
    let mut out = [0.0; 6];

    let t = Instant::now();
    for i in 0..ITERS {
        assert!(fs::metadata(d.missing.join(format!("nope{i}"))).is_err());
    }
    out[0] = per(t);

    let t = Instant::now();
    for i in 0..ITERS {
        fs::metadata(d.existing.join(format!("f{i}"))).unwrap();
    }
    out[1] = per(t);

    let mut buf = vec![0u8; SMALL];
    let t = Instant::now();
    for i in 0..ITERS {
        let mut f = fs::File::open(d.read.join(format!("f{i}"))).unwrap();
        f.read_exact(&mut buf).unwrap();
    }
    out[2] = per(t);

    let data = vec![2u8; SMALL];
    let t = Instant::now();
    for i in 0..ITERS {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .open(d.write.join(format!("f{i}")))
            .unwrap();
        f.write_all(&data).unwrap();
    }
    out[3] = per(t);

    let t = Instant::now();
    for i in 0..ITERS {
        fs::File::create(d.create.join(format!("n{i}"))).unwrap();
    }
    out[4] = per(t);

    let t = Instant::now();
    let n = fs::read_dir(&d.listing).unwrap().count();
    assert_eq!(n, DIR_ENTRIES);
    out[5] = t.elapsed().as_secs_f64() * 1e3;
    out
}

fn bench_options() -> MountOptions {
    MountOptions::default()
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

#[test]
#[ignore = "benchmark; mounts a filesystem and holds the shared CPU lock"]
fn latency_floor_against_native() {
    if !mount_nfs_available() {
        eprintln!("SKIP: mount_nfs is not available");
        return;
    }
    let reps: usize = std::env::var("COWFS_BENCH_REPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let native_dir = tempfile::Builder::new()
        .prefix("cowfs-bench-native-")
        .tempdir()
        .unwrap();
    let mnt_dir = tempfile::Builder::new()
        .prefix("cowfs-bench-nfs-")
        .tempdir()
        .unwrap();
    let vfs = Arc::new(MemVfs::new());
    let mount = match Mount::new(vfs.clone(), &mnt_dir.path().join("mnt"), bench_options()) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("SKIP: cannot mount: {e}");
            return;
        }
    };

    let load_before = load1();
    let mut nfs: Vec<Vec<f64>> = vec![Vec::new(); 6];
    let mut native: Vec<Vec<f64>> = vec![Vec::new(); 6];
    let mut loads = Vec::new();
    for rep in 0..reps {
        let nfs_dirs = prep(&Side::Nfs(&vfs), mount.mountpoint(), rep);
        let native_dirs = prep(&Side::Native, native_dir.path(), rep);
        let lock = CpuLock::acquire();
        loads.push(load1());
        let a = timed(&nfs_dirs);
        let b = timed(&native_dirs);
        drop(lock);
        for i in 0..6 {
            nfs[i].push(a[i]);
            native[i].push(b[i]);
        }
    }
    let load_after = load1();
    mount.unmount().unwrap();

    let worst = loads.iter().copied().fold(0.0, f64::max);
    println!("\nn={reps} repetitions, {ITERS} cold operations each (readdir: one listing of {DIR_ENTRIES})");
    println!(
        "load1 before {load_before:.1}, at each timed batch {loads:.1?}, after {load_after:.1}"
    );
    if worst > 30.0 || load_before > 30.0 || load_after > 30.0 {
        println!("FLAG: load1 > 30 during this run, treat the numbers as inflated and noisy");
    }
    println!("\n| operation | NFS median | NFS min-max | native median | native min-max | ratio |");
    println!("|---|---|---|---|---|---|");
    for (i, op) in OPS.iter().enumerate() {
        let unit = if i == 5 { "ms" } else { "us" };
        let (mut a, mut b) = (nfs[i].clone(), native[i].clone());
        let (ma, mb) = (median(&mut a), median(&mut b));
        println!(
            "| {op} | {ma:.1} {unit} | {:.1}-{:.1} | {mb:.1} {unit} | {:.1}-{:.1} | {:.1}x |",
            a[0],
            a[a.len() - 1],
            b[0],
            b[b.len() - 1],
            ma / mb
        );
    }
    assert!(reps >= 1);
}
