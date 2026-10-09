//! Issue 247 measurement, run by hand: `cargo test --release -p cowfs-store --test put_alloc --
//! --ignored --nocapture`. Writes N MiB of incompressible chunks through `Store::put`, no
//! crash-model log active, and prints allocation count/bytes and wall time per run.
#![allow(unsafe_code)] // a counting allocator cannot be written without `unsafe`

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use common::{opts, random};
use cowfs_store::Store;

struct Counting;
static BYTES: AtomicU64 = AtomicU64::new(0);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
// SAFETY: forwards to `System`; counting touches only atomics.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        BYTES.fetch_add(l.size() as u64, Relaxed);
        ALLOCS.fetch_add(1, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}
#[global_allocator]
static A: Counting = Counting;

#[test]
#[ignore = "measurement, run by hand in release mode"]
fn put_allocations_and_time() {
    const MIB: usize = 256;
    const CHUNK: usize = 256 * 1024;
    let blocks: Vec<Vec<u8>> = (0..MIB * 1024 * 1024 / CHUNK)
        .map(|i| random(i as u64, CHUNK))
        .collect();
    for run in 0..7 {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_unsynced(dir.path(), opts()).unwrap();
        let (b0, a0) = (BYTES.load(Relaxed), ALLOCS.load(Relaxed));
        let t = Instant::now();
        for b in &blocks {
            s.put(b).unwrap();
        }
        let secs = t.elapsed().as_secs_f64();
        println!(
            "run {run}: {MIB} MiB in {secs:.3}s ({:.0} MiB/s), allocs={}, bytes={}",
            MIB as f64 / secs,
            ALLOCS.load(Relaxed) - a0,
            BYTES.load(Relaxed) - b0
        );
    }
}
