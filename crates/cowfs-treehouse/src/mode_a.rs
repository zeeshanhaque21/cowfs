use cowfs_ctl::ProcessInfo;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ctl::Daemon;
use crate::error::{Error, Result};
use crate::holders;
use crate::naming;
use crate::report::Report;
use crate::th::Treehouse;

/// How a mount is expected to look for treehouse, and what `setup` reports about it.
#[derive(Clone, Debug, Serialize)]
pub struct Setup {
    /// The cowfs mount point.
    pub mount: PathBuf,
    /// The treehouse root to keep on the mount. Treehouse appends `.treehouse` to it.
    pub pool_root: PathBuf,
    /// The main checkout to keep on the mount, when one was given.
    pub main_checkout: Option<PathBuf>,
    /// Whether `apfs_sharing` is turned on in any treehouse config that applies.
    pub apfs_sharing: bool,
    /// Whether the mount is a network mount, which is what makes silly-renames possible.
    pub network_mount: bool,
    /// The mount options read from the kernel.
    pub mount_options: String,
    /// The server option that must be on for file locks to work over NFS, and whether it is set.
    pub locallocks: Option<bool>,
    /// How AppleDouble files are handled, and what was found on the mount.
    pub appledouble: AppleDouble,
    /// The exact `treehouse` invocation for this pool root.
    pub treehouse_command: String,
}

/// What the mount does with `._*` sidecar files, and what the scan found.
#[derive(Clone, Debug, Serialize)]
pub struct AppleDouble {
    /// The decision: `hide` or `translate`.
    pub policy: &'static str,
    /// Why that decision, in one line.
    pub reason: String,
    /// How many `._*` entries were found under the mount, sampled to a bound.
    pub found: usize,
    /// True when the scan stopped at its bound, so `found` is a lower bound.
    pub truncated: bool,
}

/// Upper bound on the `._*` scan, so `doctor` stays fast on a big mount.
const APPLEDOUBLE_SCAN_LIMIT: usize = 2_000;

impl AppleDouble {
    /// Scans the mount for `._*` entries and picks a policy. A mount that has some is one where
    /// they are leaking, so the recommendation flips from hiding to translating.
    pub fn scan(mount: &Path) -> AppleDouble {
        let (found, truncated) = count_prefix(mount, "._", APPLEDOUBLE_SCAN_LIMIT);
        if found > 0 {
            AppleDouble {
                policy: "translate",
                reason: format!(
                    "{found} AppleDouble entries are visible on the mount, so the adapter is not \
                     hiding them and `git status` in a slot would report them"
                ),
                found,
                truncated,
            }
        } else {
            AppleDouble {
                policy: "hide",
                reason: "no AppleDouble entries are visible, so the adapter is hiding them"
                    .to_owned(),
                found,
                truncated,
            }
        }
    }
}

/// Whether a treehouse config file turns `apfs_sharing` on.
///
/// The docs say to leave it off on a cowfs mount: on a macOS APFS volume it re-materialises the
/// slot's tracked files with `clonefile`, which is the job the block store already does, and it is
/// the only treehouse step that would rewrite a slot after a snapshot has been placed under it.
pub fn apfs_sharing_configured(config: &Path) -> bool {
    const FRESH: &str = "fresh";
    let Ok(text) = std::fs::read_to_string(config) else {
        return false;
    };
    text.lines().any(|l| {
        let t = l.trim();
        t.starts_with("apfs_sharing") && t.contains(FRESH)
    })
}

/// The treehouse user config path.
pub fn user_config(home: &Path) -> PathBuf {
    home.join(".config/treehouse/config.toml")
}

/// Walks `root` counting entries whose name starts with `prefix`, stopping at `limit`.
pub fn count_prefix(root: &Path, prefix: &str, limit: usize) -> (usize, bool) {
    let mut found = 0usize;
    let mut stack = vec![root.to_path_buf()];
    let truncated = false;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(prefix) {
                found += 1;
                if found >= limit {
                    return (found, true);
                }
                continue;
            }
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(entry.path());
            }
        }
    }
    (found, truncated)
}

/// Reports what a mount needs before unmodified treehouse can use it, and refuses to call a mount
/// ready while a check fails.
pub fn setup(
    daemon: &mut Daemon,
    mount: &Path,
    pool_root: &Path,
    main_checkout: Option<&Path>,
) -> Result<Setup> {
    let mut report = Report::new();
    let info = daemon.mount_info()?;
    report.check(
        "cowfs mount",
        info.mounted && Path::new(&info.mount_path) == mount,
        format!(
            "mount_path {} adapter {} mounted {}",
            info.mount_path, info.adapter, info.mounted
        ),
    );
    let (network_mount, mount_options) = mount_kind(mount);
    let inside = |p: &Path| p.starts_with(mount);
    report.check(
        "pool root on mount",
        inside(pool_root),
        format!(
            "{} is {} the mount",
            pool_root.display(),
            if inside(pool_root) {
                "inside"
            } else {
                "outside"
            }
        ),
    );
    if let Some(repo) = main_checkout {
        let ok = inside(repo);
        report.check(
            "main checkout on mount",
            ok,
            format!(
                "{} is {} the mount",
                repo.display(),
                if ok { "inside" } else { "outside" }
            ),
        );
    }
    let appledouble = AppleDouble::scan(mount);
    report.check(
        "appledouble",
        appledouble.found == 0,
        appledouble.reason.clone(),
    );
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    let user_sharing = apfs_sharing_configured(&user_config(&home));
    let repo_sharing = main_checkout
        .map(|c| apfs_sharing_configured(&c.join("treehouse.toml")))
        .unwrap_or(false);
    report.check(
        "apfs_sharing off",
        !user_sharing && !repo_sharing,
        if user_sharing || repo_sharing {
            "apfs_sharing = \"fresh\" is set; it rewrites a slot's tracked files after a snapshot \
             has been placed under it, so it must be off on a cowfs mount"
                .to_owned()
        } else {
            "off, which is what a cowfs mount wants".to_owned()
        },
    );

    let locallocks = if network_mount { Some(false) } else { None };
    if network_mount {
        report.fail(
            "locallocks",
            "a network mount needs the cowfs NFS server started with locallocks, and the \
             companion cannot read the server's options from the client side; start `cowfs serve` \
             with it and re-run",
        );
    }
    report.pass(
        "hardlinks and locks",
        "verified live by `cowfs-treehouse doctor`, which takes a real lock and a real link",
    );
    if !report.all_ok() {
        report.print(false);
        return Err(Error::Unsupported(format!(
            "{} is not ready for treehouse; see the failing checks above",
            mount.display()
        )));
    }
    Ok(Setup {
        apfs_sharing: user_sharing || repo_sharing,
        mount: mount.to_path_buf(),
        pool_root: pool_root.to_path_buf(),
        main_checkout: main_checkout.map(Path::to_path_buf),
        network_mount,
        mount_options,
        locallocks,
        appledouble,
        treehouse_command: format!("treehouse --root {}", pool_root.display()),
    })
}

/// Whether `path` is a network mount, and the options the kernel reports for it.
fn mount_kind(path: &Path) -> (bool, String) {
    let Ok(out) = std::process::Command::new("mount").output() else {
        return (false, String::from("unknown: cannot run mount"));
    };
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let wanted = path.display().to_string();
    for line in text.lines() {
        let Some((device, rest)) = line.split_once(" on ") else {
            continue;
        };
        if !rest.starts_with(wanted.as_str()) {
            continue;
        }
        let options = rest
            .split_once(" (")
            .map_or(rest, |(_, o)| o)
            .trim_end_matches(')')
            .to_owned();
        let network = device.starts_with("nfs")
            || device.starts_with("afp")
            || device.starts_with("smb")
            || device.starts_with("webdav");
        return (network, format!("{device} ({options})"));
    }
    (false, format!("not listed by mount(8): {wanted}"))
}

/// Everything mode (a) needs from a mount and a pool.
#[derive(Clone, Debug)]
pub struct Doctor {
    /// The cowfs mount point.
    pub mount: PathBuf,
    /// The treehouse root, without the `.treehouse` that treehouse appends.
    pub pool_root: Option<PathBuf>,
    /// The main checkout, when the caller named one.
    pub main_checkout: Option<PathBuf>,
}

/// Checks mount type, locks, hardlinks, snapshot visibility, `.nfs*` dirt and open-fd holders.
pub fn doctor(daemon: &mut Daemon, opts: &Doctor) -> Result<Report> {
    let mut report = Report::new();
    let info = daemon.mount_info()?;
    report.check(
        "mount type",
        info.mounted && Path::new(&info.mount_path) == opts.mount,
        format!(
            "adapter {} mounted {} at {}",
            info.adapter, info.mounted, info.mount_path
        ),
    );
    let mount = opts.mount.clone();
    report.check("locks (flock)", flock_works(&mount), {
        let d = mount.join(".cowfs-treehouse-doctor.lock");
        if d.exists() {
            "flock took and released a file on the mount".to_owned()
        } else {
            format!("could not create {}", d.display())
        }
    });
    report.check("hardlinks", hardlink_works(&mount), {
        format!(
            "link created at {}",
            mount.join(".cowfs-treehouse-doctor.link").display()
        )
    });

    let snapshots = daemon.snapshot_list()?;
    let missing: Vec<String> = snapshots
        .iter()
        .map(|s| s.name.clone())
        .filter(|n| !Path::new(&info.mount_path).join(n).is_dir())
        .collect();
    report.check(
        "snapshot dirs visible",
        missing.is_empty(),
        if missing.is_empty() {
            format!(
                "{} of {} snapshots visible",
                snapshots.len(),
                snapshots.len()
            )
        } else {
            format!("not visible: {}", missing.join(", "))
        },
    );

    let slots = opts
        .pool_root
        .as_deref()
        .map(pool_slots)
        .unwrap_or_default();
    let mut nfs_hits: Vec<String> = Vec::new();
    for slot in &slots {
        for e in holders::nfs_entries(slot) {
            nfs_hits.push(e.display().to_string());
        }
    }
    report.check(
        "no .nfs* dirt",
        nfs_hits.is_empty(),
        if slots.is_empty() {
            "no pool root given, so no slot was scanned".to_owned()
        } else if nfs_hits.is_empty() {
            format!("{} slots clean", slots.len())
        } else {
            format!(
                "{}: open-fd or flock holders were left behind",
                nfs_hits.join(", ")
            )
        },
    );

    let mut held: Vec<String> = Vec::new();
    let mut scanned = 0usize;
    for s in &snapshots {
        let is_slot = slots
            .iter()
            .filter_map(|p| naming::slot_snapshot(&s.name, naming::slot_of(p)?).ok())
            .any(|want| want == s.name);
        if !is_slot {
            continue;
        }
        scanned += 1;
        match daemon.ps(&s.name) {
            Ok(p) if p.is_empty() => {}
            Ok(p) => held.extend(holders::describe(&p)),
            // A snapshot with no daemon-side scan support must not read as clean.
            Err(e) => held.push(format!("{}: cannot be scanned ({e})", s.name)),
        }
    }
    report.check(
        "no open-fd holders",
        held.is_empty(),
        if scanned == 0 {
            "no slot-backed snapshot found, so nothing was scanned".to_owned()
        } else {
            format!("{scanned} slot snapshots scanned")
        },
    );
    if !held.is_empty() {
        for h in held {
            report.fail("holder", h);
        }
    }

    if let Some(repo) = &opts.main_checkout {
        report.check(
            "main checkout on mount",
            repo.starts_with(&opts.mount),
            format!(
                "{} is {} the mount",
                repo.display(),
                if repo.starts_with(&opts.mount) {
                    "inside"
                } else {
                    "outside"
                }
            ),
        );
    }
    if let Some(root) = &opts.pool_root {
        report.check(
            "pool root on mount",
            root.starts_with(&opts.mount),
            format!(
                "{} is {} the mount",
                root.display(),
                if root.starts_with(&opts.mount) {
                    "inside"
                } else {
                    "outside"
                }
            ),
        );
    }
    Ok(report)
}

/// Every slot directory of every pool under a treehouse root, which is `{root}/.treehouse/{pool}/{slot}/{repo}`.
pub fn pool_slots(root: &Path) -> Vec<PathBuf> {
    let pool_root = root.join(".treehouse");
    let Ok(pools) = fs::read_dir(&pool_root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for pool in pools.flatten() {
        let Ok(slots) = fs::read_dir(pool.path()) else {
            continue;
        };
        for slot in slots.flatten() {
            if slot.file_type().is_ok_and(|t| t.is_dir()) {
                out.push(slot.path());
            }
        }
    }
    out.sort();
    out
}

/// Takes and releases a real `flock` on a file under the mount, which is the check that matters
/// for a network mount.
pub fn flock_works(dir: &Path) -> bool {
    let path = dir.join(".cowfs-treehouse-doctor.lock");
    let Ok(f) = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
    else {
        return false;
    };
    let locked = rustix::fs::flock(&f, rustix::fs::FlockOperation::LockExclusive).is_ok()
        && rustix::fs::flock(&f, rustix::fs::FlockOperation::Unlock).is_ok();
    drop(f);
    let _ = fs::remove_file(&path);
    locked
}

/// Creates a hardlink under the mount and checks the two names share an inode.
pub fn hardlink_works(dir: &Path) -> bool {
    let src = dir.join(".cowfs-treehouse-doctor.src");
    let dst = dir.join(".cowfs-treehouse-doctor.link");
    if fs::write(&src, b"cowfs").is_err() {
        return false;
    }
    let _ = fs::remove_file(&dst);
    let ok = fs::hard_link(&src, &dst).is_ok()
        && fs::read(&dst).is_ok_and(|b| b == b"cowfs")
        && same_inode(&src, &dst);
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&dst);
    ok
}

fn same_inode(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino() && x.nlink() >= 2,
        _ => false,
    }
}

/// What a `return` did, so a caller and a test can both see it.
#[derive(Clone, Debug, Serialize)]
pub struct ReturnOutcome {
    /// The slot that was returned.
    pub slot: PathBuf,
    /// The snapshot behind it, when mode (b) has one.
    pub snapshot: Option<String>,
    /// Holders found by `ps` before anything was destroyed.
    pub holders: Vec<String>,
    /// Pids that were signalled, when `--force` was given and holders existed.
    pub terminated: Vec<u32>,
    /// Pids that needed SIGKILL.
    pub killed: Vec<u32>,
    /// Pids that were left alone because this process may not signal them.
    pub skipped: Vec<u32>,
    /// `.nfs*` entries that were still there when the wait gave up, empty when it succeeded.
    pub nfs_dirt: Vec<PathBuf>,
    /// True when the daemon refused the atomic swap because a holder appeared, so nothing changed.
    pub refused_busy: bool,
    /// The lease identity the release was pinned to, when one was known.
    pub lease_id: Option<String>,
}

/// Options of the `return` wrapper, shared by both modes.
#[derive(Clone, Debug)]
pub struct ReturnOptions {
    /// The slot directory, or the treehouse slot name.
    pub slot: PathBuf,
    /// Terminate holders using treehouse's SIGTERM, 2 s, SIGKILL policy.
    pub force: bool,
    /// How long to wait for `.nfs*` dirt to clear.
    pub nfs_timeout: Duration,
    /// The snapshot behind the slot, for mode (b). `None` means mode (a), where the slot is an
    /// ordinary directory and only the holder checks apply.
    pub snapshot: Option<String>,
    /// The snapshot to swap the slot back to before releasing it, for mode (b). `None` releases
    /// the slot as treehouse found it.
    pub reset_to: Option<String>,
    /// Remove the slot's snapshot after the release.
    pub drop_snapshot: bool,
    /// How long treehouse itself may take.
    pub treehouse_timeout: Duration,
}

/// Returns a slot: report holders, optionally terminate them, wait for silly-rename dirt to clear,
/// make the slot cheap for treehouse to reset, and only then release the lease.
///
/// The authoritative holder check is the daemon's, inside `snapshot_reset`, under the same lock as
/// the swap. `ps` only ever names who to blame, so a holder that appears between the scan and the
/// reset produces `busy` with nothing changed instead of a reset under a live writer.
/// The daemon is optional: mode (a) reports holders and hands the slot back, and needs no cowfs at
/// all. Only mode (b), which swaps the slot's snapshot, needs one.
pub fn return_slot(
    mut daemon: Option<&mut Daemon>,
    th: &Treehouse,
    opts: &ReturnOptions,
) -> Result<ReturnOutcome> {
    let mut outcome = ReturnOutcome {
        slot: opts.slot.clone(),
        snapshot: opts.snapshot.clone(),
        holders: Vec::new(),
        terminated: Vec::new(),
        killed: Vec::new(),
        skipped: Vec::new(),
        nfs_dirt: Vec::new(),
        refused_busy: false,
        lease_id: None,
    };

    // The lease identity is read once, in either mode, so the release can be pinned and cannot take
    // a slot that was re-leased since the caller looked.
    //
    // `status` is run from the main repository, not from the slot: run inside a slot, treehouse
    // reports that slot as "you're here" and leaves `lease_id` empty, so the lookup would find
    // nothing to pin the release with. The paths it reports are physical, so the match compares
    // canonical forms while the slot itself is passed on exactly as the caller spelled it.
    let canonical_slot = naming::canonical(&opts.slot);
    if outcome.lease_id.is_none() {
        let repo = naming::main_repo_root(&opts.slot).unwrap_or_else(|_| opts.slot.clone());
        outcome.lease_id = th
            .status(&repo)
            .ok()
            .and_then(|entries| {
                entries
                    .into_iter()
                    .find(|e| naming::canonical(Path::new(&e.path)) == canonical_slot)
                    .map(|e| e.lease_id)
            })
            .filter(|id| !id.is_empty());
    }

    if let Some(snapshot) = &opts.snapshot {
        let Some(daemon) = daemon.as_deref_mut() else {
            return Err(Error::Usage(
                "this slot has a snapshot, so its return needs a cowfs daemon".to_owned(),
            ));
        };
        match daemon.ps(snapshot) {
            Ok(processes) => {
                outcome.holders = holders::describe(&processes);
                if opts.force && !processes.is_empty() {
                    let pids: Vec<u32> = dedup_pids(&processes);
                    let done = holders::terminate(&pids, holders::GRACE);
                    outcome.terminated = done.signalled;
                    outcome.killed = done.killed;
                    outcome.skipped = done.skipped;
                    if !done.survivors.is_empty() {
                        return Err(Error::Busy(format!(
                            "{} is still held after termination by {}; the slot was left in place",
                            opts.slot.display(),
                            holders::describe(&survivors_as_info(&done.survivors)).join(", ")
                        )));
                    }
                }
            }
            // A mode (b) return without `ps` support cannot claim the slot is quiet.
            Err(e) => return Err(Error::Unsupported(format!("cannot scan holders: {e}"))),
        }
    }

    if let Err(e) = holders::wait_for_nfs_clear(&opts.slot, opts.nfs_timeout, holders::POLL) {
        outcome.nfs_dirt = holders::nfs_entries(&opts.slot);
        if let Error::Busy(_) = e {
            return Err(e);
        }
        return Err(e);
    }

    if let (Some(snapshot), Some(from)) = (&opts.snapshot, &opts.reset_to) {
        let Some(daemon) = daemon.as_deref_mut() else {
            return Err(Error::Usage(
                "this slot has a snapshot, so its return needs a cowfs daemon".to_owned(),
            ));
        };
        daemon.ensure_snapshot(from, None)?;
        match daemon.snapshot_reset(snapshot, from, true) {
            Ok(_) => {}
            Err(Error::Busy(_)) => {
                outcome.refused_busy = true;
                return Err(Error::Busy(format!(
                    "{snapshot} acquired a holder after the scan; nothing was changed"
                )));
            }
            Err(e) => return Err(e),
        }
    }

    // No pin means no release. An unpinned return can hand back a slot somebody else re-leased
    // between our lookup and this call, which is the one race the lease identity exists for.
    let pin = outcome
        .lease_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| {
            Error::Usage(format!(
                "refusing to release {} unpinned: no lease identity could be read for it, so it \
                 cannot be proven to still be the slot we leased",
                opts.slot.display()
            ))
        })?;
    th.return_slot(&opts.slot, opts.force, pin)?;

    if opts.drop_snapshot {
        if let Some(snapshot) = &opts.snapshot {
            if let Some(daemon) = daemon {
                daemon.snapshot_rm(snapshot, true)?;
                outcome.snapshot = None;
            }
        }
    }
    Ok(outcome)
}

fn dedup_pids(processes: &[ProcessInfo]) -> Vec<u32> {
    let mut seen = std::collections::BTreeSet::new();
    processes
        .iter()
        .filter(|p| seen.insert(p.pid))
        .map(|p| p.pid)
        .collect()
}

fn survivors_as_info(pids: &[u32]) -> Vec<ProcessInfo> {
    pids.iter()
        .map(|p| ProcessInfo {
            pid: *p,
            command: "unknown".to_owned(),
            holds: Vec::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_prefix_finds_only_the_prefix() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("sub")).expect("mkdir");
        fs::write(dir.path().join("._a"), b"x").expect("write");
        fs::write(dir.path().join("sub/._b"), b"x").expect("write");
        fs::write(dir.path().join("sub/keep"), b"x").expect("write");
        assert_eq!(count_prefix(dir.path(), "._", 100), (2, false));
    }

    #[test]
    fn count_prefix_reports_truncation() {
        let dir = tempfile::tempdir().expect("tempdir");
        for i in 0..10 {
            fs::write(dir.path().join(format!("._f{i}")), b"x").expect("write");
        }
        let (found, truncated) = count_prefix(dir.path(), "._", 4);
        assert_eq!(found, 4);
        assert!(truncated);
    }

    #[test]
    fn appledouble_policy_follows_what_is_on_the_mount() {
        let clean = tempfile::tempdir().expect("tempdir");
        let a = AppleDouble::scan(clean.path());
        assert_eq!(a.policy, "hide");
        assert_eq!(a.found, 0);

        fs::write(clean.path().join("._resource"), b"x").expect("write");
        let b = AppleDouble::scan(clean.path());
        assert_eq!(b.policy, "translate");
        assert_eq!(b.found, 1);
    }

    #[test]
    fn apfs_sharing_is_read_out_of_a_config_and_is_off_by_default() {
        assert!(!apfs_sharing_configured(Path::new(
            "/nonexistent/treehouse.toml"
        )));
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().join("treehouse.toml");
        std::fs::write(&cfg, "max_trees = 16\napfs_sharing = \"fresh\"\n").expect("write");
        assert!(apfs_sharing_configured(&cfg));
        std::fs::write(&cfg, "# apfs_sharing = \"fresh\"\n").expect("write");
        assert!(!apfs_sharing_configured(&cfg), "a comment is not a setting");
        std::fs::write(&cfg, "apfs_sharing = \"off\"\n").expect("write");
        assert!(!apfs_sharing_configured(&cfg));
    }

    #[test]
    fn flock_and_hardlink_work_on_a_native_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(flock_works(dir.path()));
        assert!(!dir.path().join(".cowfs-treehouse-doctor.lock").exists());
        assert!(hardlink_works(dir.path()));
        assert!(!dir.path().join(".cowfs-treehouse-doctor.link").exists());
    }

    #[test]
    fn flock_of_a_missing_directory_fails_rather_than_panicking() {
        assert!(!flock_works(Path::new("/nonexistent/mount")));
    }

    #[test]
    fn hardlink_of_a_missing_directory_fails_rather_than_panicking() {
        assert!(!hardlink_works(Path::new("/nonexistent/mount")));
    }

    #[test]
    fn pool_slots_finds_two_levels_below_the_treehouse_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pool = dir.path().join(".treehouse/cowfs-7c1bf8");
        fs::create_dir_all(pool.join("1/cowfs")).expect("mkdir");
        fs::create_dir_all(pool.join("2/cowfs")).expect("mkdir");
        fs::write(pool.join("treehouse-state.json"), b"{}").expect("write");
        let slots = pool_slots(dir.path());
        assert_eq!(slots.len(), 2, "{slots:?}");
        assert!(slots[0].ends_with("1"), "{slots:?}");
    }

    #[test]
    fn pool_slots_of_a_missing_root_is_empty() {
        assert!(pool_slots(Path::new("/nonexistent")).is_empty());
    }
}
