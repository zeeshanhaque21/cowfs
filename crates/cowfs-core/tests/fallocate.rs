//! `Vfs::fallocate` on `Core` (#103 slice B): snapshot safety, hole-ref integrity, blocks,
//! edge chunks read back from the store, and atomicity against concurrent growth.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};

use common::*;
use cowfs_vfs::{Error, FallocMode, Vfs, ROOT_INO};

const CHUNKY: usize = 3 << 20;

fn punch_model(model: &mut [u8], off: usize, len: usize) {
    let hi = (off + len).min(model.len());
    if off < hi {
        model[off..hi].fill(0);
    }
}

#[test]
fn a_hole_punched_in_a_shared_file_does_not_change_the_snapshot() {
    let fx = fixture();
    let c = &fx.core;
    c.create_snapshot("a").unwrap();
    let a = c.snapshot_view("a").unwrap();
    let data = pattern(CHUNKY, 21);
    let f = mkfile(&a, ROOT_INO, "f", &data).ino;
    c.sync().unwrap();
    c.fork_snapshot("a", "b").unwrap();
    let b = c.snapshot_view("b").unwrap();
    let fb = b.lookup(ROOT_INO, b"f").unwrap().ino;

    let live = |name: &str| {
        let mut ids = c
            .live_blocks(name, &mut cowfs_meta::Marker::default())
            .unwrap();
        ids.sort();
        ids
    };
    let (a_before, b_before) = (live("a"), live("b"));
    a.fallocate(f, FallocMode::PunchHole, 100_000, 1_500_000)
        .unwrap();
    c.sync().unwrap();
    // the collector's input: the fork still names every block it did, and the punched file no
    // longer names the chunks that fell inside the range
    assert_eq!(live("b"), b_before, "the fork's live blocks changed");
    assert!(
        a_before.iter().filter(|id| !live("a").contains(id)).count() > 0,
        "the punched file still names every block"
    );

    let mut want = data.clone();
    punch_model(&mut want, 100_000, 1_500_000);
    assert!(read_all(&a, f) == want, "the punched file reads wrong");
    assert!(read_all(&b, fb) == data, "the fork changed under a punch");
    c.drop_caches();
    assert!(read_all(&b, fb) == data, "the fork changed after a reload");
    assert!(
        c.fsck().unwrap().damage.is_empty(),
        "fsck found damage after a punch"
    );
}

#[test]
fn punched_ranges_are_hole_refs_the_walk_and_fsck_accept() {
    let fx = fixture();
    let c = &fx.core;
    c.create_snapshot("s").unwrap();
    let v = c.snapshot_view("s").unwrap();
    let f = mkfile(&v, ROOT_INO, "f", &pattern(CHUNKY, 5)).ino;
    c.sync().unwrap();
    let full = v.getattr(f).unwrap().blocks;
    v.fallocate(f, FallocMode::PunchHole, 0, CHUNKY as u64)
        .unwrap();
    assert!(
        v.getattr(f).unwrap().blocks < full / 2,
        "blocks did not drop after punching the whole file"
    );
    assert_eq!(v.getattr(f).unwrap().size, CHUNKY as u64);
    assert!(read_all(&v, f).iter().all(|&b| b == 0));
    c.sync().unwrap();
    c.check().expect("check");
    assert!(c.fsck().unwrap().damage.is_empty());
    let mut marker = cowfs_meta::Marker::default();
    let sid = c.list_snapshots().unwrap()[0].id;
    let snap = c
        .meta()
        .snapshot_by_id(cowfs_meta::SnapshotId(sid))
        .unwrap();
    for id in snap.live_blocks(&mut marker).unwrap() {
        let id = id.unwrap();
        assert!(
            id.as_bytes() != &[0u8; 32],
            "the walk handed out a hole as a block"
        );
        assert!(c.store().contains(id), "a live block is missing: {id}");
    }
}

/// Random punches and zero ranges over a multi-chunk file, interleaved with flushes and cache
/// drops so edge chunks are re-read from the store, against a byte model.
#[test]
fn random_fallocate_matches_the_model_across_flushes_and_reloads() {
    let fx = fixture();
    let c = &fx.core;
    c.create_snapshot("s").unwrap();
    let v = c.snapshot_view("s").unwrap();
    let mut model = pattern(CHUNKY, 9);
    let f = mkfile(&v, ROOT_INO, "f", &model).ino;
    let mut rng = Rng(0xFA11_0C47);
    for i in 0..120u64 {
        let off = rng.below(CHUNKY as u64 + 200_000);
        let len = 1 + rng.below(400_000);
        let (mode, zero, extend) = match rng.below(5) {
            0 => (FallocMode::Allocate, false, true),
            1 => (FallocMode::KeepSize, false, false),
            2 => (FallocMode::PunchHole, true, false),
            3 => (FallocMode::ZeroRange, true, true),
            _ => (FallocMode::ZeroRangeKeepSize, true, false),
        };
        v.fallocate(f, mode, off, len).unwrap();
        if zero {
            punch_model(&mut model, off as usize, len as usize);
        }
        if extend && (off + len) as usize > model.len() {
            model.resize((off + len) as usize, 0);
        }
        if i % 7 == 3 {
            let w = 1 + rng.below(50_000) as usize;
            let o = rng.below(model.len() as u64) as usize;
            let d = pattern(w, i);
            write_all(&v, f, o as u64, &d);
            if model.len() < o + w {
                model.resize(o + w, 0);
            }
            model[o..o + w].copy_from_slice(&d);
        }
        match i % 11 {
            4 => c.flush().unwrap(),
            9 => {
                c.sync().unwrap();
                c.drop_caches();
            }
            _ => {}
        }
        assert_eq!(v.getattr(f).unwrap().size, model.len() as u64, "size {i}");
    }
    assert!(read_all(&v, f) == model, "content differs from the model");
    c.sync().unwrap();
    c.drop_caches();
    assert!(read_all(&v, f) == model, "content differs after a reload");
    assert!(c.fsck().unwrap().damage.is_empty());
}

/// A reader that never sees the size go down while writers grow the file and fallocate runs
/// beside them: the shrink-a-concurrent-extension race the adapter-only emulation had.
#[test]
fn allocate_never_shrinks_a_concurrently_growing_file() {
    let fx = fixture();
    let c = &fx.core;
    c.create_snapshot("s").unwrap();
    let v = c.snapshot_view("s").unwrap();
    let f = v.create(ROOT_INO, b"f", 0o644).unwrap().ino;
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        s.spawn(|| {
            let mut last = 0;
            while !stop.load(Ordering::Acquire) {
                let size = v.getattr(f).unwrap().size;
                assert!(size >= last, "size went from {last} to {size}");
                last = size;
            }
        });
        let alloc = s.spawn(|| {
            let mut i = 0u64;
            while !stop.load(Ordering::Acquire) {
                let end = (i % 2000) * 4096;
                if end > 0 {
                    v.fallocate(f, FallocMode::Allocate, 0, end).unwrap();
                    v.fallocate(f, FallocMode::ZeroRangeKeepSize, 0, 1).unwrap();
                }
                i += 1;
            }
        });
        for n in 1..=2000u64 {
            v.write(f, n * 4096 - 1, b"x").unwrap();
        }
        stop.store(true, Ordering::Release);
        alloc.join().unwrap();
    });
    assert_eq!(v.getattr(f).unwrap().size, 2000 * 4096);
}

#[test]
fn errors_leave_the_file_alone_and_past_the_maximum_is_an_error() {
    let fx = fixture();
    let c = &fx.core;
    c.create_snapshot("s").unwrap();
    let v = c.snapshot_view("s").unwrap();
    let f = mkfile(&v, ROOT_INO, "f", b"hello").ino;
    assert_eq!(
        v.fallocate(f, FallocMode::PunchHole, 0, 0).unwrap_err(),
        Error::InvalidArgument
    );
    assert_eq!(
        v.fallocate(ROOT_INO, FallocMode::Allocate, 0, 1)
            .unwrap_err(),
        Error::IsDir
    );
    // a range past the largest file is EFBIG, like write and setattr, and changes nothing
    for (off, len) in [(1u64 << 42, 1u64), (u64::MAX, 2), (1, u64::MAX)] {
        assert_eq!(
            v.fallocate(f, FallocMode::ZeroRange, off, len).unwrap_err(),
            Error::FileTooBig
        );
    }
    assert_eq!(
        v.write(f, 1 << 42, b"x").unwrap_err(),
        v.fallocate(f, FallocMode::Allocate, 1 << 42, 1)
            .unwrap_err(),
        "fallocate and write must fail past the maximum with the same error"
    );
    assert_eq!(read_all(&v, f), b"hello");
}
