//! Partial-chunk writes, truncates and extends against a `Vec<u8>` model, with offsets and sizes
//! on and around the FastCDC bounds (16 KiB minimum, 64 KiB average, 256 KiB maximum).

mod common;

use common::{fixture_with, pattern, read_all, root_entry, test_opts, truncate, write_all};
use cowfs_core::Options;
use cowfs_vfs::{Vfs, ROOT_INO};
use proptest::prelude::*;

const EDGES: [u64; 12] = [
    0,
    1,
    16 << 10,
    (16 << 10) + 1,
    (64 << 10) - 1,
    64 << 10,
    (256 << 10) - 1,
    256 << 10,
    (256 << 10) + 1,
    512 << 10,
    (512 << 10) + 7,
    700_000,
];

#[derive(Clone, Debug)]
enum Step {
    Write { off: u64, len: u32, seed: u8 },
    Truncate(u64),
    Fsync,
    DropCaches,
    Check,
}

fn pos() -> impl Strategy<Value = u64> {
    prop_oneof![
        5 => prop::sample::select(EDGES.to_vec()),
        3 => 0u64..900_000,
        2 => (prop::sample::select(EDGES.to_vec()), -3i64..4)
            .prop_map(|(e, d)| (e as i64 + d).max(0) as u64),
    ]
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        8 => (pos(), prop_oneof![
            3 => 1u32..64,
            3 => 1u32..20_000,
            2 => prop::sample::select(vec![16384u32, 16385, 65536, 65535, 262_144, 262_145]),
            1 => 100_000u32..400_000,
        ], any::<u8>()).prop_map(|(off, len, seed)| Step::Write { off, len, seed }),
        3 => pos().prop_map(Step::Truncate),
        2 => Just(Step::Fsync),
        1 => Just(Step::DropCaches),
        2 => Just(Step::Check),
    ]
}

fn run(steps: &[Step], opts: Options) {
    let f = fixture_with(opts);
    let c = &f.core;
    c.create_snapshot("s").unwrap();
    let r = root_entry(c, "s").ino;
    let ino = c.create(r, b"f", 0o644).unwrap().ino;
    let mut model: Vec<u8> = Vec::new();
    for (n, s) in steps.iter().enumerate() {
        match s {
            Step::Write { off, len, seed } => {
                let data = pattern(*len as usize, u64::from(*seed) + 1);
                write_all(c, ino, *off, &data);
                let end = *off as usize + data.len();
                if model.len() < end {
                    model.resize(end, 0);
                }
                model[*off as usize..end].copy_from_slice(&data);
            }
            Step::Truncate(size) => {
                truncate(c, ino, *size).unwrap();
                model.resize(*size as usize, 0);
            }
            Step::Fsync => c.fsync(ino, false).unwrap(),
            Step::DropCaches => c.drop_caches(),
            Step::Check => {
                assert_eq!(
                    c.getattr(ino).unwrap().size,
                    model.len() as u64,
                    "size at step {n}"
                );
                assert!(
                    read_all(c, ino) == model,
                    "content differs at step {n}: {s:?}"
                );
            }
        }
    }
    assert_eq!(c.getattr(ino).unwrap().size, model.len() as u64);
    assert!(read_all(c, ino) == model, "final content differs");
    c.sync().unwrap();
    c.drop_caches();
    assert!(
        read_all(c, c.lookup(r, b"f").unwrap().ino) == model,
        "content after drop_caches differs"
    );
    c.check().unwrap();
    assert!(c.fsck().unwrap().is_clean());
    let _ = ROOT_INO;
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 40, max_shrink_iters: 200, ..ProptestConfig::default() })]

    #[test]
    fn writes_truncates_and_extends_match_a_vec(steps in prop::collection::vec(step(), 1..30)) {
        run(&steps, test_opts());
    }

    #[test]
    fn same_with_eager_flushing(steps in prop::collection::vec(step(), 1..30)) {
        run(&steps, Options { file_flush_bytes: 4096, max_pending_ops: 2, ..test_opts() });
    }
}

#[test]
fn boundary_sized_files_round_trip_and_sequential_appends_dedup_with_a_single_write() {
    let f = fixture_with(test_opts());
    let c = &f.core;
    c.create_snapshot("s").unwrap();
    let r = root_entry(c, "s").ino;
    for (i, len) in [
        0usize,
        1,
        16383,
        16384,
        16385,
        65535,
        65536,
        65537,
        262_143,
        262_144,
        262_145,
        1 << 20,
    ]
    .into_iter()
    .enumerate()
    {
        let data = pattern(len, i as u64 + 50);
        let a = c.create(r, format!("b{len}").as_bytes(), 0o644).unwrap();
        write_all(c, a.ino, 0, &data);
        c.fsync(a.ino, false).unwrap();
        c.drop_caches();
        assert!(
            read_all(c, c.lookup(r, format!("b{len}").as_bytes()).unwrap().ino) == data,
            "{len}"
        );
    }
    let data = pattern(3 << 20, 77);
    let one = c.create(r, b"one", 0o644).unwrap().ino;
    write_all(c, one, 0, &data);
    c.fsync(one, false).unwrap();
    let before = c.store().stats();
    let many = c.create(r, b"many", 0o644).unwrap().ino;
    for (i, piece) in data.chunks(100_003).enumerate() {
        write_all(c, many, (i * 100_003) as u64, piece);
        if i % 3 == 0 {
            c.fsync(many, false).unwrap();
        }
    }
    c.fsync(many, false).unwrap();
    let after = c.store().stats();
    assert!(read_all(c, many) == data);
    let new_bytes = after.uncompressed_bytes - before.uncompressed_bytes;
    assert!(
        new_bytes < 1 << 20,
        "sequential appends of identical content stored {new_bytes} new bytes"
    );
}
