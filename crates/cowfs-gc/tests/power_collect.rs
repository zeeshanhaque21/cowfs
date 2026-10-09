//! Issue 173, slice 4: power loss across a real, reclaiming `Gc::collect`.
//!
//! The store's op log (`oplog_start`) records every write, fsync, create, unlink and directory
//! fsync of one collect, and the metadata database runs on a recording redb backend whose events
//! are stamped into the same log as markers, so both share one order. For every op index and several
//! seeds this rebuilds the disk a power cut at that point could leave:
//!
//! - the store, by the crash model (`crashmodel::crash_image`): unsynced writes survive in any
//!   subset, an unlink survives only if the packs directory was fsynced after it;
//! - the metadata database, as the writes that precede its last completed `sync_data` (every
//!   unsynced redb write is lost, which is a legal image);
//! - the collector's own state directory, as an arbitrary mix of its old file, its new file, a torn
//!   prefix of the new file and no file. Torn, old and missing states must be safe, and
//!   `gc_state_is_advisory` is the evidence. Same-length corrupted bytes are safe too: `mark.bin`
//!   carries a trailing hash and a mismatch discards the cache (`mark_bin_bit_rot_is_rebuilt`).
//!
//! Each image is reopened with the shipped open paths. Every block any durable snapshot references
//! must read back byte for byte, the store must report no loss and pass `fsck`, and a second collect
//! over the image must finish without error and keep every receipt.
//!
//! A process crash keeps the page cache, so it cannot see an unlink that outran the watermark raise
//! or a copy that was never fsynced; `crash_inject.rs` and `crash.rs` are process-crash fidelity.

mod common;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::{eager, small_store_opts, Roots};
use cowfs_gc::{Gc, HOLE};
use cowfs_meta::{Marker, Meta, Options as MetaOptions, Snapshot, ROOT_INO};
use cowfs_store::crashmodel::{crash_image, read_image, write_image, Image, Rng};
use cowfs_store::{oplog_marker, oplog_start, oplog_take, BlockId, LogOp, Store};
use redb::StorageBackend;

const PACK: u64 = 32 << 10;
/// Marks a log marker as a metadata backend event; the store's own markers are small numbers.
const META_MARK: u64 = 1 << 40;

fn body(n: usize, seed: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut h = u32::from(seed).wrapping_mul(2_654_435_761).wrapping_add(1);
    for _ in 0..n {
        h = h.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.push(if h >> 29 == 0 {
            b'a'.wrapping_add((h >> 8) as u8)
        } else {
            (h >> 16) as u8
        });
    }
    out
}

#[derive(Debug, Clone)]
enum Ev {
    Write(u64, Vec<u8>),
    SetLen(u64),
    Sync,
}

#[derive(Debug, Default)]
struct Rec {
    data: Vec<u8>,
    log: Vec<Ev>,
}

/// redb's file, in memory, with every write, set_len and sync logged and stamped into the store's
/// op log, so a cut at store op `k` knows which metadata events had happened.
#[derive(Debug, Clone, Default)]
struct Backend(Arc<Mutex<Rec>>);

fn apply(img: &mut Vec<u8>, ev: &Ev) {
    match ev {
        Ev::Write(off, data) => {
            let end = *off as usize + data.len();
            if img.len() < end {
                img.resize(end, 0);
            }
            img[*off as usize..end].copy_from_slice(data);
        }
        Ev::SetLen(n) => img.resize(*n as usize, 0),
        Ev::Sync => {}
    }
}

impl Backend {
    fn from_image(img: Vec<u8>) -> Self {
        Self(Arc::new(Mutex::new(Rec {
            data: img,
            log: Vec::new(),
        })))
    }

    /// The file as it is now, which is durable by construction, and an empty event log.
    fn rebase(&self) -> Vec<u8> {
        let mut r = self.0.lock().unwrap();
        r.log.clear();
        r.data.clone()
    }

    fn events(&self) -> Vec<Ev> {
        self.0.lock().unwrap().log.clone()
    }

    fn push(&self, ev: Ev) {
        let mut r = self.0.lock().unwrap();
        apply(&mut r.data, &ev);
        let i = r.log.len() as u64;
        r.log.push(ev);
        drop(r);
        oplog_marker(META_MARK | i);
    }
}

impl StorageBackend for Backend {
    fn len(&self) -> Result<u64, io::Error> {
        Ok(self.0.lock().unwrap().data.len() as u64)
    }

    fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), io::Error> {
        let r = self.0.lock().unwrap();
        let end = offset as usize + out.len();
        if end > r.data.len() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        out.copy_from_slice(&r.data[offset as usize..end]);
        Ok(())
    }

    fn set_len(&self, len: u64) -> Result<(), io::Error> {
        self.push(Ev::SetLen(len));
        Ok(())
    }

    fn sync_data(&self) -> Result<(), io::Error> {
        self.push(Ev::Sync);
        Ok(())
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<(), io::Error> {
        self.push(Ev::Write(offset, data.to_vec()));
        Ok(())
    }
}

/// The metadata file after a cut at store op `k`: the base plus every event up to the last
/// `sync_data` that had completed. Unsynced redb writes are all lost, which a real disk may do.
fn meta_image(base: &[u8], evs: &[Ev], ops: &[LogOp], k: usize) -> Vec<u8> {
    let done = ops[..k.min(ops.len())]
        .iter()
        .filter_map(|o| match o {
            LogOp::Marker(v) if v & META_MARK != 0 => Some((v & !META_MARK) as usize),
            _ => None,
        })
        .max();
    let mut img = base.to_vec();
    if let Some(d) = done {
        if let Some(s) = evs[..=d].iter().rposition(|e| matches!(e, Ev::Sync)) {
            for e in &evs[..=s] {
                apply(&mut img, e);
            }
        }
    }
    img
}

fn meta_opts(store: &Arc<Store>) -> MetaOptions {
    let store = Arc::clone(store);
    MetaOptions {
        background: false,
        // Applied changes stay in memory until `sync`, so the recorded commit is the collect's own.
        sync_every_ops: u32::MAX,
        sync_interval: std::time::Duration::from_secs(3600),
        // What the mount layer does: the store is durable before the metadata that names it.
        before_sync: Some(Arc::new(move || {
            store.sync().map_err(|e| io::Error::other(e.to_string()))
        })),
        ..MetaOptions::default()
    }
}

/// Store a file's content and set it on a snapshot, returning its chunks.
fn write(store: &Store, snap: &Snapshot, name: &[u8], data: &[u8]) -> Vec<cowfs_meta::ChunkRef> {
    let chunks = store.ingest_bytes(data).expect("ingest");
    let ino = snap
        .batch(|tx| tx.create(ROOT_INO, name, 0o644))
        .expect("create")
        .ino;
    snap.batch(|tx| tx.set_content(ino, &chunks, data.len() as u64))
        .expect("set content");
    chunks
}

fn receipts(want: &mut HashMap<BlockId, Vec<u8>>, chunks: &[cowfs_meta::ChunkRef], data: &[u8]) {
    let mut at = 0usize;
    for c in chunks {
        let n = usize::try_from(c.len).unwrap_or(0);
        want.insert(c.id, data[at..(at + n).min(data.len())].to_vec());
        at += n;
    }
}

/// One recorded collect over a store whose sealed packs mix live and dead records.
struct Run {
    store_base: Image,
    meta_base: Vec<u8>,
    meta_evs: Vec<Ev>,
    ops: Vec<LogOp>,
    /// Old and new contents of every file in the collector's state directory.
    state: Vec<(String, Vec<u8>, Vec<u8>)>,
    want: HashMap<BlockId, Vec<u8>>,
    freed: u64,
    unlinked: u64,
}

fn read_dir_files(dir: &Path) -> HashMap<String, Vec<u8>> {
    fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_file())
                .filter_map(|e| {
                    Some((
                        e.file_name().to_string_lossy().into_owned(),
                        fs::read(e.path()).ok()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Phase A fills packs and collects once, so the collector has written state before the recorded
/// cycle. Phase B adds more dead and referenced records plus a snapshot that is not yet durable and
/// that references only a block the store holds as garbage, then records the collect.
fn record() -> Run {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path().join("store"), small_store_opts(PACK)).unwrap());
    let backend = Backend::default();
    let meta = Arc::new(Meta::open_with_backend(backend.clone(), meta_opts(&store)).unwrap());
    let gc = Gc::open(
        dir.path().join("gcstate"),
        store.clone(),
        meta.clone(),
        eager(),
    )
    .unwrap();
    let mut want = HashMap::new();

    let snap = meta.new_snapshot("s").unwrap();
    // A snapshot no later phase touches: from the next cycle on its blocks come from the collector's
    // persisted mark cache, so the cache decides whether they are protected.
    let frozen = meta.new_snapshot("frozen").unwrap();
    for i in 0..4u8 {
        let data = body(3000, 200 + i);
        let chunks = write(&store, &frozen, format!("frozen{i}").as_bytes(), &data);
        receipts(&mut want, &chunks, &data);
    }
    let mut fill = |seed0: u8, n: u8| {
        for i in seed0..seed0 + n {
            store.put(&body(4000, i)).unwrap();
            if i % 2 == 0 {
                let data = body(3000, i.wrapping_add(100));
                let chunks = write(&store, &snap, format!("keep{i}").as_bytes(), &data);
                receipts(&mut want, &chunks, &data);
            }
        }
    };
    fill(0, 16);
    store.sync().unwrap();
    meta.sync().unwrap();
    let a = gc.collect(Some(&*Roots::new())).unwrap();
    assert!(a.errors.is_empty(), "phase A collect: {:?}", a.errors);
    assert!(
        a.freed_bytes > 0,
        "phase A reclaims, so the state is not empty"
    );

    fill(40, 24);
    // Blocks that are only garbage until a snapshot names them. Each is one chunk the store already
    // holds, so naming it adds no write and the snapshot is the only thing keeping it alive.
    let named = |seed: u8, want: &mut HashMap<BlockId, Vec<u8>>| {
        let data = body(4000, seed);
        let id = BlockId::of(&data);
        assert!(store.contains(id), "block {seed} is garbage in the store");
        let chunks = store.ingest_bytes(&data).unwrap();
        assert!(
            chunks.len() == 1 && chunks[0].id == id,
            "the file is the one stored block"
        );
        receipts(want, &chunks, &data);
        (data, chunks)
    };
    store.sync().unwrap();
    meta.sync().unwrap();
    // Committed by the collect's freeze.
    let (late_data, late_chunks) = named(41, &mut want);
    let late = meta.new_snapshot("late").unwrap();
    let ino = late
        .batch(|tx| tx.create(ROOT_INO, b"late", 0o644))
        .unwrap()
        .ino;
    late.batch(|tx| tx.set_content(ino, &late_chunks, late_data.len() as u64))
        .unwrap();
    // Created after the freeze listing, so only the sweep's own fresh listing can protect it, and
    // committed by that listing's sync.
    let (mid_data, mid_chunks) = named(43, &mut want);
    {
        let meta = Arc::clone(&meta);
        gc.set_between_list_and_walk(Box::new(move || {
            let mid = meta.new_snapshot("mid").unwrap();
            let ino = mid
                .batch(|tx| tx.create(ROOT_INO, b"mid", 0o644))
                .unwrap()
                .ino;
            mid.batch(|tx| tx.set_content(ino, &mid_chunks, mid_data.len() as u64))
                .unwrap();
        }));
    }

    let store_base = read_image(&dir.path().join("store"));
    let meta_base = backend.rebase();
    let state_before = read_dir_files(&dir.path().join("gcstate"));
    oplog_start();
    let r = gc.collect(Some(&*Roots::new())).unwrap();
    // What the mount's background commit does next: the mid snapshot becomes durable, after any unlink
    // the collect made.
    meta.sync().unwrap();
    let ops = oplog_take();
    assert!(r.errors.is_empty(), "recorded collect: {:?}", r.errors);
    let state_after = read_dir_files(&dir.path().join("gcstate"));
    let mut state: Vec<(String, Vec<u8>, Vec<u8>)> = state_after
        .into_iter()
        .map(|(n, post)| {
            let pre = state_before.get(&n).cloned().unwrap_or_default();
            (n, pre, post)
        })
        .collect();
    state.sort();
    Run {
        store_base,
        meta_base,
        meta_evs: backend.events(),
        ops,
        state,
        want,
        freed: r.freed_bytes,
        unlinked: r.packs_unlinked,
    }
}

#[derive(Default)]
struct Tally {
    images: u32,
    pack_gone: u32,
    packs_all_kept: u32,
    late_durable: u32,
    late_missing: u32,
    mid_durable: u32,
    mid_missing: u32,
    failures: Vec<String>,
}

fn brief(op: &LogOp) -> String {
    match op {
        LogOp::Write { file, off, data } => format!("write {file}@{off}+{}", data.len()),
        LogOp::Whole { file, data } => format!("whole {file} {}B", data.len()),
        LogOp::Marker(v) if v & META_MARK != 0 => format!("meta event {}", v & !META_MARK),
        other => format!("{other:?}"),
    }
}

fn dump(ops: &[LogOp]) -> String {
    ops.iter()
        .enumerate()
        .map(|(i, o)| format!("{i}: {}\n", brief(o)))
        .collect()
}

/// Write the collector's state as one cut could leave it: per file the old bytes, the new bytes, a
/// torn prefix of the new bytes, or nothing.
fn write_state(run: &Run, dir: &Path, rng: &mut Rng) {
    fs::create_dir_all(dir).unwrap();
    for (name, pre, post) in &run.state {
        let bytes = match rng.below(4) {
            0 => pre.clone(),
            1 => post.clone(),
            2 => post[..rng.below(post.len() as u64 + 1) as usize].to_vec(),
            _ => continue,
        };
        fs::write(dir.join(name), bytes).unwrap();
    }
}

fn verify(run: &Run, dir: &Path, meta_img: Vec<u8>, t: &mut Tally) -> Result<(), String> {
    let store = Arc::new(
        Store::open(dir.join("store"), small_store_opts(PACK))
            .map_err(|e| format!("store open: {e:?}"))?,
    );
    let rep = store.recovery();
    if rep.has_corruption() {
        return Err(format!("store reported a loss: {rep:?}"));
    }
    let acked = fs::read(dir.join("store").join("ACKED")).unwrap_or_default();
    for e in acked.as_chunks::<68>().0 {
        let word = |a: usize, b: usize| u64::from_le_bytes(e[a..b].try_into().unwrap());
        let pack = u32::from_le_bytes(e[0..4].try_into().unwrap());
        let whole = e[8] == 1
            && u32::from_le_bytes(e[4..8].try_into().unwrap()) == 0
            && word(16, 24) == 0
            && word(24, 32) == u64::MAX;
        let file = dir.join("store/packs").join(format!("pack-{pack:08}.cpk"));
        if whole && file.exists() {
            return Err(format!("pack {pack} is on disk and accepted as a whole"));
        }
    }
    let meta = Arc::new(
        Meta::open_with_backend(Backend::from_image(meta_img), meta_opts(&store))
            .map_err(|e| format!("meta open: {e:?}"))?,
    );
    meta.check().map_err(|e| format!("meta check: {e:?}"))?;
    let live_ok = |at: &str| -> Result<(), String> {
        let mut marker = Marker::new();
        for info in meta.durable_snapshots().map_err(|e| format!("{e:?}"))? {
            let snap = meta.snapshot_by_id(info.id).map_err(|e| format!("{e:?}"))?;
            for b in snap
                .live_blocks(&mut marker)
                .map_err(|e| format!("{e:?}"))?
            {
                let b = b.map_err(|e| format!("{e:?}"))?;
                if b == HOLE {
                    continue;
                }
                let want = run.want.get(&b).ok_or_else(|| {
                    format!("{at}: snapshot {:?} names an unknown block", info.name)
                })?;
                match store.get(b) {
                    Ok(got) if &got == want => {}
                    other => {
                        return Err(format!(
                            "{at}: a block of snapshot {:?} is lost or wrong: {:?}",
                            info.name,
                            other.err()
                        ))
                    }
                }
            }
        }
        Ok(())
    };
    live_ok("reopened")?;
    if !store.fsck().map_err(|e| format!("fsck: {e:?}"))?.is_clean() {
        return Err("fsck dirty".into());
    }
    // `new_snapshot` is durable on return, so the late snapshot always exists; what the collect's
    // freeze commits is its content.
    let late_has_file = meta
        .durable_snapshots()
        .map_err(|e| format!("{e:?}"))?
        .iter()
        .filter(|i| i.name == "late")
        .any(|i| {
            meta.snapshot_by_id(i.id)
                .is_ok_and(|s| s.lookup(ROOT_INO, b"late").is_ok())
        });
    let mid_has_file = meta
        .durable_snapshots()
        .map_err(|e| format!("{e:?}"))?
        .iter()
        .filter(|i| i.name == "mid")
        .any(|i| {
            meta.snapshot_by_id(i.id)
                .is_ok_and(|s| s.lookup(ROOT_INO, b"mid").is_ok())
        });
    if mid_has_file {
        t.mid_durable += 1;
    } else {
        t.mid_missing += 1;
    }
    if late_has_file {
        t.late_durable += 1;
    } else {
        t.late_missing += 1;
    }
    let gc = Gc::open(dir.join("gcstate"), store.clone(), meta.clone(), eager())
        .map_err(|e| format!("gc open: {e:?}"))?;
    let r = gc
        .collect(Some(&*Roots::new()))
        .map_err(|e| format!("second collect: {e:?}"))?;
    if !r.errors.is_empty() {
        return Err(format!("second collect errors: {:?}", r.errors));
    }
    live_ok("after a second collect")?;
    if !store.fsck().map_err(|e| format!("fsck: {e:?}"))?.is_clean() {
        return Err("fsck dirty after a second collect".into());
    }
    Ok(())
}

fn sweep(run: &Run, seeds: u64, t: &mut Tally) {
    for k in 0..=run.ops.len() {
        for seed in 0..seeds {
            let mut rng = Rng(seed ^ ((k as u64) << 20) ^ 0x6C0);
            let img = crash_image(&run.store_base, &run.ops, k, &mut rng, seed % 4);
            let tag = format!(
                "k={k}/{} seed={seed} op={}",
                run.ops.len(),
                run.ops.get(k).map_or_else(|| "(all done)".into(), brief)
            );
            let dir = tempfile::tempdir().unwrap();
            write_image(&img, &dir.path().join("store"));
            write_state(run, &dir.path().join("gcstate"), &mut rng);
            t.images += 1;
            if run
                .store_base
                .keys()
                .all(|n| img.contains_key(n) || !n.starts_with("pack-"))
            {
                t.packs_all_kept += 1;
            } else {
                t.pack_gone += 1;
            }
            let meta_img = meta_image(&run.meta_base, &run.meta_evs, &run.ops, k);
            if let Err(e) = verify(run, dir.path(), meta_img, t) {
                t.failures.push(format!("{tag}: {e}"));
            }
        }
    }
}

fn seeds() -> u64 {
    std::env::var("C173_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8)
}

/// Replaying the log over the base with nothing lost gives the real disk, so the sweep starts from
/// a log that misses no store write and a metadata file that is the real one.
#[test]
fn the_log_replays_to_the_real_disk() {
    let run = record();
    assert!(
        run.freed > 0 && run.unlinked >= 2,
        "the recorded collect reclaims"
    );
    let mut rng = Rng(1);
    let end = crash_image(&run.store_base, &run.ops, run.ops.len(), &mut rng, 2);
    // Every pack the real collect unlinked, the full-log image shows as gone or still waiting on a
    // directory fsync that did run; what must hold is that no pack appears that the base lacked.
    for n in end.keys().filter(|n| n.starts_with("pack-")) {
        assert!(
            run.store_base.contains_key(n)
                || run
                    .ops
                    .iter()
                    .any(|o| matches!(o, LogOp::Create { file } if file == n)),
            "image holds {n}, which neither the base nor the log has"
        );
    }
    // The meta file after the last completed event and sync equals the backend's own bytes.
    let full = meta_image(&run.meta_base, &run.meta_evs, &run.ops, run.ops.len());
    let mut real = run.meta_base.clone();
    let last = run.meta_evs.iter().rposition(|e| matches!(e, Ev::Sync));
    for e in &run.meta_evs[..=last.expect("the freeze committed the late snapshot")] {
        apply(&mut real, e);
    }
    assert_eq!(full, real, "the stamped markers lose a metadata event");
}

/// Power loss at every op of a real, reclaiming collect, with the metadata commit inside it.
#[test]
fn power_loss_at_every_op_of_a_reclaiming_collect_keeps_live_blocks() {
    let run = record();
    assert!(!run.state.is_empty(), "the collector wrote state");
    let mut t = Tally::default();
    sweep(&run, seeds(), &mut t);
    println!(
        "collect: {} images over {} ops, a pack missing in {}, none missing in {}, late file durable in {}, missing in {}, mid file durable in {}, missing in {}, {} failed; control freed {} bytes in {} packs",
        t.images,
        run.ops.len(),
        t.pack_gone,
        t.packs_all_kept,
        t.late_durable,
        t.late_missing,
        t.mid_durable,
        t.mid_missing,
        t.failures.len(),
        run.freed,
        run.unlinked
    );
    assert!(
        t.failures.is_empty(),
        "{} of {} crash images failed; first: {:#?}\nops:\n{}",
        t.failures.len(),
        t.images,
        &t.failures[..t.failures.len().min(5)],
        dump(&run.ops)
    );
    // Fail closed: a model that never loses or keeps a pack, or never commits the late file in
    // time, proves nothing about those edges.
    assert!(t.images > 0, "no images were built");
    assert!(t.pack_gone > 0, "no image lost a source pack");
    assert!(t.packs_all_kept > 0, "no image kept every source pack");
    assert!(t.late_durable > 0, "no image had the late file durable");
    assert!(t.late_missing > 0, "no image lacked the late file");
    assert!(t.mid_durable > 0, "no image had the mid file durable");
    assert!(t.mid_missing > 0, "no image lacked the mid file");
}

/// The collector's state directory tolerates the states a power cut leaves: no mix of old, new, torn
/// and missing files may lose a block or fail a collect. This is the evidence that `gcstate` needs
/// no routing through the store's fsync model. Same-length corrupted bytes are covered by
/// `mark_bin_bit_rot_is_rebuilt`.
#[test]
fn gc_state_is_advisory() {
    let run = record();
    let final_store = {
        let mut rng = Rng(7);
        crash_image(&run.store_base, &run.ops, run.ops.len(), &mut rng, 2)
    };
    let mut t = Tally::default();
    for seed in 0..64u64 {
        let mut rng = Rng(seed ^ 0x57A7E);
        let dir = tempfile::tempdir().unwrap();
        write_image(&final_store, &dir.path().join("store"));
        write_state(&run, &dir.path().join("gcstate"), &mut rng);
        t.images += 1;
        let meta_img = meta_image(&run.meta_base, &run.meta_evs, &run.ops, run.ops.len());
        if let Err(e) = verify(&run, dir.path(), meta_img, &mut t) {
            t.failures.push(format!("seed={seed}: {e}"));
        }
    }
    assert!(
        t.failures.is_empty(),
        "{:#?}",
        &t.failures[..t.failures.len().min(5)]
    );
}

/// `mark.bin` carries a trailing hash, so a zeroed 64-byte span (bit rot, not a power cut) is
/// detected and the cache rebuilt by a full walk. Before issue 288 it parsed as a valid but
/// different set and a later cycle freed live blocks.
#[test]
fn mark_bin_bit_rot_is_rebuilt() {
    let run = record();
    let final_store = {
        let mut rng = Rng(7);
        crash_image(&run.store_base, &run.ops, run.ops.len(), &mut rng, 2)
    };
    let mut failures = Vec::new();
    for seed in 0..64u64 {
        let mut rng = Rng(seed ^ 0xB17);
        let dir = tempfile::tempdir().unwrap();
        write_image(&final_store, &dir.path().join("store"));
        let state = dir.path().join("gcstate");
        fs::create_dir_all(&state).unwrap();
        for (name, _, post) in &run.state {
            let mut bytes = post.clone();
            if name == "mark.bin" && bytes.len() > 80 {
                let at = 16 + rng.below((bytes.len() - 80) as u64) as usize;
                bytes[at..at + 64].fill(0);
            }
            fs::write(state.join(name), bytes).unwrap();
        }
        let meta_img = meta_image(&run.meta_base, &run.meta_evs, &run.ops, run.ops.len());
        if let Err(e) = verify(&run, dir.path(), meta_img, &mut Tally::default()) {
            failures.push(format!("seed={seed}: {e}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of 64 lost: {:#?}",
        failures.len(),
        &failures[..failures.len().min(3)]
    );
}
