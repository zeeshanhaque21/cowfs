//! Issue 173, slice 3: power loss across a real compaction and discard.
//!
//! The op log (`oplog_start`) records every write, fsync, create, unlink and directory fsync of one
//! recorded run. For every op index and several seeds, the crash model (`crashmodel::crash_image`)
//! rebuilds the disk a power cut at that point could leave: unsynced writes survive in any subset,
//! an unlink survives only if the packs directory was fsynced after it. Each image is reopened with
//! the shipped open path and must keep every live block, report no loss and pass `fsck`.
//!
//! The target is an unlink ahead of the durable watermark raise (mutation 2 of
//! `docs/crash-injection-173.md`), which a process-crash test cannot see, because a process crash
//! keeps the page cache and so keeps every unsynced byte. `the_log_replays_to_the_real_disk` guards
//! the premise that the log misses no write.

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
    /// The real disk after the recorded work, to check the log against.
    after: Image,
    ops: Vec<LogOp>,
    live: Vec<(BlockId, Vec<u8>)>,
    /// Blocks put (not synced) before the recording starts, so only the discard's own leading sync
    /// can make them durable.
    pending: Vec<(BlockId, Vec<u8>)>,
    old: u32,
}

/// Seal a pack of 3 live and 4 dead records and make it durable, then record the work. With
/// `copy_first` the copy is made durable before the recording starts, so only the discard is cut.
/// With `pending`, three more blocks are put after the last sync and before the recording.
fn record(copy_first: bool, pending: bool) -> Run {
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
    let pending: Vec<(BlockId, Vec<u8>)> = (0..if pending { 3u32 } else { 0 })
        .map(|i| {
            let d = noisy(4096, 500 + i);
            (s.put(&d).unwrap(), d)
        })
        .collect();
    let rw = early.unwrap_or_else(compact);
    s.discard_pack(old, &rw.condemned).unwrap();
    let ops = oplog_take();
    let after = read_image(dir.path());
    Run {
        base,
        after,
        ops,
        live,
        pending,
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

/// Apply every op of the log with nothing lost: the disk the process left if the cut came last.
fn replay_lossless(base: &Image, ops: &[LogOp]) -> Image {
    let mut img = base.clone();
    for op in ops {
        match op {
            LogOp::Create { file } if !file.ends_with(".tmp") => {
                img.entry(file.clone()).or_default();
            }
            LogOp::Write { file, off, data } => {
                let b = img.entry(file.clone()).or_default();
                let end = *off as usize + data.len();
                if b.len() < end {
                    b.resize(end, 0);
                }
                b[*off as usize..end].copy_from_slice(data);
            }
            LogOp::SetLen { file, len } => {
                img.entry(file.clone())
                    .or_default()
                    .resize(*len as usize, 0);
            }
            LogOp::Whole { file, data } => {
                img.insert(file.clone(), data.clone());
            }
            LogOp::Unlink { file } => {
                img.remove(file);
            }
            LogOp::Create { .. }
            | LogOp::Sync { .. }
            | LogOp::DirSync { .. }
            | LogOp::Marker(_) => {}
        }
    }
    img
}

/// An op without its payload, for failure messages.
fn brief(op: &LogOp) -> String {
    match op {
        LogOp::Write { file, off, data } => format!("write {file}@{off}+{}", data.len()),
        LogOp::Whole { file, data } => format!("whole {file} {}B", data.len()),
        other => format!("{other:?}"),
    }
}

/// Every op of the run, one per line, for reading a failing image by hand.
fn dump(ops: &[LogOp]) -> String {
    ops.iter()
        .enumerate()
        .map(|(i, o)| format!("{i}: {}\n", brief(o)))
        .collect()
}

fn sweep(run: &Run, seeds: u64, t: &mut Tally) {
    // `k == len` is the cut after the discard returned: nothing in flight, the end state.
    for k in 0..=run.ops.len() {
        for seed in 0..seeds {
            let mut rng = Rng(seed ^ ((k as u64) << 20) ^ 0x51ED);
            let img = crash_image(&run.base, &run.ops, k, &mut rng, seed % 4);
            let tag = format!(
                "k={k}/{} seed={seed} op={}",
                run.ops.len(),
                run.ops
                    .get(k)
                    .map_or_else(|| "(none, all done)".into(), brief)
            );
            let dir = tempfile::tempdir().unwrap();
            write_image(&img, dir.path());
            t.images += 1;
            let source = format!("pack-{:08}.cpk", run.old);
            if img.contains_key(&source) {
                t.source_kept += 1;
            } else {
                t.source_gone += 1;
            }
            if let Err(e) = verify(&img, dir.path(), run, &source, k) {
                t.failures.push(format!("{tag}: {e}"));
            }
        }
    }
}

fn verify(img: &Image, dir: &Path, run: &Run, source: &str, k: usize) -> Result<(), String> {
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
    // Once the discard has returned, its leading `sync` has made every earlier put durable. In a cut
    // before that, such a put may be lost, so it is only required at the end.
    if k == run.ops.len() {
        for (i, (id, d)) in run.pending.iter().enumerate() {
            match s.get(*id) {
                Ok(got) if &got == d => {}
                other => {
                    return Err(format!(
                        "put {i} lost after discard returned: {:?}",
                        other.err()
                    ))
                }
            }
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

fn report(what: &str, t: &Tally, ops: &[LogOp]) {
    println!(
        "{what}: {} images, source pack gone in {}, kept in {}, {} failed",
        t.images,
        t.source_gone,
        t.source_kept,
        t.failures.len()
    );
    assert!(
        t.failures.is_empty(),
        "{} of {} crash images failed; first: {:#?}\nops:\n{}",
        t.failures.len(),
        t.images,
        &t.failures[..t.failures.len().min(5)],
        dump(ops)
    );
}

/// The premise of the whole sweep: replaying the log with nothing lost gives the real disk. A write
/// that bypasses `Io` (the compaction copy once did) shows up here as a missing or short file, not
/// as a confusing live-block loss in a crash image.
#[test]
fn the_log_replays_to_the_real_disk() {
    for copy_first in [false, true] {
        let run = record(copy_first, false);
        let replayed = replay_lossless(&run.base, &run.ops);
        let diff: Vec<String> = run
            .after
            .iter()
            .filter(|(n, b)| replayed.get(*n) != Some(*b))
            .map(|(n, b)| {
                format!(
                    "{n}: disk {}B, replay {:?}B",
                    b.len(),
                    replayed.get(n).map(Vec::len)
                )
            })
            .chain(
                replayed
                    .keys()
                    .filter(|n| !run.after.contains_key(*n))
                    .map(|n| format!("{n}: only in replay")),
            )
            .collect();
        assert!(
            diff.is_empty(),
            "copy_first={copy_first}: log and disk differ: {diff:?}"
        );
    }
}

/// The unlink is in the model: logged once, and followed by a fsync of the packs directory.
#[test]
fn the_model_sees_the_unlink_and_its_directory_fsync() {
    let run = record(false, false);
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
    let run = record(false, false);
    let mut t = Tally::default();
    sweep(&run, seeds(), &mut t);
    report("compaction + discard", &t, &run.ops);
    assert!(t.images > 0, "no images were built");
    // Fail closed: a model that never loses or keeps the unlink proves nothing about it.
    assert!(t.source_gone > 0, "no image lost the source pack");
    assert!(t.source_kept > 0, "no image kept the source pack");
}

/// Power loss at every op of a discard alone, with the copy already durable.
#[test]
fn power_loss_at_every_op_of_a_discard_keeps_live_blocks() {
    let run = record(true, false);
    let mut t = Tally::default();
    sweep(&run, seeds(), &mut t);
    report("discard", &t, &run.ops);
    assert!(t.images > 0, "no images were built");
}

/// The discard's leading `sync` is what makes puts made before it durable: with it skipped (mutant
/// E7) the end state loses them, and no cut inside the discard could show that.
#[test]
fn discard_makes_earlier_puts_durable() {
    for copy_first in [false, true] {
        let run = record(copy_first, true);
        assert_eq!(run.pending.len(), 3);
        let mut t = Tally::default();
        sweep(&run, seeds(), &mut t);
        report(
            &format!("discard with pending puts, copy_first={copy_first}"),
            &t,
            &run.ops,
        );
    }
}

/// A store that lost a whole sealed pack, reopened, with the loss about to be accepted.
struct AckRun {
    base: Image,
    ops: Vec<LogOp>,
    /// Blocks that stayed readable after the loss.
    kept: Vec<(BlockId, Vec<u8>)>,
    lost: u32,
}

/// Fill several packs, checkpoint (so `index.cix` exists and names every pack), delete one sealed
/// pack file, reopen and record `acknowledge_corruption`: the recovery-time path that drops the
/// stale `index.cix`, which a discard sweep never reaches.
fn record_ack() -> AckRun {
    let dir = tempfile::tempdir().unwrap();
    let mut all = Vec::new();
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        for i in 0..40u32 {
            let d = noisy(4096, 900 + i);
            all.push((s.put(&d).unwrap(), d));
        }
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    assert!(dir.path().join("index.cix").exists(), "a checkpoint exists");
    let lost = {
        let s = Store::open(dir.path(), opts()).unwrap();
        s.packs()
            .unwrap()
            .into_iter()
            .find(|p| !p.active)
            .expect("a sealed pack")
            .id
    };
    fs::remove_file(dir.path().join("packs").join(format!("pack-{lost:08}.cpk"))).unwrap();
    let s = Store::open(dir.path(), opts()).unwrap();
    assert!(
        s.recovery().missing_synced.contains(&lost),
        "the pack loss is reported: {:?}",
        s.recovery()
    );
    let kept: Vec<(BlockId, Vec<u8>)> = all
        .into_iter()
        .filter(|(id, d)| s.get(*id).is_ok_and(|g| &g == d))
        .collect();
    assert!(
        !kept.is_empty() && kept.len() < 40,
        "some blocks are lost, some kept"
    );
    assert!(
        dir.path().join("index.cix").exists(),
        "the stale checkpoint is still there"
    );
    s.sync().unwrap();
    let base = read_image(dir.path());
    oplog_start();
    s.acknowledge_corruption().unwrap();
    let ops = oplog_take();
    AckRun {
        base,
        ops,
        kept,
        lost,
    }
}

/// Power loss at every op of `acknowledge_corruption`, including its `index.cix` removal and the
/// directory fsync after it. Whatever survives must open, keep every block that was readable, pass
/// `fsck`, and once the acceptance is on disk, report no loss again.
#[test]
fn power_loss_at_every_op_of_acknowledge_corruption_keeps_live_blocks() {
    let run = record_ack();
    assert!(
        run.ops
            .iter()
            .any(|o| matches!(o, LogOp::Unlink { file } if file == "index.cix")),
        "the index.cix removal is in the log"
    );
    let acked = run
        .ops
        .iter()
        .position(|o| matches!(o, LogOp::Whole { file, .. } if file == "ACKED"))
        .expect("the acceptance is in the log");
    let (mut images, mut kept_cix, mut dropped_cix) = (0, 0, 0);
    let mut failures = Vec::new();
    for k in 0..=run.ops.len() {
        for seed in 0..seeds() {
            let mut rng = Rng(seed ^ ((k as u64) << 20) ^ 0xACC);
            let img = crash_image(&run.base, &run.ops, k, &mut rng, seed % 4);
            let dir = tempfile::tempdir().unwrap();
            write_image(&img, dir.path());
            images += 1;
            if img.contains_key("index.cix") {
                kept_cix += 1;
            } else {
                dropped_cix += 1;
            }
            let tag = format!("k={k}/{} seed={seed}", run.ops.len());
            let r = (|| -> Result<(), String> {
                let s = Store::open(dir.path(), opts()).map_err(|e| format!("open: {e:?}"))?;
                for (i, (id, d)) in run.kept.iter().enumerate() {
                    if !s.get(*id).is_ok_and(|g| &g == d) {
                        return Err(format!("kept block {i} lost"));
                    }
                }
                if !s.fsck().map_err(|e| format!("fsck: {e:?}"))?.is_clean() {
                    return Err("fsck dirty".into());
                }
                // Once the acceptance is on disk, a stale `index.cix` that survives the cut must not
                // bring the accepted loss back: the removal's own durability is what is at stake.
                if k > acked && s.recovery().missing_synced.contains(&run.lost) {
                    return Err("accepted loss reported again".into());
                }
                Ok(())
            })();
            if let Err(e) = r {
                failures.push(format!("{tag}: {e}"));
            }
        }
    }
    println!(
        "acknowledge_corruption: {images} images, index.cix kept in {kept_cix}, dropped in {dropped_cix}, {} failed",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "{} of {images} crash images failed; first: {:#?}\nops:\n{}",
        failures.len(),
        &failures[..failures.len().min(5)],
        dump(&run.ops)
    );
    // Fail closed: the sweep must see the removal both land and not land.
    assert!(
        kept_cix > 0 && dropped_cix > 0,
        "the removal is never in doubt"
    );
}

/// The stale-checkpoint removal is followed by a fsync of the store directory. The sweep above
/// cannot see a missing one (mutant E8): `open` re-validates a resurrected checkpoint against the
/// packs and ignores one that names a missing pack, so both outcomes are safe and this is a static
/// ordering check, like `the_model_sees_the_unlink_and_its_directory_fsync`.
#[test]
fn dropping_the_stale_checkpoint_is_followed_by_a_directory_fsync() {
    let run = record_ack();
    let unlink = run
        .ops
        .iter()
        .position(|o| matches!(o, LogOp::Unlink { file } if file == "index.cix"))
        .expect("the index.cix removal is in the log");
    assert!(
        matches!(run.ops.get(unlink + 1), Some(LogOp::DirSync { dir }) if dir != "packs"),
        "no store directory fsync right after the index.cix removal: {}",
        dump(&run.ops)
    );
}
