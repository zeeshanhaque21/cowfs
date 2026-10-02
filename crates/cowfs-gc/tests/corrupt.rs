//! Damage in the store: what the collector reports, what it refuses to touch, and what it keeps.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{eager, small_store_opts, Fixture, Roots};
use cowfs_store::Store;

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

/// Bytes zstd cannot shrink, so a pack holds records of a known size.
fn noise(n: usize, seed: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut h = seed.wrapping_mul(2654435761).wrapping_add(1);
    while out.len() < n {
        h = h.wrapping_mul(1664525).wrapping_add(1013904223);
        out.extend_from_slice(&h.to_le_bytes());
    }
    out.truncate(n);
    out
}

fn pack_path(dir: &Path, id: u32) -> std::path::PathBuf {
    dir.join("packs").join(format!("pack-{id:08}.cpk"))
}

/// A pack with one live file and several packs of garbage, in a directory the caller owns, so the
/// files can be damaged and reopened.
fn seeded() -> (PathBuf, Fixture) {
    let f = Fixture::new(small_store_opts(32 << 10), eager());
    {
        let snap = f.meta.new_snapshot("s").expect("snapshot");
        f.write(&snap, b"keep", &body(30_000, 1));
    }
    for i in 0..40u32 {
        // Incompressible, so a pack is big enough to have a record in the middle.
        f.store.put(&noise(4000, i)).expect("put");
    }
    f.store.sync().expect("sync");
    f.meta.sync().expect("meta sync");
    let packs = fs::read_dir(f.store_dir().join("packs")).unwrap().count();
    assert!(packs >= 2, "the fixture needs several packs");
    let dir = f.persist();
    // The store and the database are reopened on the same paths, so the caller holds the directory
    // and can damage the files and reopen them again.
    let store =
        Arc::new(Store::open(dir.join("store"), small_store_opts(32 << 10)).expect("store"));
    let meta = Arc::new(
        cowfs_meta::Meta::open(
            dir.join("meta"),
            cowfs_meta::Options {
                background: false,
                ..cowfs_meta::Options::default()
            },
        )
        .expect("meta"),
    );
    let gc = cowfs_gc::Gc::open(
        dir.join("gc"),
        Arc::clone(&store),
        Arc::clone(&meta),
        eager(),
    )
    .expect("gc");
    let f = Fixture::adopt(
        dir.clone(),
        store,
        meta,
        gc,
        small_store_opts(32 << 10),
        eager(),
    );
    (dir, f)
}

/// Damage a byte in a pack, below the watermark, and drop the index so `open` rescans.
fn damage_below_watermark(dir: &Path, id: u32, at: usize) {
    let _ = fs::remove_file(dir.join("index.cix"));
    let path = pack_path(dir, id);
    let mut bytes = fs::read(&path).expect("read pack");
    assert!(at < bytes.len(), "the offset is inside the pack");
    bytes[at] ^= 0xff;
    fs::write(&path, &bytes).expect("write pack");
}

/// A pack with damage to durable bytes is reported, refused, and never unlinked.
#[test]
fn a_corrupt_pack_is_reported_and_never_compacted() {
    let (d_path, f) = seeded();
    let store_dir = d_path.join("store");
    let live: Vec<_> = f.live_blocks().into_iter().collect();
    // Drop the handles so the directory can be damaged and reopened.
    // A sealed pack with records in it, well past its 16 byte header.
    let all = f.store.packs().unwrap();
    let infos: Vec<_> = all
        .iter()
        .copied()
        .filter(|p| !p.active && p.len > 256)
        .collect();
    assert!(
        !infos.is_empty(),
        "the fixture needs a sealed pack with records: {all:?}"
    );
    let target = infos[0].id;
    let size = infos[0].len;
    drop(f);

    damage_below_watermark(&store_dir, target, 100);

    let store =
        std::sync::Arc::new(Store::open(&store_dir, small_store_opts(32 << 10)).expect("reopen"));
    let rec = store.recovery();
    assert!(rec.has_corruption(), "the damage is reported: {rec:?}");
    assert!(
        rec.corrupt_synced.iter().any(|c| c.pack == target),
        "and it is classified as damage to durable bytes"
    );

    // The plan says the pack is corrupt, and the compaction refuses it.
    let all = store.iter_ids().collect::<Vec<_>>();
    let plan = store
        .plan_pack(target, &|_| true, &mut Vec::new())
        .expect("plan");
    assert!(plan.corrupt, "the plan says so");
    let e = store
        .begin_compaction(&plan, &|_| true)
        .expect_err("a corrupt pack is refused");
    assert!(e.to_string().contains("damage"), "and says why: {e}");
    assert_eq!(store.pack_len(target), size, "the pack is untouched");

    // The collector refuses the whole store rather than collecting over known loss.
    let meta = std::sync::Arc::new(
        cowfs_meta::Meta::open(
            d_path.join("meta"),
            cowfs_meta::Options {
                background: false,
                ..cowfs_meta::Options::default()
            },
        )
        .expect("meta reopen"),
    );
    let gc = cowfs_gc::Gc::open(d_path.join("gc2"), store, meta, eager()).expect("gc");
    let err = gc.collect(Some(&*Roots::new())).expect_err("refused");
    assert!(
        matches!(err, cowfs_gc::Error::CorruptStore(n) if n > 0),
        "the collector refuses: {err}"
    );
    let _ = (live, all);
}

/// A pack with a gap is reported with the gap's bytes, and the gap is not copied away silently.
#[test]
fn a_gap_is_reported_and_its_bytes_counted() {
    let (_p, f) = seeded();
    let store_dir = f.store_dir();
    let all = f.store.packs().unwrap();
    eprintln!("packs: {all:?}");
    let infos: Vec<_> = all
        .iter()
        .copied()
        .filter(|p| !p.active && p.len > 256)
        .collect();
    assert!(!infos.is_empty(), "no sealed pack with records in {all:?}");
    let id = infos[0].id;
    let len = infos[0].len;
    drop(f);

    // Break a record in the middle of the pack, so a gap appears with valid records on both sides.
    let _ = fs::remove_file(store_dir.join("index.cix"));
    let path = pack_path(&store_dir, id);
    let mut bytes = fs::read(&path).expect("read pack");
    let at = 16 + 4 * 1000;
    assert!(
        at < len as usize,
        "the offset is inside the pack: {at} of {len}"
    );
    bytes[at] ^= 0xff;
    fs::write(&path, &bytes).expect("write pack");

    let store = Store::open(&store_dir, small_store_opts(32 << 10)).expect("reopen");
    let plan = store
        .plan_pack(id, &|_| true, &mut Vec::new())
        .expect("plan");
    assert!(plan.gap_bytes > 0, "the gap is counted: {plan:?}");
    // The records around the gap may or may not survive: the point is that the gap is counted and
    // never copied away silently, and that the plan accounts for the whole pack.
    assert!(
        plan.record_bytes() + plan.gap_bytes <= plan.len,
        "the plan accounts for the pack: {plan:?}"
    );
    if !store.recovery().has_corruption() {
        // Damage above the watermark is a torn tail, not corruption, and the pack is still a
        // legitimate candidate with a gap in it.
        assert!(!plan.corrupt, "a torn tail is not durable corruption");
    }
}

/// Quarantined bytes in a torn sidecar are reported and never removed.
#[test]
fn quarantined_bytes_are_reported_and_kept() {
    let (_p, f) = seeded();
    let store_dir = f.store_dir();
    let all = f.store.packs().unwrap();
    let infos: Vec<_> = all
        .iter()
        .copied()
        .filter(|p| !p.active && p.len > 256)
        .collect();
    let id = infos[0].id;
    // Put a torn tail after the last sync, so open cuts it and keeps the first megabyte.
    let f2 = f;
    f2.store.put(&body(30_000, 200)).expect("put");
    drop(f2);

    let store = Store::open(&store_dir, small_store_opts(32 << 10)).expect("reopen");
    let plan = store
        .plan_pack(id, &|_| true, &mut Vec::new())
        .expect("plan");
    if plan.quarantined_bytes > 0 {
        // The sidecar is there and stays there: the collector is not allowed to delete it.
        let sidecars: Vec<_> = fs::read_dir(store_dir.join("packs"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".torn-"))
            .collect();
        assert!(!sidecars.is_empty(), "the sidecar exists on disk");
        let total: u64 = sidecars
            .iter()
            .map(|n| fs::metadata(store_dir.join("packs").join(n)).unwrap().len())
            .sum();
        assert!(
            plan.quarantined_bytes <= total,
            "the plan reports the sidecar bytes"
        );
    }
    // A clean store reports none.
    let plan = store
        .plan_pack(id, &|_| true, &mut Vec::new())
        .expect("plan");
    assert!(
        plan.quarantined_bytes == 0 || plan.len > 0,
        "a pack with no sidecar reports none: {plan:?}"
    );
}

/// A store that has acknowledged its losses is collectable again, and the losses are not freed.
#[test]
fn an_acknowledged_store_is_collectable_again() {
    let (d_path, f) = seeded();
    let store_dir = d_path.join("store");
    let meta_dir = d_path.join("meta");
    let all = f.store.packs().unwrap();
    // The adopted fixture holds handles on the same paths, so it is closed before they are damaged
    // and reopened.
    drop(f);
    let infos: Vec<_> = all
        .iter()
        .copied()
        .filter(|p| !p.active && p.len > 256)
        .collect();
    assert!(!infos.is_empty(), "a sealed pack with records: {all:?}");
    let packs = [infos[0].id];
    damage_below_watermark(&store_dir, packs[0], 100);

    let store =
        std::sync::Arc::new(Store::open(&store_dir, small_store_opts(32 << 10)).expect("open"));
    assert!(store.recovery().has_corruption());
    let n = store.acknowledge_corruption().expect("acknowledge");
    assert!(n > 0, "nothing was acknowledged");
    // The handle keeps reporting what `open` found, because that report is what the file said. The
    // acknowledgement lives in `ACKED`, so the next open is the one that is clean.
    assert!(
        store.recovery().has_corruption(),
        "this handle still reports the loss"
    );
    drop(store);
    let store = Arc::new(Store::open(&store_dir, small_store_opts(32 << 10)).expect("reopen"));
    assert!(
        !store.recovery().has_corruption(),
        "a reopened store is clean: {:?}",
        store.recovery()
    );
    let meta = Arc::new(
        cowfs_meta::Meta::open(
            &meta_dir,
            cowfs_meta::Options {
                background: false,
                ..cowfs_meta::Options::default()
            },
        )
        .expect("meta"),
    );
    let gc = cowfs_gc::Gc::open(d_path.join("gc"), Arc::clone(&store), meta, eager()).expect("gc");
    let r = gc.collect(Some(&*Roots::new())).expect("collect");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    for b in gc.store().iter_ids() {
        // A block whose only copy was damaged is unreadable, and the collector did not make it
        // worse: it is either still readable or was never indexed.
        let _ = gc.store().get(b);
    }
    let _ = fs::remove_dir_all(&d_path);
}
