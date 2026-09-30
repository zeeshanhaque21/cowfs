//! Regression tests for the defects a critic found in the first version of the adapter. They use
//! only API that the first version also had, so they can be run against it: they fail there.
//! Run: `cargo test -p cowfs-fuse -j4 --test regress -- --ignored --test-threads=1`
#![cfg(target_os = "linux")]

mod common;

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::atomic::Ordering::SeqCst;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::*;
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
