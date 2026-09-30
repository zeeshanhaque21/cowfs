//! Kernel-facing tests. They mount a real FUSE filesystem, so they are `#[ignore]`d and skip
//! themselves (they do not fail) when `/dev/fuse` or `fusermount3` is unusable. Run with:
//!
//! `cargo test -p cowfs-fuse -j4 -- --ignored --test-threads=1 --nocapture`

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use cowfs_vfs::{Vfs, ROOT_INO};
use cowfs_vfs_test::MemVfs;
use tempfile::TempDir;

use crate::{Mount, MountOptions};

fn fuse_usable() -> bool {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fuse")
        .is_ok()
        && ["fusermount3", "fusermount"]
            .iter()
            .any(|b| Command::new(b).arg("-V").output().is_ok())
}

struct Fixture {
    mount: Option<Mount>,
    vfs: Arc<MemVfs>,
    dir: PathBuf,
    _tmp: TempDir,
}

impl Fixture {
    fn new(opts: &str) -> Option<Self> {
        Self::with(opts, |_| {})
    }

    fn with(opts: &str, prepare: impl FnOnce(&MemVfs)) -> Option<Self> {
        if !fuse_usable() {
            eprintln!("SKIP: /dev/fuse or fusermount3 not usable");
            return None;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("mnt");
        fs::create_dir(&dir).unwrap();
        let vfs = Arc::new(MemVfs::new());
        prepare(&vfs);
        let opts: MountOptions = opts.parse().unwrap();
        let mount = Mount::new(vfs.clone(), &dir, opts).unwrap();
        Some(Self {
            mount: Some(mount),
            vfs,
            dir,
            _tmp: tmp,
        })
    }

    fn p(&self, n: &str) -> PathBuf {
        self.dir.join(n)
    }

    fn mount(&self) -> &Mount {
        self.mount.as_ref().unwrap()
    }
}

fn is_mounted(dir: &Path) -> bool {
    let want = dir.to_string_lossy().into_owned();
    fs::read_to_string("/proc/mounts")
        .unwrap()
        .lines()
        .any(|l| l.split(' ').nth(1) == Some(want.as_str()))
}

fn errno(r: std::io::Result<impl Sized>) -> i32 {
    r.err().and_then(|e| e.raw_os_error()).unwrap_or(0)
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
fn invalidation_drops_cached_names_and_attributes() {
    let Some(fx) = Fixture::with("", |v| {
        for (n, data) in [(&b"f"[..], &b"one"[..]), (b"p", b"p")] {
            let a = v.create(ROOT_INO, n, 0o644).unwrap();
            v.write(a.ino, 0, data).unwrap();
        }
    }) else {
        return;
    };
    assert_eq!(fs::metadata(fx.p("f")).unwrap().len(), 3);
    let f = fx.vfs.lookup(ROOT_INO, b"f").unwrap();
    fx.vfs.write(f.ino, 0, b"three").unwrap();
    assert_eq!(
        fs::metadata(fx.p("f")).unwrap().len(),
        3,
        "attributes are cached"
    );
    fx.mount().invalidate_inode(f.ino).unwrap();
    assert_eq!(fs::metadata(fx.p("f")).unwrap().len(), 5);
    assert_eq!(fs::read(fx.p("f")).unwrap(), b"three");

    fs::metadata(fx.p("p")).unwrap();
    fx.vfs
        .rename(ROOT_INO, b"p", ROOT_INO, b"q", Default::default())
        .unwrap();
    assert!(fs::metadata(fx.p("p")).is_ok(), "names are cached");
    fx.mount().invalidate_entry(ROOT_INO, "p".as_ref()).unwrap();
    assert_eq!(errno(fs::metadata(fx.p("p"))), libc::ENOENT);
    assert!(fs::metadata(fx.p("q")).is_ok());
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn negative_answers_expire_by_negative_ttl() {
    let Some(fx) = Fixture::new("neg_ttl=2") else {
        return;
    };
    assert_eq!(errno(fs::metadata(fx.p("late"))), libc::ENOENT);
    let a = fx.vfs.create(ROOT_INO, b"late", 0o644).unwrap();
    assert_eq!(
        errno(fs::metadata(fx.p("late"))),
        libc::ENOENT,
        "negative answer is cached"
    );
    std::thread::sleep(std::time::Duration::from_millis(2200));
    assert_eq!(fs::metadata(fx.p("late")).unwrap().ino(), a.ino);
}

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse -- --ignored --test-threads=1"]
fn zero_ttl_sees_changes_made_behind_the_mount() {
    let Some(fx) = Fixture::new("ttl=0,noneg,nokeep_cache") else {
        return;
    };
    assert_eq!(errno(fs::metadata(fx.p("late"))), libc::ENOENT);
    let a = fx.vfs.create(ROOT_INO, b"late", 0o644).unwrap();
    assert_eq!(fs::metadata(fx.p("late")).unwrap().ino(), a.ino);
    fx.vfs.write(a.ino, 0, b"abc").unwrap();
    assert_eq!(fs::metadata(fx.p("late")).unwrap().len(), 3);
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
fn dropping_the_mount_unmounts_it() {
    let Some(fx) = Fixture::new("fsname=droptest") else {
        return;
    };
    assert!(is_mounted(&fx.dir));
    let dir = fx.dir.clone();
    let Fixture { mount, _tmp, .. } = fx;
    drop(mount);
    assert!(!is_mounted(&dir));
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
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
    let t = std::thread::spawn(move || crate::run(vfs, d, MountOptions::default()));
    for _ in 0..100 {
        if is_mounted(&dir) {
            break;
        }
        assert!(!t.is_finished(), "run returned before the mount appeared");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(is_mounted(&dir));
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
    assert!(matches!(r, Err(crate::MountError::Io(_))));
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

#[test]
#[ignore = "needs FUSE: cargo test -p cowfs-fuse --release -- --ignored --test-threads=1 --nocapture bench"]
fn bench_latency_floor() {
    for (label, opts) in [
        ("default options", ""),
        ("ttl=0,noneg (every op reaches the adapter)", "ttl=0,noneg"),
    ] {
        let Some(fx) = Fixture::new(opts) else { return };
        let report = crate::bench::measure(&fx.dir, &crate::bench::Config::default()).unwrap();
        println!("== bench, MemVfs, {label}\n{report}");
    }
}
