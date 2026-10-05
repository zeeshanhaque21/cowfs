//! The server requirements issue #19 lists, reproduced against the real backing filesystem.
//!
//! `protocol.rs` proves them against `MemVfs`, which answers whatever it is asked and can never
//! get a syscall wrong. The failures in #19 were all in the server's own syscalls, so these run
//! the same procedures over `PathVfs` on a scratch directory on the host, and check the result by
//! looking at that directory natively. Each group has a native control that shows the check can
//! see a symlink being followed, so a test that passes for the wrong reason fails here.
mod common;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, UNIX_EPOCH};

use common::*;
use cowfs_nfs::MountOptions;
use cowfs_vfs_path::PathVfs;
use nfsserve::nfs::{ftype3, nfs_fh3};

struct Scratch {
    dir: tempfile::TempDir,
    /// What the server is given.
    backing: PathBuf,
    /// A second tree the test only touches natively, as the control.
    native: PathBuf,
}

fn scratch() -> Scratch {
    let dir = tempfile::tempdir().expect("scratch");
    let backing = dir.path().join("backing");
    let native = dir.path().join("native");
    fs::create_dir(&backing).unwrap();
    fs::create_dir(&native).unwrap();
    Scratch {
        dir,
        backing,
        native,
    }
}

/// A server over the real filesystem, driven by the raw client. No mount, no daemon.
fn serve_backing(root: &Path) -> (cowfs_nfs::Server, Nfs) {
    serve(
        Arc::new(PathVfs::new(root).expect("open the backing directory")),
        MountOptions::default(),
    )
}

fn mode_of(p: &Path) -> u32 {
    fs::symlink_metadata(p).expect("lstat").permissions().mode() & 0o777
}

fn mtime_of(p: &Path) -> (i64, u32) {
    let t = fs::symlink_metadata(p)
        .expect("lstat")
        .modified()
        .expect("mtime")
        .duration_since(UNIX_EPOCH)
        .expect("after 1970");
    (t.as_secs() as i64, t.subsec_nanos())
}

fn listing_nlink(c: &mut Nfs, name: &str) -> u32 {
    let root = c.root.clone();
    let all = c.list(&root, true, 4096);
    let hits: Vec<&common::Listed> = all.iter().filter(|e| e.name == name).collect();
    assert_eq!(hits.len(), 1, "{name} is listed {} times", hits.len());
    hits[0].attr.expect("READDIRPLUS carries attributes").nlink
}

/// A native `chmod` through a symlink lands on the target. Without this the tests below would
/// pass on any server that never touched the link at all, including one that ignored SETATTR.
#[test]
fn a_native_chmod_through_a_symlink_lands_on_the_target() {
    let s = scratch();
    fs::write(s.native.join("target"), b"x").unwrap();
    std::os::unix::fs::symlink("target", s.native.join("link")).unwrap();
    fs::set_permissions(s.native.join("link"), fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        (
            mode_of(&s.native.join("link")),
            mode_of(&s.native.join("target"))
        ),
        (0o755, 0o600),
        "a followed chmod changed the target and left the link alone"
    );
}

#[test]
fn setattr_gives_a_symlink_its_own_times_and_mode_over_the_real_filesystem() {
    let s = scratch();
    let (_server, mut c) = serve_backing(&s.backing);
    let root = c.root.clone();

    let target = c.create_file(&root, "target");
    assert_eq!(c.setattr(&target, sattr_mode(0o644)).0, OK);
    let (st, link) = c.symlink(&root, "link", "target");
    assert_eq!(st, OK);
    let link = link.unwrap();
    let (_, dangling) = c.symlink(&root, "dangling", "missing");
    let dangling = dangling.unwrap();

    let before = mtime_of(&s.backing.join("target"));
    let (st, after) = c.setattr(&link, sattr_mtime(1000, 5));
    assert_eq!(st, OK, "a valid symlink takes its own times");
    assert_eq!(
        (after.unwrap().mtime.seconds, after.unwrap().mtime.nseconds),
        (1000, 5)
    );
    assert_eq!(
        mtime_of(&s.backing.join("target")),
        before,
        "the target's own mtime on disk is untouched"
    );
    assert_eq!(
        mtime_of(&s.backing.join("link")),
        (1000, 5),
        "the link's own mtime on disk is what was set"
    );

    // #19: rsync of a tree with dangling links onto the mount exited 23 because this was ENOENT.
    let (st, a) = c.setattr(&dangling, sattr_mtime(3000, 0));
    assert_eq!(st, OK, "a dangling link takes its own times");
    assert_eq!(a.unwrap().mtime.seconds, 3000);
    assert_eq!(mtime_of(&s.backing.join("dangling")), (3000, 0));

    // #19 called the chmod path untested. It must reach the link, and only the link.
    let (st, a) = c.setattr(&link, sattr_mode(0o600));
    assert_eq!(st, OK);
    assert_eq!(a.unwrap().mode, 0o600);
    assert_eq!(
        (
            mode_of(&s.backing.join("link")),
            mode_of(&s.backing.join("target"))
        ),
        (0o600, 0o644),
        "the link's mode changed on disk and the target's did not"
    );

    let a = c.attrs(&link);
    assert_eq!(a.ftype, ftype3::NF3LNK, "the handle still names the link");
    assert_eq!(
        (a.mode, a.size),
        (0o600, 6),
        "the link keeps its own attributes"
    );
}

/// #19: readdir replies carried a stale `nlink`, so `find -links +1` undercounted. The count in
/// the reply has to follow every name the server itself adds and removes.
#[test]
fn readdir_replies_carry_the_link_count_of_the_moment() {
    let s = scratch();
    let (_server, mut c) = serve_backing(&s.backing);
    let root = c.root.clone();

    let f = c.create_file(&root, "f");
    assert_eq!(c.write(&f, 0, b"hello", 2).0, OK);
    let (st, attrs) = c.link(&f, &root, "g");
    assert_eq!(st, OK);
    assert_eq!(attrs.unwrap().nlink, 2, "LINK reports the new count");

    let native = fs::symlink_metadata(s.backing.join("f"))
        .expect("lstat")
        .nlink();
    assert_eq!(native, 2, "the control: the backing file has two names");
    assert_eq!(listing_nlink(&mut c, "f"), 2, "READDIRPLUS says two");
    assert_eq!(
        listing_nlink(&mut c, "g"),
        2,
        "and the second name of the same file says two, not zero and not one"
    );

    assert_eq!(c.remove(&root, "g"), OK);
    assert_eq!(
        fs::symlink_metadata(s.backing.join("f")).unwrap().nlink(),
        1,
        "the control: one name is left"
    );
    assert_eq!(listing_nlink(&mut c, "f"), 1, "a fresh listing says one");
}

/// The hardlink readdir regression from #19, over the real filesystem rather than `MemVfs`: two
/// names of one file in one directory, listed once each, with no error.
#[test]
fn hardlinked_names_in_one_directory_are_listed_once_each() {
    let s = scratch();
    let (_server, mut c) = serve_backing(&s.backing);
    let root = c.root.clone();
    let f = c.create_file(&root, "a");
    for n in ["b", "c", "d"] {
        assert_eq!(c.link(&f, &root, n).0, OK);
    }
    let all = c.list(&root, true, 4096);
    let names: Vec<String> = all.into_iter().map(|e| e.name).collect();
    for n in ["a", "b", "c", "d"] {
        assert_eq!(
            names.iter().filter(|x| *x == n).count(),
            1,
            "{n} appears once in {names:?}"
        );
    }
    assert_eq!(c.remove(&root, "b"), OK);
    assert_eq!(c.remove(&root, "c"), OK);
    assert_eq!(c.remove(&root, "d"), OK);
    assert_eq!(c.remove(&root, "a"), OK);
    assert_eq!(
        c.list(&root, true, 4096).len(),
        0,
        "the directory is empty again after removing every name"
    );
}

/// #19: "readdir re-reads and re-sorts the whole directory per page, 23 to 71 ms per page on a
/// 10,000 to 15,000 entry `deps/`". The listing cache in `cowfs-vfs-path` was meant to close that.
///
/// The one bound asserted here is the one that separates the two designs: a page taken from a
/// cached listing costs the attributes of its own entries, and a page that re-read the directory
/// costs at least one whole directory read. So the mean of pages two onwards must stay under the
/// cost of a single native directory read of the same directory. That holds with a wide margin
/// either way, and no tighter number is claimed, because this machine is shared.
#[test]
#[ignore = "measurement; run with --ignored --nocapture"]
fn a_large_directory_is_read_once_per_listing() {
    const ENTRIES: usize = 12_000;
    const PER_PAGE: usize = 64;
    let s = scratch();
    fs::create_dir(s.backing.join("deps")).unwrap();
    for i in 0..ENTRIES {
        fs::write(s.backing.join(format!("deps/f{i:05}")), b"x").unwrap();
    }
    let native = s.backing.join("deps");

    // Two native baselines: what a re-reading server pays per page, and what a whole listing costs.
    let t = Instant::now();
    let listed = fs::read_dir(&native).unwrap().count();
    let native_read_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    for e in fs::read_dir(&native).unwrap() {
        let _ = fs::symlink_metadata(e.unwrap().path()).unwrap();
    }
    let native_read_stat_ms = t.elapsed().as_secs_f64() * 1e3;
    assert_eq!(listed, ENTRIES);

    let (_server, mut c) = serve_backing(&s.backing);
    let deps = c.must_lookup(&c.root.clone(), "deps");
    let mut pages: Vec<f64> = Vec::new();
    let mut cookie = 0u64;
    let mut seen = 0usize;
    loop {
        let t = Instant::now();
        let (st, page, eof) = c.readdir_page(&deps, cookie, true, (PER_PAGE * 24) as u32);
        pages.push(t.elapsed().as_secs_f64() * 1e3);
        assert_eq!(st, OK);
        seen += page.len();
        match page.last() {
            Some(l) => cookie = l.cookie,
            None => assert!(eof, "an empty page must end the listing"),
        }
        if eof {
            break;
        }
    }
    assert_eq!(seen, ENTRIES, "every entry exactly once");
    assert!(
        pages.len() > 4,
        "{} pages is not a paged listing",
        pages.len()
    );
    let first = pages[0];
    let mean_rest = pages[1..].iter().sum::<f64>() / (pages.len() - 1) as f64;
    println!(
        "PAGES entries={ENTRIES} pages={} first={first:.2}ms mean_rest={mean_rest:.2}ms \
         native_read={native_read_ms:.2}ms native_read_stat={native_read_stat_ms:.2}ms",
        pages.len()
    );
    assert!(
        mean_rest < native_read_ms,
        "a page costs {mean_rest:.2}ms and reading the directory once costs {native_read_ms:.2}ms: \
         the listing is being re-read per page"
    );
    let _ = native_read_stat_ms;
}
/// what the adapter and the inode table still hold after a namespace churn, so this churns the
/// real filesystem and reads the process back.
#[test]
fn namespace_churn_over_the_real_filesystem_stays_bounded() {
    let s = scratch();
    let (_server, mut c) = serve_backing(&s.backing);
    let root: nfs_fh3 = c.root.clone();
    let iterations = 6_000u32;
    let warm = 600u32;
    let mut at_warm = 0u64;
    for i in 0..iterations {
        assert_eq!(c.mkdir(&root, "d").0, OK);
        let f = c.must_lookup(&root, "d");
        let fh = c.create_file(&f, "f");
        assert_eq!(c.setattr(&fh, sattr_mtime(1_000_000 + i, 0)).0, OK);
        assert_eq!(c.link(&fh, &f, "g").0, OK);
        assert_eq!(c.list(&f, true, 4096).len(), 2);
        assert_eq!(c.remove(&f, "g"), OK);
        assert_eq!(c.remove(&f, "f"), OK);
        assert_eq!(c.rmdir(&root, "d"), OK);
        if i == warm {
            at_warm = rss_bytes();
        }
    }
    let at_end = rss_bytes();
    println!(
        "CHURN iterations={iterations} rss_warm={} MiB rss_end={} MiB growth={} MiB",
        at_warm >> 20,
        at_end >> 20,
        (at_end.saturating_sub(at_warm)) >> 20
    );
    assert_eq!(
        fs::read_dir(&s.backing).unwrap().count(),
        0,
        "nothing is left on disk"
    );
    let growth = at_end.saturating_sub(at_warm);
    assert!(
        growth < 64 << 20,
        "resident memory grew {} MiB over {iterations} churn cycles",
        growth >> 20
    );
}

// The kernel NFS client is the only thing that can still get the requirements above wrong after
// the server answers them: it caches attributes for `actimeo` seconds and it turns a `touch` of a
// symlink into SETATTR on the link's handle. Everything below is a real `mount_nfs` of a private
// server over a private directory. All `#[ignore]`d, like the other mount tests.
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
    mount: Option<cowfs_nfs::Mount>,
    _dog: Watchdog,
    _dir: tempfile::TempDir,
}

impl Mounted {
    fn path(&self) -> PathBuf {
        self.mount.as_ref().unwrap().mountpoint().to_path_buf()
    }

    fn finish(mut self) {
        let path = self.path();
        self.mount.take().unwrap().unmount().unwrap();
        let table = Command::new("/sbin/mount").output().unwrap();
        assert!(
            !cowfs_nfs::is_listed(&String::from_utf8_lossy(&table.stdout), &path),
            "still mounted"
        );
    }
}

fn mounted_backing(backing: &Path, opts: MountOptions) -> Option<(Mounted, PathBuf)> {
    if !cowfs_nfs::mount_nfs_available() {
        eprintln!("SKIP: mount_nfs is not available");
        return None;
    }
    let dir = tempfile::Builder::new()
        .prefix("cowfs-nfs-ready19-")
        .tempdir()
        .unwrap();
    let vfs = Arc::new(PathVfs::new(backing).expect("open the backing directory"));
    match cowfs_nfs::Mount::new(vfs, &dir.path().join("mnt"), opts) {
        Ok(m) => {
            let p = m.mountpoint().to_path_buf();
            Some((
                Mounted {
                    _dog: Watchdog::start(p.clone(), 900),
                    mount: Some(m),
                    _dir: dir,
                },
                p,
            ))
        }
        Err(e) => {
            eprintln!("SKIP: cannot mount: {e}");
            None
        }
    }
}

fn sh(dir: &Path, script: &str) -> (bool, String) {
    let out = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .current_dir(dir)
        .output()
        .expect("run sh");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// `find <dir> -links +1 | wc -l`, the command #19 reported undercounting on the mount.
fn multi_linked(dir: &Path) -> (bool, String) {
    sh(dir, "find . -links +1 | wc -l")
}

/// #19: `find -links +1` counted 343 against 644 on the mount, and the spike never checked
/// whether `actimeo` or the readdirplus attributes were the reason. The second pass is the one
/// the kernel serves from its own attribute cache, which is the shape that used to be wrong.
#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn find_links_counts_the_same_through_the_mount_as_natively() {
    let s = scratch();
    fs::create_dir(s.backing.join("tree")).unwrap();
    for i in 0..200 {
        fs::write(s.backing.join(format!("tree/f{i}")), b"x").unwrap();
    }
    // 60 files get a second name, 30 of those a third, so the mount has to carry 1, 2 and 3.
    for i in 0..60 {
        fs::hard_link(
            s.backing.join(format!("tree/f{i}")),
            s.backing.join(format!("tree/g{i}")),
        )
        .unwrap();
    }
    for i in 0..30 {
        fs::hard_link(
            s.backing.join(format!("tree/f{i}")),
            s.backing.join(format!("tree/h{i}")),
        )
        .unwrap();
    }
    let (native_ok, native) = multi_linked(&s.backing.join("tree"));
    assert!(native_ok, "native find failed: {native}");
    let native: u64 = native.trim().parse().expect("native count");

    let Some((m, mnt)) = mounted_backing(&s.backing, MountOptions::default()) else {
        return;
    };
    let (ok1, first) = multi_linked(&mnt.join("tree"));
    let (ok2, second) = multi_linked(&mnt.join("tree"));
    let first: u64 = first.trim().parse().expect("mount count");
    let second: u64 = second.trim().parse().expect("cached mount count");
    println!("LINKS native={native} mount_first={first} mount_cached={second}");
    assert!(ok1 && ok2, "find failed on the mount: {first} {second}");
    assert!(native > 0, "the fixture has multi-linked files: {native}");
    assert_eq!(
        (first, second),
        (native, native),
        "the mount agrees with the backing directory, cold and cached"
    );
    m.finish();
}

/// #19: SETATTR through the kernel client. `touch -h` on a mounted symlink becomes SETATTR on the
/// link's own file handle, and `rsync -a` of a tree with dangling links exited 23 because the
/// server followed them.
#[test]
#[ignore = "mounts a filesystem; run with --ignored"]
fn touching_a_symlink_on_the_mount_leaves_its_target_alone() {
    let s = scratch();
    let src = s.dir.path().join("src");
    fs::create_dir_all(src.join("node_modules/.bin")).unwrap();
    fs::write(src.join("node_modules/.bin/tool"), b"#!/bin/sh\n").unwrap();
    fs::write(src.join("plain"), b"hello\n").unwrap();
    std::os::unix::fs::symlink("tool", src.join("node_modules/.bin/link")).unwrap();
    std::os::unix::fs::symlink("nowhere", src.join("node_modules/.bin/dangling")).unwrap();
    let target_mtime = mtime_of(&src.join("node_modules/.bin/tool"));

    // The control: the same rsync into a plain directory succeeds, so exit 23 could only be ours.
    let (ok, out) = sh(&s.native, &format!("rsync -a {}/ dst/", src.display()));
    assert!(ok, "native rsync failed: {out}");

    let Some((m, mnt)) = mounted_backing(&s.backing, MountOptions::default()) else {
        return;
    };
    let (ok, out) = sh(&mnt, &format!("rsync -a {}/ tree/", src.display()));
    println!("RSYNC ok={ok} out={out:?}");
    assert!(ok, "rsync onto the mount failed: {out}");

    let (ok, listed) = sh(
        &mnt.join("tree/node_modules/.bin"),
        "ls -1 | sort | tr '\\n' ' '",
    );
    assert!(ok, "ls failed: {listed}");
    for n in ["dangling", "link", "tool"] {
        assert!(
            listed.split(' ').any(|x| x == n),
            "{n} is on the mount, listed as {listed:?}"
        );
    }

    // The kernel client itself, not rsync: `touch -h` never follows, and on the mount it becomes
    // SETATTR on the link's own file handle. The same stamp is applied to a native copy of the
    // same tree, so the expected value needs no hardcoded epoch.
    let (ok, out) = sh(
        &s.native,
        &format!(
            "rsync -a {}/ . && touch -h -t 200001020304 node_modules/.bin/link",
            src.display()
        ),
    );
    assert!(ok, "native control touch failed: {out}");
    let wanted = mtime_of(&s.native.join("node_modules/.bin/link"));
    let (ok, out) = sh(
        &mnt,
        "touch -h -t 200001020304 tree/node_modules/.bin/link && \
         touch -h -t 200001020304 tree/node_modules/.bin/dangling",
    );
    assert!(ok, "touch -h failed: {out}");

    for name in ["link", "dangling"] {
        let on_mount = mtime_of(&mnt.join(format!("tree/node_modules/.bin/{name}")));
        assert_eq!(
            on_mount, wanted,
            "{name} took the time the client asked for"
        );
    }
    assert_eq!(
        mtime_of(&s.backing.join("tree/node_modules/.bin/link")),
        wanted,
        "and the backing directory agrees, so the link's own mtime is what was set"
    );
    assert_eq!(
        mtime_of(&s.backing.join("tree/node_modules/.bin/tool")),
        target_mtime,
        "the target's mtime is what rsync left it at: neither rsync nor touch followed the link"
    );
    assert!(
        target_mtime != wanted,
        "the control: that stamp is not the target's own time"
    );
    m.finish();
}
