//! Worst single-write latency while a collect runs, at a few pack counts.
//!
//! A writer here is a bare `ingest_bytes` plus a metadata commit, which is the work a real write
//! does. The barrier is a gate the collector takes per pack, so the number this prints is what a
//! write waits for one pack's check and discard. Load is reported so a number taken on a busy
//! machine is not mistaken for one taken on a quiet one.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use cowfs_gc::{Barrier, ExtraRoots, Gc, Held, RootsError};
use cowfs_meta::Meta;
use cowfs_store::{BlockId, Options, Store};

fn noisy(n: usize, seed: u32) -> Vec<u8> {
    let mut h = seed.wrapping_mul(2654435761).wrapping_add(1);
    (0..n)
        .map(|_| {
            h = h.wrapping_mul(1664525).wrapping_add(1013904223);
            (h >> 24) as u8
        })
        .collect()
}

#[derive(Default)]
struct Flag {
    held: bool,
    writers: usize,
}

struct Gate {
    inner: Arc<GateInner>,
}

struct GateInner {
    st: Mutex<Flag>,
    cv: Condvar,
    taken: AtomicU64,
}

struct Guard(Arc<GateInner>);

impl Drop for Guard {
    fn drop(&mut self) {
        let mut g = self.0.st.lock().unwrap();
        g.held = false;
        self.0.cv.notify_all();
    }
}

impl Held for Guard {}
impl Barrier for Gate {
    fn take(&mut self) -> Option<Box<dyn Held>> {
        let mut g = self.inner.st.lock().unwrap();
        g.held = true;
        while g.writers > 0 {
            g = self.inner.cv.wait(g).unwrap();
        }
        drop(g);
        self.inner.taken.fetch_add(1, Relaxed);
        Some(Box::new(Guard(Arc::clone(&self.inner))))
    }
}

struct Release<'a>(&'a Gate);
impl Drop for Release<'_> {
    fn drop(&mut self) {
        let mut g = self.0.inner.st.lock().unwrap();
        g.writers -= 1;
        self.0.inner.cv.notify_all();
    }
}

struct Roots(Arc<Gate>);
impl ExtraRoots for Roots {
    fn pinned_blocks(&self) -> Result<Vec<BlockId>, RootsError> {
        Ok(Vec::new())
    }
    fn reference_barrier(&self) -> Result<Option<Box<dyn Barrier>>, RootsError> {
        Ok(Some(Box::new(Gate {
            inner: Arc::clone(&self.0.inner),
        }) as Box<dyn Barrier>))
    }
}

impl Roots {
    fn write<T>(&self, f: impl FnOnce() -> T) -> T {
        let mut g = self.0.inner.st.lock().unwrap();
        while g.held {
            g = self.0.inner.cv.wait(g).unwrap();
        }
        g.writers += 1;
        drop(g);
        let _r = Release(&self.0);
        f()
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let packs: usize = args.get(1).map_or(64, |s| s.parse().unwrap());
    let rounds: usize = args.get(2).map_or(5, |s| s.parse().unwrap());

    println!("packs {packs}, rounds {rounds}, load {:.2}", load());
    println!("packs candidates cycle_ms taken per_pack_us writer_worst_us");

    let d = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(
        Store::open(
            d.path().join("store"),
            Options {
                max_pack_size: 64 << 10,
                ..Options::default()
            },
        )
        .expect("store"),
    );
    let meta = Arc::new(
        Meta::open(
            d.path().join("meta"),
            cowfs_meta::Options {
                background: false,
                ..cowfs_meta::Options::default()
            },
        )
        .expect("meta"),
    );

    let snap = meta.new_snapshot("s").expect("snapshot");
    let live: Vec<BlockId> = (0..3u32)
        .map(|i| store.put(&noisy(4096, i)).unwrap())
        .collect();
    // Garbage interleaved with live data, so every pack is a real candidate.
    let mut n = 0u32;
    while store.stats().packs < packs as u64 {
        let seed = 100 + n;
        if n.is_multiple_of(3) {
            let name = format!("f{n}");
            let chunks = store.ingest_bytes(&noisy(4096, seed)).expect("ingest");
            let ino = snap
                .batch(|tx| tx.create(cowfs_meta::ROOT_INO, name.as_bytes(), 0o644))
                .expect("create")
                .ino;
            snap.batch(|tx| tx.set_content(ino, &chunks, 4096))
                .expect("set content");
        } else {
            store.put(&noisy(4096, seed)).expect("put");
        }
        n += 1;
    }
    store.sync().unwrap();
    meta.sync().unwrap();

    let gate = Arc::new(Gate {
        inner: Arc::new(GateInner {
            st: Mutex::new(Flag::default()),
            cv: Condvar::new(),
            taken: AtomicU64::new(0),
        }),
    });
    let roots = Roots(Arc::clone(&gate));
    let gc = Gc::open(
        d.path().join("gc"),
        Arc::clone(&store),
        Arc::clone(&meta),
        cowfs_gc::Options {
            dead_ratio: 0.05,
            min_dead_bytes: 1 << 10,
            ..cowfs_gc::Options::default()
        },
    )
    .expect("gc");

    for round in 0..rounds {
        let before = gate.inner.taken.load(Relaxed);
        let worst = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let stop_w = Arc::clone(&stop);
        let stop_w2 = Arc::clone(&stop);
        let t0 = Instant::now();
        std::thread::scope(|sc| {
            let store = Arc::clone(&store);
            let meta = Arc::clone(&meta);
            let worst_w = Arc::clone(&worst);
            let writer_gate = Arc::clone(&gate);
            let tag = round as u64;
            sc.spawn(move || {
                let mut i = 0u64;
                while !stop_w.load(Relaxed) {
                    let s = 5000 + (i % 97) as u32;
                    let t = Instant::now();
                    let name = format!("w{tag}-{i}");
                    let gw = Roots(Arc::clone(&writer_gate));
                    gw.write(|| {
                        let snap = meta.new_snapshot(&name).expect("snapshot");
                        let chunks = store.ingest_bytes(&noisy(4096, s)).expect("ingest");
                        let ino = snap
                            .batch(|tx| tx.create(cowfs_meta::ROOT_INO, name.as_bytes(), 0o644))
                            .expect("create")
                            .ino;
                        snap.batch(|tx| tx.set_content(ino, &chunks, 4096))
                            .expect("set content");
                        meta.sync().expect("sync");
                    });
                    worst_w.fetch_max(t.elapsed().as_micros() as u64, Relaxed);
                    i += 1;
                }
            });
            let r = gc.collect(Some(&roots)).expect("collect");
            stop_w2.store(true, Relaxed);
            let ms = t0.elapsed().as_millis();
            let taken = gate.inner.taken.load(Relaxed) - before;
            println!(
                "{} {} {} {} {} {}",
                round,
                r.candidates,
                ms,
                taken,
                if r.candidates > 0 {
                    (ms * 1000) / r.candidates as u128
                } else {
                    0
                },
                worst.load(Relaxed)
            );
            let _ = live;
        });
    }
    let _ = rounds;
}

fn load() -> f64 {
    let Ok(out) = std::process::Command::new("/bin/sh")
        .args(["-c", "sysctl -n vm.loadavg"])
        .output()
    else {
        return 0.0;
    };
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0)
}
