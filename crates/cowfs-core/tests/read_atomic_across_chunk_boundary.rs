//! A 4 KiB read that straddles a chunk boundary is atomic against a concurrent write of that block
//! (issue #45, the PR #249 critic follow-up).
//!
//! `cowfs-fuse/tests/coherence.rs::core_view_keeps_aligned_4k_blocks_atomic_while_flushing` already
//! pins the 4 KiB atomicity of a `Core` read, but its 64 KiB file of uniform blocks sits inside one
//! chunk, so a read implemented as one lookup per chunk would pass it. The `Core` read path takes
//! one consistent view (chunk list plus dirty overlay) and then copies chunk by chunk. A read split
//! per chunk, each half taking its own view, is exactly the regression a straddling block exposes.
//! This file pins that case with the real chunker, and does not repeat the uniform-block harness.
//!
//! How the boundary is guaranteed, not hoped for:
//! - The file is a high entropy image, so `cowfs_store::chunks` (the chunker `Core` flushes with)
//!   cuts it at several interior offsets. The test takes the first cut that is not 4 KiB aligned
//!   and sits at least 512 bytes inside its block: the block that straddles it is the unit.
//! - Every version the writer stores is the whole block, tagged at both ends with its version so a
//!   mix of two versions can never equal a third. The 256 bytes around the cut are identical in
//!   every version and each candidate version is accepted only if the chunker still cuts the whole
//!   image at the same offsets. A rewrite re-chunks the neighbouring chunks, so this is what keeps
//!   the boundary in place for the whole run, and the test asserts it instead of assuming it.
//!
//! Two arms: `file_flush_bytes` of one block with the background flusher (every write stores and
//! republishes the chunk list), and the default threshold (the dirty overlay alone).
//!
//! Mutation check (not committed): in `op_read`, split the range into two `read_range` calls that
//! each take their own `node.st.rd()` view. Both arms then fail with a torn read.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::pattern;
use cowfs_core::{Core, Options};
use cowfs_vfs::{Vfs, ROOT_INO};

const BLOCK: usize = 4096;
const IMAGE: usize = 1 << 20;
const VERSIONS: usize = 8;
const READERS: usize = 3;
/// The run stops at this, so a slow machine does fewer iterations instead of hanging.
const CAP: Duration = Duration::from_secs(8);
const WRITES: usize = 4000;

fn cuts(image: &[u8]) -> Vec<usize> {
    let mut at = 0;
    cowfs_store::chunks(image)
        .map(|c| {
            at += c.len();
            at
        })
        .collect()
}

/// A file image, the straddling block's offset, the chunk boundary inside it and the block
/// versions the writer cycles through (version 0 is what the image starts with).
struct Layout {
    image: Vec<u8>,
    block: usize,
    boundary: usize,
    versions: Vec<Vec<u8>>,
}

fn layout() -> Layout {
    let mut image = pattern(IMAGE, 45);
    let all = cuts(&image);
    // An interior cut, not 4 KiB aligned, with room for the shared window on both sides.
    let boundary = all[..all.len() - 1]
        .iter()
        .copied()
        .find(|b| (512..BLOCK - 512).contains(&(b % BLOCK)))
        .expect("the image has no interior chunk boundary inside a block");
    let block = boundary - boundary % BLOCK;
    let rel = boundary - block;
    assert!(rel > 256 && rel + 256 < BLOCK);
    let shared = block + rel - 256..block + rel + 256;

    let mut versions: Vec<Vec<u8>> = Vec::new();
    let mut seed = 1000;
    while versions.len() < VERSIONS {
        seed += 1;
        let mut v = pattern(BLOCK, seed);
        v[..8].copy_from_slice(&(versions.len() as u64).to_le_bytes());
        v[BLOCK - 8..].copy_from_slice(&(versions.len() as u64).to_le_bytes());
        v[rel - 256..rel + 256].copy_from_slice(&image[shared.clone()]);
        let mut probe = image.clone();
        probe[block..block + BLOCK].copy_from_slice(&v);
        // Accept a version only if the chunker cuts the whole image exactly where it did.
        if cuts(&probe) == all {
            if versions.is_empty() {
                image = probe;
            }
            versions.push(v);
        }
        assert!(
            seed < 5000,
            "could not build {VERSIONS} boundary-preserving versions"
        );
    }
    Layout {
        image,
        block,
        boundary,
        versions,
    }
}

fn run(opts: Options, arm: &str) {
    let l = layout();
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), opts).unwrap();
    core.create_snapshot("main").unwrap();
    let fs: Arc<dyn Vfs> = Arc::new(core.snapshot_view("main").unwrap());
    let ino = fs.create(ROOT_INO, b"f", 0o644).unwrap().ino;
    common::write_all(&*fs, ino, 0, &l.image);

    let off = l.block as u64;
    let stop = AtomicBool::new(false);
    let (reads, torn, seen) = (
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    );
    let first_tear = std::sync::Mutex::new(None::<String>);
    let start = Instant::now();
    let mut last = 0;
    std::thread::scope(|s| {
        for _ in 0..READERS {
            s.spawn(|| {
                let mut kinds = [false; VERSIONS];
                while !stop.load(Relaxed) && start.elapsed() < CAP {
                    let got = fs.read(ino, off, BLOCK as u32).unwrap();
                    reads.fetch_add(1, Relaxed);
                    match l.versions.iter().position(|v| *v == got) {
                        Some(i) => kinds[i] = true,
                        None => {
                            torn.fetch_add(1, Relaxed);
                            let tags = (got.len() >= BLOCK).then(|| {
                                (
                                    u64::from_le_bytes(got[..8].try_into().unwrap()),
                                    u64::from_le_bytes(got[BLOCK - 8..].try_into().unwrap()),
                                )
                            });
                            first_tear.lock().unwrap().get_or_insert(format!(
                                "len={} head/tail tags={tags:?}",
                                got.len()
                            ));
                        }
                    }
                }
                seen.fetch_max(kinds.iter().filter(|k| **k).count(), Relaxed);
            });
        }
        for i in 0..WRITES {
            if start.elapsed() > CAP {
                break;
            }
            last = (i + 1) % VERSIONS;
            let n = fs.write(ino, off, &l.versions[last]).unwrap();
            assert_eq!(n as usize, BLOCK, "short write");
        }
        stop.store(true, Relaxed);
    });

    let (reads, torn) = (reads.load(Relaxed), torn.load(Relaxed));
    println!(
        "{arm}: boundary {} inside block {}, {reads} reads, {torn} torn, a reader saw up to {} versions",
        l.boundary,
        l.block,
        seen.load(Relaxed)
    );
    assert_eq!(
        torn,
        0,
        "a 4 KiB read across the chunk boundary at {} mixed two writes ({arm}): {:?}",
        l.boundary,
        first_tear.lock().unwrap()
    );
    // Not vacuous: readers ran, and at least one of them saw the block change under it.
    assert!(reads >= 1000, "only {reads} reads ran");
    assert!(
        seen.load(Relaxed) >= 2,
        "no reader saw more than one version, so the race never happened"
    );
    // At rest the block is exactly the last acknowledged write.
    assert_eq!(fs.read(ino, off, BLOCK as u32).unwrap(), l.versions[last]);
}

#[test]
fn a_read_across_a_chunk_boundary_is_atomic_while_every_write_flushes() {
    run(
        Options {
            background: true,
            file_flush_bytes: BLOCK,
            ..Options::default()
        },
        "flush every write",
    );
}

#[test]
fn a_read_across_a_chunk_boundary_is_atomic_against_the_dirty_overlay() {
    run(
        Options {
            background: false,
            ..Options::default()
        },
        "overlay only",
    );
}
