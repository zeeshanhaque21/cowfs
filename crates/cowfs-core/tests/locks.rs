//! Lock discipline: a cold read of a file concurrent with a batch commit (F1) and a mixed 60 s
//! stress of every operation that takes more than one lock.
//!
//! Each test has a watchdog that fails (and dumps thread stacks) on no progress, so a deadlock is
//! a test failure rather than a hang.

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use cowfs_core::{Core, Options};
use cowfs_vfs::{FileKind, RenameFlags, SetAttr, Vfs, ROOT_INO};

/// Fails the test if `f` makes no progress for `limit`, dumping every thread's stack.
fn watchdog<T: Send + 'static>(
    limit: Duration,
    what: &str,
    f: impl FnOnce() -> T + Send + 'static,
) {
    let (done, progress) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU64::new(0)),
    );
    let (d, p) = (done.clone(), progress.clone());
    let h = std::thread::spawn(move || {
        let r = f();
        p.fetch_add(1, Ordering::SeqCst);
        drop(r);
        d.store(true, Ordering::SeqCst);
    });
    let start = Instant::now();
    let mut last = 0;
    while !done.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
        let now = progress.load(Ordering::SeqCst);
        if now == last && start.elapsed() > limit {
            let out = std::process::Command::new("sh")
                .args(["-c", "sample $PPID 2 -mayDie 2>/dev/null || true"])
                .output();
            let raw = out.map(|o| o.stdout).unwrap_or_default();
            let stacks = String::from_utf8_lossy(&raw);
            eprintln!("DEADLOCK in {what}: no progress for {limit:?}\n{stacks}");
            std::process::exit(3);
        }
        last = now;
    }
    h.join().expect("worker panicked");
}

fn seed(c: &Core, root: u64, n: usize) -> Vec<String> {
    let names: Vec<String> = (0..n).map(|i| format!("f{i}")).collect();
    for nm in &names {
        let a = c.create(root, nm.as_bytes(), 0o644).expect("create");
        c.write(a.ino, 0, &pattern(2000, 3)).expect("write");
        c.forget(a.ino, 1);
    }
    c.sync().expect("sync");
    names
}

/// F1: a first read of a not-yet-loaded file (cold cache) while another thread commits a batch
/// that touched those files' attributes.
#[test]
fn cold_read_of_a_file_concurrent_with_a_commit_never_deadlocks() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    let names = seed(&c, r, 2000);
    watchdog(Duration::from_secs(20), "cold read vs commit", move || {
        let done = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU64::new(0));
        let mut readers = Vec::new();
        for t in 0..4u64 {
            let (c, done, progress) = (c.clone(), done.clone(), progress.clone());
            let names = names.clone();
            readers.push(std::thread::spawn(move || {
                let mut rng = Rng(77 + t);
                while !done.load(Ordering::Relaxed) {
                    let nm = &names[rng.below(names.len() as u64) as usize];
                    if let Ok(a) = c.lookup(r, nm.as_bytes()) {
                        let _ = c.read(a.ino, 0, 1);
                        c.forget(a.ino, 1);
                    }
                    progress.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        for round in 0..40 {
            c.drop_caches();
            for nm in &names {
                if let Ok(a) = c.lookup(r, nm.as_bytes()) {
                    let _ = c.setattr(
                        a.ino,
                        SetAttr {
                            mode: Some(0o600 + (round % 2) as u32),
                            ..SetAttr::default()
                        },
                    );
                    c.forget(a.ino, 1);
                }
            }
            c.flush().expect("flush");
            progress.fetch_add(names.len() as u64, Ordering::Relaxed);
        }
        done.store(true, Ordering::Relaxed);
        for h in readers {
            h.join().expect("reader panicked");
        }
    });
}

/// Every operation that takes more than one lock, run concurrently for `secs` seconds.
#[test]
fn mixed_lock_stress_for_a_minute_never_deadlocks() {
    let secs: u64 = std::env::var("LOCK_STRESS_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(
        dir.path(),
        Options {
            background: true,
            flush_interval: Duration::from_millis(20),
            sync_interval: Duration::from_millis(50),
            max_pending_ops: 32,
            file_flush_bytes: 128 << 10,
            ..Options::default()
        },
    )
    .unwrap();
    c.create_snapshot("s").unwrap();
    c.create_snapshot("churn").unwrap();
    let r = root_entry(&c, "s").ino;
    let d = c.mkdir(r, b"d", 0o755).unwrap().ino;
    let names = seed(&c, r, 400);
    for i in 0..50 {
        let a = c.create(d, format!("x{i}").as_bytes(), 0o644).unwrap();
        c.write(a.ino, 0, &pattern(9000, i as u64)).unwrap();
        c.forget(a.ino, 1);
    }
    c.sync().expect("sync");
    let c2 = c.clone();
    watchdog(
        Duration::from_secs(secs + 30),
        "mixed lock stress",
        move || {
            let stop = Arc::new(AtomicBool::new(false));
            let progress = Arc::new(AtomicU64::new(0));
            let mut hs = Vec::new();
            for t in 0..8u64 {
                let (c, stop, progress) = (c2.clone(), stop.clone(), progress.clone());
                let names = names.clone();
                hs.push(std::thread::spawn(move || {
                    let mut rng = Rng(0xABCD + t);
                    let mut i = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        let nm = &names[rng.below(names.len() as u64) as usize];
                        let what = rng.below(10);
                        if what < 4 {
                            if let Ok(a) = c.lookup(r, nm.as_bytes()) {
                                let _ = c.read(a.ino, rng.below(1500), 512);
                                c.forget(a.ino, 1);
                            }
                        } else if what < 5 {
                            c.drop_caches();
                        } else if what < 6 {
                            let _ = c.readdir(d, 0, 200);
                        } else if what < 7 {
                            if let Ok(a) = c.create(d, format!("y{i}").as_bytes(), 0o644) {
                                let _ = c.write(a.ino, 0, &pattern(2000 + i as usize, i));
                                c.forget(a.ino, 1);
                            }
                        } else if what < 8 {
                            let _ = c.unlink(d, format!("y{}", rng.below(i.max(1))).as_bytes());
                        } else if what < 9 {
                            let _ = c.setattr(
                                r,
                                SetAttr {
                                    mtime: None,
                                    mode: None,
                                    ..SetAttr::default()
                                },
                            );
                        } else {
                            let _ = c.rmdir(r, nm.as_bytes());
                        }
                        i += 1;
                        progress.fetch_add(1, Ordering::Relaxed);
                    }
                }));
            }
            {
                let (c, stop, progress) = (c2.clone(), stop.clone(), progress.clone());
                hs.push(std::thread::spawn(move || {
                    let mut i = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        let _ = c.fork_snapshot("s", &format!("f{i}"));
                        let _ = c.remove_snapshot(&format!("f{}", i.wrapping_sub(1)));
                        let _ = c.merkle_root("s");
                        let _ = c.rename_snapshot("s", "moving");
                        let _ = c.rename_snapshot("moving", "s");
                        i += 1;
                        progress.fetch_add(1, Ordering::Relaxed);
                    }
                }));
            }
            {
                let (c, stop, progress) = (c2.clone(), stop.clone(), progress.clone());
                hs.push(std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        c.sync().expect("sync");
                        std::thread::sleep(Duration::from_millis(30));
                        progress.fetch_add(1, Ordering::Relaxed);
                    }
                }));
            }
            {
                let (c, stop, progress) = (c2.clone(), stop.clone(), progress.clone());
                hs.push(std::thread::spawn(move || {
                    let mut i = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        let _ = c.mkdir(r, format!("m{i}").as_bytes(), 0o755);
                        let _ = c.rename(
                            r,
                            format!("m{i}").as_bytes(),
                            d,
                            format!("m{i}").as_bytes(),
                            RenameFlags::default(),
                        );
                        let _ = c.rename(
                            d,
                            format!("m{i}").as_bytes(),
                            r,
                            format!("m{i}").as_bytes(),
                            RenameFlags::default(),
                        );
                        let _ = c.rmdir(r, format!("m{i}").as_bytes());
                        i += 1;
                        progress.fetch_add(1, Ordering::Relaxed);
                    }
                }));
            }
            std::thread::sleep(Duration::from_secs(secs));
            stop.store(true, Ordering::Relaxed);
            for h in hs {
                h.join().expect("worker panicked");
            }
            c2.sync().expect("sync");
        },
    );
    let mut got = BTreeMap::new();
    walk(&c.snapshot_view("s").unwrap(), ROOT_INO, "", &mut got);
    c.check().expect("check");
    assert!(c.fsck().expect("fsck").is_clean());
    assert!(got.len() >= 400, "{}", got.len());
}

fn walk(fs: &dyn Vfs, dir: u64, prefix: &str, out: &mut BTreeMap<String, Vec<u8>>) {
    let mut cookie = 0;
    loop {
        let r = fs.readdir(dir, cookie, 100).expect("readdir");
        for e in &r.entries {
            let name = format!("{prefix}{}", String::from_utf8_lossy(&e.name));
            if e.kind == FileKind::Directory {
                walk(fs, e.ino, &format!("{name}/"), out);
            } else {
                out.insert(name, read_all(fs, e.ino));
            }
        }
        if r.eof {
            return;
        }
        cookie = r.entries.last().expect("entry").cookie;
    }
}
