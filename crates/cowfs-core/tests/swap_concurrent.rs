//! Issue 300: two replacements of one target racing in one `Core`.
//!
//! The replacing import stages its tree under a name derived from the target, and the promote
//! stages under the same name. Without a per-target lock the second caller removed the first one's
//! staging snapshot (or its intent file) while the first was still using it.
//! Each test parks one call inside its ingest (after its staging snapshot exists) with the progress
//! hook, starts the other call on the same target, and then lets the first one go on.

mod common;

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use common::*;
use cowfs_core::{ingest, ingest_replacing, Core, Hooks};
use cowfs_vfs::{Vfs, ROOT_INO};

fn src(root: &Path, tag: &str, body: &str) -> PathBuf {
    let d = root.join(tag);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("f"), body).unwrap();
    d
}

fn content(c: &Core, snap: &str) -> String {
    let fs = c.snapshot_view(snap).expect("view");
    let a = fs.lookup(ROOT_INO, b"f").expect("lookup");
    String::from_utf8(read_all(&fs, a.ino)).expect("utf8")
}

/// Replacing import of `from` over `name`, parked in its first write-phase progress report
/// (`total > 0`: the planning walk reports with a total of 0) until `go` fires.
fn parked_import(
    c: &Core,
    from: &Path,
    name: &str,
    parked: mpsc::Sender<()>,
    go: mpsc::Receiver<()>,
) -> Result<(), String> {
    let mut first = true;
    let mut hooks = Hooks {
        progress: &mut |_, total| {
            if total > 0 && first {
                first = false;
                parked.send(()).unwrap();
                let _ = go.recv_timeout(Duration::from_secs(10));
            }
            true
        },
    };
    ingest_replacing(c, from, name, &mut hooks)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn no_leftovers(dir: &Path, c: &Core) {
    let names: Vec<String> = c
        .list_snapshots()
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert!(!names.iter().any(|n| n.contains("cowfs-swap")), "{names:?}");
    let files: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("swap-") || n.starts_with("tmp-swap-"))
        .collect();
    assert!(files.is_empty(), "{files:?}");
    c.check().unwrap();
}

fn base_with_src(dir: &Path, scratch: &Path) -> Core {
    let c = Core::open(dir, test_opts()).unwrap();
    let mut hooks = Hooks {
        progress: &mut |_, _| true,
    };
    ingest(&c, &src(scratch, "v0", "old"), "target", &mut hooks).unwrap();
    ingest(&c, &src(scratch, "vs", "promoted"), "src", &mut hooks).unwrap();
    c
}

/// A promote of the same target while a replacing import is mid-ingest must not take the import's
/// staging snapshot away: both calls succeed, one after the other, and the last one wins.
#[test]
fn a_promote_during_a_replacing_import_of_the_same_target_does_not_break_the_import() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let c = base_with_src(dir.path(), scratch.path());
    let v1 = src(scratch.path(), "v1", "imported");
    let (parked_tx, parked_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let imp = {
        let c = c.clone();
        std::thread::spawn(move || parked_import(&c, &v1, "target", parked_tx, go_rx))
    };
    parked_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let prom = {
        let c = c.clone();
        std::thread::spawn(move || c.promote_base("src", "target").map(|_| ()))
    };
    std::thread::sleep(Duration::from_millis(300));
    go_tx.send(()).unwrap();
    let (imp, prom) = (imp.join().unwrap(), prom.join().unwrap());
    assert!(imp.is_ok(), "import: {imp:?}, promote: {prom:?}");
    assert!(prom.is_ok(), "import: {imp:?}, promote: {prom:?}");
    let got = content(&c, "target");
    assert!(got == "imported" || got == "promoted", "{got}");
    no_leftovers(dir.path(), &c);
}

/// Two replacing imports of one target: both succeed in turn and nothing is left behind.
#[test]
fn two_replacing_imports_of_the_same_target_run_one_after_the_other() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let c = base_with_src(dir.path(), scratch.path());
    let (v1, v2) = (
        src(scratch.path(), "v1", "first"),
        src(scratch.path(), "v2", "second"),
    );
    let (parked_tx, parked_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel();
    let one = {
        let c = c.clone();
        std::thread::spawn(move || parked_import(&c, &v1, "target", parked_tx, go_rx))
    };
    parked_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let two = {
        let c = c.clone();
        std::thread::spawn(move || {
            let mut hooks = Hooks {
                progress: &mut |_, _| true,
            };
            ingest_replacing(&c, &v2, "target", &mut hooks)
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    };
    std::thread::sleep(Duration::from_millis(300));
    go_tx.send(()).unwrap();
    let (one, two) = (one.join().unwrap(), two.join().unwrap());
    assert!(one.is_ok(), "first: {one:?}, second: {two:?}");
    assert!(two.is_ok(), "first: {one:?}, second: {two:?}");
    let got = content(&c, "target");
    assert!(got == "first" || got == "second", "{got}");
    no_leftovers(dir.path(), &c);
}

/// Two promotes of one target, one parked between its steps by the fault-free path cannot be
/// parked, so run them hard against each other and check the end state.
#[test]
fn racing_promotes_of_one_target_leave_one_clean_result() {
    let dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let c = base_with_src(dir.path(), scratch.path());
    for _ in 0..20 {
        let hs: Vec<_> = (0..4)
            .map(|_| {
                let c = c.clone();
                std::thread::spawn(move || c.promote_base("src", "target").map(|_| ()))
            })
            .collect();
        for h in hs {
            let r = h.join().unwrap();
            assert!(r.is_ok(), "{r:?}");
        }
        assert_eq!(content(&c, "target"), "promoted");
        no_leftovers(dir.path(), &c);
    }
}
