//! Issues #178, #167 and #170: `base_refresh` hardening, driven the way a caller hits it, on both
//! backends.
//!
//! - #178: a refresh of a name that is a plain snapshot (no base record) must not silently replace
//!   it unless the caller asks to,
//! - #167: two concurrent refreshes of one name must not leave one refresh's tree under a record
//!   naming the other's commit,
//! - #170: a base name the Core cannot refresh must be refused at the first call, not published
//!   and then stuck.

use cowfs_ctl::{BaseRefreshParams, BaseRefreshReport, CtlResult, ErrorCode, OpContext};
use cowfs_daemon::{Backend, CoreBackend, PathBackend};
use cowfs_vfs::ROOT_INO;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

fn backends() -> Vec<(&'static str, tempfile::TempDir, Arc<dyn Backend>)> {
    let mut out: Vec<(&'static str, tempfile::TempDir, Arc<dyn Backend>)> = Vec::new();
    let d = tempfile::tempdir().unwrap();
    let b: Arc<dyn Backend> = Arc::new(
        CoreBackend::open(d.path().join("store"), cowfs_core::Options::default()).unwrap(),
    );
    out.push(("core", d, b));
    let d = tempfile::tempdir().unwrap();
    let b: Arc<dyn Backend> = Arc::new(PathBackend::open(d.path().join("store")).unwrap());
    out.push(("path", d, b));
    out
}

/// A real git repository whose one commit carries `tag` as the content of the file `tag`.
fn repo(dir: &Path, leaf: &str, tag: &str) -> PathBuf {
    let repo = dir.join(leaf);
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .stdin(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("tag"), tag).unwrap();
    git(&["add", "tag"]);
    git(&[
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "-m",
        tag,
    ]);
    repo
}

fn commit_of_repo(repo: &Path) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn params(repo: &Path, name: &str, replace: bool) -> BaseRefreshParams {
    BaseRefreshParams {
        repo: repo.display().to_string(),
        git_ref: "main".into(),
        name: Some(name.into()),
        replace,
    }
}

fn refresh(b: &dyn Backend, p: &BaseRefreshParams) -> CtlResult<BaseRefreshReport> {
    cowfs_daemon::import::base_refresh(b, b.snapshots(), p, &OpContext::detached())
}

/// The content of the file `name` at the root of snapshot `snap`, if it exists.
fn read(b: &dyn Backend, snap: &str, name: &str) -> Option<String> {
    let vfs = b.snapshot(snap).unwrap();
    let a = vfs.lookup(ROOT_INO, name.as_bytes()).ok()?;
    let h = vfs.open(a.ino).unwrap();
    let n = vfs.read(a.ino, 0, 4096).unwrap();
    vfs.release(h).unwrap();
    Some(String::from_utf8_lossy(&n).into_owned())
}

fn seed_plain(b: &dyn Backend, name: &str, tag: &str) {
    b.snapshots().create(name, None).unwrap();
    let vfs = b.snapshot(name).unwrap();
    let f = vfs.create(ROOT_INO, b"tag", 0o644).unwrap();
    let h = vfs.open(f.ino).unwrap();
    vfs.write(f.ino, 0, tag.as_bytes()).unwrap();
    vfs.fsync(f.ino, false).unwrap();
    vfs.release(h).unwrap();
}

#[test]
fn a_refresh_does_not_silently_replace_a_plain_snapshot() {
    for (which, d, b) in backends() {
        let r = repo(d.path(), "repo", "from-git");
        seed_plain(b.as_ref(), "mine", "users-work");

        let e = refresh(b.as_ref(), &params(&r, "mine", false)).unwrap_err();
        assert_eq!(e.code, ErrorCode::AlreadyExists, "{which}: {e}");
        assert!(
            e.message.contains("--replace"),
            "{which}: the error must say how to proceed: {e}"
        );
        assert_eq!(
            read(b.as_ref(), "mine", "tag").as_deref(),
            Some("users-work"),
            "{which}: the user's snapshot was replaced"
        );
        assert!(
            b.snapshots().create_meta("mine").unwrap().base.is_none(),
            "{which}: the user's snapshot became a base"
        );

        let ok = refresh(b.as_ref(), &params(&r, "mine", true)).unwrap();
        assert_eq!(
            ok.snapshot.base.unwrap().commit.as_deref(),
            Some(commit_of_repo(&r).as_str()),
            "{which}"
        );
        assert_eq!(
            read(b.as_ref(), "mine", "tag").as_deref(),
            Some("from-git"),
            "{which}: an explicit replace must replace"
        );
    }
}

#[test]
fn refreshing_an_existing_base_needs_no_replace_flag() {
    for (which, d, b) in backends() {
        let r = repo(d.path(), "repo", "one");
        refresh(b.as_ref(), &params(&r, "warm", false)).unwrap();
        std::fs::write(r.join("tag"), "two").unwrap();
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(&r)
                .args(args)
                .status()
                .unwrap()
                .success());
        };
        git(&["add", "tag"]);
        git(&[
            "-c",
            "user.email=t@example.invalid",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "two",
        ]);
        let second = refresh(b.as_ref(), &params(&r, "warm", false)).unwrap();
        assert!(second.previous_commit.is_some(), "{which}");
        assert_eq!(read(b.as_ref(), "warm", "tag").as_deref(), Some("two"));
    }
}

/// Two refreshes of one name from two repositories, started together. Whatever each call answers,
/// the tree that is left and the record that is left must come from the same commit.
#[test]
fn concurrent_refreshes_of_one_name_leave_a_record_that_matches_the_tree() {
    for (which, d, b) in backends() {
        let a = repo(d.path(), "repo-a", "tree-A");
        let c = repo(d.path(), "repo-c", "tree-C");
        let commit_a = commit_of_repo(&a);
        let commit_c = commit_of_repo(&c);
        for round in 0..12 {
            let gate = Arc::new(Barrier::new(2));
            let handles: Vec<_> = [a.clone(), c.clone()]
                .into_iter()
                .map(|r| {
                    let b = b.clone();
                    let gate = gate.clone();
                    std::thread::spawn(move || {
                        gate.wait();
                        refresh(b.as_ref(), &params(&r, "warm", true))
                    })
                })
                .collect();
            let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            for r in &results {
                if let Err(e) = r {
                    assert_eq!(
                        e.code,
                        ErrorCode::Busy,
                        "{which} round {round}: a refused refresh must say busy: {e}"
                    );
                }
            }
            assert!(
                results.iter().any(Result::is_ok),
                "{which} round {round}: at least one refresh must win: {results:?}"
            );
            let info = b.snapshots().create_meta("warm").unwrap();
            let recorded = info.base.and_then(|m| m.commit).expect("a record");
            let tag = read(b.as_ref(), "warm", "tag").expect("a tree");
            let expected = if recorded == commit_a {
                "tree-A"
            } else if recorded == commit_c {
                "tree-C"
            } else {
                panic!("{which} round {round}: record names an unknown commit {recorded}")
            };
            assert_eq!(
                tag, expected,
                "{which} round {round}: the record names {recorded} but the tree is {tag}"
            );
        }
    }
}

/// Issue 252: a refresh is serialised per name by `REFRESHING`, but `snapshot_promote` is not (it
/// takes the holder guard only). Hammer one name with a promote loop while a refresh alternates two
/// repositories: the promote may find the name briefly missing, and may write the "unknown" record,
/// but every report must name the commit its own refresh built and the final record must match
/// the final tree.
#[test]
fn a_same_name_promote_racing_a_refresh_never_leaves_a_record_that_disagrees_with_the_tree() {
    for (which, d, b) in backends() {
        let a = repo(d.path(), "repo-a", "tree-A");
        let c = repo(d.path(), "repo-c", "tree-C");
        let commits = [commit_of_repo(&a), commit_of_repo(&c)];
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let promoter = {
            let (b, stop) = (b.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut n = 0u32;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Err(e) = b.snapshots().promote("warm") {
                        // the only legitimate failure: the name is between its old and new tree
                        assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "promote: {e}");
                    }
                    n += 1;
                }
                n
            })
        };
        for round in 0..16 {
            let i = round % 2;
            let repo = if i == 0 { &a } else { &c };
            let report = refresh(b.as_ref(), &params(repo, "warm", true))
                .unwrap_or_else(|e| panic!("{which} round {round}: {e}"));
            let got = report.snapshot.base.and_then(|m| m.commit);
            assert_eq!(got.as_deref(), Some(commits[i].as_str()), "{which} {round}");
            let tag = read(b.as_ref(), "warm", "tag").expect("a tree");
            assert_eq!(
                tag,
                if i == 0 { "tree-A" } else { "tree-C" },
                "{which} {round}"
            );
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            promoter.join().unwrap() > 0,
            "{which}: the promoter never ran"
        );
    }
}

/// The Core stores a swap intent as `swap-<name>.tmp` in a store directory, so the longest name it
/// can refresh is shorter than the longest name it can hold.
#[test]
fn a_base_name_that_cannot_be_refreshed_is_refused_at_the_first_call() {
    let long = "n".repeat(250);
    for (which, d, b) in backends() {
        let r = repo(d.path(), "repo", "x");
        let e = refresh(b.as_ref(), &params(&r, &long, false)).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidParams, "{which}: {e}");
        assert!(
            !b.snapshots().list().unwrap().contains(&long),
            "{which}: a refused name must not be published"
        );

        let longest = "n".repeat(cowfs_ctl::BASE_NAME_MAX);
        refresh(b.as_ref(), &params(&r, &longest, false)).unwrap();
        refresh(b.as_ref(), &params(&r, &longest, false))
            .unwrap_or_else(|e| panic!("{which}: the longest legal base must refresh again: {e}"));
        b.snapshots().promote(&longest).unwrap();
    }
}
