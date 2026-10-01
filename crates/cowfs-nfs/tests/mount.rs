//! Mount tests: a real `mount_nfs` of a `MemVfs`. All `#[ignore]`d; they skip cleanly where
//! `mount_nfs` is unusable. Run on macOS with:
//!
//! `cargo test -p cowfs-nfs --test mount -j4 -- --ignored --test-threads=1 --nocapture`
//!
//! Needs `python3` for the batteries in `tests/battery/` and `cargo` for the build check.
mod common;

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{symlink, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cowfs_nfs::{is_listed, mount_nfs_available, AppleDoubleMode, Mount, MountOptions};
use cowfs_vfs_test::MemVfs;

/// Force-unmounts the mount point if a test outlives its deadline, so a hung syscall fails with
/// EIO instead of blocking the run. Dropping it (normally or by panic) cancels the timer.
struct Watchdog(Option<mpsc::Sender<()>>);

impl Watchdog {
    fn start(path: PathBuf, secs: u64) -> Watchdog {
        let (tx, rx) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            if rx.recv_timeout(Duration::from_secs(secs)) == Err(mpsc::RecvTimeoutError::Timeout) {
                eprintln!(
                    "WATCHDOG: {} still busy after {secs}s, forcing unmount",
                    path.display()
                );
                let _ = Command::new("/sbin/umount").arg("-f").arg(&path).status();
            }
        });
        Watchdog(Some(tx))
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.0.take();
    }
}

struct Mounted {
    mount: Option<Mount>,
    _dog: Watchdog,
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
    mounted_vfs(Arc::new(MemVfs::new()), opts)
}

fn mounted_vfs(vfs: Arc<dyn cowfs_vfs::Vfs>, opts: MountOptions) -> Option<Mounted> {
    if !mount_nfs_available() {
        eprintln!("SKIP: mount_nfs is not available");
        return None;
    }
    let dir = tempfile::Builder::new()
        .prefix("cowfs-nfs-")
        .tempdir()
        .unwrap();
    match Mount::new(vfs, &dir.path().join("mnt"), opts) {
        Ok(m) => Some(Mounted {
            _dog: Watchdog::start(m.mountpoint().to_path_buf(), 1500),
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
fn readonly_owner_posix_permissions() {
    let native = tempfile::Builder::new()
        .prefix("cowfs-native-permissions-")
        .tempdir()
        .unwrap();
    let Some(m) = mounted(MountOptions::default()) else {
        panic!("permission regression needs a real NFS mount");
    };
    for (label, root) in [("APFS", native.path()), ("NFS", m.path())] {
        let mut cmd = Command::new("python3");
        cmd.arg(battery("readonly_owner.py"))
            .arg(root)
            .arg("--check");
        let (ok, out) = run_limited(&mut cmd, 180);
        println!("{label}: {out}");
        assert!(ok, "{label}: {out}");
    }
    m.finish();
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

#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn references_drain_after_a_workload_on_a_real_mount() {
    let vfs = common::counting::CountingVfs::new();
    let Some(m) = mounted_vfs(vfs.clone(), MountOptions::default()) else {
        return;
    };
    let root = m.path().to_path_buf();
    fs::write(root.join("target"), "v0").unwrap();
    for i in 0..200 {
        fs::write(root.join("tmp"), format!("v{i}")).unwrap();
        fs::rename(root.join("tmp"), root.join("target")).unwrap();
    }
    let n: usize = std::env::var("COWFS_REFBALANCE_FILES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    for i in 0..n {
        let p = root.join(format!("c{i}"));
        fs::write(&p, "x").unwrap();
        fs::remove_file(&p).unwrap();
    }
    fs::create_dir_all(root.join("d/sub")).unwrap();
    fs::write(root.join("d/a"), "a").unwrap();
    fs::hard_link(root.join("d/a"), root.join("d/l")).unwrap();
    fs::rename(root.join("d/sub"), root.join("sub2")).unwrap();
    fs::rename(root.join("d"), root.join("d2")).unwrap();
    fs::remove_file(root.join("d2/a")).unwrap();
    fs::remove_file(root.join("d2/l")).unwrap();
    fs::remove_dir(root.join("d2")).unwrap();
    fs::remove_dir(root.join("sub2")).unwrap();
    fs::remove_file(root.join("target")).unwrap();
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(
        vfs.live(),
        0,
        "inodes still held after everything was removed"
    );
    assert_eq!(vfs.outstanding(), 0, "lookup references not given back");
    m.finish();
}

/// `sh` with extra environment for the script.
fn sh_env(dir: &Path, env: &[(&str, String)], script: &str) -> (bool, String) {
    let mut c = Command::new("/bin/sh");
    c.arg("-c").arg(script).current_dir(dir);
    for (k, v) in env {
        c.env(k, v);
    }
    run_limited(&mut c, 120)
}

/// The mode under test: `Translate` keeps `._` files out of the store, which is what these tests
/// are about. The crate default is `Hide`.
fn translated() -> MountOptions {
    MountOptions {
        appledouble: AppleDoubleMode::Translate,
        ..MountOptions::default()
    }
}

fn sh(dir: &Path, script: &str) -> (bool, String) {
    run_limited(
        Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .current_dir(dir),
        120,
    )
}

#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn xattrs_round_trip_without_sidecar_inodes() {
    let vfs = common::counting::CountingVfs::new();
    let Some(m) = mounted_vfs(vfs.clone(), translated()) else {
        return;
    };
    let root = m.path().to_path_buf();
    let (ok, out) = sh(
        &root,
        r#"set -e
        echo hello > f
        xattr -w user.color blue f
        xattr -w user.big "$(head -c 3000 /dev/zero | tr '\0' x)" f
        test "$(xattr -p user.color f)" = blue
        xattr -l f | grep -q user.color
        xattr -d user.color f
        ! xattr -p user.color f 2>/dev/null
        xattr -p user.big f | wc -c | grep -q 3001
        mkdir d
        xattr -w user.dir yes d
        test "$(xattr -p user.dir d)" = yes
        xattr -w user.k v f
        cp -Rp f g
        test "$(xattr -p user.k g)" = v
        xattr -c g
        test -z "$(xattr -l g | grep user.)"
        mv f h
        test "$(xattr -p user.k h)" = v
        ln h h2
        test "$(xattr -p user.k h2)" = v
        xattr -w user.ln 1 h2
        test "$(xattr -p user.ln h)" = 1
        rm h
        test "$(xattr -p user.k h2)" = v
        ls -A | grep -v '^\._' > /dev/null
        test -z "$(ls -A | grep '^\._' || true)"
        echo many > many
        val=$(printf 'v%.0s' $(seq 1 100))
        i=0
        while [ $i -lt 200 ]; do
            xattr -w "user.a$i" "$val" many
            i=$((i + 1))
        done
        # 200 of ours, plus the com.apple.provenance the client adds to a new file.
        test "$(xattr many | grep -c '^user\.a[0-9]')" = 200
        test "$(xattr many | wc -l | tr -d ' ')" = 201
        xattr -p user.a199 many | wc -c | grep -q 101
        test "$(xattr -l many | grep -c 'user\.a[0-9]')" = 200
        "#,
    );
    println!("{out}");
    assert!(ok, "xattr script failed: {}", tail(&out, 12));
    let f = vfs.inner();
    assert!(
        vfs.appledouble_names().is_empty(),
        "sidecars were stored: {:?}",
        vfs.appledouble_names()
    );
    let _ = f;
    m.finish();
}

/// Runs as a child of the signal tests below: mounts, announces itself and waits to be killed.
#[test]
#[ignore = "helper process of the signal tests"]
fn child_host() {
    let Ok(dir) = std::env::var("COWFS_CHILD_MOUNT") else {
        return;
    };
    Mount::install_signal_cleanup().unwrap();
    let m = Mount::new(
        Arc::new(MemVfs::new()),
        Path::new(&dir),
        MountOptions::default(),
    )
    .unwrap();
    println!("READY {}", m.mountpoint().display());
    std::io::stdout().flush().unwrap();
    std::thread::sleep(Duration::from_secs(120));
}

struct Child {
    child: std::process::Child,
    lines: mpsc::Receiver<String>,
    mountpoint: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_host() -> Option<Child> {
    spawn_host_with(MountOptions::default())
}

/// `child_host` with mount options, for the tests that need a particular mount.
fn spawn_host_with(opts: MountOptions) -> Option<Child> {
    if !mount_nfs_available() {
        eprintln!("SKIP: mount_nfs is not available");
        return None;
    }
    let dir = tempfile::Builder::new()
        .prefix("cowfs-nfs-host-")
        .tempdir()
        .unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_host",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("COWFS_CHILD_MOUNT", dir.path().join("mnt"))
        .env(
            "COWFS_CHILD_OPTS",
            format!("{}|{}", opts.soft, opts.appledouble as u8),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let (tx, lines) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for l in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            let _ = tx.send(l);
        }
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match lines.recv_timeout(left) {
            Ok(l) => {
                if let Some(p) = l.split("READY ").nth(1) {
                    return Some(Child {
                        child,
                        lines,
                        mountpoint: PathBuf::from(p.trim()),
                        _dir: dir,
                    });
                }
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the child host never became ready");
            }
        }
    }
}

fn mount_table_has(path: &Path) -> bool {
    let out = Command::new("/sbin/mount").output().unwrap();
    is_listed(&String::from_utf8_lossy(&out.stdout), path)
}

fn wait_exit(child: &mut std::process::Child, secs: u64) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if let Some(st) = child.try_wait().unwrap() {
            return Some(st);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn sigterm_of_the_host_unmounts_before_it_exits() {
    let Some(mut h) = spawn_host() else {
        return;
    };
    assert!(mount_table_has(&h.mountpoint));
    std::fs::write(h.mountpoint.join("f"), "x").unwrap();
    let pid = h.child.id().to_string();
    assert!(Command::new("/bin/kill")
        .args(["-TERM", &pid])
        .status()
        .unwrap()
        .success());
    let status = wait_exit(&mut h.child, 60);
    if status.is_none() {
        let _ = h.child.kill();
        let _ = Command::new("/sbin/umount")
            .arg("-f")
            .arg(&h.mountpoint)
            .status();
    }
    assert_eq!(
        status.and_then(|s| s.code()),
        Some(128 + 15),
        "the host exits after unmounting"
    );
    assert!(
        !mount_table_has(&h.mountpoint),
        "a dead host left its mount behind"
    );
    let (ok, out) = run_limited(Command::new("/bin/ls").arg(&h.mountpoint), 10);
    assert!(ok, "ls hung or failed on the leftover path: {out}");
    let _ = &h.lines;
}

#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn a_killed_host_leaves_a_mount_that_sweep_removes() {
    let Some(mut h) = spawn_host() else {
        return;
    };
    let other = mounted(MountOptions::default());
    let pid = h.child.id().to_string();
    assert!(Command::new("/bin/kill")
        .args(["-KILL", &pid])
        .status()
        .unwrap()
        .success());
    assert!(wait_exit(&mut h.child, 30).is_some());
    assert!(
        mount_table_has(&h.mountpoint),
        "SIGKILL cannot clean up: the mount is stale"
    );

    let prefix = h.mountpoint.parent().unwrap().to_path_buf();
    let dog = Watchdog::start(h.mountpoint.clone(), 120);
    let swept = cowfs_nfs::sweep_stale_mounts(&prefix).unwrap();
    drop(dog);
    assert_eq!(
        swept,
        vec![h.mountpoint.canonicalize().unwrap_or(h.mountpoint.clone())]
    );
    assert!(!mount_table_has(&h.mountpoint));
    let (ok, out) = run_limited(Command::new("/bin/ls").arg(&h.mountpoint), 10);
    assert!(ok, "ls hung after the sweep: {out}");

    if let Some(o) = other {
        let live = o.path().to_path_buf();
        let swept = cowfs_nfs::sweep_stale_mounts(live.parent().unwrap()).unwrap();
        assert!(swept.is_empty(), "a live mount was swept: {swept:?}");
        assert!(mount_table_has(&live));
        o.finish();
    }
}

#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn copy_tools_carry_xattrs_and_leave_no_sidecar_inodes() {
    let vfs = common::counting::CountingVfs::new();
    let Some(m) = mounted_vfs(vfs.clone(), translated()) else {
        return;
    };
    let root = m.path().to_path_buf();
    let (ok, out) = sh(
        &root,
        r#"set -e
        mkdir src src/sub
        echo data > src/a
        xattr -w user.k v src/a
        echo x > src/sub/b
        xattr -w user.z 9 src/sub/b
        cp -Rp src cp1
        test "$(xattr -p user.k cp1/a)" = v
        test "$(xattr -p user.z cp1/sub/b)" = 9
        tar cf t.tar src
        mkdir tarx
        tar xf t.tar -C tarx
        test "$(xattr -p user.k tarx/src/a)" = v
        rsync -aX src/ rs/
        test "$(xattr -p user.k rs/a)" = v
        test "$(xattr -p user.z rs/sub/b)" = 9
        ditto src dt
        test "$(xattr -p user.k dt/a)" = v
        mkdir g
        cd g
        git init -q .
        echo 1 > f
        git add f
        git -c user.email=a@b -c user.name=n commit -qm m
        xattr -w user.q 1 f
        test -z "$(git status --porcelain)"
        cd ..
        for d in . src src/sub cp1 tarx/src rs dt g; do
            test -z "$(ls -A $d | grep '^\._' || true)"
        done
        echo hello > prov
        xattr -l prov | grep -q com.apple.provenance
        "#,
    );
    println!("{out}");
    assert!(ok, "script failed: {}", tail(&out, 12));
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        vfs.appledouble_names().is_empty(),
        "sidecars reached the Vfs: {:?}",
        vfs.appledouble_names()
    );
    let inner = vfs.inner();
    let lookup =
        |parent: u64, name: &[u8]| cowfs_vfs::Vfs::lookup(inner, parent, name).unwrap().ino;
    let src = lookup(cowfs_vfs::ROOT_INO, b"src");
    let a = lookup(src, b"a");
    let names = cowfs_vfs::Vfs::listxattr(inner, a).unwrap();
    assert!(
        names.contains(&b"user.k".to_vec()),
        "xattr not stored on the file: {names:?}"
    );
    let prov = lookup(cowfs_vfs::ROOT_INO, b"prov");
    let names = cowfs_vfs::Vfs::listxattr(inner, prov).unwrap();
    assert!(
        names.contains(&b"com.apple.provenance".to_vec()),
        "{names:?}"
    );
    m.finish();
}

#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn hide_mode_still_stores_and_hides_sidecars() {
    let vfs = common::counting::CountingVfs::new();
    let opts = MountOptions {
        appledouble: AppleDoubleMode::Hide,
        ..MountOptions::default()
    };
    let Some(m) = mounted_vfs(vfs.clone(), opts) else {
        return;
    };
    let root = m.path().to_path_buf();
    fs::write(root.join("doc"), "x").unwrap();
    let names: Vec<String> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["doc"], "listing hides the sidecar");
    assert!(
        !vfs.appledouble_names().is_empty(),
        "the client's sidecar is stored in Hide mode"
    );
    fs::remove_file(root.join("doc")).unwrap();
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(vfs.live(), 0, "removing the file removed its sidecar too");
    m.finish();
}

fn total_rpcs(stats: &str) -> u64 {
    stats
        .lines()
        .find_map(|l| l.strip_prefix("TOTAL "))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

#[test]
#[ignore = "mounts a filesystem; prints a table"]
fn rpcs_per_operation_by_appledouble_mode() {
    println!("| mode | create+write+close | open+write+close (existing) | inodes for 200 files |");
    println!("|---|---|---|---|");
    for (name, mode) in [
        ("Translate", AppleDoubleMode::Translate),
        ("Hide", AppleDoubleMode::Hide),
        ("Store", AppleDoubleMode::Store),
    ] {
        let opts = MountOptions {
            appledouble: mode,
            ..MountOptions::default()
        };
        let vfs = common::counting::CountingVfs::new();
        let Some(m) = mounted_vfs(vfs.clone(), opts) else {
            return;
        };
        let root = m.path().to_path_buf();
        let n = 100u64;
        for i in 0..n {
            fs::write(root.join(format!("e{i}")), b"x").unwrap();
        }
        std::thread::sleep(Duration::from_secs(1));
        cowfs_nfs::take_stats();
        for i in 0..n {
            fs::write(root.join(format!("c{i}")), vec![1u8; 4096]).unwrap();
        }
        let create = total_rpcs(&cowfs_nfs::take_stats());
        for i in 0..n {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .open(root.join(format!("e{i}")))
                .unwrap();
            f.write_all(&[2u8; 4096]).unwrap();
        }
        let existing = total_rpcs(&cowfs_nfs::take_stats());
        println!(
            "| {name} | {:.1} | {:.1} | {} |",
            create as f64 / n as f64,
            existing as f64 / n as f64,
            vfs.live()
        );
        m.finish();
    }
}

/// Mixed metadata workload for `COWFS_SOAK_SECS` seconds (default 600), printing the resident
/// memory of this process (server, file system and test) every minute:
/// `COWFS_SOAK_SECS=600 cargo test -p cowfs-nfs --release --test mount -- --ignored --nocapture rss_soak`
#[test]
#[ignore = "10 minute soak; run by hand"]
fn rss_soak() {
    let secs: u64 = std::env::var("COWFS_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let vfs = common::counting::CountingVfs::new();
    let Some(m) = mounted_vfs(vfs.clone(), MountOptions::default()) else {
        return;
    };
    let root = m.path().to_path_buf();
    let start = Instant::now();
    let mut next_sample = Duration::ZERO;
    let mut i: u64 = 0;
    let mut samples = Vec::new();
    while start.elapsed() < Duration::from_secs(secs) {
        if start.elapsed() >= next_sample {
            let rss = common::rss_bytes() >> 20;
            println!(
                "SOAK t={:>4}s iterations={i:>8} rss={rss} MiB live_inodes={}",
                start.elapsed().as_secs(),
                vfs.live()
            );
            samples.push(rss);
            next_sample += Duration::from_secs(60);
        }
        fs::write(root.join("tmp"), format!("v{i}")).unwrap();
        fs::rename(root.join("tmp"), root.join("target")).unwrap();
        let p = root.join("scratch");
        fs::write(&p, "x").unwrap();
        fs::hard_link(&p, root.join("scratch2")).unwrap();
        fs::remove_file(&p).unwrap();
        fs::remove_file(root.join("scratch2")).unwrap();
        fs::create_dir_all(root.join("d/e")).unwrap();
        fs::write(root.join("d/e/f"), "y").unwrap();
        fs::rename(root.join("d"), root.join("d2")).unwrap();
        fs::remove_dir_all(root.join("d2")).unwrap();
        if i.is_multiple_of(200) {
            let (ok, out) = sh(&root, "xattr -w user.n 1 target && xattr -d user.n target");
            assert!(ok, "{out}");
        }
        i += 1;
    }
    fs::remove_file(root.join("target")).unwrap();
    std::thread::sleep(Duration::from_secs(2));
    println!("SOAK done: {i} iterations, samples (MiB) {samples:?}");
    assert_eq!(vfs.live(), 0, "inodes left after the soak");
    assert_eq!(vfs.outstanding(), 0);
    let first = samples.get(1).copied().unwrap_or(0);
    let last = samples.last().copied().unwrap_or(0);
    assert!(
        last <= first + first / 2 + 20,
        "RSS kept growing: {samples:?}"
    );
    m.finish();
}

/// Builds a zip and a tar on the local file system that hold `__MACOSX/._name` entries, and a git
/// repository that tracks `._name` files, so the extraction and checkout cases of the round 2
/// review can be run against a real mount in any AppleDouble mode.
fn make_archives(dir: &Path) {
    let src = dir.join("src");
    fs::create_dir_all(src.join("__MACOSX/pkg")).unwrap();
    fs::create_dir_all(src.join("pkg")).unwrap();
    fs::write(src.join("pkg/x.txt"), "hello\n").unwrap();
    fs::write(src.join("__MACOSX/pkg/._x.txt"), "sidecar bytes\n").unwrap();
    fs::write(src.join("__MACOSX/._lonely"), "no main file\n").unwrap();
    let mut z = Command::new("/usr/bin/zip");
    z.arg("-q")
        .arg("-r")
        .arg(dir.join("a.zip"))
        .arg("__MACOSX")
        .arg("pkg")
        .current_dir(&src);
    assert!(z.status().unwrap().success(), "zip");
    let mut t = Command::new("/usr/bin/tar");
    t.arg("cf")
        .arg(dir.join("a.tar"))
        .arg("__MACOSX")
        .arg("pkg")
        .current_dir(&src);
    assert!(t.status().unwrap().success(), "tar");
}

fn make_repo(dir: &Path) {
    let repo = dir.join("repo");
    fs::create_dir_all(repo.join("sub")).unwrap();
    fs::write(repo.join("._x"), "tracked sidecar\n").unwrap();
    fs::write(repo.join("._lone"), "tracked, no main file\n").unwrap();
    fs::write(repo.join("sub/._a"), "tracked in a subdirectory\n").unwrap();
    fs::write(repo.join("normal"), "ordinary file\n").unwrap();
    let git = |args: &[&str]| {
        let ok = Command::new("/opt/homebrew/bin/git")
            .args(args)
            .current_dir(&repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "."]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "m"]);
}

/// The extraction and checkout cases of the round 2 review, in every mode. `._` files that arrive
/// before the file they belong to must not make the tool fail.
#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn archives_and_checkouts_of_dot_underscore_files_work_in_every_mode() {
    for mode in [
        AppleDoubleMode::Translate,
        AppleDoubleMode::Hide,
        AppleDoubleMode::Store,
    ] {
        let vfs = common::counting::CountingVfs::new();
        let opts = MountOptions {
            appledouble: mode,
            ..MountOptions::default()
        };
        let Some(m) = mounted_vfs(vfs.clone(), opts) else {
            return;
        };
        let root = m.path().to_path_buf();
        let native = tempfile::Builder::new()
            .prefix("cowfs-archives-")
            .tempdir()
            .unwrap();
        make_archives(native.path());
        make_repo(native.path());
        fs::copy(native.path().join("a.zip"), root.join("a.zip")).unwrap();
        fs::copy(native.path().join("a.tar"), root.join("a.tar")).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("/opt/homebrew/bin/git")
                .args(args)
                .current_dir(&root)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .output()
                .unwrap();
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            )
        };

        let (ok, out) = sh_env(
            &root,
            &[("MODE", format!("{mode:?}"))],
            r#"set -e
            mkdir zt tt
            cd zt && unzip -q ../a.zip && cd ..
            test -f zt/pkg/x.txt
            cd tt && tar xf ../a.tar && cd ..
            test -f tt/pkg/x.txt
            mkdir dt
            (cd dt && ditto -xk ../a.zip .)
            test -f dt/pkg/x.txt
            "#,
        );
        assert!(ok, "{mode:?}: {out}");

        let (ok, err) = git(&[
            "clone",
            "-q",
            &native.path().join("repo").to_string_lossy(),
            "clone",
        ]);
        assert!(ok, "{mode:?}: git clone failed: {err}");
        let (ok, out) = sh_env(
            &root,
            &[("MODE", format!("{mode:?}"))],
            r#"set -e
            test -f clone/._x
            test -f clone/._lone
            test -f clone/sub/._a
            test -f clone/normal
            cd clone
            if [ "$MODE" != Store ]; then
                # Store shows whatever the client wrote as a sidecar, so a checkout can leave
                # untracked ._ files behind. That is what Store is for, so nothing is asserted
                # about the status there.
                test -z "$(git status --porcelain)"
            fi
            "#,
        );
        assert!(ok, "{mode:?}: checkout: {out}");
        // Whatever the mode, every file the archives and the repository held is readable back.
        let (ok, out) = sh(
            &root,
            r#"set -e
            test -f zt/__MACOSX/._lonely
            test -f zt/__MACOSX/pkg/._x.txt
            test -f clone/._x
            test -f clone/._lone
            test -f clone/sub/._a
            test "$(cat zt/pkg/x.txt)" = hello
            test "$(cat clone/._x)" = "tracked sidecar"
            "#,
        );
        assert!(ok, "{mode:?}: contents: {out}");
        m.finish();
    }
}

/// What a dead server costs the caller, with and without `soft`, and what frees the caller.
/// Prints a table. Asserts only what holds on this system: no mount option bounds the wait, so
/// the cure is the signal handler and the sweep, both of which are tested elsewhere.
#[test]
#[ignore = "mounts a filesystem and kills it; run with --ignored"]
fn a_dead_server_costs_the_caller_bounded_time_when_soft() {
    println!("| options | ls on an uncached path | touch on an uncached path | after the sweep |");
    println!("|---|---|---|---|");
    for (name, soft) in [("hard", false), ("soft,timeo=6,retrans=2", true)] {
        let opts = MountOptions {
            soft,
            timeo: 6,
            retrans: 2,
            ..MountOptions::default()
        };
        let Some(mut h) = spawn_host_with(opts) else {
            return;
        };
        let table = Command::new("/usr/bin/nfsstat")
            .args(["-m"])
            .output()
            .unwrap()
            .stdout
            .into_iter()
            .map(char::from)
            .collect::<String>();
        let listed = table.contains("soft") || table.contains("hard");
        let pid = h.child.id().to_string();
        assert!(Command::new("/bin/kill")
            .args(["-KILL", &pid])
            .status()
            .unwrap()
            .success());
        assert!(wait_exit(&mut h.child, 30).is_some());
        let dog = Watchdog::start(h.mountpoint.clone(), 300);
        let (ls_ok, _) = run_limited(Command::new("/bin/ls").arg(h.mountpoint.join("nosuch")), 20);
        let t = Instant::now();
        let (touch_ok, _) = run_limited(
            Command::new("/usr/bin/touch").arg(h.mountpoint.join("new")),
            20,
        );
        let secs = t.elapsed().as_secs_f64();
        let prefix = h.mountpoint.parent().unwrap().to_path_buf();
        let swept = cowfs_nfs::sweep_stale_mounts(&prefix).unwrap();
        let (after_ok, _) =
            run_limited(Command::new("/bin/ls").arg(h.mountpoint.join("nosuch")), 10);
        println!(
            "| {name} (soft shown: {listed}) | {} | {:.0}s {} | ls {} |",
            if ls_ok { "answered" } else { "hung" },
            secs,
            if touch_ok { "ok" } else { "hung" },
            if after_ok { "answered" } else { "hung" },
        );
        drop(dog);
        assert!(!swept.is_empty(), "the sweep must find the dead mount");
        assert!(
            !mount_table_has(&h.mountpoint),
            "and the mount table must no longer list it"
        );
        eprintln!("{name}: after the sweep ls on the old path answered: {after_ok}");
    }
}
