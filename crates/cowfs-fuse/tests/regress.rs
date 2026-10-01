//! Regression tests for the defects a critic found in the first version of the adapter. They use
//! only API that the first version also had, so they can be run against it: they fail there.
//! Run: `cargo test -p cowfs-fuse -j4 --test regress -- --ignored --test-threads=1`
#![cfg(target_os = "linux")]

mod common;

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use common::*;
use cowfs_fuse::Health;
use cowfs_vfs::{SetAttr, Vfs, ROOT_INO};

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn default_options_show_changes_made_behind_the_mount_within_a_second() {
    let Some(fx) = Fixture::with("", |v| {
        for (n, d) in [(&b"f"[..], &b"one"[..]), (b"g", b"gg"), (b"h", b"hh")] {
            let a = v.create(ROOT_INO, n, 0o644).unwrap();
            v.write(a.ino, 0, d).unwrap();
        }
    }) else {
        return;
    };
    assert_eq!(fs::read(fx.p("f")).unwrap(), b"one");
    fs::metadata(fx.p("g")).unwrap();
    fs::metadata(fx.p("h")).unwrap();
    let raw = fx.raw();
    let f = raw.lookup(ROOT_INO, b"f").unwrap();
    raw.write(f.ino, 0, b"THREEE").unwrap();
    let g = raw.lookup(ROOT_INO, b"g").unwrap();
    raw.setattr(
        g.ino,
        SetAttr {
            mode: Some(0),
            ..Default::default()
        },
    )
    .unwrap();
    raw.rename(ROOT_INO, b"h", ROOT_INO, b"h2", Default::default())
        .unwrap();

    std::thread::sleep(Duration::from_millis(1300));
    assert_eq!(fs::metadata(fx.p("f")).unwrap().len(), 6, "size");
    assert_eq!(fs::read(fx.p("f")).unwrap(), b"THREEE", "data");
    assert_eq!(
        fs::metadata(fx.p("g")).unwrap().permissions().mode() & 0o777,
        0,
        "mode"
    );
    assert_eq!(
        errno(File::open(fx.p("g"))),
        libc::EACCES,
        "permission check uses fresh mode"
    );
    assert_eq!(errno(fs::metadata(fx.p("h"))), libc::ENOENT, "old name");
    assert!(fs::metadata(fx.p("h2")).is_ok(), "new name");
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn append_after_growth_behind_the_mount_does_not_overwrite() {
    let Some(fx) = Fixture::with("", |v| {
        let a = v.create(ROOT_INO, b"f", 0o644).unwrap();
        v.write(a.ino, 0, b"AAA").unwrap();
    }) else {
        return;
    };
    assert_eq!(fs::read(fx.p("f")).unwrap(), b"AAA");
    assert_eq!(fs::metadata(fx.p("f")).unwrap().len(), 3);
    let f = fx.raw().lookup(ROOT_INO, b"f").unwrap();
    fx.raw().write(f.ino, 3, b"BBBBBBB").unwrap();
    let mut a = OpenOptions::new().append(true).open(fx.p("f")).unwrap();
    a.write_all(b"CC").unwrap();
    drop(a);
    assert_eq!(fx.raw().read(f.ino, 0, 100).unwrap(), b"AAABBBBBBBCC");
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn vfs_panic_fails_one_request_not_the_mount() {
    let Some(fx) = Fixture::with("", |v| {
        let a = v.create(ROOT_INO, b"f", 0o644).unwrap();
        v.write(a.ino, 0, b"data").unwrap();
    }) else {
        return;
    };
    fx.vfs.panic_read.store(true, SeqCst);
    let (tx, rx) = mpsc::channel();
    let p = fx.p("f");
    std::thread::spawn(move || {
        let _ = tx.send(errno(fs::read(p)));
    });
    let got = rx.recv_timeout(Duration::from_secs(5));
    assert_eq!(got, Ok(libc::EIO), "the read must fail with EIO, not hang");
    assert!(is_mounted(&fx.dir), "the mount must survive a Vfs panic");
    assert!(fs::metadata(fx.p("f")).is_ok());
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn slow_read_does_not_delay_metadata() {
    let Some(fx) = Fixture::with("noneg,ttl=0", |v| {
        let a = v.create(ROOT_INO, b"big", 0o644).unwrap();
        v.write(a.ino, 0, &[7u8; 4096]).unwrap();
    }) else {
        return;
    };
    fx.vfs.read_delay_ms.store(1000, SeqCst);
    let p = fx.p("big");
    let reader = std::thread::spawn(move || {
        let t = Instant::now();
        fs::read(p).unwrap();
        t.elapsed()
    });
    std::thread::sleep(Duration::from_millis(150));
    let mut worst = Duration::ZERO;
    for i in 0..5 {
        let t = Instant::now();
        let _ = fs::metadata(fx.p(&format!("missing{i}")));
        worst = worst.max(t.elapsed());
    }
    let t = Instant::now();
    let _ = fs::read_dir(&fx.dir).unwrap().count();
    worst = worst.max(t.elapsed());
    let read = reader.join().unwrap();
    eprintln!("worst metadata latency during a 1 s read: {worst:?} (read took {read:?})");
    assert!(
        read >= Duration::from_millis(900),
        "the read was really slow"
    );
    assert!(
        worst < Duration::from_millis(100),
        "metadata waited {worst:?} behind a slow read"
    );
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn impossible_attributes_from_the_vfs_do_not_brick_the_mount() {
    let Some(fx) = Fixture::with("ttl=0", |v| {
        v.mkdir(ROOT_INO, b"d", 0o755).unwrap();
        v.create(ROOT_INO, b"f", 0o644).unwrap();
    }) else {
        return;
    };
    fx.vfs.bad_attrs.store(true, SeqCst);
    for p in [fx.dir.clone(), fx.p("d"), fx.p("f")] {
        let m = fs::metadata(&p).unwrap_or_else(|e| panic!("stat {}: {e}", p.display()));
        assert_eq!(m.len(), i64::MAX as u64, "size is clamped");
        assert!(m.nlink() >= 1);
    }
    assert!(fs::metadata(fx.dir.join("d")).unwrap().nlink() >= 2);
    fx.vfs.bad_attrs.store(false, SeqCst);
    assert_eq!(fs::metadata(fx.p("f")).unwrap().len(), 0);
}

/// F1: the real `Shared` contract for a file descriptor that is already open.
#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn held_descriptor_sees_a_rewrite_behind_the_mount_after_a_revalidation() {
    let Some(fx) = Fixture::with("", |v| {
        let a = v.create(ROOT_INO, b"held", 0o644).unwrap();
        v.write(a.ino, 0, b"old").unwrap();
    }) else {
        return;
    };
    use std::os::unix::fs::FileExt;
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(fx.p("held"))
        .unwrap();
    assert_eq!(f.read_at(&mut [0u8; 3], 0).unwrap(), 3);
    let ino = fx.raw().lookup(ROOT_INO, b"held").unwrap().ino;
    fx.raw().write(ino, 0, b"new").unwrap();
    let mut seen = None;
    for _ in 0..40 {
        let mut b = [0u8; 3];
        let _ = f.read_at(&mut b, 0);
        if &b == b"new" {
            seen = Some(b);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        seen,
        Some(*b"new"),
        "a held descriptor must see the rewrite behind the mount; the Vfs holds {:?}",
        fx.raw().read(ino, 0, 3).unwrap()
    );
}

// F3: a create whose open failed must not leave the name behind.
#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn a_create_whose_open_fails_leaves_nothing_behind() {
    let Some(fx) = Fixture::new("") else { return };
    fx.vfs.open_fails.store(true, SeqCst);
    let e = errno(File::create(fx.p("half")));
    assert_ne!(e, 0, "the create must fail");
    let still_there = fx.raw().lookup(ROOT_INO, b"half").is_ok();
    eprintln!("EVIDENCE create errno={e} vfs_holds={still_there}");
    assert!(
        !still_there,
        "the Vfs still holds a file the caller was told failed"
    );
    fx.vfs.open_fails.store(false, SeqCst);
    assert!(
        eventually(5, || errno(File::create(fx.p("half"))) == 0),
        "a retry must succeed within the negative ttl, not hit EEXIST"
    );
    assert_eq!(fs::read(fx.p("half")).unwrap(), b"");
    assert_eq!(
        fs::metadata(fx.p("half")).unwrap().len(),
        0,
        "the retry made an empty file"
    );
    assert_eq!(
        errno(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(fx.p("half"))
        ),
        libc::EEXIST,
        "and now the name really exists"
    );
}

// F4: an empty page that is not the end must not truncate the listing.
#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn a_readdir_page_that_ends_early_is_retried_not_truncated() {
    let Some(fx) = Fixture::new("") else { return };
    for i in 0..40 {
        File::create(fx.p(&format!("real{i:03}"))).unwrap();
    }
    assert_eq!(
        fs::read_dir(&fx.dir).unwrap().count(),
        40,
        "baseline listing"
    );
    fx.vfs.empty_no_eof.store(true, SeqCst);
    let mut listed = 0;
    let mut err = 0;
    match fs::read_dir(&fx.dir) {
        Ok(rd) => {
            for e in rd {
                match e {
                    Ok(_) => listed += 1,
                    Err(e) => {
                        err = e.raw_os_error().unwrap_or(-1);
                    }
                }
            }
        }
        Err(e) => err = e.raw_os_error().unwrap_or(-1),
    }
    fx.vfs.empty_no_eof.store(false, SeqCst);
    let after = fs::read_dir(&fx.dir)
        .unwrap()
        .filter_map(Result::ok)
        .count();
    eprintln!("EVIDENCE listed={listed} err={err} after={after}");
    assert_eq!(after, 40, "the tree is intact, so the listing lost entries");
    assert!(
        listed == 40 || err != 0,
        "a short listing with no error is silent data loss: got {listed}"
    );
}

// F4: cookies are opaque, including a zero-based numbering.
#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn zero_based_readdir_cookies_are_accepted() {
    let Some(fx) = Fixture::new("") else { return };
    for i in 0..300 {
        File::create(fx.p(&format!("real{i:03}"))).unwrap();
    }
    fx.vfs.zero_cookies.store(true, SeqCst);
    let (listed, err) = (
        fs::read_dir(&fx.dir)
            .unwrap()
            .filter_map(Result::ok)
            .count(),
        errno(fs::read_dir(&fx.dir).map(|_| ())),
    );
    fx.vfs.zero_cookies.store(false, SeqCst);
    let after = fs::read_dir(&fx.dir)
        .unwrap()
        .filter_map(Result::ok)
        .count();
    eprintln!("EVIDENCE zero-cookie listed={listed} err={err} after={after}");
    assert_eq!(after, 300);
    assert_eq!(listed, 300, "zero-based cookies must not break a listing");
    assert_eq!(err, 0);
}

// F5: a request that never returns must show up in health().
#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn a_wedged_lane_is_reported_by_health() {
    let Some(fx) = Fixture::new("lane_bound=0.4,workers=4,inline_below_us=0") else {
        return;
    };
    for (n, d) in [
        ("f0", b"x".as_slice()),
        ("f1", b"y".as_slice()),
        ("f2", b"z".as_slice()),
    ] {
        fs::write(fx.p(n), d).unwrap();
    }
    assert_eq!(fx.mount().health(), Health::Ok);
    fx.vfs.wedge_read_ms.store(2_000, SeqCst);
    let p = fx.p("f0");
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = done.clone();
    let stuck = std::thread::spawn(move || {
        let _ = fs::read(p);
        flag.store(true, SeqCst);
    });
    let wedged = eventually(10, || !fx.mount().wedged_lanes().is_empty());
    // Some inodes share the stuck lane, so probe all of them: one must still answer.
    let others = ["f1", "f2"]
        .into_iter()
        .filter(|n| fs::metadata(fx.p(n)).is_ok())
        .count();
    let health = fx.mount().health();
    eprintln!(
        "EVIDENCE wedged={wedged} health={health:?} other_lanes_ok={others} inodes={:?}",
        fx.mount().wedged_lanes()
    );
    assert!(wedged, "a stuck Vfs call must be reported");
    assert!(
        matches!(health, Health::Wedged(_)),
        "health must say wedged"
    );
    assert!(fx.mount().is_alive(), "the session is still up");
    assert!(others >= 1, "other lanes keep working");
    fx.vfs.wedge_read_ms.store(0, SeqCst);
    assert!(
        eventually(15, || done.load(SeqCst)
            && fx.mount().wedged_lanes().is_empty()),
        "the lane must be reported free again"
    );
    assert_eq!(fx.mount().health(), Health::Ok);
    stuck.join().unwrap();
}

// F2: kernel-ordered sequences stay consistent when the Vfs is slow under concurrency.
#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn kernel_ordered_sequences_are_consistent_with_a_slow_concurrent_vfs() {
    let Some(fx) = Fixture::new("inline_below_us=0,workers=4") else {
        return;
    };
    fx.vfs.read_delay_ms.store(2, SeqCst);
    for _ in 0..25 {
        let p = fx.p("seq");
        let mut made = File::create(&p).unwrap();
        made.write_all(b"payload").unwrap();
        made.sync_all().unwrap();
        drop(made);
        assert_eq!(
            fs::read(&p).unwrap(),
            b"payload",
            "write then fsync then read"
        );
        assert_eq!(fs::metadata(&p).unwrap().len(), 7);
        assert!(
            fs::metadata(&p).is_ok(),
            "create then lookup of the same name"
        );
        fs::rename(&p, fx.p("seq2")).unwrap();
        assert_eq!(
            errno(fs::metadata(&p)),
            libc::ENOENT,
            "rename then old name"
        );
        assert_eq!(fs::read(fx.p("seq2")).unwrap(), b"payload");
        fs::remove_file(fx.p("seq2")).unwrap();
    }
    assert_eq!(fx.mount().health(), Health::Ok);
}
