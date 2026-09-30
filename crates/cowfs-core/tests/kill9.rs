//! kill -9 test: a child process mutates a `Core` (background flusher on) and the parent SIGKILLs
//! it at random moments, reopens the directory and verifies it.
//!
//! `COWFS_KILL_ROUNDS` sets the number of rounds (default 8).

mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{pattern, Rng};
use cowfs_core::{Core, Options};
use cowfs_vfs::{FileKind, Vfs, ROOT_INO};

const FILES: u64 = 40;

fn opts() -> Options {
    Options {
        flush_interval: Duration::from_millis(30),
        sync_interval: Duration::from_millis(60),
        max_pending_ops: 16,
        file_flush_bytes: 128 << 10,
        store: cowfs_store::Options {
            max_pack_size: 600 << 10,
            ..Default::default()
        },
        meta: cowfs_meta::Options {
            node_size: 1024,
            sync_every_ops: 4,
            ..Default::default()
        },
        ..Options::default()
    }
}

fn value(n: u64) -> Vec<u8> {
    let mut r = Rng(n.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let size = match r.below(10) {
        0 => 0,
        1..=5 => r.below(5000) as usize,
        6..=8 => r.below(150_000) as usize,
        _ => 300_000 + r.below(300_000) as usize,
    };
    pattern(size, n)
}

/// Child half of the test; does nothing unless the parent spawned it.
#[test]
fn kill_child() {
    let Ok(dir) = std::env::var("COWFS_KILL_DIR") else {
        return;
    };
    let core = Core::open(&dir, opts()).unwrap();
    core.create_snapshot("s0").unwrap();
    let fs = core.snapshot_view("s0").unwrap();
    let mut out = std::io::stdout();
    writeln!(out, "R").unwrap();
    for n in 1..=1_000_000u64 {
        let name = format!("f{}", n % FILES);
        let a = match fs.lookup(ROOT_INO, name.as_bytes()) {
            Ok(a) => a,
            Err(_) => fs.create(ROOT_INO, name.as_bytes(), 0o644).unwrap(),
        };
        common::truncate(&fs, a.ino, 0).unwrap();
        common::write_all(&fs, a.ino, 0, &value(n));
        if n % 9 == 0 {
            let t = format!("t{}", n);
            let b = fs.create(ROOT_INO, t.as_bytes(), 0o600).unwrap();
            fs.forget(b.ino, 1);
            if n >= 18 {
                fs.unlink(ROOT_INO, format!("t{}", n - 9).as_bytes())
                    .unwrap();
            }
        }
        writeln!(out, "P {n}").unwrap();
        if n % 5 == 0 {
            fs.fsync(a.ino, false).unwrap();
            writeln!(out, "S {n}").unwrap();
        }
        fs.forget(a.ino, 1);
    }
}

fn round(round: u64, rng: &mut Rng) -> (u64, u64) {
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut child = Command::new(exe)
        .args(["kill_child", "--exact", "--nocapture", "--test-threads=1"])
        .env("COWFS_KILL_DIR", dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let ready_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = ready_flag.clone();
    let reader = std::thread::spawn(move || {
        let (mut p, mut s, mut ready) = (0u64, 0u64, false);
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            match line.split_once(' ') {
                Some(("P", n)) => p = n.parse().unwrap_or(p),
                Some(("S", n)) => s = n.parse().unwrap_or(s),
                _ if line.ends_with("... R") || line == "R" => {
                    ready = true;
                    flag.store(true, std::sync::atomic::Ordering::Release);
                }
                _ => {}
            }
        }
        (ready, p, s)
    });
    let wait = Instant::now();
    while !ready_flag.load(std::sync::atomic::Ordering::Acquire) {
        assert!(
            wait.elapsed() < Duration::from_secs(120) && child.try_wait().unwrap().is_none(),
            "round {round}: the child never became ready"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let start = Instant::now();
    let life = Duration::from_millis(400 + rng.below(2200));
    loop {
        if start.elapsed() > life || child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = child.kill();
    let status = child.wait().unwrap();
    let (ready, p, s) = reader.join().unwrap();
    assert!(
        !status.success() || !ready,
        "round {round}: the child was not killed"
    );
    if !ready {
        return (0, 0);
    }
    verify(dir.path(), round, p, s);
    (p, s)
}

fn verify(dir: &std::path::Path, round: u64, p: u64, s: u64) {
    let what = format!("round {round} (progress {p}, synced {s})");
    let core = Core::open(dir, opts()).unwrap_or_else(|e| panic!("{what}: reopen: {e:?}"));
    assert!(
        !core.store().recovery().has_corruption(),
        "{what}: store corruption"
    );
    core.check()
        .unwrap_or_else(|e| panic!("{what}: check: {e:?}"));
    let rep = core.fsck().unwrap();
    assert!(rep.is_clean(), "{what}: fsck {rep:?}");
    let Ok(fs) = core.snapshot_view("s0") else {
        assert_eq!(s, 0, "{what}: durable snapshot missing");
        return;
    };
    let mut got: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut cookie = 0;
    loop {
        let r = fs.readdir(ROOT_INO, cookie, 30).unwrap();
        for e in &r.entries {
            assert_eq!(e.kind, FileKind::Regular);
            let a = fs.getattr(e.ino).unwrap();
            let data = fs.read(e.ino, 0, a.size as u32).unwrap_or_else(|er| {
                panic!(
                    "{what}: {:?} unreadable: {er:?}",
                    String::from_utf8_lossy(&e.name)
                )
            });
            assert_eq!(data.len() as u64, a.size, "{what}: short read");
            got.insert(String::from_utf8_lossy(&e.name).into_owned(), data);
        }
        if r.eof {
            break;
        }
        cookie = r.entries.last().map_or(0, |e| e.cookie);
    }
    for f in 0..FILES {
        let name = format!("f{f}");
        let last_synced = (1..=s).rev().find(|m| m % FILES == f);
        let have = got.get(&name);
        let Some(first) = last_synced else {
            continue;
        };
        let have = have.unwrap_or_else(|| panic!("{what}: fsynced file {name} is missing"));
        let ok = have.is_empty()
            || (first..=p + 1)
                .filter(|m| m % FILES == f)
                .any(|m| *have == value(m));
        assert!(
            ok,
            "{what}: {name} matches no write since step {first} ({} bytes)",
            have.len()
        );
    }
}

#[test]
fn kill_minus_nine_leaves_a_consistent_store() {
    if std::env::var("COWFS_KILL_DIR").is_ok() {
        return;
    }
    let rounds: u64 = std::env::var("COWFS_KILL_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);
    let mut rng = Rng(0xC0FFEE);
    let (mut steps, mut synced, mut empty) = (0, 0, 0);
    for r in 0..rounds {
        let (p, s) = round(r, &mut rng);
        steps += p;
        synced += s;
        empty += u64::from(p == 0);
    }
    println!("{rounds} rounds, {steps} steps completed, {synced} steps fsynced, {empty} killed before the first step");
}
