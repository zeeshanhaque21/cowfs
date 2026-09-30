//! Benchmarks for the collector. No heavy dependencies: the store and the metadata crate are the
//! only ones involved, and the corpus is generated so the numbers are reproducible.
//!
//! Run under the shared CPU lock, release, n>=5:
//!
//! ```text
//! cargo run --release -p cowfs-gc --example gc_bench -- mark <blocks> <n>
//! cargo run --release -p cowfs-gc --example gc_bench -- compact <blocks> <n>
//! cargo run --release -p cowfs-gc --example gc_bench -- garbage <blocks> <n> [dead_ratio]
//! ```

use std::sync::Arc;
use std::time::Instant;

use cowfs_gc::{Barrier, ExtraRoots, Gc, Options};
use cowfs_meta::Meta;
use cowfs_store::{Options as StoreOptions, Store};

/// A no-op barrier, standing in for `cowfs-core`'s flusher lock, so the bench measures the
/// collector and not a contended mutex.
struct Root;
struct Held;
impl Barrier for Held {}
impl ExtraRoots for Root {
    fn pinned_blocks(&self) -> Vec<cowfs_store::BlockId> {
        Vec::new()
    }
    fn reference_barrier(&self) -> Option<Box<dyn Barrier>> {
        Some(Box::new(Held))
    }
}

/// Half compressible, so a pack holds a realistic mix and a candidate has real bytes to copy.
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

struct World {
    dir: tempfile::TempDir,
    store: Arc<Store>,
    meta: Arc<Meta>,
    gc: Gc,
}

impl World {
    fn new(pack: u64, gopts: Options) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(
            Store::open(
                dir.path().join("store"),
                StoreOptions {
                    max_pack_size: pack,
                    ..StoreOptions::default()
                },
            )
            .expect("store"),
        );
        let meta = Arc::new(
            Meta::open(
                dir.path().join("meta"),
                cowfs_meta::Options {
                    background: false,
                    ..cowfs_meta::Options::default()
                },
            )
            .expect("meta"),
        );
        let gc = Gc::open(
            dir.path().join("gc"),
            Arc::clone(&store),
            Arc::clone(&meta),
            gopts,
        )
        .expect("gc");
        Self {
            dir,
            store,
            meta,
            gc,
        }
    }

    /// `files` files of 64 KiB, in a nested tree, each its own block or two.
    fn fill(&self, files: u32) -> u64 {
        let snap = self.meta.new_snapshot("base").expect("snapshot");
        for d in 0..16u32 {
            let dir = format!("d{d:02}");
            snap.batch(|tx| tx.mkdir(cowfs_meta::ROOT_INO, dir.as_bytes(), 0o755))
                .expect("mkdir");
            let di = snap
                .batch(|tx| {
                    tx.lookup(cowfs_meta::ROOT_INO, dir.as_bytes())
                        .map(|a| a.ino)
                })
                .expect("lookup");
            for i in 0..files {
                let data = body(65_536, d * files + i);
                let chunks = self.store.ingest_bytes(&data).expect("ingest");
                let name = format!("f{i:04}");
                let ino = snap
                    .batch(|tx| tx.create(di, name.as_bytes(), 0o644))
                    .expect("create")
                    .ino;
                snap.batch(|tx| tx.set_content(ino, &chunks, data.len() as u64))
                    .expect("set content");
            }
        }
        self.meta.sync().expect("meta sync");
        self.store.sync().expect("store sync");
        self.store.stats().blocks
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).expect("no NaN"));
    v[v.len() / 2]
}

/// Mark with and without subtree skipping, after a small change.
fn mark(blocks: u32, runs: usize) {
    let files = blocks / 16;
    let mut with_skip = Vec::new();
    let mut without_skip = Vec::new();
    let mut marked_with = 0u64;
    let mut marked_without = 0u64;
    let mut blocks = 0u64;
    for _ in 0..runs {
        let w = World::new(8 << 20, Options::default());
        blocks = w.fill(files);
        // The forks share every node with the base, so one marker over all of them walks the tree
        // once. `n_skip` of them is the workload: a collector run over n+1 snapshots that share a
        // subtree, which is the case subtree skipping exists for.
        let base = w.meta.snapshot("base").expect("snap");
        let forks: Vec<_> = (0..5)
            .map(|k| base.fork(&format!("f{k}")).expect("fork"))
            .collect();
        w.meta.sync().expect("sync");
        let di = base
            .batch(|tx| tx.lookup(cowfs_meta::ROOT_INO, b"d00").map(|a| a.ino))
            .expect("lookup");
        let si = base
            .batch(|tx| tx.lookup(di, b"f0000").map(|a| a.ino))
            .expect("lookup");
        for (k, fork) in forks.iter().enumerate() {
            let chunks = w
                .store
                .ingest_bytes(&body(65_536, 900_000 + k as u32))
                .expect("ingest");
            let fdi = fork
                .batch(|tx| tx.lookup(cowfs_meta::ROOT_INO, b"d00").map(|a| a.ino))
                .expect("lookup");
            let fsi = fork
                .batch(|tx| tx.lookup(fdi, b"f0000").map(|a| a.ino))
                .expect("lookup");
            fork.batch(|tx| tx.set_content(fsi, &chunks, 65_536))
                .expect("set content");
        }
        let _ = (di, si);
        w.meta.sync().expect("sync");

        // With one shared marker: the first snapshot pays for the tree, the rest pay for their change.
        let t = Instant::now();
        let mut marker = cowfs_meta::Marker::new();
        let mut n = 0u64;
        for s in std::iter::once(&base).chain(forks.iter()) {
            for b in s.live_blocks(&mut marker).expect("walk") {
                let _ = b.expect("block");
                n += 1;
            }
        }
        with_skip.push(t.elapsed().as_secs_f64());
        marked_with = n;

        // With a fresh marker per snapshot: every snapshot pays for the whole tree.
        let t = Instant::now();
        let mut n = 0u64;
        for s in std::iter::once(&base).chain(forks.iter()) {
            let mut fresh = cowfs_meta::Marker::new();
            for b in s.live_blocks(&mut fresh).expect("walk") {
                let _ = b.expect("block");
                n += 1;
            }
        }
        without_skip.push(t.elapsed().as_secs_f64());
        marked_without = n;
    }
    println!("mark: {blocks} live blocks in one snapshot, 6 snapshots sharing it, n={runs}");
    println!(
        "  with subtree skipping:    {:8.3} s median (range {:.3} to {:.3}), {marked_with} yields",
        median(with_skip.clone()),
        with_skip.iter().cloned().fold(f64::MAX, f64::min),
        with_skip.iter().cloned().fold(0.0, f64::max)
    );
    println!(
        "  without (a marker each):  {:8.3} s median (range {:.3} to {:.3}), {marked_without} yields",
        median(without_skip.clone()),
        without_skip.iter().cloned().fold(f64::MAX, f64::min),
        without_skip.iter().cloned().fold(0.0, f64::max)
    );
}

/// Compaction throughput, in MiB/s of records copied.
fn compact(blocks: u32, runs: usize) {
    let files = blocks / 16;
    let mut mib_s = Vec::new();
    let mut copied = 0u64;
    for _ in 0..runs {
        let w = World::new(
            8 << 20,
            Options {
                dead_ratio: 0.0,
                min_dead_bytes: 1,
                io_budget_bytes: 0,
                ..Options::default()
            },
        );
        w.fill(files);
        // Garbage in sealed packs, interleaved so every candidate has live records to copy.
        for i in 0..files {
            w.store.put(&body(32_768, 500_000 + i)).expect("put");
        }
        w.store.sync().expect("sync");
        let live: std::collections::HashSet<_> = {
            let mut m = cowfs_meta::Marker::new();
            let s = w.meta.snapshot("base").expect("snap");
            s.live_blocks(&mut m)
                .expect("walk")
                .map(|b| b.expect("block"))
                .collect()
        };
        // A dry run first, so the timed cycle copies exactly what the plan said it would.
        let dry = Gc::open(
            w.dir.path().join("dry"),
            Arc::clone(&w.store),
            Arc::clone(&w.meta),
            Options {
                dry_run: true,
                dead_ratio: 0.0,
                min_dead_bytes: 1,
                io_budget_bytes: 0,
                ..Options::default()
            },
        )
        .expect("gc");
        let plan = dry.collect(Some(&Root)).expect("dry run");
        let t = Instant::now();
        let r = w.gc.collect(Some(&Root)).expect("collect");
        let secs = t.elapsed().as_secs_f64();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        // The dry run changes nothing, so it finds the same candidates. It reports the work in
        // `candidate_bytes`, because it does not copy.
        assert_eq!(
            r.candidates, plan.candidates,
            "the dry run found the same candidates"
        );
        assert!(
            r.bytes_copied > 0 && plan.candidate_bytes > 0,
            "the cycle copied {} bytes over {} candidate bytes",
            r.bytes_copied,
            plan.candidate_bytes
        );
        copied = r.bytes_copied;
        mib_s.push(r.bytes_copied as f64 / (1024.0 * 1024.0) / secs);
        let _ = live;
    }
    let med = median(mib_s.clone());
    println!(
        "compaction: {:.1} MiB/s median (range {:.1} to {:.1}), {copied} bytes copied per run, n={runs}",
        med,
        mib_s.iter().cloned().fold(f64::MAX, f64::min),
        mib_s.iter().cloned().fold(0.0, f64::max)
    );
}

/// A store that is 90% garbage, at a dead-ratio threshold the caller chooses.
fn garbage(blocks: u32, runs: usize, dead_ratio: f64) {
    let files = blocks / 16;
    let mut secs = Vec::new();
    let mut freed = 0u64;
    let mut before = 0u64;
    let mut after = 0u64;
    for _ in 0..runs {
        let w = World::new(
            8 << 20,
            Options {
                dead_ratio,
                min_dead_bytes: 1,
                io_budget_bytes: 0,
                ..Options::default()
            },
        );
        w.fill(files);
        // Enough garbage to bury the live set, which is the 90% case. `blocks` is the number of
        // live blocks, and `files` is a sixteenth of that, so this is 80 times the live file count.
        for i in 0..(blocks * 5) {
            w.store.put(&body(32_768, 500_000 + i)).expect("put");
        }
        w.store.sync().expect("sync");
        before = w.store.stats().pack_bytes;
        let t = Instant::now();
        let r = w.gc.collect(Some(&Root)).expect("collect");
        secs.push(t.elapsed().as_secs_f64());
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        freed += r.freed_bytes;
        after += w.store.stats().pack_bytes;
        for b in {
            let mut m = cowfs_meta::Marker::new();
            let s = w.meta.snapshot("base").expect("snap");
            s.live_blocks(&mut m)
                .expect("walk")
                .map(|x| x.expect("block"))
                .collect::<Vec<_>>()
        } {
            assert!(w.store.get(b).is_ok(), "a live block did not survive");
        }
    }
    println!(
        "90% garbage at dead_ratio {dead_ratio}: {:.3} s median (range {:.3} to {:.3}) to free \
         {freed} bytes; packs {:.1} MiB -> {:.1} MiB, n={runs}",
        median(secs.clone()),
        secs.iter().cloned().fold(f64::MAX, f64::min),
        secs.iter().cloned().fold(0.0, f64::max),
        before as f64 / (1024.0 * 1024.0),
        after as f64 / (1024.0 * 1024.0) / runs as f64,
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map_or("mark", String::as_str);
    let blocks: u32 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(160_000);
    let runs: usize = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(5);
    let dead_ratio: f64 = args.get(4).and_then(|v| v.parse().ok()).unwrap_or(0.5);
    match mode {
        "mark" => mark(blocks, runs),
        "compact" => compact(blocks, runs),
        "garbage" => garbage(blocks, runs, dead_ratio),
        other => {
            eprintln!("unknown mode {other}: mark, compact or garbage");
            std::process::exit(2);
        }
    }
}
