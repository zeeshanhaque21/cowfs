//! Kernel-facing tests. They mount a real FUSE filesystem, so they are `#[ignore]`d and skip
//! themselves (they do not fail) when `/dev/fuse` or `fusermount3` is unusable. Run with:
//!
//! `cargo test -p cowfs-fuse -j4 -- --ignored --test-threads=1 --nocapture`
//!
//! Do not filter with `--test`: the signal tests need the `mounthost` example, which cargo
//! builds only when all targets are selected.
#![cfg(target_os = "linux")]

mod common;

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use cowfs_fuse::{bench, sweep_stale_mounts, Mount, MountError, MountOptions, Unmounted};
use cowfs_vfs::{SetAttr, Vfs, ROOT_INO};
use cowfs_vfs_test::MemVfs;

fn is_root() -> bool {
    fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn std_fs_round_trip() {
    let Some(fx) = Fixture::new("") else { return };
    fs::write(fx.p("a"), b"hello").unwrap();
    assert_eq!(fs::read(fx.p("a")).unwrap(), b"hello");
    assert_eq!(fs::metadata(fx.p("a")).unwrap().len(), 5);

    fs::create_dir_all(fx.p("d/e")).unwrap();
    fs::write(fx.p("d/e/f"), "x").unwrap();
    fs::rename(fx.p("d"), fx.p("d2")).unwrap();
    assert_eq!(fs::read_to_string(fx.p("d2/e/f")).unwrap(), "x");
    assert_eq!(errno(fs::remove_dir(fx.p("d2"))), libc::ENOTEMPTY);
    fs::remove_dir_all(fx.p("d2")).unwrap();

    let f = OpenOptions::new().write(true).open(fx.p("a")).unwrap();
    f.set_len(2).unwrap();
    f.write_all_at(b"Z", 1).unwrap();
    f.set_len(4).unwrap();
    drop(f);
    assert_eq!(fs::read(fx.p("a")).unwrap(), b"hZ\0\0");

    fs::set_permissions(fx.p("a"), fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(
        fs::metadata(fx.p("a")).unwrap().permissions().mode() & 0o7777,
        0o640
    );
    let f = File::open(fx.p("a")).unwrap();
    let t = std::time::UNIX_EPOCH + std::time::Duration::new(1_000_000_000, 123);
    f.set_modified(t).unwrap();
    assert_eq!(fs::metadata(fx.p("a")).unwrap().modified().unwrap(), t);
    drop(f);

    std::os::unix::fs::symlink("a", fx.p("l")).unwrap();
    assert_eq!(fs::read_link(fx.p("l")).unwrap(), Path::new("a"));
    assert!(fs::symlink_metadata(fx.p("l"))
        .unwrap()
        .file_type()
        .is_symlink());

    fs::hard_link(fx.p("a"), fx.p("h")).unwrap();
    assert_eq!(fs::metadata(fx.p("a")).unwrap().nlink(), 2);
    assert_eq!(
        fs::metadata(fx.p("a")).unwrap().ino(),
        fs::metadata(fx.p("h")).unwrap().ino()
    );
    fs::write(fx.p("h"), b"via link").unwrap();
    assert_eq!(fs::read(fx.p("a")).unwrap(), b"via link");
    fs::remove_file(fx.p("a")).unwrap();
    assert_eq!(fs::read(fx.p("h")).unwrap(), b"via link");
    assert_eq!(fs::metadata(fx.p("h")).unwrap().nlink(), 1);

    assert_eq!(errno(fs::metadata(fx.p("nope"))), libc::ENOENT);
    assert_eq!(errno(fs::create_dir(fx.p("h"))), libc::EEXIST);
    assert_eq!(errno(File::open(fx.dir.join("h/x"))), libc::ENOTDIR);
    assert_eq!(errno(fs::read(&fx.dir)), libc::EISDIR);
    let mut names: Vec<_> = fs::read_dir(&fx.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["h", "l"]);
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn unlinked_open_file_is_pinned_until_release() {
    let Some(fx) = Fixture::new("") else { return };
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(fx.p("u"))
        .unwrap();
    f.write_all(b"data").unwrap();
    fs::remove_file(fx.p("u")).unwrap();
    assert_eq!(f.metadata().unwrap().nlink(), 0);
    let mut buf = [0u8; 4];
    f.read_exact_at(&mut buf, 0).unwrap();
    assert_eq!(&buf, b"data");
    f.write_all_at(b"more", 4).unwrap();
    assert_eq!(f.metadata().unwrap().len(), 8);
    drop(f);
    assert_eq!(errno(fs::metadata(fx.p("u"))), libc::ENOENT);
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn deleting_while_listing_neither_repeats_nor_skips() {
    let Some(fx) = Fixture::new("") else { return };
    for i in 0..3000 {
        File::create(fx.p(&format!("f{i:05}"))).unwrap();
    }
    let mut seen = Vec::new();
    for e in fs::read_dir(&fx.dir).unwrap() {
        let e = e.unwrap();
        fs::remove_file(e.path()).unwrap();
        seen.push(e.file_name());
    }
    let n = seen.len();
    seen.sort();
    seen.dedup();
    assert_eq!((n, seen.len()), (3000, 3000));
    assert_eq!(fs::read_dir(&fx.dir).unwrap().count(), 0);
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn read_only_mount_refuses_writes() {
    let Some(fx) = Fixture::with("ro", |v| {
        let a = v.create(ROOT_INO, b"f", 0o644).unwrap();
        v.write(a.ino, 0, b"kept").unwrap();
    }) else {
        return;
    };
    assert_eq!(fs::read(fx.p("f")).unwrap(), b"kept");
    assert_eq!(errno(fs::write(fx.p("f"), b"x")), libc::EROFS);
    assert_eq!(errno(File::create(fx.p("g"))), libc::EROFS);
    assert_eq!(errno(fs::create_dir(fx.p("d"))), libc::EROFS);
    assert_eq!(errno(fs::remove_file(fx.p("f"))), libc::EROFS);
    assert_eq!(fs::read(fx.p("f")).unwrap(), b"kept");
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn unsupported_operations_report_enotsup() {
    let Some(fx) = Fixture::new("") else { return };
    fs::write(fx.p("a"), b"copy me").unwrap();
    let out = Command::new("mkfifo").arg(fx.p("fifo")).output().unwrap();
    assert!(
        !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("not supported")
    );
    let out = Command::new("fallocate")
        .args(["-l", "4096"])
        .arg(fx.p("a"))
        .output()
        .unwrap();
    assert!(
        !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("not supported")
    );
    assert!(Command::new("cp")
        .arg(fx.p("a"))
        .arg(fx.p("b"))
        .status()
        .unwrap()
        .success());
    assert_eq!(fs::read(fx.p("b")).unwrap(), b"copy me");
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn run_blocks_until_unmounted_from_outside() {
    if !fuse_usable() {
        eprintln!("SKIP: /dev/fuse or fusermount3 not usable");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("mnt");
    fs::create_dir(&dir).unwrap();
    let vfs: Arc<dyn Vfs> = Arc::new(MemVfs::new());
    let d = dir.clone();
    let t = std::thread::spawn(move || cowfs_fuse::run(vfs, d, MountOptions::default()));
    assert!(
        eventually(5, || is_mounted(&dir) || t.is_finished()),
        "run did not mount"
    );
    assert!(!t.is_finished(), "run returned before the mount appeared");
    fs::write(dir.join("x"), b"1").unwrap();
    assert!(Command::new("fusermount3")
        .arg("-u")
        .arg(&dir)
        .status()
        .unwrap()
        .success());
    assert!(t.join().unwrap().is_ok());
    assert!(!is_mounted(&dir));
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn mounting_a_missing_directory_fails_cleanly() {
    if !fuse_usable() {
        return;
    }
    let vfs: Arc<dyn Vfs> = Arc::new(MemVfs::new());
    let r = Mount::new(
        vfs,
        "/nonexistent/cowfs-mountpoint",
        MountOptions::default(),
    );
    assert!(matches!(r, Err(MountError::Io(_))));
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn invalidator_drops_cached_inodes_names_and_children() {
    let Some(fx) = Fixture::with("sole_writer", |v| {
        for (n, d) in [(&b"f"[..], &b"one"[..]), (b"p", b"p")] {
            let a = v.create(ROOT_INO, n, 0o644).unwrap();
            v.write(a.ino, 0, d).unwrap();
        }
        let d = v.mkdir(ROOT_INO, b"d", 0o755).unwrap();
        v.create(d.ino, b"a", 0o644).unwrap();
        v.create(d.ino, b"b", 0o644).unwrap();
    }) else {
        return;
    };
    let inv = fx.mount().invalidator();

    assert_eq!(fs::metadata(fx.p("f")).unwrap().len(), 3);
    let f = fx.raw().lookup(ROOT_INO, b"f").unwrap();
    fx.raw().write(f.ino, 0, b"three").unwrap();
    assert_eq!(
        fs::metadata(fx.p("f")).unwrap().len(),
        3,
        "attributes are cached"
    );
    inv.invalidate_inode(f.ino).unwrap();
    assert_eq!(fs::metadata(fx.p("f")).unwrap().len(), 5);
    assert_eq!(fs::read(fx.p("f")).unwrap(), b"three");

    fs::metadata(fx.p("p")).unwrap();
    fx.raw()
        .rename(ROOT_INO, b"p", ROOT_INO, b"q", Default::default())
        .unwrap();
    assert!(fs::metadata(fx.p("p")).is_ok(), "names are cached");
    inv.invalidate_entry(ROOT_INO, "p".as_ref()).unwrap();
    assert_eq!(errno(fs::metadata(fx.p("p"))), libc::ENOENT);
    assert!(fs::metadata(fx.p("q")).is_ok());

    fs::metadata(fx.p("d/a")).unwrap();
    fs::metadata(fx.p("d/b")).unwrap();
    let d = fx.raw().lookup(ROOT_INO, b"d").unwrap();
    fx.raw().unlink(d.ino, b"a").unwrap();
    fx.raw()
        .rename(d.ino, b"b", d.ino, b"c", Default::default())
        .unwrap();
    assert!(fs::metadata(fx.p("d/a")).is_ok(), "children are cached");
    inv.invalidate_children(d.ino).unwrap();
    assert_eq!(errno(fs::metadata(fx.p("d/a"))), libc::ENOENT);
    assert_eq!(errno(fs::metadata(fx.p("d/b"))), libc::ENOENT);
    assert!(fs::metadata(fx.p("d/c")).is_ok());
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn invalidate_children_refreshes_the_parent_directory() {
    let Some(fx) = Fixture::new("sole_writer") else {
        return;
    };
    assert_eq!(fs::metadata(&fx.dir).unwrap().nlink(), 2);
    fx.raw().mkdir(ROOT_INO, b"new", 0o755).unwrap();
    assert_eq!(
        fs::metadata(&fx.dir).unwrap().nlink(),
        2,
        "directory attributes are cached"
    );
    fx.mount()
        .invalidator()
        .invalidate_children(ROOT_INO)
        .unwrap();
    assert_eq!(fs::metadata(&fx.dir).unwrap().nlink(), 3);
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn invalidate_all_and_bump_epoch_drop_everything_cached() {
    let Some(fx) = Fixture::with("sole_writer", |v| {
        v.create(ROOT_INO, b"g", 0o644).unwrap();
        v.create(ROOT_INO, b"h", 0o644).unwrap();
    }) else {
        return;
    };
    let inv = fx.mount().invalidator();
    fs::metadata(fx.p("g")).unwrap();
    fs::metadata(fx.p("h")).unwrap();
    let g = fx.raw().lookup(ROOT_INO, b"g").unwrap();
    fx.raw()
        .setattr(
            g.ino,
            SetAttr {
                mode: Some(0),
                ..Default::default()
            },
        )
        .unwrap();
    fx.raw().unlink(ROOT_INO, b"h").unwrap();
    assert!(
        File::open(fx.p("g")).is_ok(),
        "the old mode is still cached"
    );
    assert!(
        fs::metadata(fx.p("h")).is_ok(),
        "the old name is still cached"
    );
    assert_eq!(inv.epoch(), 0);
    assert_eq!(inv.bump_epoch().unwrap(), 1);
    assert_eq!(inv.epoch(), 1);
    if !is_root() {
        assert_eq!(errno(File::open(fx.p("g"))), libc::EACCES);
    }
    assert_eq!(errno(fs::metadata(fx.p("h"))), libc::ENOENT);
    inv.invalidate_all().unwrap();
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn sole_writer_append_after_growth_behind_the_mount_does_not_overwrite() {
    let Some(fx) = Fixture::with("sole_writer", |v| {
        let a = v.create(ROOT_INO, b"f", 0o644).unwrap();
        v.write(a.ino, 0, b"AAA").unwrap();
    }) else {
        return;
    };
    assert_eq!(fs::metadata(fx.p("f")).unwrap().len(), 3);
    let f = fx.raw().lookup(ROOT_INO, b"f").unwrap();
    fx.raw().write(f.ino, 3, b"BBBBBBB").unwrap();
    let mut a = OpenOptions::new().append(true).open(fx.p("f")).unwrap();
    a.write_all(b"CC").unwrap();
    drop(a);
    assert_eq!(fx.raw().read(f.ino, 0, 100).unwrap(), b"AAABBBBBBBCC");
    assert!(
        eventually(2, || fs::metadata(fx.p("f")).unwrap().len() == 12),
        "the size the kernel caches is refreshed after a corrected append"
    );
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn concurrent_appenders_do_not_overwrite_each_other() {
    let Some(fx) = Fixture::new("") else { return };
    let path = fx.p("log");
    File::create(&path).unwrap();
    let ts: Vec<_> = (0..4)
        .map(|t| {
            let p = path.clone();
            std::thread::spawn(move || {
                let mut f = OpenOptions::new().append(true).open(p).unwrap();
                for _ in 0..200 {
                    f.write_all(&[b'a' + t as u8; 7]).unwrap();
                }
            })
        })
        .collect();
    for t in ts {
        t.join().unwrap();
    }
    let data = fs::read(&path).unwrap();
    assert_eq!(data.len(), 4 * 200 * 7);
    for c in data.chunks(7) {
        assert!(c.iter().all(|b| *b == c[0]), "an append was torn: {c:?}");
    }
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn negative_answers_expire_by_negative_ttl() {
    let Some(fx) = Fixture::new("neg_ttl=2") else {
        return;
    };
    assert_eq!(errno(fs::metadata(fx.p("late"))), libc::ENOENT);
    let a = fx.raw().create(ROOT_INO, b"late", 0o644).unwrap();
    assert_eq!(
        errno(fs::metadata(fx.p("late"))),
        libc::ENOENT,
        "negative answer is cached"
    );
    std::thread::sleep(Duration::from_millis(2200));
    assert_eq!(fs::metadata(fx.p("late")).unwrap().ino(), a.ino);
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn default_negative_ttl_is_one_second() {
    let Some(fx) = Fixture::new("") else { return };
    assert_eq!(errno(fs::metadata(fx.p("snap"))), libc::ENOENT);
    fx.raw().mkdir(ROOT_INO, b"snap", 0o755).unwrap();
    assert_eq!(
        errno(fs::metadata(fx.p("snap"))),
        libc::ENOENT,
        "cached for a moment"
    );
    assert!(
        eventually(3, || fs::metadata(fx.p("snap")).is_ok()),
        "a snapshot directory created behind the mount must appear within seconds"
    );
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn zero_ttl_sees_changes_made_behind_the_mount() {
    let Some(fx) = Fixture::new("ttl=0,noneg") else {
        return;
    };
    assert_eq!(errno(fs::metadata(fx.p("late"))), libc::ENOENT);
    let a = fx.raw().create(ROOT_INO, b"late", 0o644).unwrap();
    assert_eq!(fs::metadata(fx.p("late")).unwrap().ino(), a.ino);
    fx.raw().write(a.ino, 0, b"abc").unwrap();
    assert_eq!(fs::metadata(fx.p("late")).unwrap().len(), 3);
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn attribute_lifetime_is_not_the_entry_lifetime() {
    let Some(fx) = Fixture::with("entry_ttl=0,attr_ttl=1", |v| {
        let a = v.create(ROOT_INO, b"f", 0o644).unwrap();
        v.write(a.ino, 0, b"aaaa").unwrap();
    }) else {
        return;
    };
    let f = File::open(fx.p("f")).unwrap();
    assert_eq!(f.metadata().unwrap().len(), 4);
    let ino = fx.raw().lookup(ROOT_INO, b"f").unwrap().ino;
    fx.raw()
        .setattr(
            ino,
            SetAttr {
                size: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        f.metadata().unwrap().len(),
        4,
        "attributes are cached for attr_ttl"
    );
    std::thread::sleep(Duration::from_millis(1300));
    assert_eq!(f.metadata().unwrap().len(), 2, "and no longer after it");
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn shared_mode_drops_cached_pages_on_open() {
    let Some(fx) = Fixture::with("", |v| {
        let a = v.create(ROOT_INO, b"f", 0o644).unwrap();
        v.write(a.ino, 0, b"AAAA").unwrap();
    }) else {
        return;
    };
    assert_eq!(fs::read(fx.p("f")).unwrap(), b"AAAA");
    let ino = fx.raw().lookup(ROOT_INO, b"f").unwrap().ino;
    fx.raw().write(ino, 0, b"BBBB").unwrap();
    assert_eq!(fs::read(fx.p("f")).unwrap(), b"BBBB");
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn sole_writer_keeps_cached_pages_across_opens() {
    let Some(fx) = Fixture::with("sole_writer", |v| {
        let a = v.create(ROOT_INO, b"f", 0o644).unwrap();
        v.write(a.ino, 0, b"AAAA").unwrap();
    }) else {
        return;
    };
    assert_eq!(fs::read(fx.p("f")).unwrap(), b"AAAA");
    let ino = fx.raw().lookup(ROOT_INO, b"f").unwrap().ino;
    fx.raw().write(ino, 0, b"BBBB").unwrap();
    assert_eq!(
        fs::read(fx.p("f")).unwrap(),
        b"AAAA",
        "keep_cache serves the old pages"
    );
    fx.mount().invalidator().invalidate_inode(ino).unwrap();
    assert_eq!(fs::read(fx.p("f")).unwrap(), b"BBBB");
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn fsync_flush_and_release_reach_the_vfs() {
    let Some(fx) = Fixture::new("") else { return };
    let mut f = File::create(fx.p("x")).unwrap();
    f.write_all(b"data").unwrap();
    f.sync_all().unwrap();
    assert!(fx.vfs.fsyncs.load(SeqCst) >= 1, "fsync must reach the Vfs");
    f.sync_data().unwrap();
    assert!(
        fx.vfs.fsyncs_data_only.load(SeqCst) >= 1,
        "fdatasync must reach the Vfs as data_only"
    );
    let before = fx.vfs.fsyncs.load(SeqCst);
    File::open(&fx.dir).unwrap().sync_all().unwrap();
    assert!(
        fx.vfs.fsyncs.load(SeqCst) > before,
        "fsync of a directory must reach the Vfs"
    );
    drop(f);
    assert!(fx.vfs.flushes.load(SeqCst) >= 1, "close must flush");
    assert!(
        eventually(3, || {
            let (o, r) = (fx.vfs.opens.load(SeqCst), fx.vfs.releases.load(SeqCst));
            o > 0 && o == r
        }),
        "every open handle must be released: opens {} releases {}",
        fx.vfs.opens.load(SeqCst),
        fx.vfs.releases.load(SeqCst)
    );
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn forget_reaches_the_vfs_and_balances_lookups() {
    let Some(fx) = Fixture::new("") else { return };
    for i in 0..20 {
        File::create(fx.p(&format!("f{i}"))).unwrap();
    }
    fs::create_dir(fx.p("d")).unwrap();
    std::os::unix::fs::symlink("f0", fx.p("l")).unwrap();
    fs::hard_link(fx.p("f1"), fx.p("h")).unwrap();
    for i in 0..20 {
        fs::remove_file(fx.p(&format!("f{i}"))).unwrap();
    }
    fs::remove_file(fx.p("h")).unwrap();
    fs::remove_file(fx.p("l")).unwrap();
    fs::remove_dir(fx.p("d")).unwrap();
    assert!(
        eventually(5, || fx.vfs.refs.load(SeqCst) == 0),
        "lookup references must be forgotten: {} outstanding",
        fx.vfs.refs.load(SeqCst)
    );
    assert!(fx.vfs.forgotten.load(SeqCst) >= 23);
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn access_follows_mode_bits_without_default_permissions() {
    if is_root() {
        return;
    }
    let Some(fx) = Fixture::with("nodefault_permissions", |v| {
        v.create(ROOT_INO, b"z", 0).unwrap();
        v.create(ROOT_INO, b"w", 0o644).unwrap();
    }) else {
        return;
    };
    let ok = |flag: &str, n: &str| {
        Command::new("test")
            .arg(flag)
            .arg(fx.p(n))
            .status()
            .unwrap()
            .success()
    };
    assert!(!ok("-r", "z"));
    assert!(!ok("-w", "z"));
    assert!(ok("-r", "w"));
    assert!(ok("-w", "w"));
    assert!(!ok("-x", "w"));
}

fn create_read_only_and_write(fx: &Fixture) {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o444)
        .open(fx.p("ro"))
        .unwrap();
    f.write_all(b"pack").unwrap();
    drop(f);
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn default_permissions_allow_the_creating_handle_but_not_a_later_open_for_write() {
    if is_root() {
        return;
    }
    let Some(fx) = Fixture::new("") else { return };
    create_read_only_and_write(&fx);
    assert_eq!(fs::read(fx.p("ro")).unwrap(), b"pack");
    assert_eq!(
        errno(OpenOptions::new().write(true).open(fx.p("ro"))),
        libc::EACCES
    );
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn without_default_permissions_the_owner_can_write_a_read_only_file() {
    if is_root() {
        return;
    }
    let Some(fx) = Fixture::new("nodefault_permissions") else {
        return;
    };
    create_read_only_and_write(&fx);
    let mut f = OpenOptions::new().write(true).open(fx.p("ro")).unwrap();
    f.write_all(b"PACK").unwrap();
    drop(f);
    assert_eq!(fs::read(fx.p("ro")).unwrap(), b"PACK");
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn dotdot_lists_the_parent_inode() {
    let Some(fx) = Fixture::new("") else { return };
    fs::create_dir_all(fx.p("a/b")).unwrap();
    fs::create_dir(fx.p("c")).unwrap();
    let dots = |p: &Path| -> (u64, u64) {
        let out = Command::new("ls").arg("-ia").arg(p).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let find = |name: &str| {
            text.lines()
                .find_map(|l| {
                    let mut f = l.split_whitespace();
                    let (ino, n) = (f.next()?, f.next()?);
                    (n == name).then(|| ino.parse::<u64>().unwrap())
                })
                .unwrap_or_else(|| panic!("no {name} in {text}"))
        };
        (find("."), find(".."))
    };
    let ino = |p: &str| fs::metadata(fx.p(p)).unwrap().ino();
    assert_eq!(dots(&fx.p("a/b")), (ino("a/b"), ino("a")));
    assert_eq!(
        dots(&fx.p("a")),
        (ino("a"), fs::metadata(&fx.dir).unwrap().ino())
    );
    fs::rename(fx.p("a/b"), fx.p("c/b")).unwrap();
    assert_eq!(
        dots(&fx.p("c/b")),
        (ino("c/b"), ino("c")),
        "the parent follows a directory rename"
    );
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn paranoid_ino_rejects_a_reused_inode_number() {
    let Some(fx) = Fixture::with("paranoid_ino", |v| {
        v.create(ROOT_INO, b"f1", 0o644).unwrap();
    }) else {
        return;
    };
    let f1 = fs::metadata(fx.p("f1")).unwrap().ino();
    *fx.vfs.reuse_ino_for_create.lock().unwrap() = Some(f1);
    assert_eq!(errno(File::create(fx.p("f2"))), libc::EIO);
    *fx.vfs.reuse_ino_for_create.lock().unwrap() = None;
    assert!(File::create(fx.p("f3")).is_ok());
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn xattr_namespaces_are_filtered() {
    let Some(fx) = Fixture::new("") else { return };
    File::create(fx.p("x")).unwrap();
    let p = fx.p("x");
    let set = |n: &str| {
        Command::new("setfattr")
            .args(["-n", n, "-v", "1"])
            .arg(&p)
            .output()
            .unwrap()
    };
    if Command::new("setfattr").arg("--version").output().is_err() {
        eprintln!("SKIP: setfattr not installed");
        return;
    }
    assert!(set("user.a").status.success());
    assert!(!set("system.posix_acl_access").status.success());
    let out = set("system.foo");
    assert!(!out.status.success(), "system.* is never stored");
    assert!(String::from_utf8_lossy(&out.stderr)
        .to_lowercase()
        .contains("not supported"));
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn repeated_vfs_panics_fail_the_mount_with_enotconn() {
    let Some(mut fx) = Fixture::with("max_panics=2", |v| {
        let a = v.create(ROOT_INO, b"f", 0o644).unwrap();
        v.write(a.ino, 0, b"data").unwrap();
    }) else {
        return;
    };
    fx.vfs.panic_read.store(true, SeqCst);
    assert_eq!(errno(fs::read(fx.p("f"))), libc::EIO);
    assert!(fx.vfs.panics.load(SeqCst) >= 1);
    assert!(
        eventually(5, || {
            let _ = fs::read(fx.p("f"));
            fx.mount().failed()
        }),
        "the panic budget must fail the mount"
    );
    assert!(!fx.mount().is_alive());
    assert!(
        is_mounted(&fx.dir),
        "a failed mount stays mounted, so it cannot pass for an empty directory"
    );
    assert_eq!(errno(File::create(fx.p("new"))), libc::ENOTCONN);
    let listing = fs::read_dir(&fx.dir).map(|d| d.filter_map(Result::err).next());
    let e = match listing {
        Err(e) => e.raw_os_error(),
        Ok(e) => e.and_then(|e| e.raw_os_error()),
    };
    assert_eq!(e, Some(libc::ENOTCONN));
    let dir = fx.dir.clone();
    fx.mount.take().unwrap().unmount().unwrap();
    assert!(!is_mounted(&dir));
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn unmount_is_clean_when_idle_and_lazy_when_busy() {
    let Some(mut fx) = Fixture::new("unmount_timeout=0.3") else {
        return;
    };
    let dir = fx.dir.clone();
    let m = fx.mount.take().unwrap();
    assert!(m.is_alive());
    assert_eq!(m.unmount().unwrap(), Unmounted::Clean);
    assert!(!is_mounted(&dir));
    drop(fx);

    let Some(mut fx) = Fixture::new("unmount_timeout=0.3") else {
        return;
    };
    let dir = fx.dir.clone();
    let f = File::create(fx.p("busy")).unwrap();
    let t = Instant::now();
    let how = fx.mount.take().unwrap().unmount().unwrap();
    assert_eq!(
        how,
        Unmounted::Lazy,
        "a mount with an open file is detached lazily"
    );
    assert!(t.elapsed() < Duration::from_secs(5));
    assert!(!is_mounted(&dir));
    drop(f);
}

fn host_binary() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let p = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/mounthost");
    assert!(
        p.exists(),
        "build it: cargo build -p cowfs-fuse --examples ({})",
        p.display()
    );
    p
}

fn spawn_host(dir: &Path) -> std::process::Child {
    let mut child = Command::new(host_binary())
        .arg(dir)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::BufRead::read_line(&mut std::io::BufReader::new(out), &mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(20)).unwrap_or_default();
    assert_eq!(line.trim(), "READY", "the host did not start");
    child
}

fn wait_exit(child: &mut std::process::Child) -> std::process::ExitStatus {
    let end = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        assert!(Instant::now() < end, "the host did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn signal(child: &std::process::Child, sig: &str) {
    Command::new("kill")
        .arg(format!("-{sig}"))
        .arg(child.id().to_string())
        .status()
        .unwrap();
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn termination_signals_unmount_and_kill9_is_swept() {
    if !fuse_usable() {
        eprintln!("SKIP: /dev/fuse or fusermount3 not usable");
        return;
    }
    for (name, num) in [("TERM", 15), ("INT", 2), ("HUP", 1)] {
        let tmp = tempfile::tempdir().unwrap();
        let mnt = tmp.path().join("mnt");
        fs::create_dir(&mnt).unwrap();
        let mut host = spawn_host(&mnt);
        assert!(is_mounted(&mnt));
        signal(&host, name);
        let status = wait_exit(&mut host);
        assert!(!is_mounted(&mnt), "SIG{name} must leave no mount behind");
        assert_eq!(
            status.signal(),
            Some(num),
            "SIG{name} keeps its default effect"
        );
    }

    let tmp = tempfile::tempdir().unwrap();
    let mnt = tmp.path().join("mnt");
    fs::create_dir(&mnt).unwrap();
    let mut host = spawn_host(&mnt);
    signal(&host, "KILL");
    wait_exit(&mut host);
    assert!(is_mounted(&mnt), "kill -9 leaves the mount behind");
    assert_eq!(
        errno(fs::read_dir(&mnt)).max(libc::ENOTCONN),
        libc::ENOTCONN
    );
    assert_eq!(errno(fs::metadata(&mnt)), libc::ENOTCONN);
    assert_eq!(sweep_stale_mounts(tmp.path()), std::slice::from_ref(&mnt));
    assert!(!is_mounted(&mnt));
    assert!(sweep_stale_mounts(tmp.path()).is_empty());
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn auto_unmount_without_user_allow_other_is_explained() {
    if is_root() || !fuse_usable() {
        return;
    }
    let conf = fs::read_to_string("/etc/fuse.conf").unwrap_or_default();
    if conf.lines().any(|l| l.trim() == "user_allow_other") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let opts: MountOptions = "auto_unmount".parse().unwrap();
    let r = Mount::new(Arc::new(MemVfs::new()), tmp.path(), opts);
    assert!(matches!(r, Err(MountError::NeedsAllowOther)));
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn rename_link_unlink_race_keeps_the_tree_consistent() {
    let Some(fx) = Fixture::new("") else { return };
    let dir = fx.p("r");
    fs::create_dir(&dir).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let bad = Arc::new(std::sync::Mutex::new(Vec::new()));
    let ts: Vec<_> = (0..6u64)
        .map(|t| {
            let (dir, stop, bad) = (dir.clone(), stop.clone(), bad.clone());
            std::thread::spawn(move || {
                let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ (t + 1);
                let mut rnd = move |n: u64| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x % n
                };
                while !stop.load(SeqCst) {
                    let a = dir.join(format!("n{}", rnd(12)));
                    let b = dir.join(format!("n{}", rnd(12)));
                    let r = match rnd(7) {
                        0 => File::create(&a).map(|_| ()),
                        1 => fs::write(&a, b"data"),
                        2 => fs::rename(&a, &b),
                        3 => fs::hard_link(&a, &b),
                        4 => fs::remove_file(&a),
                        5 => fs::metadata(&a).map(|_| ()),
                        _ => fs::read_dir(&dir).map(|d| d.for_each(drop)),
                    };
                    if let Err(e) = r {
                        let n = e.raw_os_error().unwrap_or(-1);
                        if n != libc::ENOENT && n != libc::EEXIST {
                            bad.lock().unwrap().push(e.to_string());
                        }
                    }
                }
            })
        })
        .collect();
    std::thread::sleep(Duration::from_secs(4));
    stop.store(true, SeqCst);
    for t in ts {
        t.join().unwrap();
    }
    assert!(
        bad.lock().unwrap().is_empty(),
        "unexpected errors: {:?}",
        bad.lock().unwrap()
    );
    assert!(fx.mount().is_alive());
    let mut by_ino = std::collections::HashMap::<u64, (u32, u64)>::new();
    for e in fs::read_dir(&dir).unwrap() {
        let m = e.unwrap().metadata().unwrap();
        let ent = by_ino.entry(m.ino()).or_insert((0, m.nlink()));
        ent.0 += 1;
    }
    for (ino, (names, nlink)) in by_ino {
        assert_eq!(
            u64::from(names),
            nlink,
            "inode {ino} has {names} names but nlink {nlink}"
        );
    }
}

#[test]
#[ignore = "needs FUSE and cc: cargo test -p cowfs-fuse -- --ignored --test-threads=1 --nocapture"]
fn fsx_random_operations_match_a_shadow_copy() {
    let Some(fx) = Fixture::new("") else { return };
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/battery/fsx.c");
    let bin = fx.tmp.path().join("fsx");
    match Command::new("cc")
        .args(["-O2", "-o"])
        .arg(&bin)
        .arg(src)
        .status()
    {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!("SKIP: no C compiler");
            return;
        }
    }
    for seed in ["1", "2"] {
        let out = Command::new("timeout")
            .args(["900"])
            .arg(&bin)
            .arg(fx.p(&format!("fsx{seed}")))
            .args(["100000", seed])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        println!("fsx seed {seed}: {text}");
        assert!(out.status.success(), "fsx seed {seed} failed: {text}");
    }
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() as f64 * p) as usize).min(v.len() - 1)]
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse --release -- --ignored --test-threads=1 --nocapture bench_head_of_line"]
fn bench_head_of_line() {
    for (label, opts) in [
        (
            "workers=0 (single threaded loop, as before)",
            "workers=0,noneg,ttl=0",
        ),
        ("default workers", "noneg,ttl=0"),
    ] {
        println!("== head of line, {label}");
        let Some(fx) = Fixture::with(opts, |v| {
            for i in 0..4 {
                let a = v
                    .create(ROOT_INO, format!("rf{i}").as_bytes(), 0o644)
                    .unwrap();
                v.write(a.ino, 0, &vec![b'z'; 4096 * 50]).unwrap();
            }
        }) else {
            return;
        };
        fx.vfs.read_delay_ms.store(1000, SeqCst);
        let p = fx.p("rf0");
        let r = std::thread::spawn(move || {
            let t = Instant::now();
            fs::read(p).unwrap();
            t.elapsed()
        });
        std::thread::sleep(Duration::from_millis(150));
        let worst = (0..5)
            .map(|i| {
                let t = Instant::now();
                let _ = fs::metadata(fx.p(&format!("m{i}")));
                t.elapsed().as_secs_f64() * 1e3
            })
            .fold(0.0, f64::max);
        println!(
            "one 1 s read in flight: worst stat {worst:.1} ms (read took {:?})",
            r.join().unwrap()
        );
        for (delay, readers) in [(20u64, 4usize), (100, 4)] {
            fx.vfs.read_delay_ms.store(delay, SeqCst);
            let stop = Arc::new(AtomicBool::new(false));
            let reads = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let ts: Vec<_> = (0..readers)
                .map(|i| {
                    let (p, stop, reads) = (fx.p(&format!("rf{i}")), stop.clone(), reads.clone());
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        let mut k = 0u64;
                        while !stop.load(SeqCst) {
                            let f = File::open(&p).unwrap();
                            f.read_exact_at(&mut buf, (k % 50) * 4096).unwrap();
                            k += 1;
                            reads.fetch_add(1, SeqCst);
                        }
                    })
                })
                .collect();
            std::thread::sleep(Duration::from_millis(500));
            reads.store(0, SeqCst);
            let start = Instant::now();
            let mut lat = Vec::new();
            while start.elapsed() < Duration::from_secs(5) {
                let t = Instant::now();
                let _ = fs::metadata(fx.p(&format!("probe{}", lat.len())));
                lat.push(t.elapsed().as_secs_f64() * 1e3);
            }
            let n = reads.load(SeqCst);
            stop.store(true, SeqCst);
            for t in ts {
                t.join().unwrap();
            }
            println!(
                "{readers} readers, {delay} ms/read: {:.1} reads/s (ideal {:.0}, serial {:.0}); lookup p50 {:.2} p99 {:.2} max {:.2} ms, n={}",
                n as f64 / start.elapsed().as_secs_f64(),
                readers as f64 * 1000.0 / delay as f64,
                1000.0 / delay as f64,
                percentile(&mut lat.clone(), 0.5),
                percentile(&mut lat.clone(), 0.99),
                percentile(&mut lat.clone(), 1.0),
                lat.len()
            );
        }
    }
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse --release -- --ignored --test-threads=1 --nocapture bench_latency"]
fn bench_latency_floor() {
    for (label, opts) in [
        (
            "workers=0, ttl=0,noneg (every op reaches the adapter)",
            "workers=0,ttl=0,noneg",
        ),
        (
            "default workers, ttl=0,noneg (every op reaches the adapter)",
            "ttl=0,noneg",
        ),
        ("default options", ""),
    ] {
        let Some(fx) = Fixture::new(opts) else { return };
        let report = bench::measure(&fx.dir, &bench::Config::default()).unwrap();
        println!("== bench, MemVfs, {label}\n{report}");
    }
}

fn battery(fx: &Fixture, script: &str, args: &[&str]) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/battery")
        .join(script);
    let out = Command::new("timeout")
        .args(["1500", "python3"])
        .arg(path)
        .arg(&fx.dir)
        .args(args)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    println!(
        "== {script} {args:?} (exit {:?})\n{text}",
        out.status.code()
    );
    assert!(
        out.status.success() || script == "test_mount.py",
        "{script} failed"
    );
    text
}

#[test]
#[ignore = "needs FUSE and python3: cargo test -p cowfs-fuse -- --ignored --test-threads=1 --nocapture"]
fn python_battery_test_mount() {
    let Some(fx) = Fixture::new("") else { return };
    let text = battery(&fx, "test_mount.py", &[]);
    assert!(text.contains("Not passing: []"), "some checks did not pass");
}

#[test]
#[ignore = "needs FUSE and python3: cargo test -p cowfs-fuse -- --ignored --test-threads=1 --nocapture"]
fn python_battery_hardlink_readdir_and_mmap() {
    let Some(fx) = Fixture::new("") else { return };
    let text = battery(&fx, "test_hardlink_readdir.py", &["4000"]);
    assert!(text.contains("PASS hardlink readdir: listed 8000/8000 unique 8000"));
    for mode in ["nosync", "msync", "fsync"] {
        let text = battery(&fx, "mmap_race.py", &["100", mode]);
        assert!(
            text.contains("size_mismatch=0") && text.contains("content_mismatch=0"),
            "{mode}"
        );
    }
}

#[test]
#[ignore = "needs FUSE and python3: cargo test -p cowfs-fuse -- --ignored --test-threads=1 --nocapture"]
fn python_battery_extras() {
    let Some(fx) = Fixture::new("") else { return };
    let text = battery(&fx, "test_cowfs_extra.py", &[]);
    assert!(!text.contains("FAIL"));
}
