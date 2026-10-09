//! Process-exit injection inside a real `Gc::collect` that reclaims real bytes (issue 173).
//!
//! The store exits with status 77 at its n-th durability boundary (write, fsync, dir fsync,
//! rename, truncate) when `C7D_EXIT_BOUNDARY_N=n` is set and the `fault-injection` feature is on.
//! This test re-executes itself as a child that runs one whole collect over a seeded store with
//! several mostly-dead sealed packs, once per n from 1 upward, until a child finishes without
//! dying. After every death the parent reopens the store and checks the receipts: every live block
//! reads back with exactly the bytes recorded before the crash, the store reports no corruption,
//! `fsck` is clean, and a second collect completes.
//!
//! The sweep covers every store `Io` boundary of the cycle. It does not crash between the metadata
//! sync at the freeze or the collector's own `gcstate` writes, which do not go through `Io`. An
//! unlink is not a boundary either, but a process exit keeps the page cache, so "unlinked, dir not
//! yet fsynced" looks the same as "dir fsynced"; a regression that moves the unlink earlier is
//! therefore not caught here.
//! It says nothing about power loss, which drops unsynced bytes; see `docs/crash-injection-173.md`.

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use common::{eager, small_store_opts, Fixture, Roots};
use cowfs_gc::Gc;
use cowfs_store::{BlockId, Store};

const CHILD_DIR: &str = "COWFS_GC_INJECT_DIR";
const PACK: u64 = 32 << 10;
/// A counter that never fires must not loop forever.
const CAP: u32 = 4000;

fn body(n: usize, seed: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut h = u32::from(seed).wrapping_mul(2654435761).wrapping_add(1);
    for _ in 0..n {
        h = h.wrapping_mul(1664525).wrapping_add(1013904223);
        out.push(if h >> 29 == 0 {
            b'a'.wrapping_add((h >> 8) as u8)
        } else {
            (h >> 16) as u8
        });
    }
    out
}

fn open_meta(dir: &Path) -> Arc<cowfs_meta::Meta> {
    Arc::new(
        cowfs_meta::Meta::open(
            dir.join("meta"),
            cowfs_meta::Options {
                background: false,
                ..cowfs_meta::Options::default()
            },
        )
        .expect("meta"),
    )
}

/// The child: one whole collect, then a one-line report. Does nothing outside the harness.
#[test]
fn child_collects_once() {
    let Ok(dir) = std::env::var(CHILD_DIR) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let store = Arc::new(Store::open(dir.join("store"), small_store_opts(PACK)).expect("store"));
    let gc = Gc::open(dir.join("gcstate"), store, open_meta(&dir), eager()).expect("gc");
    let roots = Roots::new();
    let r = gc.collect(Some(&*roots)).expect("collect");
    assert!(r.errors.is_empty(), "child collect: {:?}", r.errors);
    std::fs::write(
        dir.join("report"),
        format!("{} {}", r.freed_bytes, r.packs_unlinked),
    )
    .expect("report");
}

/// A template store: sealed packs that each mix dead and referenced records.
/// Returns the template directory and the receipt (block id to the bytes it must read back as).
fn template() -> (PathBuf, HashMap<BlockId, Vec<u8>>) {
    let f = Fixture::new(small_store_opts(PACK), eager());
    let snap = f.meta.new_snapshot("s").expect("snapshot");
    // Garbage and kept files interleaved, so every sealed pack holds both live and dead records
    // and a collect has to copy before it unlinks. Packs that are all dead would only be unlinked.
    let mut want = HashMap::new();
    for i in 0..24u8 {
        f.store.put(&body(4000, i)).expect("put");
        if i % 2 == 0 {
            let data = body(3000, i.wrapping_add(100));
            let chunks = f.write(&snap, format!("keep{i}").as_bytes(), &data);
            let mut at = 0usize;
            for c in &chunks {
                let n = usize::try_from(c.len).unwrap_or(0);
                want.insert(c.id, data[at..(at + n).min(data.len())].to_vec());
                at += n;
            }
        }
    }
    f.store.sync().expect("store sync");
    f.meta.sync().expect("meta sync");
    (f.persist(), want)
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for e in std::fs::read_dir(from).expect("read_dir").flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            copy_tree(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).expect("copy");
        }
    }
}

/// Run the child on a fresh copy of the template. `n` is the boundary to die at, if any.
/// Returns the scratch dir and the child's exit code.
fn run_child(tpl: &Path, n: Option<u32>) -> (tempfile::TempDir, i32) {
    let dir = tempfile::tempdir().expect("tempdir");
    copy_tree(tpl, dir.path());
    let mut cmd = Command::new(std::env::current_exe().expect("current exe"));
    cmd.args(["child_collects_once", "--exact", "--nocapture"])
        .env(CHILD_DIR, dir.path())
        .env_remove("C7D_EXIT_SYNC_N")
        .env_remove("C7D_EXIT_FILE")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match n {
        Some(n) => cmd.env("C7D_EXIT_BOUNDARY_N", n.to_string()),
        None => cmd.env_remove("C7D_EXIT_BOUNDARY_N"),
    };
    let code = cmd.status().expect("spawn child").code().unwrap_or(-1);
    (dir, code)
}

/// Names of the pack files on disk under a store directory's parent, read without opening it.
fn pack_files(dir: &Path) -> std::collections::BTreeSet<String> {
    std::fs::read_dir(dir.join("store").join("packs"))
        .expect("packs dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".cpk"))
        .collect()
}

fn expect_receipts(store: &Store, want: &HashMap<BlockId, Vec<u8>>, at: &str) {
    for (b, bytes) in want {
        let got = store
            .get(*b)
            .unwrap_or_else(|e| panic!("{at}: live block lost: {e}"));
        assert_eq!(&got, bytes, "{at}: live block has the wrong bytes");
    }
}

#[test]
fn a_process_exit_at_every_boundary_of_a_reclaiming_collect_loses_nothing() {
    let (tpl, want) = template();

    // Control: an uncrashed collect over this fixture really reclaims, so the sweep below is not
    // another gc case that frees 0 bytes.
    let (ctl, code) = run_child(&tpl, None);
    assert_eq!(code, 0, "the control child must finish");
    let report = std::fs::read_to_string(ctl.path().join("report")).expect("control report");
    let mut it = report
        .split_whitespace()
        .map(|v| v.parse::<u64>().expect("number"));
    let (freed, unlinked) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
    assert!(freed > 0, "the control collect freed nothing: {report}");
    assert!(
        unlinked >= 2,
        "the control collect unlinked <2 packs: {report}"
    );

    {
        // The control's receipts too: a collect that frees bytes by losing live ones is not a pass.
        let s =
            Store::open(ctl.path().join("store"), small_store_opts(PACK)).expect("control store");
        expect_receipts(&s, &want, "uncrashed control");
        assert!(
            s.fsck().expect("fsck").is_clean(),
            "control: fsck found damage"
        );
    }
    let seed_packs = pack_files(&tpl);
    assert!(
        seed_packs.len() >= 4,
        "the fixture needs sealed packs: {seed_packs:?}"
    );

    let mut crashed = 0u32;
    let mut mid_reclaim = 0u32;
    let mut done_at = None;
    for n in 1..=CAP {
        let (dir, code) = run_child(&tpl, Some(n));
        if code == 0 {
            done_at = Some(n);
            break;
        }
        assert_eq!(
            code, 77,
            "boundary {n}: the child died some other way ({code})"
        );
        crashed += 1;

        let at = format!("boundary {n}");
        // Read the raw directory before the store is opened: an open can ignore a file below the
        // watermark floor, so only the files on disk say whether the unlink really happened.
        let unlinked_before_reopen = !seed_packs.is_subset(&pack_files(dir.path()));
        let store = Arc::new(
            Store::open(dir.path().join("store"), small_store_opts(PACK))
                .unwrap_or_else(|e| panic!("{at}: the store does not open: {e}")),
        );
        assert!(
            !store.recovery().has_corruption(),
            "{at}: corruption {:?}",
            store.recovery()
        );
        expect_receipts(&store, &want, &at);
        assert!(
            store.fsck().expect("fsck").is_clean(),
            "{at}: fsck found damage"
        );
        // A crash after at least one unlink, with every receipt intact: the crash landed mid-reclaim.
        if unlinked_before_reopen {
            mid_reclaim += 1;
        }

        // A second collect over the crash image runs without error and keeps every receipt.
        let gc = Gc::open(
            dir.path().join("gcstate"),
            Arc::clone(&store),
            open_meta(dir.path()),
            eager(),
        )
        .expect("gc reopen");
        let roots = Roots::new();
        let r = gc.collect(Some(&*roots)).expect("second collect");
        assert!(r.errors.is_empty(), "{at}: second collect {:?}", r.errors);
        expect_receipts(gc.store(), &want, &format!("{at} after recovery"));
        assert!(
            gc.store().fsck().expect("fsck").is_clean(),
            "{at}: fsck after recovery"
        );
    }
    let done_at = done_at.unwrap_or_else(|| panic!("no boundary count under {CAP} finished"));
    assert!(crashed >= 10, "the sweep crashed only {crashed} times");
    assert!(
        mid_reclaim > 0,
        "no crash image had a pack already unlinked, so none landed mid-reclaim"
    );

    // The count is stable: the last boundary still kills and the first past the end does not.
    assert_eq!(
        run_child(&tpl, Some(done_at - 1)).1,
        77,
        "boundary count is not stable"
    );
    assert_eq!(
        run_child(&tpl, Some(done_at + 1)).1,
        0,
        "boundary count is not stable"
    );
    eprintln!(
        "crash_inject: {crashed} crashes over {done_at} boundaries, {mid_reclaim} after an unlink, \
         control freed {freed} bytes in {unlinked} packs"
    );
    let _ = std::fs::remove_dir_all(&tpl);
}
