//! Benchmark on real files. Usage: bench <data-dir> <store-dir> [MiB=1024] [runs=5]
//! Reads a slice of `data-dir` (every k-th file, sorted), then times chunk+hash, ingest, verified reads and dedup lookups.

use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::thread;
use std::time::Instant;

use cowfs_store::{chunks, BlockId, ChunkRef, Options, Store};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            walk(&e.path(), out);
        } else if t.is_file() {
            out.push(e.path());
        }
    }
}

fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s[s.len() / 2]
}

fn report(name: &str, unit: &str, runs: &[f64]) {
    let list: Vec<String> = runs.iter().map(|r| format!("{r:.0}")).collect();
    println!(
        "{name:<34} n={} median={:.0} {unit}  min={:.0} max={:.0}  runs=[{}]",
        runs.len(),
        median(runs),
        runs.iter().cloned().fold(f64::MAX, f64::min),
        runs.iter().cloned().fold(0.0, f64::max),
        list.join(", ")
    );
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, f64) {
    let t = Instant::now();
    let r = f();
    (r, t.elapsed().as_secs_f64())
}

fn parallel<T: Send>(threads: usize, n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let next = AtomicUsize::new(0);
    let out: Vec<Vec<(usize, T)>> = thread::scope(|sc| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                sc.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Relaxed);
                        if i >= n {
                            return mine;
                        }
                        mine.push((i, f(i)));
                    }
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut flat: Vec<(usize, T)> = out.into_iter().flatten().collect();
    flat.sort_by_key(|x| x.0);
    flat.into_iter().map(|x| x.1).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data_dir = PathBuf::from(&args[1]);
    let store_dir = PathBuf::from(&args[2]);
    let want: u64 = args.get(3).map_or(1024, |s| s.parse().unwrap()) << 20;
    let runs: usize = args.get(4).map_or(5, |s| s.parse().unwrap());

    let mut files = Vec::new();
    walk(&data_dir, &mut files);
    files.sort();
    let total: u64 = files
        .iter()
        .filter_map(|p| fs::metadata(p).ok())
        .map(|m| m.len())
        .sum();
    let stride = (total / want).max(1) as usize;
    let picked: Vec<&PathBuf> = files.iter().step_by(stride).collect();

    let mut mib = Vec::new();
    let mut data: Vec<Vec<u8>> = Vec::new();
    for r in 0..runs {
        let (d, t) = timed(|| {
            picked
                .iter()
                .filter_map(|p| fs::read(p).ok())
                .collect::<Vec<_>>()
        });
        let bytes: usize = d.iter().map(Vec::len).sum();
        mib.push(bytes as f64 / (1 << 20) as f64 / t);
        if r + 1 == runs {
            data = d;
        }
    }
    let bytes: u64 = data.iter().map(|d| d.len() as u64).sum();
    let mb = bytes as f64 / (1 << 20) as f64;
    println!(
        "data: {} of {} files ({} MiB of {} MiB), stride {stride}, {} files with data",
        picked.len(),
        files.len(),
        bytes >> 20,
        total >> 20,
        data.len()
    );
    report("baseline: read files (warm)", "MiB/s", &mib);

    let mut chunk_only = Vec::new();
    let mut hash_only = Vec::new();
    let mut both = Vec::new();
    let mut nchunks = 0usize;
    for _ in 0..runs {
        let (n, t) = timed(|| data.iter().map(|d| chunks(d).count()).sum::<usize>());
        nchunks = n;
        chunk_only.push(mb / t);
        let (_, t) = timed(|| {
            data.iter().for_each(|d| {
                black_box(blake3::hash(d));
            })
        });
        hash_only.push(mb / t);
        let (_, t) = timed(|| {
            data.iter().for_each(|d| {
                chunks(d).for_each(|c| {
                    black_box(BlockId::of(c));
                })
            })
        });
        both.push(mb / t);
    }
    println!(
        "chunks: {nchunks}, average {} bytes",
        bytes as usize / nchunks.max(1)
    );
    report("chunk only, 1 thread", "MiB/s", &chunk_only);
    report("blake3 whole-file only, 1 thread", "MiB/s", &hash_only);
    report("chunk + hash, 1 thread", "MiB/s", &both);

    let opts = Options {
        checkpoint_on_drop: false,
        ..Options::default()
    };

    let mut comp = Vec::new();
    let mut raw_write = Vec::new();
    let mut nosync = Vec::new();
    for _ in 0..runs {
        let (_, t) = timed(|| {
            let mut c = zstd::bulk::Compressor::new(3).unwrap();
            data.iter().for_each(|d| {
                chunks(d).for_each(|ch| {
                    black_box(c.compress(ch).unwrap());
                })
            })
        });
        comp.push(mb / t);
        let _ = fs::remove_dir_all(&store_dir);
        fs::create_dir_all(&store_dir).unwrap();
        let (_, t) = timed(|| {
            use std::io::Write;
            let mut f = fs::File::create(store_dir.join("raw.bin")).unwrap();
            data.iter().for_each(|d| f.write_all(d).unwrap());
            f.sync_data().unwrap();
        });
        raw_write.push(mb / t);
        let _ = fs::remove_dir_all(&store_dir);
        let st = Store::open(&store_dir, opts).unwrap();
        let (_, t) = timed(|| {
            data.iter().for_each(|d| {
                st.ingest_bytes(d).unwrap();
            })
        });
        nosync.push(mb / t);
        drop(st);
    }
    report("zstd-3 compress of chunks, 1 thread", "MiB/s", &comp);
    report("baseline: raw write + fdatasync", "MiB/s", &raw_write);
    report("ingest without sync, 1 thread", "MiB/s", &nosync);
    let mut lists: Vec<Vec<ChunkRef>> = Vec::new();
    let mut store = None;
    for threads in [1usize, 8] {
        let mut v = Vec::new();
        for _ in 0..runs {
            drop(store.take());
            let _ = fs::remove_dir_all(&store_dir);
            let s = Store::open(&store_dir, opts).unwrap();
            let (l, t) = timed(|| {
                let l = parallel(threads, data.len(), |i| s.ingest_bytes(&data[i]).unwrap());
                s.sync().unwrap();
                l
            });
            v.push(mb / t);
            lists = l;
            store = Some(s);
        }
        report(&format!("ingest + sync, {threads} thread(s)"), "MiB/s", &v);
    }

    let refs_files: Vec<&Vec<u8>> = data.iter().collect();
    let mut v = Vec::new();
    for _ in 0..runs {
        drop(store.take());
        let s2 = Store::open(&store_dir, Options::default()).unwrap();
        let (_, t) = timed(|| {
            refs_files.iter().for_each(|d| {
                s2.ingest_bytes(d).unwrap();
            })
        });
        v.push(mb / t);
        drop(s2);
        store = Some(Store::open(&store_dir, opts).unwrap());
    }
    report("re-ingest dupes after reopen, 1 thr", "MiB/s", &v);
    let s = store.unwrap();
    let st = s.stats();
    println!(
        "store: {} blocks, {} MiB uncompressed unique, {} MiB stored, ratio {:.2}x on input, {} dedup hits of {} puts",
        st.blocks,
        st.uncompressed_bytes >> 20,
        st.stored_bytes >> 20,
        bytes as f64 / st.stored_bytes as f64,
        st.dedup_hits,
        st.put_calls
    );

    let refs: Vec<ChunkRef> = lists.iter().flatten().copied().collect();
    for threads in [1usize, 8] {
        let mut v = Vec::new();
        for _ in 0..runs {
            let (n, t) = timed(|| {
                parallel(threads, refs.len(), |i| {
                    s.get(refs[i].id).unwrap().len() as u64
                })
                .iter()
                .sum::<u64>()
            });
            v.push(n as f64 / (1 << 20) as f64 / t);
        }
        report(&format!("verified read, {threads} thread(s)"), "MiB/s", &v);
    }

    for threads in [1usize, 8] {
        let mut v = Vec::new();
        for _ in 0..runs {
            let (_, t) =
                timed(|| parallel(threads, data.len(), |i| s.ingest_bytes(&data[i]).unwrap()));
            v.push(mb / t);
        }
        report(
            &format!("re-ingest all dupes, {threads} thread(s)"),
            "MiB/s",
            &v,
        );
    }
    let mut v = Vec::new();
    for _ in 0..runs {
        let (n, t) = timed(|| refs.iter().filter(|r| s.contains(r.id)).count());
        assert_eq!(n, refs.len());
        v.push(n as f64 / t / 1e3);
    }
    report("contains (index lookup), 1 thread", "k/s", &v);

    println!("hot single-block get (cache-hot, CPU bound), 2000 iterations per run:");
    for (label, want_compressible) in [
        ("compressible block", true),
        ("incompressible block", false),
    ] {
        let pick = data
            .iter()
            .flat_map(|d| chunks(d))
            .filter(|c| c.len() >= 128 * 1024)
            .find_map(|c| {
                let z = zstd::bulk::compress(c, 3).unwrap();
                ((z.len() * 100 <= c.len() * 95) == want_compressible).then_some((c, z))
            });
        let Some((c, z)) = pick else {
            println!("  no {label} of 128 KiB or more in the sample");
            continue;
        };
        let id = BlockId::of(c);
        let stored = if want_compressible { z.len() } else { c.len() };
        let rec = vec![7u8; 56 + stored];
        let mib = c.len() as f64 / (1 << 20) as f64;
        let bench = |f: &dyn Fn()| -> Vec<f64> {
            (0..runs)
                .map(|_| {
                    let (_, t) = timed(|| (0..2000).for_each(|_| f()));
                    mib * 2000.0 / t
                })
                .collect()
        };
        println!("  {label}: {} bytes, stored {stored}", c.len());
        report(
            "    get() total",
            "MiB/s",
            &bench(&|| drop(black_box(s.get(id).unwrap()))),
        );
        report(
            "    crc32c of the record",
            "MiB/s",
            &bench(&|| {
                black_box(crc32c::crc32c(&rec));
            }),
        );
        report(
            "    blake3 of the block",
            "MiB/s",
            &bench(&|| {
                black_box(blake3::hash(c));
            }),
        );
        if want_compressible {
            report(
                "    zstd decompress",
                "MiB/s",
                &bench(&|| drop(black_box(zstd::bulk::decompress(&z, c.len()).unwrap()))),
            );
        }
        report(
            "    memcpy of the block",
            "MiB/s",
            &bench(&|| drop(black_box(c.to_vec()))),
        );
    }
    println!("fsck: {:?}", s.fsck().unwrap().is_clean());
}
