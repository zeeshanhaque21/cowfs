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
use cowfs_fuse::{bench, sweep_stale_mounts, Health, Mount, MountError, MountOptions, Unmounted};
use cowfs_vfs::{SetAttr, Vfs, ROOT_INO};
use cowfs_vfs_test::MemVfs;

fn is_root() -> bool {
    fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

/// Special files through the kernel (issue #107): mkfifo and a socket node are created by the
/// kernel's `mknod`, reported with the right type, mode and `nlink`, listed, and removed; a device
/// needs root and keeps its device number.
#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn special_files_through_mknod() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileTypeExt;
    let Some(fx) = Fixture::new("") else { return };
    let c = |name: &str| CString::new(fx.p(name).as_os_str().as_bytes()).unwrap();

    let fifo = c("fifo");
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o640) }, 0);
    let m = fs::symlink_metadata(fx.p("fifo")).unwrap();
    assert!(m.file_type().is_fifo(), "{:?}", m.file_type());
    assert_eq!((m.mode() & 0o7777, m.nlink(), m.len()), (0o640, 1, 0));

    let sock = c("sock");
    assert_eq!(
        unsafe { libc::mknod(sock.as_ptr(), libc::S_IFSOCK | 0o600, 0) },
        0
    );
    assert!(fs::symlink_metadata(fx.p("sock"))
        .unwrap()
        .file_type()
        .is_socket());

    let again = unsafe { libc::mkfifo(fifo.as_ptr(), 0o640) };
    assert_eq!(
        (again, std::io::Error::last_os_error().raw_os_error()),
        (-1, Some(libc::EEXIST))
    );

    let mut names: Vec<_> = fs::read_dir(fx.p(""))
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().into_string().unwrap(),
                e.file_type().unwrap().is_fifo(),
            )
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![("fifo".to_string(), true), ("sock".to_string(), false)]
    );

    let dev = c("chr");
    let made = unsafe { libc::mknod(dev.as_ptr(), libc::S_IFCHR | 0o600, 0x103) };
    if is_root() {
        assert_eq!(made, 0, "{:?}", std::io::Error::last_os_error());
        let m = fs::symlink_metadata(fx.p("chr")).unwrap();
        assert!(m.file_type().is_char_device());
        // 0x103 is Linux makedev(1, 3)
        assert_eq!(m.rdev(), 0x103, "device number round trip");
        fs::remove_file(fx.p("chr")).unwrap();
    } else {
        assert_eq!(
            (made, std::io::Error::last_os_error().raw_os_error()),
            (-1, Some(libc::EPERM))
        );
    }

    fs::remove_file(fx.p("fifo")).unwrap();
    fs::remove_file(fx.p("sock")).unwrap();
    assert_eq!(fs::read_dir(fx.p("")).unwrap().count(), 0);
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
    let Some(fx) = Fixture::new("attr_ttl=0") else {
        return;
    };
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
    let by_path = format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&f));
    assert_eq!(
        fs::metadata(by_path).unwrap().nlink(),
        0,
        "an unlinked file that is still open keeps zero links when statted without its handle"
    );
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

/// `fallocate(2)` on an open file; 0 on success, else the errno.
#[allow(unsafe_code)]
fn falloc(f: &File, mode: i32, off: i64, len: i64) -> i32 {
    use std::os::fd::AsRawFd;
    // SAFETY: plain syscall on a valid open descriptor
    if unsafe { libc::fallocate(f.as_raw_fd(), mode, off, len) } == 0 {
        0
    } else {
        std::io::Error::last_os_error().raw_os_error().unwrap()
    }
}

const KEEP: i32 = libc::FALLOC_FL_KEEP_SIZE;
const PUNCH: i32 = libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE;
const ZERO: i32 = libc::FALLOC_FL_ZERO_RANGE;

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn fallocate_modes_through_a_mount() {
    let Some(fx) = Fixture::new("") else { return };
    let data: Vec<u8> = (0..65_536u32).map(|i| (i % 251) as u8 + 1).collect();
    fs::write(fx.p("f"), &data).unwrap();
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(fx.p("f"))
        .unwrap();
    let st = || fs::metadata(fx.p("f")).unwrap();
    let blocks = st().blocks();
    let mut model = data;

    // punch: bytes read as zeros, size kept, whole pages freed
    assert_eq!(falloc(&f, PUNCH, 4096, 8192), 0);
    model[4096..12_288].fill(0);
    assert_eq!(st().len(), 65_536);
    assert_eq!(fs::read(fx.p("f")).unwrap(), model);
    assert!(st().blocks() < blocks, "blocks did not drop after a punch");

    // zero-range inside the file, with and without keep-size, unaligned
    assert_eq!(falloc(&f, ZERO | KEEP, 20_001, 3000), 0);
    model[20_001..23_001].fill(0);
    assert_eq!(falloc(&f, ZERO, 30_000, 100), 0);
    model[30_000..30_100].fill(0);
    assert_eq!(st().len(), 65_536);
    assert_eq!(fs::read(fx.p("f")).unwrap(), model);

    // keep-size past the end changes nothing; zero-range and allocate past the end grow the file
    assert_eq!(falloc(&f, KEEP, 65_536, 4096), 0);
    assert_eq!(st().len(), 65_536);
    assert_eq!(falloc(&f, ZERO | KEEP, 65_000, 5000), 0);
    model[65_000..].fill(0);
    assert_eq!(st().len(), 65_536);
    assert_eq!(falloc(&f, ZERO, 66_000, 1000), 0);
    model.resize(67_000, 0);
    assert_eq!(st().len(), 67_000);
    assert_eq!(falloc(&f, 0, 70_000, 1), 0);
    model.resize(70_001, 0);
    assert_eq!(falloc(&f, 0, 0, 10), 0);
    assert_eq!(st().len(), 70_001);
    assert_eq!(fs::read(fx.p("f")).unwrap(), model);

    // refusals: collapse, insert and unshare are unsupported, length 0 is invalid, a range past
    // the largest file is EFBIG, and nothing above changed the file
    for mode in [0x08, 0x20, 0x40, 0x04] {
        assert_eq!(
            falloc(&f, mode, 0, 4096),
            libc::EOPNOTSUPP,
            "mode {mode:#x}"
        );
    }
    assert_eq!(falloc(&f, 0, 0, 0), libc::EINVAL);
    assert_eq!(falloc(&f, 0, 1 << 42, 1), libc::EFBIG);
    assert_eq!(st().len(), 70_001);
    assert_eq!(fs::read(fx.p("f")).unwrap(), model);
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
    // collapse and insert range are not supported; the other modes are (see the next test)
    let f = OpenOptions::new().write(true).open(fx.p("a")).unwrap();
    assert_eq!(falloc(&f, 0x08, 0, 4096), libc::EOPNOTSUPP);
    assert_eq!(falloc(&f, 0x20, 0, 4096), libc::EOPNOTSUPP);
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
fn negative_answers_expire_by_negative_ttl_not_the_entry_ttl() {
    let Some(fx) = Fixture::new("neg_ttl=2,entry_ttl=60") else {
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

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn chown_to_another_uid_is_refused_and_changes_nothing() {
    // nodefault_permissions: with it the kernel itself refuses a non-root chown, which would hide
    // the adapter. Root still reaches the adapter either way.
    let Some(fx) = Fixture::with("nodefault_permissions", |v| {
        v.create(ROOT_INO, b"f", 0o644).unwrap();
        v.create(ROOT_INO, b"s", 0o4755).unwrap();
    }) else {
        return;
    };
    let m = fs::metadata(fx.p("f")).unwrap();
    let (uid, gid) = (m.uid(), m.gid());
    // Everything belongs to the mounter, so another uid is a change the filesystem will not make.
    assert_eq!(
        errno(std::os::unix::fs::chown(fx.p("f"), Some(uid + 1), None)),
        libc::EPERM,
        "other uid"
    );
    assert_eq!(
        errno(std::os::unix::fs::chown(
            fx.p("f"),
            Some(uid + 1),
            Some(gid)
        )),
        libc::EPERM,
        "other uid with the current gid"
    );
    let m = fs::metadata(fx.p("f")).unwrap();
    assert_eq!(
        (m.uid(), m.gid()),
        (uid, gid),
        "refused chown changes nothing"
    );
    // A refused chown refuses the whole request: a chown the kernel pairs with a cleared setuid
    // bit must not apply the mode half.
    assert_eq!(fs::metadata(fx.p("s")).unwrap().mode() & 0o7777, 0o4755);
    assert_eq!(
        errno(std::os::unix::fs::chown(fx.p("s"), Some(uid + 1), None)),
        libc::EPERM,
        "other uid on a setuid file"
    );
    assert_eq!(fs::metadata(fx.p("s")).unwrap().mode() & 0o7777, 0o4755);
    // The current uid changes nothing, and a gid is accepted and ignored.
    std::os::unix::fs::chown(fx.p("f"), Some(uid), None).unwrap();
    std::os::unix::fs::chown(fx.p("f"), Some(uid), Some(gid)).unwrap();
    std::os::unix::fs::chown(fx.p("f"), None, Some(gid)).unwrap();
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
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/battery/getdents.py");
        let out = Command::new("python3").arg(script).arg(p).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let find = |name: &str| {
            text.lines()
                .find_map(|l| {
                    let (n, ino) = l.split_once(' ')?;
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

/// Path of the `mounthost` example, or `None` when cargo did not build examples for this run.
fn host_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().unwrap();
    let p = exe
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/mounthost");
    p.exists().then_some(p)
}

fn spawn_host(dir: &Path) -> std::process::Child {
    let mut child =
        Command::new(host_binary().expect("the mounthost example is built with --examples"))
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
    if !fuse_usable() || host_binary().is_none() {
        eprintln!("SKIP: /dev/fuse, fusermount3 or the mounthost example is missing");
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

fn run_fsx(opts: &str, seeds: &[&str], ops: &str) {
    let Some(fx) = Fixture::new(opts) else { return };
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
    for seed in seeds {
        let out = Command::new("timeout")
            .args(["900"])
            .arg(&bin)
            .arg(fx.p(&format!("fsx{seed}")))
            .args([ops, seed])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        println!("fsx [{opts}] seed {seed}: {text}");
        assert!(out.status.success(), "fsx seed {seed} failed: {text}");
    }
}

#[test]
#[ignore = "needs FUSE and cc: cargo test -p cowfs-fuse -- --ignored --test-threads=1 --nocapture"]
fn fsx_random_operations_match_a_shadow_copy() {
    run_fsx("", &["1", "2"], "100000");
}

#[test]
#[ignore = "needs FUSE and cc: cargo test -p cowfs-fuse -- --ignored --test-threads=1 --nocapture"]
fn fsx_with_every_request_on_a_lane() {
    run_fsx("inline_below_us=0,workers=3", &["3"], "60000");
}

#[test]
#[ignore = "needs FUSE and python3: cargo test -p cowfs-fuse -- --ignored --test-threads=1 --nocapture"]
fn batteries_with_every_request_on_a_lane() {
    let Some(fx) = Fixture::new("inline_below_us=0,workers=3") else {
        return;
    };
    let text = battery(&fx, "test_mount.py", &[]);
    assert!(text.contains("Not passing: []"), "some checks did not pass");
    let text = battery(&fx, "test_hardlink_readdir.py", &["2000"]);
    assert!(text.contains("PASS hardlink readdir: listed 4000/4000 unique 4000"));
    let text = battery(&fx, "mmap_race.py", &["50", "fsync"]);
    assert!(text.contains("size_mismatch=0") && text.contains("content_mismatch=0"));
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
    let mut variants: Vec<(String, String)> = vec![
        (
            "workers=0 (single threaded loop), ttl=0,noneg".into(),
            "workers=0,ttl=0,noneg".into(),
        ),
        (
            "everything on the lanes (inline_below_us=0), ttl=0,noneg".into(),
            "inline_below_us=0,ttl=0,noneg".into(),
        ),
        (
            "default (adaptive), ttl=0,noneg".into(),
            "ttl=0,noneg".into(),
        ),
        ("default options".into(), String::new()),
    ];
    if let Some(v) = std::env::var("COWFS_BENCH_OPTS")
        .ok()
        .filter(|v| !v.is_empty())
    {
        variants = v.split(';').map(|o| (o.to_owned(), o.to_owned())).collect();
    }
    for (label, opts) in variants {
        let Some(fx) = Fixture::new(&opts) else {
            return;
        };
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

/// Renames take one lane, the source directory's. That is enough because the kernel serialises
/// renames per superblock (`s_vfs_rename_mutex`, `lock_rename`), so at most one RENAME request is
/// ever in flight for a mount: measured here with 16 threads over 16 directories, the `Vfs` never
/// saw two renames at once, in either direction.
#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn cross_directory_renames_in_both_directions_never_overlap_in_the_vfs() {
    const DIRS: u64 = 8;
    // Eight directories over four lanes: every pair spans two lanes.
    let Some(fx) = Fixture::with("inline_below_us=0,workers=4", |_| {}) else {
        return;
    };
    // More directories than lanes, so some pair of them is always served by different lanes.
    let mut dirs = Vec::new();
    for i in 0..DIRS {
        let d = fx.p(&format!("d{i}"));
        fs::create_dir(&d).unwrap();
        // One pair of names per thread, or the kernel would serialise them on the same dentry
        // and the test would prove nothing about the lanes.
        for t in 0..DIRS {
            fs::write(d.join(format!("a{t}")), b"1").unwrap();
            fs::write(d.join(format!("b{t}")), b"2").unwrap();
        }
        dirs.push(d);
    }
    fx.vfs.rename_delay_ms.store(2, SeqCst);
    let stop = Arc::new(AtomicBool::new(false));
    // Two threads per pair of directories, one renaming x -> y and the other y -> x. A rename
    // that took only its source directory's lane would let the two directions run at once.
    let ts: Vec<_> = (0..DIRS)
        .map(|t| {
            let dirs = dirs.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let (x, y) = (t / 2, t / 2 + 1);
                while !stop.load(SeqCst) {
                    let (from, to) = if t % 2 == 0 { (x, y) } else { (y, x) };
                    let f = dirs[from as usize].join(format!("a{t}"));
                    let g = dirs[to as usize].join(format!("b{t}"));
                    let _ = fs::rename(&f, &g);
                    let _ = fs::rename(&g, &f);
                }
            })
        })
        .collect();
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, SeqCst);
    for t in ts {
        t.join().unwrap();
    }
    let shared = fx.vfs.renames_shared_max.load(SeqCst);
    eprintln!(
        "EVIDENCE renames={} max concurrent={} max on a shared dir={shared}",
        fx.vfs.renames.load(SeqCst),
        fx.vfs.renames_max.load(SeqCst)
    );
    assert!(fx.vfs.renames.load(SeqCst) > 20, "the hammer did not run");
    assert!(
        shared <= 1,
        "renames that share a directory reached the Vfs at once ({shared} overlapped)"
    );
    assert_eq!(fx.mount().health(), Health::Ok, "no lane wedged");
    assert_eq!(
        fx.vfs.renames_max.load(SeqCst),
        1,
        "the kernel did not serialise the renames, so one lane is not enough"
    );
}
