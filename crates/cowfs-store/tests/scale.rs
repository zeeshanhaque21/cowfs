mod common;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::thread;
use std::time::{Duration, Instant};

use common::{index_bytes, install_wm, opts, random, record, PACK_HEADER};
use cowfs_store::{BlockId, Error, Store};

const LOCK_ENV: &str = "COWFS_LOCK_CHILD_DIR";

fn million_id_store(dir: &std::path::Path) {
    let d = random(1, 300);
    let rec = record(0, 300, *BlockId::of(&d).as_bytes(), &d);
    let mut pack = PACK_HEADER.to_vec();
    pack.extend_from_slice(&rec);
    let entries: Vec<_> = (0..1_000_000u64)
        .map(|i| {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&i.to_le_bytes());
            id[8..16].copy_from_slice(&(i.wrapping_mul(0x9E37_79B9_7F4A_7C15)).to_le_bytes());
            (
                BlockId::from_bytes(id),
                [0, PACK_HEADER.len() as u32, 300, 300],
            )
        })
        .collect();
    let ix = index_bytes(&[(0, pack.len() as u64)], &entries);
    install_wm(dir, &[(0, &pack)], Some(&ix), Some((0, pack.len() as u64)));
}

#[test]
fn puts_keep_flowing_during_a_checkpoint_of_a_million_ids() {
    let dir = tempfile::tempdir().unwrap();
    million_id_store(dir.path());
    let s = Store::open(dir.path(), opts()).unwrap();
    assert!(s.recovery().index_loaded);
    assert_eq!(s.stats().blocks, 1_000_000);
    for round in 0..3u64 {
        let done = AtomicBool::new(false);
        let (puts, worst) = thread::scope(|sc| {
            let cp = sc.spawn(|| {
                let t = Instant::now();
                s.checkpoint().unwrap();
                done.store(true, Relaxed);
                t.elapsed()
            });
            let mut n = 0u64;
            let mut worst = Duration::ZERO;
            while !done.load(Relaxed) {
                let t = Instant::now();
                s.put(&random(round * 1_000_000 + n, 200)).unwrap();
                worst = worst.max(t.elapsed());
                n += 1;
            }
            let cp_time = cp.join().unwrap();
            println!("round {round}: checkpoint {cp_time:?}, {n} puts, worst put {worst:?}");
            (n, worst)
        });
        assert!(puts > 10, "puts made no progress during checkpoint");
        assert!(worst < Duration::from_millis(400), "put stalled {worst:?}");
    }
    let t = Instant::now();
    for _ in 0..1000 {
        let _ = s.stats();
    }
    assert!(t.elapsed() < Duration::from_millis(50), "stats is not O(1)");
}

#[test]
fn lock_is_held_across_processes_and_released_by_kill_9() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lock_child", "--ignored", "--nocapture"])
        .env(LOCK_ENV, dir.path())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    loop {
        let l = lines.next().expect("child exited early").unwrap();
        if l.trim() == "LOCKED" {
            break;
        }
    }
    assert!(matches!(
        Store::open(dir.path(), opts()),
        Err(Error::Locked { .. })
    ));
    child.kill().unwrap();
    child.wait().unwrap();
    let s = Store::open(dir.path(), opts()).unwrap();
    assert!(s.contains(BlockId::of(b"from child")));
}

#[test]
#[ignore]
fn lock_child() {
    let Some(dir) = std::env::var_os(LOCK_ENV) else {
        return;
    };
    let s = Store::open(&dir, opts()).unwrap();
    s.put(b"from child").unwrap();
    s.sync().unwrap();
    println!("LOCKED");
    thread::sleep(Duration::from_secs(120));
}
