mod common;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use common::{compressible, pack_ids, pack_path, random};
use cowfs_store::{BlockId, Error, Options, Store};
use proptest::prelude::*;

const POOL: usize = 24;

fn data(i: usize) -> Vec<u8> {
    let len = (i * 131) % 1500;
    if i.is_multiple_of(3) {
        compressible(i as u64, len)
    } else {
        random(i as u64, len)
    }
}

#[derive(Clone, Debug)]
enum Op {
    Put(usize),
    Get(usize),
    Sync,
    Checkpoint,
    Crash(u64),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        5 => (0..POOL).prop_map(Op::Put),
        3 => (0..POOL).prop_map(Op::Get),
        2 => Just(Op::Sync),
        1 => Just(Op::Checkpoint),
        1 => (0u64..400).prop_map(Op::Crash),
    ]
}

fn opts() -> Options {
    Options {
        max_pack_size: 2500,
        checkpoint_on_drop: false,
    }
}

fn pack_lens(dir: &Path) -> HashMap<u32, u64> {
    pack_ids(dir)
        .into_iter()
        .map(|id| (id, fs::metadata(pack_path(dir, id)).unwrap().len()))
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn store_matches_model_across_crashes(ops in prop::collection::vec(op(), 1..40)) {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let mut store = Some(Store::open(d, opts()).unwrap());
        let mut present: HashSet<usize> = HashSet::new();
        let mut durable: HashSet<usize> = HashSet::new();
        let mut synced = pack_lens(d);

        for op in ops {
            match op {
                Op::Put(i) => {
                    let s = store.as_ref().unwrap();
                    prop_assert_eq!(s.put(&data(i)).unwrap(), BlockId::of(&data(i)));
                    present.insert(i);
                }
                Op::Get(i) => {
                    let s = store.as_ref().unwrap();
                    let id = BlockId::of(&data(i));
                    if present.contains(&i) {
                        prop_assert_eq!(s.get(id).unwrap(), data(i));
                    } else {
                        prop_assert!(matches!(s.get(id), Err(Error::NotFound(_))));
                    }
                    prop_assert_eq!(s.stats().blocks, present.len() as u64);
                }
                Op::Sync | Op::Checkpoint => {
                    let s = store.as_ref().unwrap();
                    if matches!(op, Op::Sync) {
                        s.sync().unwrap();
                    } else {
                        s.checkpoint().unwrap();
                    }
                    durable = present.clone();
                    synced = pack_lens(d);
                }
                Op::Crash(extra) => {
                    drop(store.take());
                    let last = *pack_ids(d).last().unwrap();
                    let p = pack_path(d, last);
                    let actual = fs::metadata(&p).unwrap().len();
                    let keep = (synced.get(&last).copied().unwrap_or(16) + extra).min(actual);
                    fs::OpenOptions::new().write(true).open(&p).unwrap().set_len(keep).unwrap();

                    let s = Store::open(d, opts()).unwrap();
                    let mut survivors = HashSet::new();
                    for i in 0..POOL {
                        let id = BlockId::of(&data(i));
                        match s.get(id) {
                            Ok(bytes) => {
                                prop_assert_eq!(bytes, data(i));
                                prop_assert!(present.contains(&i), "block {} appeared from nowhere", i);
                                survivors.insert(i);
                            }
                            Err(e) => {
                                prop_assert!(matches!(e, Error::NotFound(_)), "{e}");
                                prop_assert!(!durable.contains(&i), "synced block {} lost", i);
                            }
                        }
                    }
                    prop_assert!(s.fsck().unwrap().is_clean());
                    present = survivors;
                    durable = present.clone();
                    synced = pack_lens(d);
                    store = Some(s);
                }
            }
        }

        drop(store.take());
        let s = Store::open(d, opts()).unwrap();
        for i in &present {
            prop_assert_eq!(s.get(BlockId::of(&data(*i))).unwrap(), data(*i));
        }
        prop_assert_eq!(s.stats().blocks, present.len() as u64);
        prop_assert!(s.fsck().unwrap().is_clean());
    }
}
