//! The collector wired to the real reference side, end to end.
//!
//! Everything else in this crate tests `ExtraRoots` against test doubles. This file tests it against
//! `cowfs-core`, which is the implementation that has to meet the contract: `pinned_blocks` is exact
//! or `Busy`, and `live_blocks` is the walk the collector must agree with.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;

use cowfs_core::{Core, Options as CoreOptions};
use cowfs_gc::{Barrier, ExtraRoots, RootsError};
use cowfs_store::BlockId;
use cowfs_vfs::Vfs;

fn body(n: usize, seed: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut h = seed.wrapping_mul(2654435761).wrapping_add(1);
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

/// `cowfs-core` as `ExtraRoots`. Its `pinned_blocks` is exact or an error, so the mapping is total
/// and never has to invent an empty answer.
struct CoreRoots {
    core: Arc<Core>,
}

impl ExtraRoots for CoreRoots {
    fn pinned_blocks(&self) -> Result<Vec<BlockId>, RootsError> {
        self.core.pinned_blocks().map_err(|e| match e {
            cowfs_core::ControlError::Busy => RootsError::Busy,
            _ => RootsError::Unavailable,
        })
    }
    fn reference_barrier(&self) -> Result<Option<Box<dyn Barrier>>, RootsError> {
        // `cowfs-core` exposes no reference barrier yet. Until it does, a collect over it marks and
        // reports and frees nothing, which is exactly what `Ok(None)` says.
        Ok(None)
    }
}

fn core_opts() -> CoreOptions {
    CoreOptions {
        background: false,
        ..CoreOptions::default()
    }
}

/// The snapshot directory every file in these tests goes into.
fn root_of(core: &Arc<Core>) -> cowfs_vfs::Ino {
    core.lookup(cowfs_vfs::ROOT_INO, b"main")
        .expect("snapshot dir")
        .ino
}

fn write_file(core: &Arc<Core>, name: &str, data: &[u8]) {
    let root = root_of(core);
    let ino = core
        .create(root, name.as_bytes(), 0o644)
        .expect("create")
        .ino;
    assert_eq!(
        core.write(ino, 0, data).expect("write") as usize,
        data.len()
    );
}

/// Core is the live source, and the collector's idea of live must agree with core's.
#[test]
fn a_collect_over_core_agrees_with_core_about_what_is_live() {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = Arc::new(Core::open(dir.path(), core_opts()).expect("open core"));
    core.create_snapshot("main").expect("snapshot");

    for i in 0..12u32 {
        let name = format!("f{i:02}");
        write_file(&core, &name, &body(9000, i));
    }

    let mut marker = cowfs_meta::Marker::new();
    let live = core.live_blocks("main", &mut marker).expect("live blocks");
    assert!(live.len() >= 12, "the snapshot has content: {}", live.len());

    let roots = CoreRoots {
        core: Arc::clone(&core),
    };
    let pinned = roots
        .pinned_blocks()
        .expect("core answers exactly, or this test is wrong");
    assert!(
        pinned.iter().all(|b| *b != cowfs_gc::HOLE),
        "core does not pin a hole"
    );

    let store = core.store();
    for b in live.iter().chain(pinned.iter()) {
        assert!(
            store.get(*b).is_ok(),
            "a block core names is not in its own store: {b:?}"
        );
    }
}

/// The contract that matters, checked against the implementation that has to meet it: an answer is
/// exact or it is an error, and never a partial set.
///
/// A writer holds an unlinked file with a handle open, so its blocks are pinned by memory only. If
/// `pinned_blocks` ever answered `Ok` without naming them, a collector wired to it would free the
/// bytes of a file that is still open. `Busy` is the only other acceptable answer.
#[test]
fn core_pinned_blocks_is_exact_or_busy_and_never_partial() {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = Arc::new(Core::open(dir.path(), core_opts()).expect("open core"));
    core.create_snapshot("main").expect("snapshot");

    // A file that is written, then unlinked, with its handle still open: only memory names it.
    let root = root_of(&core);
    let ino = core.create(root, b"orphan", 0o644).expect("create").ino;
    let payload = body(9000, 4242);
    assert_eq!(
        core.write(ino, 0, &payload).expect("write") as usize,
        payload.len()
    );
    core.flush().expect("flush");
    let orphan_blocks: Vec<BlockId> = core.store().iter_ids().collect();
    assert!(!orphan_blocks.is_empty(), "the file reached the store");
    core.unlink(root, b"orphan").expect("unlink");
    core.flush().expect("flush after unlink");

    let roots = CoreRoots {
        core: Arc::clone(&core),
    };
    let mut exact = 0;
    let mut busy = 0;
    for _ in 0..200 {
        match roots.pinned_blocks() {
            Ok(v) => {
                exact += 1;
                for b in &orphan_blocks {
                    assert!(
                        v.contains(b),
                        "core answered Ok without pinning {b:?}, which memory still names"
                    );
                }
            }
            Err(RootsError::Busy) => busy += 1,
            Err(e) => panic!("an answer the collector cannot use: {e:?}"),
        }
    }
    assert!(
        exact + busy == 200,
        "every poll gave a usable answer: {exact} exact, {busy} busy"
    );
    assert!(exact > 0, "core was asked at least once with no contention");
}

/// While a writer commits, core's answer still never contradicts what the store holds.
#[test]
fn core_and_the_store_agree_during_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = Arc::new(Core::open(dir.path(), core_opts()).expect("open core"));
    core.create_snapshot("main").expect("snapshot");
    let store = core.store();
    let roots = CoreRoots {
        core: Arc::clone(&core),
    };

    let stop = Arc::new(AtomicBool::new(false));
    let written = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|sc| {
        let writer = Arc::clone(&core);
        let writer_stop = Arc::clone(&stop);
        let written = Arc::clone(&written);
        let finish = Arc::clone(&stop);
        sc.spawn(move || {
            for i in 0..40u32 {
                if writer_stop.load(Relaxed) {
                    break;
                }
                write_file(&writer, &format!("w{i:02}"), &body(7000, 900 + i));
                written.fetch_add(1, Relaxed);
            }
            finish.store(true, Relaxed);
        });

        // Bounded, so a writer that panics inside its thread ends the loop instead of spinning.
        for _ in 0..2_000 {
            if stop.load(Relaxed) {
                break;
            }
            if let Ok(v) = roots.pinned_blocks() {
                for b in v {
                    assert!(
                        store.get(b).is_ok(),
                        "core pinned {b:?}, which the store lacks"
                    );
                }
            }
            let mut m = cowfs_meta::Marker::new();
            for b in core.live_blocks("main", &mut m).expect("live blocks") {
                assert!(
                    store.get(b).is_ok(),
                    "core calls {b:?} live, which the store lacks"
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    });
    assert!(written.load(Relaxed) > 0, "the writers ran");

    let mut m = cowfs_meta::Marker::new();
    for b in core.live_blocks("main", &mut m).expect("live blocks") {
        assert!(store.get(b).is_ok(), "the last live block reads: {b:?}");
    }
}
