//! Concurrency stress: overlapping writers to one file, many files in parallel, snapshot forks
//! while writes are in flight, and renames. Every test runs under a watchdog that turns a
//! deadlock into a failure.

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::{fixture_with, pattern, read_all, root_entry, Rng};
use cowfs_core::Options;
use cowfs_vfs::{FileKind, Ino, RenameFlags, Vfs, ROOT_INO};

fn opts() -> Options {
    Options {
        background: true,
        flush_interval: Duration::from_millis(20),
        sync_interval: Duration::from_millis(50),
        max_pending_ops: 64,
        file_flush_bytes: 256 << 10,
        ..Options::default()
    }
}

fn watchdog(limit: Duration, f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = mpsc::channel();
    let h = std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    if rx.recv_timeout(limit).is_err() {
        if h.is_finished() {
            h.join().expect("worker panicked");
            return;
        }
        panic!("no progress after {limit:?}: deadlock");
    }
    h.join().expect("worker panicked");
}

fn walk(fs: &dyn Vfs, dir: Ino, prefix: &str, out: &mut BTreeMap<String, Vec<u8>>) {
    let mut cookie = 0;
    loop {
        let r = fs.readdir(dir, cookie, 100).unwrap();
        for e in &r.entries {
            let name = format!("{prefix}{}", String::from_utf8_lossy(&e.name));
            match e.kind {
                FileKind::Directory => walk(fs, e.ino, &format!("{name}/"), out),
                _ => {
                    out.insert(name, read_all(fs, e.ino));
                }
            }
        }
        if r.eof {
            return;
        }
        cookie = r.entries.last().unwrap().cookie;
    }
}

#[test]
fn overlapping_writers_to_one_file_never_tear_a_block() {
    watchdog(Duration::from_secs(300), || {
        const BLOCKS: u64 = 512;
        const WRITERS: u64 = 8;
        let f = fixture_with(opts());
        let c = f.core.clone();
        c.create_snapshot("s").unwrap();
        let r = root_entry(&c, "s").ino;
        let ino = c.create(r, b"shared", 0o644).unwrap().ino;
        let stop = AtomicBool::new(false);
        let torn = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for t in 0..WRITERS {
                let c = &c;
                s.spawn(move || {
                    let mut rng = Rng(t * 7919 + 13);
                    for i in 0..400u64 {
                        let b = rng.below(BLOCKS - 4);
                        let n = 1 + rng.below(4);
                        let tag = (t * 31 + i % 29) as u8 | 1;
                        c.write(ino, b * 4096, &vec![tag; (n * 4096) as usize])
                            .unwrap();
                        if i % 97 == 0 {
                            c.fsync(ino, false).unwrap();
                        }
                    }
                });
            }
            for _ in 0..3 {
                let (c, stop, torn) = (&c, &stop, &torn);
                s.spawn(move || {
                    let mut rng = Rng(99);
                    while !stop.load(Ordering::Acquire) {
                        let b = rng.below(BLOCKS);
                        let got = c.read(ino, b * 4096, 4096).unwrap();
                        if got.len() == 4096 && got.iter().any(|x| *x != got[0]) {
                            torn.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
            let end = Instant::now() + Duration::from_secs(1);
            while Instant::now() < end {
                std::thread::sleep(Duration::from_millis(50));
            }
            stop.store(true, Ordering::Release);
        });
        assert_eq!(
            torn.load(Ordering::Relaxed),
            0,
            "a read saw a torn 4 KiB block"
        );
        let data = read_all(&c, ino);
        for (i, blk) in data.chunks(4096).enumerate() {
            assert!(
                blk.iter().all(|x| *x == blk[0]),
                "block {i} is torn after the writers finished"
            );
        }
        c.sync().unwrap();
        c.drop_caches();
        assert_eq!(
            read_all(&c, ino),
            data,
            "content changed across flush and reload"
        );
        c.check().unwrap();
        assert!(c.fsck().unwrap().is_clean());
    });
}

#[test]
fn many_files_in_parallel_with_snapshot_forks_and_renames() {
    watchdog(Duration::from_secs(600), || {
        const THREADS: u64 = 6;
        const FILES: u64 = 150;
        let f = fixture_with(opts());
        let c = f.core.clone();
        c.create_snapshot("main").unwrap();
        let r = root_entry(&c, "main").ino;
        let dirs: Vec<Ino> = (0..THREADS)
            .map(|t| c.mkdir(r, format!("t{t}").as_bytes(), 0o755).unwrap().ino)
            .collect();
        let done = AtomicUsize::new(0);
        let forks = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for t in 0..THREADS {
                let (c, done) = (&c, &done);
                let dir = dirs[t as usize];
                s.spawn(move || {
                    for i in 0..FILES {
                        let len = (i * 997 % 70_000) as usize;
                        let a = c.create(dir, format!("f{i}").as_bytes(), 0o644).unwrap();
                        c.write(a.ino, 0, &pattern(len, t * 1000 + i)).unwrap();
                        if i % 10 == 3 {
                            c.rename(
                                dir,
                                format!("f{i}").as_bytes(),
                                dir,
                                format!("g{i}").as_bytes(),
                                RenameFlags::default(),
                            )
                            .unwrap();
                        }
                        if i % 25 == 7 {
                            c.unlink(dir, format!("f{}", i - 4).as_bytes()).ok();
                        }
                        if i % 40 == 0 {
                            c.fsync(a.ino, false).unwrap();
                        }
                        c.forget(a.ino, 1);
                    }
                    done.fetch_add(1, Ordering::Release);
                });
            }
            let (c, done, forks) = (&c, &done, &forks);
            s.spawn(move || {
                let mut n = 0;
                while done.load(Ordering::Acquire) < THREADS as usize {
                    let name = format!("fork{n}");
                    c.fork_snapshot("main", &name).unwrap();
                    forks.fetch_add(1, Ordering::Relaxed);
                    n += 1;
                    if n > 4 {
                        c.remove_snapshot(&format!("fork{}", n - 5)).unwrap();
                    }
                    std::thread::sleep(Duration::from_millis(15));
                }
            });
        });
        assert!(
            forks.load(Ordering::Relaxed) >= 2,
            "no forks happened during the writes"
        );
        c.sync().unwrap();
        for e in c.list_snapshots().unwrap() {
            let fs = c.snapshot_view(&e.name).unwrap();
            let mut got = BTreeMap::new();
            walk(&fs, ROOT_INO, "", &mut got);
            for (path, data) in &got {
                let (dir, name) = path.split_once('/').unwrap();
                let t: u64 = dir[1..].parse().unwrap();
                let i: u64 = name[1..].parse().unwrap();
                let want = pattern((i * 997 % 70_000) as usize, t * 1000 + i);
                assert!(
                    data.is_empty() || *data == want,
                    "{}/{path}: neither empty nor the written content ({} bytes)",
                    e.name,
                    data.len()
                );
                if e.name == "main" {
                    assert!(*data == want, "main/{path} lost its data");
                }
            }
        }
        c.check().unwrap();
        assert!(c.fsck().unwrap().is_clean());
        let s = c.stats();
        assert_eq!(s.forget_underflows, 0);
    });
}

#[test]
fn concurrent_directory_churn_keeps_the_tree_consistent() {
    watchdog(Duration::from_secs(300), || {
        let f = fixture_with(opts());
        let c = f.core.clone();
        c.create_snapshot("s").unwrap();
        let r = root_entry(&c, "s").ino;
        let d = c.mkdir(r, b"d", 0o755).unwrap().ino;
        std::thread::scope(|s| {
            for t in 0..6u64 {
                let c = &c;
                s.spawn(move || {
                    let mut rng = Rng(t + 5);
                    for i in 0..600u64 {
                        let name = format!("n{}", rng.below(24));
                        let other = format!("n{}", rng.below(24));
                        match rng.below(5) {
                            0 => {
                                if let Ok(a) = c.create(d, name.as_bytes(), 0o644) {
                                    let _ = c.write(a.ino, 0, &pattern(100 + i as usize, i));
                                    c.forget(a.ino, 1);
                                }
                            }
                            1 => {
                                let _ = c.unlink(d, name.as_bytes());
                            }
                            2 => {
                                let _ = c.rename(
                                    d,
                                    name.as_bytes(),
                                    d,
                                    other.as_bytes(),
                                    RenameFlags::default(),
                                );
                            }
                            3 => {
                                if let Ok(a) = c.lookup(d, name.as_bytes()) {
                                    let _ = c
                                        .link(a.ino, d, other.as_bytes())
                                        .map(|b| c.forget(b.ino, 1));
                                    c.forget(a.ino, 1);
                                }
                            }
                            _ => {
                                let _ = c.readdir(d, 0, 100);
                            }
                        }
                    }
                });
            }
        });
        c.check().unwrap();
        let mut got = BTreeMap::new();
        walk(&c.snapshot_view("s").unwrap(), ROOT_INO, "", &mut got);
        assert!(c.fsck().unwrap().is_clean());
    });
}
