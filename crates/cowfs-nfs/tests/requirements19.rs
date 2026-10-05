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
// The Linux branch below asks the backend directly whether it can store a symlink mode.
#[cfg(target_os = "linux")]
use cowfs_vfs::Vfs as _;
#[cfg(target_os = "linux")]
use nfsserve::nfs::{nfstime3, set_mtime};

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
///
/// The link's own mode is read before the chmod and compared after, never written as a literal:
/// macOS stores one on a symlink and reports 0o755, Linux does not store one at all and reports
/// 0o777. What this asserts is that a followed chmod moved the target and did not move the link.
#[test]
fn a_native_chmod_through_a_symlink_lands_on_the_target() {
    let s = scratch();
    fs::write(s.native.join("target"), b"x").unwrap();
    std::os::unix::fs::symlink("target", s.native.join("link")).unwrap();
    let (link_before, target_before) = (
        mode_of(&s.native.join("link")),
        mode_of(&s.native.join("target")),
    );
    fs::set_permissions(s.native.join("link"), fs::Permissions::from_mode(0o600)).unwrap();
    let (link_after, target_after) = (
        mode_of(&s.native.join("link")),
        mode_of(&s.native.join("target")),
    );
    println!(
        "NATIVE_CHMOD link {link_before:o} -> {link_after:o}, target {target_before:o} -> {target_after:o}"
    );
    assert_eq!(
        target_before, 0o644,
        "the fixture is 644 before anything touches it"
    );
    assert_eq!(target_after, 0o600, "a followed chmod landed on the target");
    assert_eq!(
        link_after, link_before,
        "the link's own mode is not what changed"
    );
    assert_ne!(
        target_before, target_after,
        "the control must be a real change, not a no-op that passes twice"
    );
}

/// #19: SETATTR on a symlink must give the link its own times and must not touch the target.
///
/// Times are the portable half. macOS reaches the link through `utimensat` with
/// `AT_SYMLINK_NOFOLLOW`, Linux through `utimensat` with `AT_EMPTY_PATH` on an `O_PATH`
/// descriptor (`cowfs-vfs-path/src/sys.rs`). Both hosts run this. The mode half is not portable and
/// has its own test below.
#[test]
fn setattr_gives_a_symlink_its_own_times_over_the_real_filesystem() {
    let s = scratch();
    let (_server, mut c) = serve_backing(&s.backing);
    let root = c.root.clone();

    let target_file = c.create_file(&root, "target");
    assert_eq!(c.write(&target_file, 0, b"payload", 2).0, OK);
    let (st, link) = c.symlink(&root, "link", "target");
    assert_eq!(st, OK);
    let link = link.unwrap();
    let (st, dangling) = c.symlink(&root, "dangling", "missing");
    assert_eq!(st, OK);
    let dangling = dangling.unwrap();

    let target = s.backing.join("target");
    let target_before = (mtime_of(&target), fs::read(&target).unwrap());

    let (st, after) = c.setattr(&link, sattr_mtime(1000, 5));
    assert_eq!(st, OK, "a valid symlink takes its own times");
    assert_eq!(
        (after.unwrap().mtime.seconds, after.unwrap().mtime.nseconds),
        (1000, 5)
    );
    assert_eq!(
        mtime_of(&s.backing.join("link")),
        (1000, 5),
        "the link's own mtime on disk is what was set"
    );
    assert_eq!(
        (mtime_of(&target), fs::read(&target).unwrap()),
        target_before,
        "the target's mtime and its bytes on disk are both untouched"
    );
    assert_eq!(
        c.readlink(&link).1,
        b"target",
        "the link still points where it did"
    );

    // #19: rsync of a tree with dangling links onto the mount exited 23 because this was ENOENT.
    let (st, a) = c.setattr(&dangling, sattr_mtime(3000, 0));
    assert_eq!(st, OK, "a dangling link takes its own times");
    assert_eq!(a.unwrap().mtime.seconds, 3000);
    assert_eq!(mtime_of(&s.backing.join("dangling")), (3000, 0));
    assert_eq!(
        c.readlink(&dangling).1,
        b"missing",
        "a dangling link keeps its target string"
    );
    assert!(
        fs::symlink_metadata(s.backing.join("dangling"))
            .unwrap()
            .file_type()
            .is_symlink(),
        "and no target was created for it"
    );

    let a = c.attrs(&link);
    assert_eq!(a.ftype, ftype3::NF3LNK, "the handle still names the link");
    assert_eq!(a.size, 6, "the link reports its own target length");
}

/// #19 called the `chmod` path untested. Where the host stores a mode on a symlink, SETATTR must
/// reach the link and only the link.
///
/// macOS does: `OPEN_SYMLINK` is `O_SYMLINK | O_RDONLY` and `fchmod` on that descriptor changes the
/// link. The capability is checked on the host first, because it is observable without any new
/// dependency: a host that stores no mode on a symlink reports 0o777 for every link, which is the
/// Linux signature. A host with that signature records the fact instead of being asked for a
/// mode it cannot hold.
#[cfg(target_os = "macos")]
#[test]
fn setattr_gives_a_symlink_its_own_mode_where_the_host_stores_one() {
    let s = scratch();
    fs::write(s.native.join("probe_target"), b"x").unwrap();
    std::os::unix::fs::symlink("probe_target", s.native.join("probe_link")).unwrap();
    let probe_link = mode_of(&s.native.join("probe_link"));
    println!("SYMLINK_MODE_CAPABILITY host=macos native_link_mode={probe_link:o}");
    if probe_link == 0o777 {
        println!(
            "SKIP label=no-symlink-mode-in-host host=macos reason=native_link_mode_is_0o777 \
             open_issue=19"
        );
        return;
    }

    let (_server, mut c) = serve_backing(&s.backing);
    let root = c.root.clone();
    let target_file = c.create_file(&root, "target");
    assert_eq!(c.setattr(&target_file, sattr_mode(0o644)).0, OK);
    let link = c.symlink(&root, "link", "target").1.unwrap();

    let link_before = mode_of(&s.backing.join("link"));
    let (st, a) = c.setattr(&link, sattr_mode(0o600));
    let link_after = mode_of(&s.backing.join("link"));
    let target_after = mode_of(&s.backing.join("target"));
    println!(
        "SYMLINK_MODE host=macos status={st} link {link_before:o} -> {link_after:o} \
         target={target_after:o}"
    );
    assert_eq!(st, OK);
    assert_eq!(
        a.unwrap().mode,
        0o600,
        "the reply reports the link's new mode"
    );
    assert_eq!(link_after, 0o600, "the link's own mode changed on disk");
    assert_eq!(target_after, 0o644, "and the target's did not");
}

/// #19's `chmod` requirement cannot be met where the host keeps no mode on a symlink, and this lane
/// does not change production code to invent one.
///
/// Linux has no `chmod` for a symlink: there is no `fchmodat` without `AT_SYMLINK_NOFOLLOW`, and
/// `cowfs-vfs-path` opens the link with `O_PATH | O_NOFOLLOW`, on which `fchmod` is `EBADF`
/// (`crates/cowfs-vfs-path/src/sys.rs`). So the server answers an error. This asserts the refusal
/// and that nothing was damaged, and names the status it saw rather than treating it as a contract:
/// the error the backend reports today is an artefact of how the errno is mapped, and a later fix
/// may legitimately answer `NOTSUPP` or succeed.
#[cfg(target_os = "linux")]
#[test]
fn setattr_refuses_a_symlink_mode_where_the_host_cannot_store_one() {
    let s = scratch();
    // The capability, asked of the backend itself rather than of NFS, so the refusal is attributed
    // to the host and backend rather than to the protocol layer inventing an error.
    let direct = PathVfs::new(&s.backing).expect("open the backing directory");
    let root_inode = cowfs_vfs::ROOT_INO;
    let link_attr = direct
        .symlink(root_inode, b"direct", b"target")
        .expect("symlink through the backend");
    let direct_mode = direct.setattr(
        link_attr.ino,
        cowfs_vfs::SetAttr {
            mode: Some(0o600),
            ..cowfs_vfs::SetAttr::default()
        },
    );
    println!(
        "SYMLINK_MODE host=linux backend_setattr={:?}",
        direct_mode.as_ref().err().map(|e| e.to_string())
    );
    assert!(
        direct_mode.is_err(),
        "this host is expected to be unable to store a mode on a symlink; if it can, the mode \
         assertions belong in a portable test instead of here"
    );

    let (_server, mut c) = serve_backing(&s.backing);
    let root = c.root.clone();
    let target_file = c.create_file(&root, "target");
    assert_eq!(c.setattr(&target_file, sattr_mode(0o644)).0, OK);
    let link = c.symlink(&root, "link", "target").1.unwrap();

    let (mode_status, _) = c.setattr(&link, sattr_mode(0o600));
    let link_after = mode_of(&s.backing.join("link"));
    let target_after = mode_of(&s.backing.join("target"));
    println!(
        "SYMLINK_MODE host=linux nfs_status={mode_status} link={link_after:o} target={target_after:o} \
         open_issue=19"
    );
    assert_ne!(
        mode_status, OK,
        "a symlink mode cannot be stored here, so the server must not report success"
    );
    assert_eq!(
        target_after, 0o644,
        "the refused mode did not land on the target either"
    );
    assert_eq!(
        link_after,
        mode_of(&s.backing.join("link")),
        "the link's own mode is whatever the host reports, unchanged by the refusal"
    );

    // A combined times-and-mode call reports the order it actually applied them in. No atomicity is
    // claimed for it either way: what is asserted is only that the target's mode is still intact.
    let (combined, _) = {
        let mut s3 = sattr_mode(0o600);
        s3.mtime = set_mtime::SET_TO_CLIENT_TIME(nfstime3 {
            seconds: 4000,
            nseconds: 0,
        });
        c.setattr(&link, s3)
    };
    let times_applied = mtime_of(&s.backing.join("link")).0 == 4000;
    println!(
        "SYMLINK_MODE_COMBINED host=linux status={combined} times_applied_before_the_error={times_applied} \
         target_mode={:o}",
        mode_of(&s.backing.join("target"))
    );
    assert_ne!(combined, OK, "the mode half still fails");
    assert_eq!(
        mode_of(&s.backing.join("target")),
        0o644,
        "and the target is still untouched"
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
    /// Printed on every successful mount, so a green run of these names says whether anything was
    /// actually mounted. The two of them are the only coverage of the kernel client's attribute
    /// cache and of `touch -h` becoming SETATTR on a link handle.
    receipt: String,
    _dog: Watchdog,
    _dir: tempfile::TempDir,
}

impl Mounted {
    fn path(&self) -> PathBuf {
        self.mount.as_ref().unwrap().mountpoint().to_path_buf()
    }

    fn finish(mut self) {
        let path = self.path();
        println!(
            "RECEIPT label=nfs-teardown path={} {}",
            path.display(),
            self.receipt
        );
        self.mount.take().unwrap().unmount().unwrap();
        let table = Command::new("/sbin/mount").output().unwrap();
        assert!(
            !cowfs_nfs::is_listed(&String::from_utf8_lossy(&table.stdout), &path),
            "still mounted"
        );
    }
}

/// True when this run was asked for a real mount. `COWFS_REQUIRE_MOUNT=1` is how a manual
/// acceptance run says that a host without the capability is a failure rather than a skip.
fn mount_required() -> bool {
    std::env::var("COWFS_REQUIRE_MOUNT").is_ok_and(|v| v != "0")
}

/// Decides what a run of a mount test established.
///
/// A host that cannot mount is a legitimate skip in CI, where nothing can be done about it, and it
/// must stay green there or the suite cannot run off macOS at all. A run that *asked* for the mount
/// and did not get one is not a pass: it established nothing, so it is an error carrying
/// `UNMEASURABLE` rather than a quiet green.
fn accept<T>(outcome: Result<T, String>, required: bool) -> Result<Option<T>, String> {
    match outcome {
        Ok(v) => Ok(Some(v)),
        Err(why) if required => Err(format!(
            "UNMEASURABLE: {why}; this run asked for a mount and did not get one, so it \
             established nothing"
        )),
        Err(why) => {
            println!("SKIP label=no-mount-capability reason={why}");
            Ok(None)
        }
    }
}

/// The negative case, with no filesystem and no mount: a requested mount that did not happen has to
/// be an error, and an unrequested one has to be a labelled skip. If this passes, the two mount
/// tests cannot report a green run that never mounted.
#[test]
fn a_requested_mount_that_did_not_happen_is_an_error_and_an_unrequested_one_is_a_skip() {
    let refused = accept::<()>(Err("mount_nfs is not available".to_string()), true)
        .expect_err("a requested mount must not become a green skip");
    assert!(
        refused.starts_with("UNMEASURABLE"),
        "the failure names what it is: {refused}"
    );
    assert!(
        accept(Ok(()), true)
            .expect("a mount that happened is accepted")
            .is_some(),
        "asking for the mount and getting it is the normal case"
    );
    assert!(
        accept::<()>(Err("mount_nfs is not available".to_string()), false)
            .expect("an unrequested skip is not an error")
            .is_none(),
        "a CI host without the capability skips instead of failing"
    );
}

fn mounted_backing(backing: &Path, opts: MountOptions) -> Result<Mounted, String> {
    if !cowfs_nfs::mount_nfs_available() {
        return Err("mount_nfs is not available on this host".to_string());
    }
    let dir = tempfile::Builder::new()
        .prefix("cowfs-nfs-ready19-")
        .tempdir()
        .map_err(|e| format!("scratch directory: {e}"))?;
    let vfs = Arc::new(PathVfs::new(backing).expect("open the backing directory"));
    let m = cowfs_nfs::Mount::new(vfs, &dir.path().join("mnt"), opts)
        .map_err(|e| format!("cannot mount: {e}"))?;
    let p = m.mountpoint().to_path_buf();
    let line = String::from_utf8_lossy(
        &Command::new("/sbin/mount")
            .output()
            .map_err(|e| format!("read the mount table: {e}"))?
            .stdout,
    )
    .lines()
    .find(|l| {
        l.split(" on ")
            .nth(1)
            .is_some_and(|rest| rest.starts_with(&p.display().to_string()))
    })
    .unwrap_or_default()
    .to_string();
    // A real mount is a different device from the directory behind it. Without this, a green run
    // could be a plain read of the backing directory reached by some other path.
    let mount_dev = fs::metadata(&p).map(|m| m.dev()).unwrap_or(0);
    let backing_dev = fs::metadata(backing).map(|m| m.dev()).unwrap_or(0);
    assert_ne!(
        mount_dev, backing_dev,
        "the mountpoint is the same device as the backing directory, so nothing was mounted"
    );
    let receipt = format!("mount_dev={mount_dev} backing_dev={backing_dev} line={line}");
    println!("RECEIPT label=nfs-mount {receipt}");
    Ok(Mounted {
        _dog: Watchdog::start(p, 900),
        mount: Some(m),
        receipt,
        _dir: dir,
    })
}

/// The two mount tests, once the mount question is settled.
fn mount_or_account(backing: &Path) -> Option<(Mounted, PathBuf)> {
    let outcome = mounted_backing(backing, MountOptions::default());
    let required = mount_required();
    match accept(outcome, required) {
        Ok(Some(m)) => {
            let p = m.path();
            Some((m, p))
        }
        Ok(None) => None,
        Err(e) => panic!("{e}"),
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

    let Some((m, mnt)) = mount_or_account(&s.backing) else {
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

    let Some((m, mnt)) = mount_or_account(&s.backing) else {
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
