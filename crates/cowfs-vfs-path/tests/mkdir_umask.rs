//! The private umask `mkdir` and `mknod` use never reaches the process or another thread.
//!
//! `cargo test -p cowfs-vfs-path --test mkdir_umask`
//! Cost of one mkdir: `... -- --ignored --nocapture mkdir_cost`

#![cfg(target_os = "linux")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use cowfs_vfs::{Vfs, ROOT_INO};
use cowfs_vfs_path::PathVfs;

/// Every `Umask:` line of `/proc/self` and of each of its threads, as written in the kernel.
fn umasks() -> Vec<String> {
    let mut out = Vec::new();
    let mut paths = vec!["/proc/self/status".to_string()];
    if let Ok(rd) = std::fs::read_dir("/proc/self/task") {
        paths.extend(
            rd.flatten()
                .map(|e| format!("{}/status", e.path().display())),
        );
    }
    for p in paths {
        if let Ok(text) = std::fs::read_to_string(p) {
            out.extend(
                text.lines()
                    .filter_map(|l| l.strip_prefix("Umask:"))
                    .map(|v| v.trim().to_string()),
            );
        }
    }
    out
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("cowfs-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// While many directories and nodes are made, a poller reads the umask of the process and of
/// every thread. Only a thread that did `unshare(CLONE_FS)` may show 0000; the process (first
/// entry) and every thread sharing it must keep the value in force at the start.
#[test]
fn the_process_umask_never_leaks_during_mkdir() {
    let dir = scratch("umask-leak");
    let fs = PathVfs::new(&dir).unwrap();
    let before = umasks()[0].clone();
    let stop = Arc::new(AtomicBool::new(false));
    let poller = {
        let (stop, before) = (stop.clone(), before.clone());
        std::thread::spawn(move || {
            let (mut samples, mut bad) = (0u32, Vec::new());
            while !stop.load(Ordering::Relaxed) {
                let all = umasks();
                samples += 1;
                if all[0] != before || all.iter().any(|u| *u != before && u != "0000") {
                    bad.push(all);
                }
            }
            (samples, bad)
        })
    };
    for i in 0..3000u32 {
        let name = format!("d{i}");
        fs.mkdir(ROOT_INO, name.as_bytes(), 0o755).unwrap();
        fs.mknod(
            ROOT_INO,
            format!("f{i}").as_bytes(),
            cowfs_vfs::FileKind::Fifo,
            0o640,
            0,
        )
        .unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    let (samples, bad) = poller.join().unwrap();
    assert!(samples > 10, "the poller barely ran: {samples}");
    assert!(bad.is_empty(), "process umask changed: {:?}", bad.first());
    assert_eq!(umasks()[0], before);
    drop(fs);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Not a pass/fail test: prints the cost of one `mkdir` + `rmdir` pair through `PathVfs`.
#[test]
#[ignore = "measurement: cargo test --release -p cowfs-vfs-path --test mkdir_umask -- --ignored --nocapture"]
fn mkdir_cost() {
    let dir = scratch("mkdir-cost");
    let fs = PathVfs::new(&dir).unwrap();
    let n = 20_000u32;
    let t = Instant::now();
    for _ in 0..n {
        let a = fs.mkdir(ROOT_INO, b"d", 0o755).unwrap();
        fs.rmdir(ROOT_INO, b"d").unwrap();
        fs.forget(a.ino, 1);
    }
    let us = t.elapsed().as_secs_f64() * 1e6 / f64::from(n);
    println!("MKDIR_RMDIR_US {us:.2}");
    drop(fs);
    std::fs::remove_dir_all(dir).unwrap();
}
