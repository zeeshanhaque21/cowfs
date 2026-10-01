//! kill -9 test: a child process mutates the tree in a loop, the parent SIGKILLs it at random
//! moments, reopens the database and compares against a replay of the same workload.
//!
//! `COWFS_KILL_ROUNDS` overrides the number of rounds (default 100).

mod common;

use common::{digest, step, Rng, WState};
use cowfs_meta::{Meta, Options, ROOT_INO};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn opts() -> Options {
    Options {
        node_size: 512,
        sync_every_ops: 3,
        sync_interval: Duration::from_secs(3600),
        ..Options::default()
    }
}

/// Child half of the test; does nothing unless the parent spawned it.
#[test]
fn kill_child() {
    let (Ok(path), Ok(seed)) = (
        std::env::var("COWFS_KILL_DB"),
        std::env::var("COWFS_KILL_SEED"),
    ) else {
        return;
    };
    let seed: u64 = seed.parse().unwrap();
    let m = Meta::open(path, opts()).unwrap();
    m.new_snapshot("s0").unwrap();
    let mut out = std::io::stdout();
    writeln!(out, "P 0\nS 0").unwrap();
    let (mut rng, mut st) = (Rng(seed), WState::new());
    for n in 1..=1_000_000usize {
        step(&m, &mut rng, &mut st);
        writeln!(out, "P {n}").unwrap();
        if n % 5 == 0 {
            m.sync().unwrap();
            writeln!(out, "S {n}").unwrap();
        }
    }
}

/// Steps finished and steps known durable, once the child has created its snapshot.
type Progress = Arc<Mutex<Option<(usize, usize)>>>;

fn replay_digests(seed: u64, done: usize, synced: usize) -> Vec<[u8; 32]> {
    let dir = tempfile::tempdir().unwrap();
    let m = Meta::open(dir.path().join("replay.redb"), Options::default()).unwrap();
    m.new_snapshot("s0").unwrap();
    let mut out = Vec::new();
    if synced == 0 {
        out.push(digest(&m));
    }
    let (mut rng, mut st) = (Rng(seed), WState::new());
    for k in 1..=done + 1 {
        step(&m, &mut rng, &mut st);
        if k >= synced {
            out.push(digest(&m));
        }
    }
    out
}

#[test]
fn kill_minus_nine_leaves_a_consistent_tree() {
    let rounds: usize = std::env::var("COWFS_KILL_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(100);
    let exe = std::env::current_exe().unwrap();
    let mut rng = Rng(0xC0FFEE);
    let mut reached = Vec::new();
    for round in 0..rounds {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kill.redb");
        let seed = round as u64 + 1;
        let mut child = Command::new(&exe)
            .args([
                "--exact",
                "kill_child",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env("COWFS_KILL_DB", &path)
            .env("COWFS_KILL_SEED", seed.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let progress: Progress = Arc::default();
        let stdout = child.stdout.take().unwrap();
        let p2 = progress.clone();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let mut it = line.split(' ');
                let (Some(tag), Some(n)) = (it.next(), it.next().and_then(|n| n.parse().ok()))
                else {
                    continue;
                };
                if tag != "P" && tag != "S" {
                    continue;
                }
                let mut g = p2.lock().unwrap();
                let cur = g.get_or_insert((0, 0));
                if tag == "P" {
                    cur.0 = n;
                } else {
                    cur.1 = n;
                }
            }
        });
        let start = Instant::now();
        while progress.lock().unwrap().is_none() {
            assert!(
                start.elapsed() < Duration::from_secs(60) && child.try_wait().unwrap().is_none(),
                "child did not start"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(rng.below(120) as u64));
        child.kill().unwrap();
        child.wait().unwrap();
        reader.join().unwrap();
        let (done, synced) = progress.lock().unwrap().unwrap();
        reached.push(done);

        let m = Meta::open(&path, opts())
            .unwrap_or_else(|e| panic!("round {round}: open failed after kill: {e}"));
        m.check()
            .unwrap_or_else(|e| panic!("round {round}: check failed after kill: {e}"));
        let got = digest(&m);
        assert!(
            replay_digests(seed, done, synced).contains(&got),
            "round {round}: reopened state matches no step in {synced}..={} of the workload",
            done + 1
        );
        let s = m.snapshots().unwrap().remove(0);
        m.snapshot(&s.name)
            .unwrap()
            .create(ROOT_INO, b"after-kill", 0o644)
            .ok();
        m.check().unwrap();
    }
    reached.sort_unstable();
    eprintln!(
        "kill -9 rounds {rounds}, steps finished before the kill: min {} median {} max {}",
        reached[0],
        reached[reached.len() / 2],
        reached[reached.len() - 1]
    );
}
