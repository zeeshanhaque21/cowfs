//! What the reference gate costs the write path. Ignored: it measures, it does not assert.
//!
//! The same file builds against a core without the gate, so a number from this tree can be set
//! beside one from the commit before it:
//!
//! ```text
//! cargo test -p cowfs-core --release --test gate_overhead -- --ignored --nocapture
//! ```

use std::time::Instant;

use cowfs_core::{Core, Options};
use cowfs_vfs::{Vfs, ROOT_INO};

fn body(n: usize, seed: u32) -> Vec<u8> {
    let mut h = seed.wrapping_mul(2654435761).wrapping_add(1);
    (0..n)
        .map(|_| {
            h = h.wrapping_mul(1664525).wrapping_add(1013904223);
            (h >> 16) as u8
        })
        .collect()
}

fn run(round: u32) -> (f64, f64) {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(dir.path(), Options::default()).unwrap();
    core.create_snapshot("main").unwrap();
    let t = Instant::now();
    std::thread::scope(|s| {
        for th in 0..4u32 {
            let v = core.snapshot_view("main").unwrap();
            s.spawn(move || {
                for i in 0..60u32 {
                    let a = v
                        .create(ROOT_INO, format!("t{th}f{i}").as_bytes(), 0o644)
                        .unwrap();
                    let data = body(256 << 10, round * 1_000_000 + th * 1000 + i);
                    v.write(a.ino, 0, &data).unwrap();
                    if i % 8 == 7 {
                        v.fsync(a.ino, false).unwrap();
                    }
                }
            });
        }
    });
    core.sync().unwrap();
    let writes = t.elapsed().as_secs_f64();

    let v = core.snapshot_view("main").unwrap();
    let dirino = v.mkdir(ROOT_INO, b"many", 0o755).unwrap().ino;
    let t = Instant::now();
    for i in 0..20_000u32 {
        v.create(dirino, format!("e{i}").as_bytes(), 0o644).unwrap();
    }
    core.sync().unwrap();
    let creates = t.elapsed().as_secs_f64();
    drop(v);
    core.close().unwrap();
    (writes, creates)
}

#[test]
#[ignore = "measures"]
fn the_write_path_with_this_core() {
    let mut w = Vec::new();
    let mut c = Vec::new();
    for round in 0..7 {
        let (a, b) = run(round);
        println!("round {round}: writes {a:.3}s creates {b:.3}s");
        w.push(a);
        c.push(b);
    }
    w.sort_by(f64::total_cmp);
    c.sort_by(f64::total_cmp);
    println!(
        "RESULT n=7 writes_median={:.3}s writes_min={:.3}s writes_max={:.3}s creates_median={:.3}s creates_min={:.3}s creates_max={:.3}s",
        w[3], w[0], w[6], c[3], c[0], c[6]
    );
}
