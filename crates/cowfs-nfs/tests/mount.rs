//! Mount tests: a real `mount_nfs` of a `MemVfs`. All `#[ignore]`d; they skip cleanly where
//! `mount_nfs` is unusable. Run on macOS with:
//!
//! `cargo test -p cowfs-nfs --test mount -j4 -- --ignored --test-threads=1 --nocapture`
//!
//! Needs `python3` for the batteries in `tests/battery/` and `cargo` for the build check.
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{symlink, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use cowfs_nfs::{is_listed, mount_nfs_available, Mount, MountOptions};
use cowfs_vfs_test::MemVfs;

struct Mounted {
    mount: Option<Mount>,
    _dir: tempfile::TempDir,
}

impl Mounted {
    fn path(&self) -> &Path {
        self.mount.as_ref().unwrap().mountpoint()
    }

    fn finish(mut self) {
        let path = self.path().to_path_buf();
        self.mount.take().unwrap().unmount().unwrap();
        let table = Command::new("/sbin/mount").output().unwrap();
        assert!(
            !is_listed(&String::from_utf8_lossy(&table.stdout), &path),
            "still mounted"
        );
    }
}

fn mounted(opts: MountOptions) -> Option<Mounted> {
    if !mount_nfs_available() {
        eprintln!("SKIP: mount_nfs is not available");
        return None;
    }
    let dir = tempfile::Builder::new()
        .prefix("cowfs-nfs-")
        .tempdir()
        .unwrap();
    match Mount::new(Arc::new(MemVfs::new()), &dir.path().join("mnt"), opts) {
        Ok(m) => Some(Mounted {
            mount: Some(m),
            _dir: dir,
        }),
        Err(e) => {
            eprintln!("SKIP: cannot mount: {e}");
            None
        }
    }
}

fn battery(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/battery")
        .join(name)
}

/// Runs `cmd` to completion or kills it after `secs`; returns (success, combined output).
fn run_limited(cmd: &mut Command, secs: u64) -> (bool, String) {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let o = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        s
    });
    let e = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + Duration::from_secs(secs);
    let ok = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st.success();
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break false;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut text = o.join().unwrap();
    text.push_str(&e.join().unwrap());
    (ok, text)
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn std_fs_semantics_through_the_mount() {
    let Some(m) = mounted(MountOptions::default()) else {
        return;
    };
    let root = m.path().to_path_buf();

    fs::write(root.join("a"), b"hello").unwrap();
    assert_eq!(fs::read(root.join("a")).unwrap(), b"hello");
    let md = fs::metadata(root.join("a")).unwrap();
    assert_eq!((md.len(), md.nlink(), md.mode() & 0o777), (5, 1, 0o644));

    fs::create_dir_all(root.join("d/e")).unwrap();
    fs::write(root.join("d/e/f"), "x").unwrap();
    fs::rename(root.join("d"), root.join("d2")).unwrap();
    assert_eq!(fs::read_to_string(root.join("d2/e/f")).unwrap(), "x");
    assert!(fs::remove_dir(root.join("d2")).is_err(), "not empty");
    fs::remove_dir_all(root.join("d2")).unwrap();

    symlink("a", root.join("l")).unwrap();
    symlink("nowhere", root.join("dangling")).unwrap();
    assert_eq!(fs::read_link(root.join("l")).unwrap(), Path::new("a"));
    assert_eq!(fs::read(root.join("l")).unwrap(), b"hello");
    assert!(fs::symlink_metadata(root.join("dangling"))
        .unwrap()
        .is_symlink());

    fs::hard_link(root.join("a"), root.join("a2")).unwrap();
    assert_eq!(fs::metadata(root.join("a")).unwrap().nlink(), 2);
    assert_eq!(
        fs::metadata(root.join("a")).unwrap().ino(),
        fs::metadata(root.join("a2")).unwrap().ino()
    );
    fs::remove_file(root.join("a")).unwrap();
    assert_eq!(fs::read(root.join("a2")).unwrap(), b"hello");

    let f = fs::OpenOptions::new()
        .write(true)
        .open(root.join("a2"))
        .unwrap();
    f.set_len(2).unwrap();
    f.set_len(6).unwrap();
    assert_eq!(fs::read(root.join("a2")).unwrap(), b"he\0\0\0\0");
    f.write_all_at(b"YY", 4).unwrap();
    drop(f);
    assert_eq!(fs::read(root.join("a2")).unwrap(), b"he\0\0YY");

    assert!(fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join("a2"))
        .is_err());
    fs::write(root.join("victim"), "old").unwrap();
    fs::write(root.join("new"), "new").unwrap();
    fs::rename(root.join("new"), root.join("victim")).unwrap();
    assert_eq!(fs::read_to_string(root.join("victim")).unwrap(), "new");

    let mut open = fs::File::open(root.join("victim")).unwrap();
    fs::remove_file(root.join("victim")).unwrap();
    let mut s = String::new();
    open.read_to_string(&mut s).unwrap();
    assert_eq!(s, "new", "unlink while open");
    drop(open);

    let mtime = filetime_secs(&root.join("a2"), 1_700_000_000);
    assert_eq!(mtime, 1_700_000_000);

    for i in 0..2000 {
        fs::write(root.join(format!("many-{i:05}")), "").unwrap();
    }
    let mut n = 0;
    for e in fs::read_dir(&root).unwrap() {
        n += usize::from(
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("many-"),
        );
    }
    assert_eq!(n, 2000);
    m.finish();
}

fn filetime_secs(p: &Path, secs: i64) -> i64 {
    let f = fs::File::options().write(true).open(p).unwrap();
    let t = std::time::UNIX_EPOCH + Duration::from_secs(secs as u64);
    f.set_modified(t).unwrap();
    drop(f);
    fs::metadata(p).unwrap().mtime()
}

#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn read_only_modes_stay_writable_for_the_owner() {
    let Some(m) = mounted(MountOptions::default()) else {
        return;
    };
    let root = m.path().to_path_buf();
    for mode in [0o444, 0o400] {
        for i in 0..20 {
            let p = root.join(format!("ro-{mode:o}-{i}"));
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(mode)
                .open(&p)
                .unwrap();
            f.write_all(&vec![7u8; 1 << 20]).unwrap();
            f.sync_all().unwrap();
            drop(f);
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, mode);
            assert_eq!(fs::metadata(&p).unwrap().len(), 1 << 20);
        }
    }
    m.finish();
}

#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn appledouble_sidecars_are_hidden_by_default() {
    let Some(m) = mounted(MountOptions::default()) else {
        return;
    };
    let root = m.path().to_path_buf();
    fs::write(root.join("doc"), "x").unwrap();
    fs::write(root.join("._doc"), "sidecar").unwrap();
    let names: Vec<String> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["doc"]);
    assert_eq!(fs::read_to_string(root.join("._doc")).unwrap(), "sidecar");
    fs::remove_file(root.join("doc")).unwrap();
    assert!(!root.join("._doc").exists());
    m.finish();
}

#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn drop_unmounts_and_a_second_mount_on_the_same_path_is_refused() {
    let Some(m) = mounted(MountOptions::default()) else {
        return;
    };
    let path = m.path().to_path_buf();
    let second = Mount::new(Arc::new(MemVfs::new()), &path, MountOptions::default());
    assert!(matches!(
        second,
        Err(cowfs_nfs::MountError::AlreadyMounted(_))
    ));
    drop(m);
    let table = Command::new("/sbin/mount").output().unwrap();
    assert!(
        !is_listed(&String::from_utf8_lossy(&table.stdout), &path),
        "drop left a mount behind"
    );
}

#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn python_battery() {
    let Some(m) = mounted(MountOptions::default()) else {
        return;
    };
    let (ok, out) = run_limited(
        Command::new("python3")
            .arg(battery("test_mount.py"))
            .arg(m.path()),
        1200,
    );
    println!("{out}");
    let summary = tail(&out, 1);
    let failed: Vec<&str> = out
        .lines()
        .filter(|l| l.starts_with("FAIL") || l.starts_with("ERR"))
        .collect();
    assert!(ok, "battery did not finish: {}", tail(&out, 8));
    assert!(failed.is_empty(), "failing checks: {failed:?}");
    assert!(summary.contains("passed. Not passing: []"), "{summary}");
    m.finish();
}

#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn hardlink_pair_readdir() {
    let Some(m) = mounted(MountOptions::default()) else {
        return;
    };
    let (ok, out) = run_limited(
        Command::new("python3")
            .arg(battery("test_hardlink_readdir.py"))
            .arg(m.path()),
        600,
    );
    println!("{out}");
    assert!(
        ok && out.contains("PASS hardlink readdir"),
        "{}",
        tail(&out, 5)
    );
    m.finish();
}

#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn mmap_without_msync_is_durable() {
    let Some(m) = mounted(MountOptions::default()) else {
        return;
    };
    for mode in ["nosync", "msync", "fsync"] {
        let (ok, out) = run_limited(
            Command::new("python3")
                .arg(battery("mmap_race.py"))
                .arg(m.path().join(format!("mm-{mode}")))
                .arg("60")
                .arg(mode),
            600,
        );
        println!("{out}");
        assert!(
            ok && out.contains("size_mismatch=0 (tail_only=0) content_mismatch=0"),
            "{mode}: {}",
            tail(&out, 5)
        );
    }
    m.finish();
}

#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn cargo_build_on_the_mount() {
    let Some(m) = mounted(MountOptions::default()) else {
        return;
    };
    let krate = m.path().join("hello");
    fs::create_dir_all(krate.join("src")).unwrap();
    fs::write(
        krate.join("Cargo.toml"),
        "[package]\nname = \"hello\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(
        krate.join("src/main.rs"),
        "fn main() { println!(\"hi\"); }\n",
    )
    .unwrap();
    for pass in 0..2 {
        if pass == 1 {
            fs::write(
                krate.join("src/main.rs"),
                "fn main() { println!(\"hi again\"); }\n",
            )
            .unwrap();
        }
        let (ok, out) = run_limited(
            Command::new("cargo")
                .args(["run", "-j2", "--quiet"])
                .env("CARGO_INCREMENTAL", "1")
                .current_dir(&krate),
            600,
        );
        assert!(ok, "pass {pass}: {}", tail(&out, 10));
        assert!(
            out.contains(if pass == 0 { "hi" } else { "hi again" }),
            "{out}"
        );
    }
    m.finish();
}
