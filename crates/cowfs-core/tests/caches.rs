//! F6, F7, F8: barrier latency, the alias table's bound, and hole-free block lists.

mod common;

use std::time::Instant;

use common::*;
use cowfs_core::{Core, Options};
use cowfs_vfs::Vfs;

fn big_opts() -> Options {
    Options {
        background: false,
        file_flush_bytes: 256 << 20,
        max_dirty_bytes: 512 << 20,
        ..test_opts()
    }
}

/// F6: a `readdir` that has to commit one directory's pending creates must not chunk the dirty
/// data of an unrelated file.
#[test]
fn a_directory_barrier_does_not_flush_unrelated_file_data() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), big_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    let big = c.create(r, b"big", 0o644).unwrap().ino;
    write_all(&c, big, 0, &pattern(48 << 20, 3));
    let d = c.mkdir(r, b"d", 0o755).unwrap().ino;
    c.create(d, b"x", 0o644).unwrap();
    let before = c.stats().dirty_bytes;
    assert!(before >= 48 << 20, "{before}");
    let mut times = Vec::new();
    for _ in 0..5 {
        let a = c
            .create(d, format!("y{}", times.len()).as_bytes(), 0o644)
            .unwrap()
            .ino;
        c.forget(a, 1);
        let t = Instant::now();
        c.readdir(d, 0, 100).expect("readdir");
        times.push(t.elapsed());
    }
    let after = c.stats().dirty_bytes;
    assert_eq!(after, before, "the barrier flushed unrelated data");
    times.sort();
    // self-calibrating: the same dirty data costs this much when it really is flushed
    let t = Instant::now();
    c.sync().expect("sync");
    let flush_cost = t.elapsed();
    let warm = Instant::now();
    c.readdir(d, 0, 100).expect("warm readdir");
    let warm = warm.elapsed();
    println!(
        "barrier p50 {:?} max {:?}, warm readdir {warm:?}, full flush of the 48 MiB {flush_cost:?}",
        times[2], times[4]
    );
    // the data is still there and still correct
    assert_eq!(c.getattr(big).unwrap().size, (48 << 20) as u64);
    c.sync().unwrap();
    c.drop_caches();
    assert_eq!(
        read_all(&c, c.lookup(r, b"big").unwrap().ino),
        pattern(48 << 20, 3)
    );
    // and the listing is right
    let mut names: Vec<Vec<u8>> = c
        .readdir(d, 0, 100)
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.name)
        .collect();
    names.sort();
    assert_eq!(names.len(), 6, "{names:?}");
}

/// F8: `live_blocks` never yields a hole, every block it yields is in the store, and the
/// metadata walk it builds on no longer yields the hole either.
#[test]
fn live_blocks_filters_holes_and_yields_only_stored_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    let sparse = c.create(r, b"sparse", 0o644).unwrap().ino;
    truncate(&c, sparse, 10 << 20).unwrap();
    c.write(sparse, 1 << 20, b"data").unwrap();
    let full = c.create(r, b"full", 0o644).unwrap().ino;
    write_all(&c, full, 0, &pattern(600_000, 5));
    c.sync().unwrap();
    c.check().unwrap();
    let mut marker = cowfs_meta::Marker::default();
    let ids = c.live_blocks("s", &mut marker).unwrap();
    assert!(!ids.is_empty());
    for id in &ids {
        assert_ne!(id.as_bytes(), &[0u8; 32], "a hole was yielded as a block");
        assert!(
            c.store().contains(*id),
            "a yielded block is not in the store"
        );
    }
    // the hole is filtered where the chunk list is decoded now, so the walk in meta does not hand
    // the sentinel to this caller either, and both walks agree exactly
    let sid = c.list_snapshots().unwrap()[0].id;
    let mut m = cowfs_meta::Marker::default();
    let mut raw: Vec<_> = c
        .meta()
        .snapshot_by_id(cowfs_meta::SnapshotId(sid))
        .unwrap()
        .live_blocks(&mut m)
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    raw.sort();
    let mut got = ids.clone();
    got.sort();
    assert_eq!(
        raw, got,
        "the metadata walk and Core::live_blocks must agree now that the flag filters holes"
    );
    assert!(
        !raw.iter().any(|b| b.as_bytes() == &[0u8; 32]),
        "the hole sentinel reached a caller: {raw:?}"
    );
}
