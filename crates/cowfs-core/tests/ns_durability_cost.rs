//! What one namespace barrier costs, and what it does not cost. Labels a number, it is not a
//! benchmark: the run is a few hundred operations on whatever else the host is doing.
//!
//! The claim being priced is the one in `docs/nfs-namespace-durability90.md`: a metadata operation
//! on the NFS mount now pays one metadata sync and one store sync, and a `WRITE` pays nothing extra.

mod common;

use std::time::Instant;

use common::*;
use cowfs_core::{Core, Options};
use cowfs_vfs::{Vfs, ROOT_INO};

const OPS: usize = 200;

fn opts() -> Options {
    Options {
        background: false,
        ..Options::default()
    }
}

fn ms(per_op: std::time::Duration) -> f64 {
    per_op.as_secs_f64() * 1e3
}

#[test]
fn the_price_of_a_namespace_barrier() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let s = c.snapshot_view("s").unwrap();
    mkfile(&s, ROOT_INO, "a", b"x");
    s.sync_namespace(ROOT_INO).unwrap();

    // One rename plus one barrier, repeated. The barrier is what the repair added.
    let t = Instant::now();
    for _ in 0..OPS {
        s.rename(ROOT_INO, b"a", ROOT_INO, b"b", Default::default())
            .unwrap();
        s.sync_namespace(ROOT_INO).unwrap();
        s.rename(ROOT_INO, b"b", ROOT_INO, b"a", Default::default())
            .unwrap();
    }
    s.sync_namespace(ROOT_INO).unwrap();
    let barriered = t.elapsed() / (2 * OPS) as u32;

    // The same renames without a barrier, for the part that was always there.
    let t = Instant::now();
    for _ in 0..OPS {
        s.rename(ROOT_INO, b"a", ROOT_INO, b"b", Default::default())
            .unwrap();
        s.rename(ROOT_INO, b"b", ROOT_INO, b"a", Default::default())
            .unwrap();
    }
    let queued = t.elapsed() / (2 * OPS) as u32;
    s.sync_namespace(ROOT_INO).unwrap();

    // A write of a payload larger than the flush threshold, so the chunker really runs.
    let body = pattern(4 << 20, 11);
    let hot = mkfile(&s, ROOT_INO, "hot", &body[..0]);
    let t = Instant::now();
    s.write(hot.ino, 0, &body).unwrap();
    let wrote = t.elapsed();

    // A do-nothing baseline: a barrier with nothing pending. Whatever the number above is, this is
    // the floor, so the difference is the commit and not the call.
    s.sync_namespace(ROOT_INO).unwrap();
    let t = Instant::now();
    for _ in 0..OPS {
        s.sync_namespace(ROOT_INO).unwrap();
    }
    let idle = t.elapsed() / OPS as u32;

    // The same durable commit, reached the way a client reaches it today: rename, then a COMMIT of
    // the renamed file. This is what a client that emitted COMMIT after every rename already paid,
    // and what this one does not. If the barrier is not dearer than that, the repair costs no more
    // than the protocol's own price for the same guarantee.
    let t = Instant::now();
    for _ in 0..OPS {
        s.rename(ROOT_INO, b"a", ROOT_INO, b"b", Default::default())
            .unwrap();
        s.fsync(hot.ino, false).unwrap();
        s.rename(ROOT_INO, b"b", ROOT_INO, b"a", Default::default())
            .unwrap();
    }
    s.sync_namespace(ROOT_INO).unwrap();
    let commit = t.elapsed() / (2 * OPS) as u32;

    println!(
        "durability90 cost, {OPS} reps, debug build on a host with other mounts busy: \
         rename queued {:.3} ms; rename plus barrier {:.3} ms, so the barrier is about {:.3} ms; \
         rename plus a COMMIT of a file, which is what a client that emitted one would pay, \
         {:.3} ms; a barrier with nothing pending {:.3} ms; one 4 MiB write {:.3} ms",
        ms(queued),
        ms(barriered),
        ms(barriered.saturating_sub(queued)),
        ms(commit),
        ms(idle),
        ms(wrote),
    );
}
