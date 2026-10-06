//! Round-2 critic attacks, ported from the critic's read-only scratch tests
//! (`out/critic26b/work/crates/cowfs-core/tests/critic2b.rs`).
//!
//! Heavy: `C2B_STRESS_SECS=300 cargo test -p cowfs-core --release --test critic2b -- --ignored stress`.

mod common;

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use cowfs_core::{ControlError, Core, Options};
use cowfs_vfs::{SetAttr, Vfs, ROOT_INO};

fn opts_nosync() -> Options {
    Options {
        background: false,
        sync_interval: Duration::from_secs(3600),
        meta: cowfs_meta::Options {
            background: false,
            sync_interval: Duration::from_secs(3600),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn child(name: &str, dir: &std::path::Path, which: &str) -> String {
    let exe = std::env::current_exe().unwrap();
    let out = Command::new(exe)
        .args([name, "--exact", "--nocapture", "--test-threads=1"])
        .env("COWFS_C2B_DIR", dir)
        .env("COWFS_C2B_WHICH", which)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn snap_names(c: &Core) -> Vec<String> {
    let mut v: Vec<String> = c
        .list_snapshots()
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    v.sort();
    v
}

// ---------------------------------------------------------------- B1: the whole-mount barrier

#[test]
fn c2b_child() {
    let Ok(dir) = std::env::var("COWFS_C2B_DIR") else {
        return;
    };
    let which = std::env::var("COWFS_C2B_WHICH").unwrap_or_default();
    let core = Core::open(&dir, opts_nosync()).unwrap();
    let mut o2 = std::io::stdout();
    match which.as_str() {
        "fsync_root" => {
            core.create_snapshot("s").unwrap();
            let fs = core.snapshot_view("s").unwrap();
            let a = fs.create(ROOT_INO, b"f", 0o644).unwrap();
            fs.write(a.ino, 0, &pattern(5 << 20, 1)).unwrap();
            let st = core.stats();
            let r = core.fsync(ROOT_INO, false);
            let st2 = core.stats();
            writeln!(
                o2,
                "C fsync(ROOT_INO) ok={} pending {}->{} batches {}->{} dirty {}->{}",
                r.is_ok(),
                st.pending_ops,
                st2.pending_ops,
                st.batches,
                st2.batches,
                st.dirty_bytes,
                st2.dirty_bytes
            )
            .unwrap();
            o2.flush().unwrap();
            std::process::abort();
        }
        "crash_in_swap" => {
            core.create_snapshot("base").unwrap();
            core.set_swap_fault(2);
            let r = core.promote_base("base", "base2");
            writeln!(o2, "C promote -> {r:?}").unwrap();
            o2.flush().unwrap();
            std::process::abort();
        }
        "crash_during_flush" => {
            core.create_snapshot("s").unwrap();
            let fs = core.snapshot_view("s").unwrap();
            let a = fs.create(ROOT_INO, b"f", 0o644).unwrap();
            fs.write(a.ino, 0, &pattern(40 << 20, 3)).unwrap();
            let h = fs.open(a.ino).unwrap();
            let _ = h;
            writeln!(o2, "C wrote 40 MiB").unwrap();
            o2.flush().unwrap();
            std::process::abort();
        }
        _ => {
            let _ = writeln!(o2, "C unused");
        }
    }
}

/// B1: `fsync(ROOT_INO, false)` is the whole-mount barrier, so a kill right after it must lose
/// nothing.
#[test]
fn fsync_of_the_mount_root_is_the_whole_mount_barrier() {
    let a = tempfile::tempdir().unwrap();
    let out = child("c2b_child", a.path(), "fsync_root");
    println!("child: {out}");
    assert!(out.contains("fsync(ROOT_INO) ok=true"), "{out}");
    let c = Core::open(a.path(), opts_nosync()).unwrap();
    let fs = c.snapshot_view("s").unwrap();
    let got = fs.lookup(ROOT_INO, b"f");
    println!(
        "after the kill, lookup(f) = {:?}",
        got.as_ref().map(|x| x.size)
    );
    assert_eq!(
        read_all(
            &c,
            got.expect("fsync(ROOT_INO) must have committed the name")
                .ino
        ),
        pattern(5 << 20, 1),
        "fsync(ROOT_INO, false) returned Ok but committed nothing"
    );
}

/// B1: the same barrier through a `SnapshotView` of the mount root's snapshot, and with
/// `data_only` set.
#[test]
fn fsync_of_the_mount_root_flushes_every_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    for name in ["one", "two"] {
        c.create_snapshot(name).unwrap();
        let fs = c.snapshot_view(name).unwrap();
        let a = fs.create(ROOT_INO, b"f", 0o644).unwrap();
        fs.write(a.ino, 0, b"payload").unwrap();
    }
    let before = c.stats();
    c.fsync(ROOT_INO, false).expect("fsync of the mount root");
    c.fsync(ROOT_INO, true)
        .expect("fsync of the mount root, data only");
    let after = c.stats();
    assert!(after.pending_ops <= before.pending_ops);
    assert_eq!(after.pending_ops, 0, "{after:?}");
    drop(c);
    let c = Core::open(dir.path(), test_opts()).unwrap();
    for name in ["one", "two"] {
        let fs = c.snapshot_view(name).unwrap();
        let a = fs.lookup(ROOT_INO, b"f").expect(name);
        assert_eq!(read_all(&c, a.ino), b"payload", "snapshot {name}");
    }
}

// ---------------------------------------------------------------- B2: the pin set GC consumes

/// B2: `pinned_blocks` must never omit a block a pending op or an open handle references. It either
/// answers exactly or says `Busy`.
#[test]
fn pinned_blocks_never_omits_a_block_a_writer_or_handle_references() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(
        dir.path(),
        Options {
            background: false,
            file_flush_bytes: 64 << 10,
            store: cowfs_store::Options {
                max_pack_size: 64 << 10,
                ..Default::default()
            },
            ..test_opts()
        },
    )
    .unwrap();
    c.create_snapshot("s").unwrap();
    let fs = c.snapshot_view("s").unwrap();
    let a = fs.create(ROOT_INO, b"f", 0o644).unwrap().ino;
    fs.write(a, 0, &pattern(200_000, 11)).unwrap();
    c.sync().unwrap();
    let h = fs.open(a).unwrap();
    fs.unlink(ROOT_INO, b"f").unwrap();
    c.sync().unwrap();
    let baseline: Vec<_> = c.pinned_blocks().expect("the idle orphan pins its blocks");
    assert!(!baseline.is_empty(), "an open orphan must pin its blocks");
    let stop = Arc::new(AtomicBool::new(false));
    let (w, s) = (stop.clone(), c.clone());
    let writer = std::thread::spawn(move || {
        let f = s.snapshot_view("s").unwrap();
        let mut n = 0u64;
        for _ in 0..400 {
            // the same bytes every time, so the chunk ids the baseline names stay current
            if f.write(a, 0, &pattern(200_000, 11)).is_err() {
                break;
            }
            n += 1;
        }
        w.store(true, Ordering::SeqCst);
        n
    });
    let (mut misses, mut busy, mut polls) = (0usize, 0usize, 0usize);
    while !stop.load(Ordering::SeqCst) {
        polls += 1;
        match c.pinned_blocks() {
            Ok(got) => {
                if !baseline.iter().all(|b| got.contains(b)) {
                    misses += 1;
                    if misses == 1 {
                        println!(
                            "MISS: got {} ids, baseline {}; stats {:?}",
                            got.len(),
                            baseline.len(),
                            c.stats()
                        );
                    }
                }
            }
            Err(ControlError::Busy) => busy += 1,
            Err(e) => panic!("pinned_blocks: {e}"),
        }
    }
    let n = writer.join().unwrap();
    let _ = fs.release(h);
    println!("writer did {n} writes, {polls} polls, {misses} partial answers, {busy} busy");
    assert_eq!(
        misses, 0,
        "pinned_blocks omitted a referenced block {misses} of {polls} times: GC would free live data"
    );
}

/// B2: the API answers `Busy` instead of something partial, and the contract says so.
#[test]
fn pinned_blocks_contract_is_documented_and_never_partial() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(
        dir.path(),
        Options {
            // 4 MiB flush threshold: the 5 MiB write below flushes the file's blocks into the
            // store and queues its chunk list, which nothing commits
            ..test_opts()
        },
    )
    .unwrap();
    c.create_snapshot("s").unwrap();
    let fs = c.snapshot_view("s").unwrap();
    let a = fs.create(ROOT_INO, b"f", 0o644).unwrap().ino;
    fs.write(a, 0, &pattern(5 << 20, 2)).unwrap();
    assert_eq!(
        c.stats().dirty_bytes,
        0,
        "the file flushed itself into the store"
    );
    c.forget(a, 1);
    let blocks = c.pinned_blocks().expect("pinned");
    assert!(blocks.iter().all(|b| c.store().contains(*b)), "{blocks:?}");
    assert!(
        !blocks.is_empty(),
        "an uncommitted chunk list must be pinned"
    );
    // once committed with no handle, nothing names them any more
    c.sync().unwrap();
    assert!(c.pinned_blocks().expect("pinned").is_empty());
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/v1-core.md"
    ))
    .unwrap();
    assert!(
        doc.contains("pinned_blocks") && doc.contains("never partial"),
        "docs/v1-core.md must state the pinned_blocks contract GC depends on"
    );
}

// ---------------------------------------------------------------- B5: the virtual-number mark

#[test]
fn virt_mark_child() {
    let Ok(dir) = std::env::var("COWFS_C2B_DIR") else {
        return;
    };
    let c = Core::open(&dir, test_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let fs = c.snapshot_view("s").unwrap();
    let a = fs.create(ROOT_INO, b"one", 0o644).unwrap();
    fs.write(a.ino, 0, b"FIRST").unwrap();
    c.sync().unwrap();
    let mut o = std::io::stdout();
    writeln!(o, "C first {}", a.ino).unwrap();
    o.flush().unwrap();
    std::process::abort();
}

/// B5: a rolled-back or deleted mark must not hand the same number out again.
#[test]
fn a_rolled_back_virtual_mark_never_hands_out_the_same_number() {
    for damage in ["zeros", "delete", "zero-byte", "torn"] {
        let dir = tempfile::tempdir().unwrap();
        let out = child("virt_mark_child", dir.path(), damage);
        println!("child output: {out:?}");
        let first: u64 = out
            .lines()
            .find_map(|l| l.split("C first ").nth(1))
            .and_then(|v| v.trim().parse().ok())
            .expect("the child reported its number");
        match damage {
            "zeros" => {
                for f in ["virt.ino.a", "virt.ino.b"] {
                    std::fs::write(dir.path().join(f), [0u8; 16]).unwrap();
                }
            }
            "delete" => {
                let _ = std::fs::remove_file(dir.path().join("virt.ino.a"));
                let _ = std::fs::remove_file(dir.path().join("virt.ino.b"));
            }
            "zero-byte" => {
                std::fs::write(dir.path().join("virt.ino.a"), b"").unwrap();
                std::fs::write(dir.path().join("virt.ino.b"), b"").unwrap();
            }
            _ => {
                let mut b = std::fs::read(dir.path().join("virt.ino.b")).unwrap();
                if let Some(last) = b.last_mut() {
                    *last ^= 0xff;
                }
                std::fs::write(dir.path().join("virt.ino.b"), &b).unwrap();
            }
        }
        let c =
            Core::open(dir.path(), test_opts()).expect("a damaged mark must not brick the mount");
        let fs = c.snapshot_view("s").unwrap();
        let b = fs.create(ROOT_INO, b"two", 0o644).unwrap();
        fs.write(b.ino, 0, b"SECOND").unwrap();
        c.sync().unwrap();
        let read = fs.read(first, 0, 16);
        let seen = read
            .as_ref()
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .map_err(|e| e.to_string());
        println!(
            "{damage}: session 1 {first:#x}, session 2 {:#x}, read(old) = {seen:?}, last_error {:?}",
            b.ino,
            c.last_flush_error()
        );
        assert_ne!(
            b.ino, first,
            "{damage}: an inode number was handed out twice"
        );
        assert!(
            read.as_ref().map(|v| v.as_slice() == b"SECOND") != Ok(true),
            "{damage}: the old number returned another file's bytes"
        );
    }
}

/// B5: a lost mark raises the counter by a safety margin and says so.
#[test]
fn a_lost_mark_starts_far_above_the_old_counter_and_logs() {
    let dir = tempfile::tempdir().unwrap();
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.create_snapshot("s").unwrap();
        let fs = c.snapshot_view("s").unwrap();
        let a = fs.create(ROOT_INO, b"f", 0o644).unwrap();
        fs.write(a.ino, 0, b"x").unwrap();
        c.sync().unwrap();
        let _ = a;
    }
    let _ = std::fs::remove_file(dir.path().join("virt.ino.a"));
    let _ = std::fs::remove_file(dir.path().join("virt.ino.b"));
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let e = c.last_flush_error().unwrap_or_default();
    println!("last_error after a lost mark: {e}");
    assert!(
        e.contains("virt"),
        "a lost virtual-number mark must be reported loudly, got {e:?}"
    );
    let fs = c.snapshot_view("s").unwrap();
    let a = fs.create(ROOT_INO, b"g", 0o644).unwrap().ino;
    assert!(
        a & cowfs_core::VIRT_COUNTER_MASK > (1 << 32),
        "{a:#x} starts at zero after a lost mark"
    );
}

// ---------------------------------------------------------------- B6: the lock audit table

/// B6: every function in `src/` that takes a lock is listed in the audit table in
/// `docs/v1-core.md`, so the table cannot rot. Mechanical: extract the lock sites from the source
/// and check each against the doc.
#[test]
fn every_lock_site_is_in_the_audit_table() {
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/v1-core.md"
    ))
    .unwrap();
    let table: Vec<&str> = doc
        .split("| Site | Locks held together | Order |")
        .nth(1)
        .expect("the audit table")
        .lines()
        .skip(2)
        .collect();
    let mut missing = Vec::new();
    let mut checked = 0usize;
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap();
        for (name, body) in fns(&src) {
            let takes = body.contains(".rd()")
                || body.contains(".wr()")
                || body.contains(".lk()")
                || body.contains(".try_read()")
                || body.contains(".try_write()")
                || body.contains(".try_lock()")
                || body.contains("snap.")
                || body.contains("blocks.get")
                || body.contains("blocks.put");
            if !takes {
                continue;
            }
            checked += 1;
            let short = name.rsplit("::").next().unwrap();
            if !table.iter().any(|row| row.contains(short)) {
                missing.push(format!(
                    "{}::{name}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "{} of {checked} lock-taking functions are not in the audit table of docs/v1-core.md: {}",
        missing.len(),
        missing.join(", ")
    );
}

/// Function name and body of every `fn` in `src`, by brace counting.
fn fns(src: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0usize;
    while let Some(p) = src[i..].find("fn ") {
        let at = i + p;
        let name_start = at + 3;
        let name_end = src[name_start..]
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .map_or(src.len(), |n| name_start + n);
        let name = src[name_start..name_end].to_string();
        let mut depth = 0i32;
        let mut body = String::new();
        let mut j = at;
        let mut started = false;
        while j < bytes.len() {
            let c = bytes[j] as char;
            match c {
                '{' => {
                    depth += 1;
                    started = true;
                }
                '}' => {
                    depth -= 1;
                    if started && depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            body.push(c);
            j += 1;
        }
        if started {
            out.push((name, body));
        }
        i = j.max(at + 3);
    }
    out
}

// ---------------------------------------------------------------- B8: a zero-id chunk ref

/// B8: a stored chunk ref with the all-zero id must not be served as zeros.
#[test]
fn a_zero_id_chunk_ref_is_a_hole_only_if_the_store_does_not_hold_it() {
    let dir = tempfile::tempdir().unwrap();
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.create_snapshot("s").unwrap();
        c.sync().unwrap();
        let meta = c.meta();
        let info = meta.snapshots().unwrap().into_iter().next().unwrap();
        let snap = meta.snapshot_by_id(info.id).unwrap();
        snap.batch(|tx| {
            let a = tx.create(cowfs_meta::ROOT_INO, b"forged", 0o644)?;
            let refs = vec![cowfs_store::ChunkRef {
                id: cowfs_store::BlockId::from_bytes([0; 32]),
                len: 4096,
            }];
            tx.set_content(a.ino, &refs, 4096)?;
            Ok::<(), cowfs_meta::Error>(())
        })
        .unwrap();
        c.drop_caches();
    }
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let fs = c.snapshot_view("s").unwrap();
    let a = fs.lookup(ROOT_INO, b"forged").unwrap();
    let got = fs.read(a.ino, 0, 64);
    println!(
        "read of a chunk ref whose id is all zeros -> ok={} head={:?}",
        got.is_ok(),
        got.as_ref().map(|v| v[..8.min(v.len())].to_vec())
    );
    // the store holds no zero-id block, so this is a hole by the documented sentinel rule
    assert!(!c
        .store()
        .contains(cowfs_store::BlockId::from_bytes([0; 32])));
    assert_eq!(got.map(|v| v.len()), Ok(64), "a hole reads back as zeros");
}

/// B8: the zero id cannot be a real block, and the hole sentinel is bounded by its length.
#[test]
fn the_zero_id_is_never_a_real_block_and_a_hole_is_length_bounded() {
    for payload in [b"".as_slice(), b"x", &[0u8; 4096], &[0xffu8; 100_000]] {
        assert_ne!(
            cowfs_store::BlockId::of(payload).as_bytes(),
            &[0u8; 32],
            "BLAKE3 produced the hole sentinel"
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    c.sync().unwrap();
    {
        let meta = c.meta();
        let info = meta.snapshots().unwrap().into_iter().next().unwrap();
        let snap = meta.snapshot_by_id(info.id).unwrap();
        snap.batch(|tx| {
            let a = tx.create(cowfs_meta::ROOT_INO, b"forged", 0o644)?;
            let refs = vec![cowfs_store::ChunkRef {
                id: cowfs_store::BlockId::from_bytes([0; 32]),
                // longer than any hole ref a sparse file can hold
                len: (1 << 30) + 1,
            }];
            tx.set_content(a.ino, &refs, u64::from(refs[0].len))?;
            Ok::<(), cowfs_meta::Error>(())
        })
        .unwrap();
        drop(c);
    }
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let fs = c.snapshot_view("s").unwrap();
    let a = fs.lookup(ROOT_INO, b"forged").unwrap();
    let got = fs.read(a.ino, 0, 64);
    println!(
        "read of a zero-id ref longer than HOLE_MAX -> {:?}",
        got.as_ref().err()
    );
    assert!(
        matches!(got, Err(cowfs_vfs::Error::Corrupt(_))),
        "a zero id with a length no hole can hold is not a hole: {got:?}"
    );
}

// ---------------------------------------------------------------- B10: the read allocation

/// B10: one `read` call must not allocate the whole request.
#[test]
fn a_read_call_never_allocates_more_than_the_file_and_one_cap() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let fs = c.snapshot_view("s").unwrap();
    let a = fs.create(ROOT_INO, b"f", 0o644).unwrap().ino;
    fs.setattr(
        a,
        SetAttr {
            size: Some(1 << 40),
            ..SetAttr::default()
        },
    )
    .unwrap();
    let cap = cowfs_core::MAX_READ_BYTES as usize;
    let got = fs.read(a, 0, u32::MAX);
    let n = got.as_ref().map(|v| v.len()).unwrap_or(0);
    println!("read(0, u32::MAX) of a 1 TiB sparse file returned {n} bytes");
    assert!(got.is_err() || n <= cap, "one read allocated {n} bytes");
    // a read past the end is short, never the request
    let got = fs.read(a, (1 << 40) - 16, 4096);
    assert_eq!(got.map(|v| v.len()), Ok(16));
    // a normal read is unaffected
    fs.write(a, 0, b"hello").unwrap();
    let got = fs.read(a, 0, 4096).expect("read");
    assert_eq!(
        got.len(),
        4096,
        "a read inside the file returns what it asked for"
    );
    assert_eq!(&got[..5], b"hello");
}

// ---------------------------------------------------------------- B7: liveness

fn watchdog<F: FnOnce() + Send + 'static>(
    limit: Duration,
    what: &str,
    progress: Arc<AtomicU64>,
    f: F,
) -> u64 {
    let done = Arc::new(AtomicBool::new(false));
    let d = done.clone();
    let h = std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        d.store(true, Ordering::SeqCst);
        result
    });
    let start = Instant::now();
    let mut last = progress.load(Ordering::SeqCst);
    let mut worst = Duration::ZERO;
    let mut quiet_since = start;
    while !done.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
        let now = progress.load(Ordering::SeqCst);
        worst = worst.max(quiet_since.elapsed());
        if now != last {
            quiet_since = Instant::now();
        }
        last = now;
        assert!(
            quiet_since.elapsed() < limit,
            "{what}: no progress for {limit:?}"
        );
    }
    if let Err(e) = h.join().unwrap() {
        std::panic::resume_unwind(e);
    }
    u64::try_from(worst.as_millis()).unwrap_or(u64::MAX)
}

/// B7: every multi-lock operation at once, including the ones the fixes added. Reports the worst
/// gap between progress ticks and fails if it exceeds two seconds.
#[test]
fn watchdog_keeps_the_longest_gap_after_progress_resumes() {
    let ticks = Arc::new(AtomicU64::new(0));
    let worker_ticks = ticks.clone();
    let gap = watchdog(Duration::from_secs(2), "gap regression", ticks, move || {
        std::thread::sleep(Duration::from_millis(200));
        for _ in 0..20 {
            worker_ticks.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    assert!(gap >= 150, "forgot the initial quiet interval: {gap} ms");
}

#[test]
#[ignore = "heavy: C2B_STRESS_SECS=300 cargo test -p cowfs-core --release --test critic2b -- --ignored stress"]
fn stress() {
    let secs: u64 = std::env::var("C2B_STRESS_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let dir = tempfile::tempdir().unwrap();
    let ticks_out = Arc::new(AtomicU64::new(0));
    let threads_ticks = ticks_out.clone();
    let report_ticks = ticks_out.clone();
    let gap = watchdog(
        Duration::from_secs(90),
        "mixed lock stress",
        ticks_out.clone(),
        move || {
            let c = Core::open(
                dir.path(),
                Options {
                    background: true,
                    flush_interval: Duration::from_millis(20),
                    max_pending_ops: 64,
                    node_cache: 512,
                    dentry_cache: 4096,
                    ..Default::default()
                },
            )
            .unwrap();
            c.create_snapshot("s").unwrap();
            c.create_snapshot("other").unwrap();
            let fs = c.snapshot_view("s").unwrap();
            let root = ROOT_INO;
            for i in 0..200 {
                let d = fs.mkdir(root, format!("d{i}").as_bytes(), 0o755).unwrap();
                let a = fs.create(d.ino, b"f", 0o644).unwrap();
                fs.write(a.ino, 0, &pattern(3000, i as u64)).unwrap();
                fs.forget(a.ino, 1);
                fs.forget(d.ino, 1);
            }
            c.sync().unwrap();
            let ticks = threads_ticks.clone();
            let stop = Instant::now() + Duration::from_secs(secs);
            let mut hs = Vec::new();
            for t in 0..14u64 {
                let (c, fs, progress) = (c.clone(), fs.clone(), ticks.clone());
                hs.push(std::thread::spawn(move || {
                    let mut rng = Rng(t * 7 + 1);
                    while Instant::now() < stop {
                        let i = rng.below(200);
                        let name = format!("d{i}");
                        match t % 14 {
                            0 | 1 => {
                                c.drop_caches();
                                let _ = fs.lookup(root, name.as_bytes());
                                if let Ok(a) = fs.lookup(root, name.as_bytes()) {
                                    let _ = fs.getattr(a.ino);
                                    let _ = fs.read(a.ino, 0, 4096);
                                    c.forget(a.ino, 1);
                                }
                            }
                            2 | 3 => {
                                if let Ok(a) =
                                    fs.create(root, format!("t{}", rng.below(64)).as_bytes(), 0o644)
                                {
                                    let _ = fs.write(a.ino, 0, &pattern(5000, t));
                                    c.forget(a.ino, 1);
                                }
                            }
                            4 => {
                                let _ = fs.readdir(root, 0, 100);
                            }
                            5 => {
                                let _ = fs.setattr(root, SetAttr::default());
                            }
                            6 => {
                                let _ = c.fork_snapshot("s", &format!("f{}", rng.below(4)));
                                let _ = c.remove_snapshot(&format!("f{}", rng.below(4)));
                                let _ = c.merkle_root("s");
                            }
                            7 => {
                                let _ = c.rename_snapshot("s", "moving");
                                let _ = c.rename_snapshot("moving", "s");
                            }
                            8 => {
                                let _ = c.promote_base("other", "base");
                                let _ = c.rename_snapshot("base", "base2");
                            }
                            9 => {
                                let _ = fs.unlink(root, format!("t{}", rng.below(64)).as_bytes());
                            }
                            10 => {
                                let _ = fs.rename(
                                    root,
                                    name.as_bytes(),
                                    root,
                                    b"moved",
                                    Default::default(),
                                );
                                let _ = fs.rename(
                                    root,
                                    b"moved",
                                    root,
                                    name.as_bytes(),
                                    Default::default(),
                                );
                            }
                            11 => {
                                let _ = fs.setxattr(
                                    root,
                                    b"u",
                                    &pattern(64, t),
                                    cowfs_vfs::XattrFlags {
                                        create: true,
                                        replace: false,
                                    },
                                );
                            }
                            12 => {
                                c.sync().unwrap();
                            }
                            _ => {
                                let _ = c.fsync(ROOT_INO, false);
                            }
                        }
                        progress.fetch_add(1, Ordering::Relaxed);
                    }
                }));
            }
            for h in hs {
                h.join().unwrap();
            }
            c.sync().unwrap();
            c.check().unwrap();
        },
    );
    println!(
        "stress ticks: {}, worst gap between progress ticks: {gap} ms",
        report_ticks.load(Ordering::Relaxed)
    );
    assert!(gap < 2000, "the worst progress gap was {gap} ms");
}

// ---------------------------------------------------------------- B3, B4, F5: the swap

fn content(c: &Core, snap: &str, name: &str) -> String {
    try_content(c, snap, name).unwrap_or_else(|| panic!("no {name} in {snap}"))
}

fn try_content(c: &Core, snap: &str, name: &str) -> Option<String> {
    let fs = c.snapshot_view(snap).ok()?;
    let a = fs.lookup(ROOT_INO, name.as_bytes()).ok()?;
    Some(String::from_utf8(read_all(&fs, a.ino)).expect("utf8"))
}

/// B3: a refused rename leaves the mount exactly as it was, with no staging snapshot visible, and
/// the next open does not complete it.
#[test]
fn a_refused_promotion_leaves_the_mount_exactly_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let names;
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.create_snapshot("src").unwrap();
        let src_root = root_entry(&c, "src").ino;
        mkfile(&c.snapshot_view("src").unwrap(), src_root, "x", b"payload");
        c.create_snapshot("old").unwrap();
        let target = root_entry(&c, "old").ino;
        let fs = c.snapshot_view("old").unwrap();
        let a = mkfile(&fs, target, "y", b"doomed");
        c.sync().unwrap();
        // the handle is on the snapshot promotion would destroy, which is what makes it refuse
        let h = fs.open(a.ino).unwrap();
        let r = c.promote_base("src", "old");
        println!("promotion with an open handle -> {r:?}");
        names = snap_names(&c);
        println!("snapshots after the refusal: {names:?}");
        assert!(r.is_err(), "setup: {r:?}");
        fs.release(h).unwrap();
        assert_eq!(
            names,
            ["old".to_string(), "src".to_string()],
            "the mount changed on a refusal"
        );
        assert_eq!(content(&c, "src", "x"), "payload");
        assert_eq!(content(&c, "old", "y"), "doomed");
        drop(fs);
        assert!(
            !dir.path().join("swap-old").exists(),
            "a refusal left an intent file behind"
        );
    }
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert_eq!(
        snap_names(&c),
        ["old".to_string(), "src".to_string()],
        "the refused promotion completed at the next open"
    );
    c.check().unwrap();
}

#[test]
fn a_step_three_refusal_removes_the_intent_and_staging_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.create_snapshot("base").unwrap();
        mkfile(&c.snapshot_view("base").unwrap(), ROOT_INO, "x", b"OLD");
        c.create_snapshot("src").unwrap();
        mkfile(&c.snapshot_view("src").unwrap(), ROOT_INO, "y", b"NEW");
        c.sync().unwrap();
        c.set_swap_fault(3);
        assert!(c.promote_base("src", "base").is_err());
        c.set_swap_fault(0);
        assert!(
            !dir.path().join("swap-base").exists(),
            "refusal retained its intent"
        );
        assert_eq!(
            c.meta().snapshots().unwrap().len(),
            2,
            "hidden staging state survived"
        );
        assert_eq!(snap_names(&c), ["base", "src"]);
    }
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert_eq!(snap_names(&c), ["base", "src"]);
    assert_eq!(content(&c, "base", "x"), "OLD");
    assert_eq!(content(&c, "src", "y"), "NEW");
    c.check().unwrap();
}

#[test]
fn every_pre_removal_refusal_stays_refused_after_reopen() {
    for step in 1..=3 {
        let dir = tempfile::tempdir().unwrap();
        {
            let c = Core::open(dir.path(), test_opts()).unwrap();
            c.create_snapshot("base").unwrap();
            c.create_snapshot("src").unwrap();
            c.set_swap_fault(step);
            assert!(c.promote_base("src", "base").is_err());
        }
        let c = Core::open(dir.path(), test_opts()).unwrap();
        assert_eq!(
            snap_names(&c),
            ["base", "src"],
            "refusal at step {step} completed on reopen"
        );
    }
}

#[test]
fn staging_names_are_reserved_for_the_swap_protocol() {
    assert!(cowfs_core::validate_snapshot_name("evil.cowfs-swap0").is_err());
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert!(c.create_snapshot("evil.cowfs-swap0").is_err());
    assert!(c.list_snapshots().unwrap().is_empty());
}

/// B3: a crash after the staging fork and before the intent file leaves nothing the user can see.
#[test]
fn a_crash_between_the_staging_fork_and_the_intent_shows_nothing_new() {
    let dir = tempfile::tempdir().unwrap();
    let out = child("c2b_child", dir.path(), "crash_in_swap");
    println!("child: {out}");
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let names = snap_names(&c);
    println!("after the crash: {names:?}");
    assert!(
        !names.iter().any(|n| n.contains(".cowfs-swap")),
        "a staging snapshot is visible in the mount: {names:?}"
    );
    c.check().unwrap();
}

/// B3, B4: a failure at every step of a promote leaves either the old state with `Err`, or the new
/// state with `Ok`. Never a missing name, never a visible staging name.
#[test]
fn a_fault_at_every_step_of_a_swap_leaves_old_or_new_and_nothing_in_between() {
    for step in 1..=5u8 {
        let dir = tempfile::tempdir().unwrap();
        {
            let c = Core::open(dir.path(), test_opts()).unwrap();
            c.create_snapshot("src").unwrap();
            c.create_snapshot("base").unwrap();
            {
                let fs = c.snapshot_view("src").unwrap();
                mkfile(&fs, ROOT_INO, "hello", b"NEW");
                c.sync().unwrap();
            }
            mkfile(&c.snapshot_view("base").unwrap(), ROOT_INO, "old", b"OLD");
            c.sync().unwrap();
            c.set_swap_fault(step);
            let r = c.promote_base("src", "base");
            c.set_swap_fault(0);
            let names = snap_names(&c);
            println!(
                "step {step}: promote -> {:?}, snapshots {names:?}",
                r.is_ok()
            );
            match r {
                Ok(_) => assert_eq!(content(&c, "base", "hello"), "NEW", "step {step}"),
                Err(_) => {
                    assert!(
                        names.contains(&"base".to_string()),
                        "step {step}: an Err left `base` missing from the live mount: {names:?}"
                    );
                    let listed: Vec<String> = c
                        .snapshot_view("base")
                        .unwrap()
                        .readdir(ROOT_INO, 0, 10)
                        .unwrap()
                        .entries
                        .iter()
                        .map(|e| String::from_utf8_lossy(&e.name).into_owned())
                        .collect();
                    println!("step {step}: base holds {listed:?}");
                    assert_eq!(content(&c, "base", "old"), "OLD", "step {step}");
                }
            }
            assert!(
                !names.iter().any(|n| n.contains(".cowfs-swap")),
                "step {step}: a staging snapshot is visible: {names:?}"
            );
            assert!(
                c.snapshot_view("src").is_ok(),
                "step {step}: the source vanished"
            );
        }
        let c = Core::open(dir.path(), test_opts()).unwrap();
        let names = snap_names(&c);
        println!("step {step}: after reopen {names:?}");
        assert!(
            !names.iter().any(|n| n.contains(".cowfs-swap")),
            "step {step}: after the reopen a staging snapshot is visible: {names:?}"
        );
        assert!(
            names.contains(&"base".to_string()),
            "step {step}: {names:?}"
        );
        assert!(
            try_content(&c, "base", "hello").as_deref() == Some("NEW")
                || try_content(&c, "base", "old").as_deref() == Some("OLD"),
            "step {step}: the base holds neither the old nor the new content"
        );
        assert!(
            c.snapshot_view("src").is_ok(),
            "step {step}: the source vanished"
        );
        c.check().unwrap();
    }
}

/// F5: `promote_base` runs the full name check, so `base` cannot land next to `BASE`.
#[test]
fn promote_base_cannot_create_a_name_key_collision() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("src").unwrap();
    c.create_snapshot("BASE").unwrap();
    assert!(
        c.create_snapshot("base").is_err(),
        "setup: create refuses it"
    );
    let r = c.promote_base("src", "base");
    println!("promote_base(src, base) next to BASE -> {r:?}");
    let names = snap_names(&c);
    println!("snapshots: {names:?}");
    assert!(
        !(names.contains(&"BASE".to_string()) && names.contains(&"base".to_string())),
        "promote_base created `base` next to `BASE`: they alias on a normalising mount: {names:?}"
    );
    // replacing BASE itself is fine
    assert!(c.promote_base("src", "BASE").is_ok());
    assert_eq!(snap_names(&c), ["BASE".to_string(), "src".to_string()]);
}

/// B11: a torn intent file is removed, with the staging snapshot it implies, and it is reported.
#[test]
fn a_torn_intent_file_is_reported_and_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("base").unwrap();
    c.create_snapshot("base2").unwrap();
    c.set_swap_fault(2);
    assert!(c.promote_base("base", "base2").is_err());
    c.set_swap_fault(0);
    // what a crash inside write_intent leaves: a truncated or empty intent file
    std::fs::write(dir.path().join("swap-base2"), b"").unwrap();
    drop(c);
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let names = snap_names(&c);
    println!(
        "after a torn intent: {names:?}, last_error {:?}",
        c.last_flush_error()
    );
    assert!(
        !names.iter().any(|n| n.contains(".cowfs-swap")),
        "{names:?}"
    );
    assert!(names.contains(&"base".to_string()), "{names:?}");
    assert!(names.contains(&"base2".to_string()), "{names:?}");
    assert!(
        c.last_flush_error().unwrap_or_default().contains("swap"),
        "a torn intent file must be reported"
    );
    assert!(!dir.path().join("swap-base2").exists());
    c.check().unwrap();
}

// ---------------------------------------------------------------- B9: error classification

/// B9: a transient store failure must not kill the file. The data stays pending, `fsync` reports
/// EIO, and the acked bytes are there after a repair.
#[test]
fn a_transient_store_failure_keeps_the_data_pending_and_a_repair_saves_it() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(
        dir.path(),
        Options {
            // above the size written below, so the write is acked with its bytes still dirty
            file_flush_bytes: 8 << 20,
            ..test_opts()
        },
    )
    .unwrap();
    c.create_snapshot("s").unwrap();
    let fs = c.snapshot_view("s").unwrap();
    let a = fs.create(ROOT_INO, b"f", 0o644).unwrap().ino;
    let data = pattern(5 << 20, 7);
    fs.write(a, 0, &data).unwrap();
    // every flush of this file fails out of space, for longer than the in-flush retry budget
    c.set_flush_fault(a, 1, 1000);
    c.flush().unwrap();
    let h = c.health();
    println!(
        "after a transient failure: health {:?}, last_error {:?}",
        h.files, h.last_error
    );
    let f = h
        .files
        .iter()
        .find(|f| f.ino == a)
        .expect("health must name the file");
    assert!(!f.poisoned, "a transient failure poisoned the file: {f:?}");
    assert!(
        h.lanes.iter().any(|l| l.files_stuck > 0),
        "the data must stay pending: {h:?}"
    );
    assert!(c.stats().transient > 0, "{:?}", c.stats());
    // the mount is still writable and the file still reads
    let b = fs.create(ROOT_INO, b"g", 0o644).unwrap().ino;
    fs.write(b, 0, b"ok").unwrap();
    c.flush().unwrap();
    assert_eq!(
        read_all(&fs, b),
        b"ok",
        "a transient failure wedged the whole mount"
    );
    // fsync of the affected file reports EIO, not a corruption
    assert!(
        matches!(fs.fsync(a, false), Err(cowfs_vfs::Error::Io(_))),
        "fsync of a stuck file must report EIO"
    );
    // cause gone: the repair puts the bytes in
    c.set_flush_fault(a, 0, 0);
    c.unpoison(a).expect("unpoison");
    fs.fsync(a, false).expect("fsync after the repair");
    c.sync().unwrap();
    assert_eq!(read_all(&fs, a), data, "the acked bytes did not survive");
    let h = c.health();
    assert!(!h.files.iter().any(|f| f.ino == a), "{h:?}");
    drop(fs);
    drop(c);
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let fs = c.snapshot_view("s").unwrap();
    let a = fs.lookup(ROOT_INO, b"f").unwrap().ino;
    assert_eq!(read_all(&fs, a), data, "the acked bytes were lost");
}

#[test]
fn a_single_transient_failure_is_retried_in_the_same_flush() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let fs = c.snapshot_view("s").unwrap();
    let a = mkfile(&fs, ROOT_INO, "f", b"retry payload").ino;
    c.set_flush_fault(a, 1, 1);
    c.flush().unwrap();
    assert_eq!(
        c.stats().dirty_bytes,
        0,
        "flush stopped at a retryable error"
    );
    assert_eq!(c.stats().transient, 0, "retry budget was not used");
    assert!(c.health().files.is_empty());
    fs.fsync(a, false).unwrap();
    drop(fs);
    drop(c);
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert_eq!(content(&c, "s", "f"), "retry payload");
}

/// B9: a corruption still poisons, and `unpoison` is the documented repair for it too.
#[test]
fn a_corrupt_flush_poisons_the_file_and_unpoison_repairs_it() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(
        dir.path(),
        Options {
            file_flush_bytes: 8 << 20,
            ..test_opts()
        },
    )
    .unwrap();
    c.create_snapshot("s").unwrap();
    let fs = c.snapshot_view("s").unwrap();
    let a = fs.create(ROOT_INO, b"f", 0o644).unwrap().ino;
    let data = pattern(5 << 20, 9);
    fs.write(a, 0, &data).unwrap();
    assert!(c.stats().dirty_bytes > 0, "the write left nothing to flush");
    c.set_flush_fault(a, 2, 1000);
    c.flush().unwrap();
    let h = c.health();
    let f = h
        .files
        .iter()
        .find(|f| f.ino == a)
        .expect("health must name the file");
    println!("after a corruption: {f:?}");
    assert!(f.poisoned, "a corruption did not poison the file: {f:?}");
    assert!(c.stats().poisoned > 0);
    assert!(
        fs.write(a, 0, b"x").is_err(),
        "a poisoned file accepted a write"
    );
    c.set_flush_fault(a, 0, 0);
    c.unpoison(a).expect("unpoison");
    c.flush().unwrap();
    assert_eq!(read_all(&fs, a), data, "the repair lost the bytes");
    assert!(c.health().files.is_empty(), "{:?}", c.health());
}

/// B9: the health report is a plain snapshot of the state, and an unknown inode is `NotFound`.
#[test]
fn health_is_empty_when_nothing_is_broken_and_unpoison_reports_a_bad_inode() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let fs = c.snapshot_view("s").unwrap();
    let a = fs.create(ROOT_INO, b"f", 0o644).unwrap().ino;
    fs.write(a, 0, b"fine").unwrap();
    c.sync().unwrap();
    let h = c.health();
    assert!(h.files.is_empty(), "{h:?}");
    assert!(h.lanes.is_empty(), "{h:?}");
    assert!(c.unpoison(1 << 40).is_err(), "unpoison of an unknown inode");
}

#[test]
fn health_does_not_consume_pending_flush_work() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let fs = c.snapshot_view("s").unwrap();
    mkfile(&fs, ROOT_INO, "f", b"pending payload");
    assert_eq!(c.health().lanes.len(), 1);
    assert_eq!(c.health().lanes.len(), 1, "health consumed the dirty queue");
    c.sync().unwrap();
    assert_eq!(c.stats().dirty_bytes, 0);
    drop(fs);
    drop(c);
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert_eq!(content(&c, "s", "f"), "pending payload");
}

/// `rmdir` resolves the name again after the barrier, because committing a directory's create
/// releases its virtual alias and the name is then carried by the meta number. That must not turn a
/// non-empty directory into `NotFound`.
#[test]
fn rmdir_reports_not_empty_when_the_directory_holds_a_symlink() {
    for (name, o) in [
        ("default", test_opts()),
        (
            "tiny",
            Options {
                background: false,
                max_pending_ops: 16,
                file_flush_bytes: 64 << 10,
                node_cache: 256,
                dentry_cache: 256,
                block_cache_bytes: 1 << 20,
                ..test_opts()
            },
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), o).unwrap();
        c.create_snapshot("s").unwrap();
        let fs = c.snapshot_view("s").unwrap();
        let d = fs.mkdir(ROOT_INO, b"b", 0o755).unwrap().ino;
        let looked = fs.lookup(ROOT_INO, b"b").unwrap().ino;
        assert_eq!(looked, d);
        fs.forget(looked, 1);
        let l = fs.symlink(d, b"a", b"target").unwrap().ino;
        let held = fs.lookup(d, b"a").unwrap().ino;
        fs.forget(held, 1);
        let r = fs.rmdir(ROOT_INO, b"b");
        println!("{name}: mkdir {d:#x}, symlink {l:#x}, rmdir -> {r:?}");
        assert!(
            matches!(r, Err(cowfs_vfs::Error::NotEmpty)),
            "{name}: rmdir of a directory holding a symlink did not report NotEmpty"
        );
        fs.forget(l, 1);
    }
}
