// Private fixture seeder for the daemon-gc end-to-end harness.
//
// NOT production code. It uses the supported `cowfs_core` / `cowfs_store` API to
// build a *private* on-disk store whose sealed packs carry enough dead payload to
// clear the production `cowfs_gc::Options::default()` thresholds (min_dead_bytes
// 8 MiB, dead_ratio 0.5). The harness then runs the unmodified `cowfs-daemon` /
// `cowfs` binaries against that store with their own default options.
//
// The only non-default knob is the *fixture* store's `max_pack_size`, which is a
// supported `cowfs_store::Options` field. It is set small so many small packs
// seal and none of them is the long-lived active pack. The production user path
// (cowfs-daemon --backend core) never sets it and keeps the 256 MiB default.
//
// This is a standalone crate (its own `[workspace]`, its own `Cargo.lock`) so a
// clean checkout can build it without the repo's root workspace. Its path
// dependencies point at this checkout's `crates/`; the harness builds it with
// `cargo build --locked --offline` where possible.
//
// Usage:
//   gc-fixture-seed --store <dir> --keep-bytes <n> --dead-bytes <n> [--live-bytes <n>]
// Prints one JSON line describing what it wrote.

use std::io::Write;
use std::path::PathBuf;

use cowfs_core::{Core, Options};
use cowfs_store::Options as StoreOptions;
use cowfs_vfs::{Vfs, ROOT_INO};

const PACK: u64 = 16 << 20; // sealed-pack size for the fixture; production default is 256 MiB

fn body(n: usize, seed: u32) -> Vec<u8> {
    // Deterministic, incompressible-looking bytes (LCG), never deduplicated across seeds.
    let mut h = seed.wrapping_mul(2654435761).wrapping_add(1);
    (0..n)
        .map(|_| {
            h = h.wrapping_mul(1664525).wrapping_add(1013904223);
            (h >> 16) as u8
        })
        .collect()
}

fn put(v: &dyn Vfs, name: &str, data: &[u8]) {
    let a = v
        .create(ROOT_INO, name.as_bytes(), 0o644)
        .unwrap_or_else(|e| panic!("create {name}: {e}"));
    v.write(a.ino, 0, data)
        .unwrap_or_else(|e| panic!("write {name}: {e}"));
}

/// Writes `total` bytes in `chunk`-sized files, fsyncing the root every file so the
/// blocks are sealed into packs as the pack boundary is crossed.
fn fill(v: &dyn Vfs, prefix: &str, total: u64, seed_base: u32, files: &mut Vec<(String, usize)>) {
    let chunk = 512 * 1024usize;
    let n = (total / chunk as u64).max(1);
    for i in 0..n {
        let name = format!("{prefix}{i:03}");
        let data = body(chunk, seed_base.wrapping_add(i as u32));
        put(v, &name, &data);
        v.fsync(ROOT_INO, false)
            .unwrap_or_else(|e| panic!("fsync after {name}: {e}"));
        files.push((name, chunk));
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut store = None;
    let mut keep_bytes = 1 << 20;
    let mut dead_bytes = 24 << 20;
    let mut live_bytes = 1 << 20;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--store" => store = args.next().map(PathBuf::from),
            "--keep-bytes" => keep_bytes = args.next().unwrap().parse().unwrap(),
            "--dead-bytes" => dead_bytes = args.next().unwrap().parse().unwrap(),
            "--live-bytes" => live_bytes = args.next().unwrap().parse().unwrap(),
            other => panic!("unknown arg {other}"),
        }
    }
    let store = store.expect("--store required");

    let opts = Options {
        store: StoreOptions {
            max_pack_size: PACK,
            ..StoreOptions::default()
        },
        ..Options::default()
    };
    let core = Core::open(&store, opts).expect("open core");

    // Negative control: a snapshot kept LIVE whose payload sits at the head of the
    // dead-dominated pack. gc must preserve every block it references while freeing
    // the dead records around it, so the pack rewrite copies these bytes out intact.
    core.create_snapshot("live").expect("create live");
    let live_files = {
        let l = core.snapshot_view("live").expect("view live");
        let mut files = Vec::new();
        fill(&l, "live", live_bytes, 500000, &mut files);
        l.fsync(ROOT_INO, false).expect("fsync live");
        files
            .iter()
            .map(|(name, len)| {
                let idx: u32 = name.trim_start_matches("live").parse().unwrap();
                let data = body(*len, 500000u32.wrapping_add(idx));
                (name.clone(), *len, blake3::hash(&data).to_hex().to_string())
            })
            .collect::<Vec<_>>()
    };

    // Dead snapshot removed immediately, so its records (in the same packs as the live
    // ones above) become unreferenced while the live snapshot keeps its references.
    core.create_snapshot("drop").expect("create drop");
    let dead_files = {
        let d = core.snapshot_view("drop").expect("view drop");
        let mut files = Vec::new();
        fill(&d, "dead", dead_bytes, 900000, &mut files);
        d.fsync(ROOT_INO, false).expect("fsync drop");
        files.len()
    };
    core.remove_snapshot("drop").expect("remove drop");

    // Survivor snapshot written after the dead packs, so its blocks land in the
    // still-active pack, which the collector skips. Survivors are never in a candidate
    // pack: their integrity proves the reclaim did not disturb unrelated live data.
    core.create_snapshot("keep").expect("create keep");
    let survivors = {
        let k = core.snapshot_view("keep").expect("view keep");
        let mut files = Vec::new();
        fill(&k, "keep", keep_bytes, 1000, &mut files);
        k.fsync(ROOT_INO, false).expect("fsync keep");
        // Recompute the bytes written so the harness can verify readback by hash.
        files
            .iter()
            .map(|(name, len)| {
                let idx: u32 = name.trim_start_matches("keep").parse().unwrap();
                let data = body(*len, 1000u32.wrapping_add(idx));
                (name.clone(), *len, blake3::hash(&data).to_hex().to_string())
            })
            .collect::<Vec<_>>()
    };

    core.sync().expect("sync");
    core.close().expect("close core");

    let survivors_json = survivors
        .iter()
        .map(|(n, l, h)| format!("{{\"name\":{n:?},\"len\":{l},\"blake3\":{h:?}}}"))
        .collect::<Vec<_>>()
        .join(",");
    let live_json = live_files
        .iter()
        .map(|(n, l, h)| format!("{{\"name\":{n:?},\"len\":{l},\"blake3\":{h:?}}}"))
        .collect::<Vec<_>>()
        .join(",");
    let out = format!(
        "{{\"store\":{store:?},\"fixture_max_pack_size\":{PACK},\"keep_bytes\":{keep_bytes},\"dead_bytes\":{dead_bytes},\"live_bytes\":{live_bytes},\"dead_files\":{dead_files},\"survivors\":[{survivors_json}],\"live\":[{live_json}]}}",
        store = store.display().to_string(),
    );
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    writeln!(lock, "{out}").unwrap();
    lock.flush().unwrap();
}
