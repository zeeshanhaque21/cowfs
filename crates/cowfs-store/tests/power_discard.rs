//! Issue 173, slice 3: power loss across a real compaction and discard.
//!
//! The op log (`oplog_start`) records every write, fsync, create, unlink and directory fsync of one
//! recorded run. For every op index and several seeds, the crash model (`crashmodel::crash_image`)
//! rebuilds the disk a power cut at that point could leave: unsynced writes survive in any subset,
//! an unlink survives only if the packs directory was fsynced after it. Each image is reopened with
//! the shipped open path and must keep every live block, report no loss and pass `fsck`.
//!
//! This is what catches an unlink ahead of the durable watermark raise (mutation 2 of
//! `docs/crash-injection-173.md`), which no process-crash test can see, because a process crash
//! keeps the page cache and so keeps every unsynced byte.

use std::fs;
use std::path::Path;

use cowfs_store::crashmodel::{crash_image, read_image, write_image, Image, Rng};
use cowfs_store::{oplog_start, oplog_take, BlockId, LogOp, Options, Store};

fn opts() -> Options {
    Options {
        max_pack_size: 32 << 10,
        checkpoint_on_drop: false,
        ..Options::default()
    }
}

/// Bytes that do not compress, so a pack fills at the size it claims.
fn noisy(n: usize, seed: u32) -> Vec<u8> {
    let mut h = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..n)
        .map(|_| {
            h = h.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (h >> 24) as u8
        })
        .collect()
}

/// A sealed pack holding 3 live and 4 dead records, plus later packs, all durable.
struct Run {
    base: Image,
    ops: Vec<LogOp>,
    live: Vec<(BlockId, Vec<u8>)>,
    old: u32,
}

/// Seal a pack of 3 live and 4 dead records and make it durable, then record the work. With
/// `copy_first` the copy is made durable before the recording starts, so only the discard is cut.
fn record(copy_first: bool) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), opts()).unwrap();
    let live: Vec<(BlockId, Vec<u8>)> = (0..3u32)
        .map(|i| {
            let d = noisy(4096, i);
            (s.put(&d).unwrap(), d)
        })
        .collect();
    for i in 20..24u32 {
        s.put(&noisy(4096, i)).unwrap();
    }
    for i in 100..140u32 {
        s.put(&noisy(4096, i)).unwrap();
    }
    s.sync().unwrap();
    s.checkpoint().unwrap();
    let old = s
        .packs()
        .unwrap()
        .into_iter()
        .find(|p| !p.active)
        .expect("a sealed pack")
        .id;
    let ids: Vec<BlockId> = live.iter().map(|(b, _)| *b).collect();
    let is_live = |b: BlockId| ids.contains(&b);
    let compact = || {
        let plan = s.plan_pack(old, &is_live, &mut Vec::new()).unwrap();
        assert!(plan.live_bytes > 0, "the pack holds live records");
        let mut c = s.begin_compaction(&plan, &is_live).unwrap();
        while !s.copy_batch(&mut c, 0).unwrap() {}
        s.finish_compaction(&c).unwrap()
    };
    // The base is the durable disk the recorded work starts from.
    let early = copy_first.then(|| {
        let rw = compact();
        s.sync().unwrap();
        rw
    });
    let base = read_image(dir.path());
    oplog_start();
    let rw = early.unwrap_or_else(compact);
    s.discard_pack(old, &rw.condemned).unwrap();
    let ops = oplog_take();
    Run {
        base,
        ops,
        live,
        old,
    }
}

/// The `ACKED` file says `pack` was accepted as a whole (nonce 0, offset 0, len max).
fn covers_whole_pack(acked: &[u8], pack: u32) -> bool {
    acked.as_chunks::<68>().0.iter().any(|e| {
        let word = |a: usize, b: usize| u64::from_le_bytes(e[a..b].try_into().unwrap());
        u32::from_le_bytes(e[0..4].try_into().unwrap()) == pack
            && e[8] == 1
            && u32::from_le_bytes(e[4..8].try_into().unwrap()) == 0
            && word(16, 24) == 0
            && word(24, 32) == u64::MAX
    })
}

#[derive(Default)]
struct Tally {
    images: u32,
    source_gone: u32,
    source_kept: u32,
    failures: Vec<String>,
}

fn sweep(run: &Run, seeds: u64, t: &mut Tally) {
    for k in 0..run.ops.len() {
        for seed in 0..seeds {
            let mut rng = Rng(seed ^ ((k as u64) << 20) ^ 0x51ED);
            let img = crash_image(&run.base, &run.ops, k, &mut rng, seed % 4);
            let tag = format!("k={k}/{} seed={seed}", run.ops.len());
            let dir = tempfile::tempdir().unwrap();
            write_image(&img, dir.path());
            t.images += 1;
            let source = format!("pack-{:08}.cpk", run.old);
            if img.contains_key(&source) {
                t.source_kept += 1;
            } else {
                t.source_gone += 1;
            }
            if let Err(e) = verify(&img, dir.path(), run, &source) {
                t.failures.push(format!("{tag}: {e}"));
            }
        }
    }
}

fn verify(img: &Image, dir: &Path, run: &Run, source: &str) -> Result<(), String> {
    let s = Store::open(dir, opts()).map_err(|e| format!("open failed: {e:?}"))?;
    let rep = s.recovery();
    if rep.has_corruption() {
        return Err(format!("reported a loss: {rep:?}"));
    }
    for (i, (id, d)) in run.live.iter().enumerate() {
        match s.get(*id) {
            Ok(got) if &got == d => {}
            other => return Err(format!("live block {i} lost or wrong: {:?}", other.err())),
        }
    }
    if !s.fsck().map_err(|e| format!("fsck: {e:?}"))?.is_clean() {
        return Err("fsck dirty".into());
    }
    // An acceptance of the whole source pack is only true once the file is really gone.
    if img.contains_key(source) {
        let acked = fs::read(dir.join("ACKED")).unwrap_or_default();
        if covers_whole_pack(&acked, run.old) {
            return Err("source pack is on disk and accepted as a whole".into());
        }
    }
    Ok(())
}

fn seeds() -> u64 {
    std::env::var("C173_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(16)
}

fn report(what: &str, t: &Tally) {
    println!(
        "{what}: {} images, source pack gone in {}, kept in {}, {} failed",
        t.images,
        t.source_gone,
        t.source_kept,
        t.failures.len()
    );
    assert!(
        t.failures.is_empty(),
        "{} of {} crash images failed; first: {:#?}",
        t.failures.len(),
        t.images,
        &t.failures[..t.failures.len().min(5)]
    );
}

/// The unlink is in the model: logged once, and followed by a fsync of the packs directory.
#[test]
fn the_model_sees_the_unlink_and_its_directory_fsync() {
    let run = record(false);
    let unlink = run
        .ops
        .iter()
        .position(|o| matches!(o, LogOp::Unlink { file } if file.contains(".cpk")))
        .expect("the discard's unlink is in the log");
    assert!(
        run.ops[unlink..]
            .iter()
            .any(|o| matches!(o, LogOp::DirSync { dir } if dir == "packs")),
        "no packs directory fsync after the unlink: {:?}",
        run.ops
    );
}

/// Power loss at every op of a real compaction followed by a discard.
#[test]
fn power_loss_at_every_op_of_compaction_and_discard_keeps_live_blocks() {
    let run = record(false);
    let mut t = Tally::default();
    sweep(&run, seeds(), &mut t);
    report("compaction + discard", &t);
    assert!(t.images > 0, "no images were built");
    // Fail closed: a model that never loses or keeps the unlink proves nothing about it.
    assert!(t.source_gone > 0, "no image lost the source pack");
    assert!(t.source_kept > 0, "no image kept the source pack");
}

/// Power loss at every op of a discard alone, with the copy already durable.
#[test]
fn power_loss_at_every_op_of_a_discard_keeps_live_blocks() {
    let run = record(true);
    let mut t = Tally::default();
    sweep(&run, seeds(), &mut t);
    report("discard", &t);
    assert!(t.images > 0, "no images were built");
}
