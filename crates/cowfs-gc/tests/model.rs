//! Model-based property test: random operations against an in-memory model of what is live.
//!
//! The invariant is the one criterion 3 rests on: after every collect, every block the model says
//! is live reads back with exactly the bytes that were put. Only unreferenced blocks may be gone.

mod common;

use std::collections::{HashMap, HashSet};

use common::{Fixture, Roots};
use cowfs_meta::Snapshot;
use cowfs_store::BlockId;
use proptest::prelude::*;

/// The model: every live snapshot, and what each file in it references.
#[derive(Debug, Default, Clone)]
struct Model {
    /// Snapshot id to its files, each file to the blocks it references.
    snaps: HashMap<u64, HashMap<String, Vec<BlockId>>>,
    /// Every block ever put, with its bytes, so a read can be checked against them.
    bytes: HashMap<BlockId, Vec<u8>>,
}

impl Model {
    /// Blocks any live snapshot references.
    fn live(&self) -> HashSet<BlockId> {
        self.snaps
            .values()
            .flat_map(|files| files.values().flatten().copied())
            .collect()
    }

    /// Drop a snapshot and everything only it referenced.
    fn remove(&mut self, id: u64) {
        self.snaps.remove(&id);
    }
}

/// The world a generated script runs against: the fixture plus the model.
struct World {
    f: Fixture,
    model: Model,
    snaps: Vec<Snapshot>,
    /// Names created in each snapshot, so a rewrite can pick one.
    names: Vec<Vec<String>>,
}

impl World {
    fn new() -> Self {
        let f = Fixture::eager(64 << 10);
        let mut w = Self {
            f,
            model: Model::default(),
            snaps: Vec::new(),
            names: Vec::new(),
        };
        for i in 0..3 {
            w.add_snapshot(i);
        }
        w
    }

    fn add_snapshot(&mut self, k: u8) -> u64 {
        let snap = self.f.meta.new_snapshot(&format!("s{k}")).unwrap();
        let id = snap.id().0;
        self.snaps.push(snap);
        self.names.push(Vec::new());
        self.model.snaps.insert(id, HashMap::new());
        id
    }

    /// Store content, write it into a file, and record both in the model.
    fn write(&mut self, snap: usize, name: &str, data: &[u8]) {
        let chunks = self.f.store.ingest_bytes(data).unwrap();
        self.f.write(&self.snaps[snap], name.as_bytes(), data);
        let ids: Vec<BlockId> = chunks.iter().map(|c| c.id).collect();
        for (c, b) in chunks.iter().zip(&data_of(data, &chunks)) {
            self.model.bytes.insert(c.id, b.clone());
        }
        if !self.names[snap].iter().any(|n| n == name) {
            self.names[snap].push(name.to_string());
        }
        self.model
            .snaps
            .get_mut(&self.snaps[snap].id().0)
            .expect("snapshot")
            .insert(name.to_string(), ids);
    }

    /// Delete a file, so the model stops referencing its blocks.
    fn unlink(&mut self, snap: usize, name: &str) {
        let ino = self.snaps[snap].batch(|tx| {
            tx.lookup(cowfs_meta::ROOT_INO, name.as_bytes())
                .map(|a| a.ino)
        });
        if let Ok(ino) = ino {
            self.snaps[snap]
                .batch(|tx| tx.unlink(cowfs_meta::ROOT_INO, name.as_bytes()))
                .expect("unlink");
            let _ = ino;
        }
        self.names[snap].retain(|n| n != name);
        self.model
            .snaps
            .get_mut(&self.snaps[snap].id().0)
            .expect("snapshot")
            .remove(name);
    }

    /// Drop the store and the database and open both again, which is what a clean restart does.
    /// Snapshot handles are taken again by name, so the model's ids are remapped.
    fn reopen(self) -> Self {
        let Self {
            f,
            model,
            snaps,
            names,
        } = self;
        let mut want = Vec::new();
        for s in &snaps {
            want.push(s.info().expect("info").name);
        }
        drop(snaps);
        let f = f.reopen();
        let mut new_snaps = Vec::new();
        for n in want {
            new_snaps.push(f.meta.snapshot(&n).expect("reopen snapshot"));
        }
        Self {
            f,
            model,
            snaps: new_snaps,
            names,
        }
    }

    /// The invariant, checked against the model and not against the store's own opinion.
    fn check(&self, where_: &str) {
        for b in self.model.live() {
            let got = self.f.store.get(b).unwrap_or_else(|e| {
                panic!("{where_}: live block {} lost: {e}", &b.to_string()[..8])
            });
            let want = self
                .model
                .bytes
                .get(&b)
                .unwrap_or_else(|| panic!("{where_}: model has no bytes for a live block"));
            assert_eq!(&got, want, "{where_}: live block has the wrong bytes");
        }
    }
}

/// The original bytes each chunk covered, so the model can check a read.
fn data_of(data: &[u8], chunks: &[cowfs_store::ChunkRef]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for c in chunks {
        let n = usize::try_from(c.len).unwrap_or(0);
        out.push(data[at..(at + n).min(data.len())].to_vec());
        at += n;
    }
    out
}

fn body(n: usize, seed: u8) -> Vec<u8> {
    // Half compressible, so packs are worth rewriting, and distinct per seed.
    let mut out = Vec::with_capacity(n);
    let mut h = u32::from(seed).wrapping_mul(2654435761).wrapping_add(1);
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

/// A generated step.
#[derive(Debug, Clone)]
enum Op {
    Write(u8, u8, usize),
    Fork(u8),
    Unlink(u8, u8),
    Remove(u8),
    Collect,
    Reopen,
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..8, 0u8..8, 1usize..20_000).prop_map(|(s, i, n)| Op::Write(s, i, n)),
        (0u8..8).prop_map(Op::Fork),
        (0u8..8, 0u8..4).prop_map(|(s, i)| Op::Unlink(s, i)),
        (0u8..8).prop_map(Op::Remove),
        Just(Op::Collect),
        Just(Op::Reopen),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 40, max_shrink_iters: 500, ..ProptestConfig::default() })]

    /// The invariant: after every collect and every reopen, every block the model says is live
    /// reads back with exactly the bytes that were put.
    #[test]
    fn every_live_block_survives_every_collect(ops in prop::collection::vec(op_strategy(), 1..60)) {
        let mut w = World::new();
        let roots = Roots::new();
        // Seed every snapshot with a few files so a rewrite and an unlink have something to hit.
        for s in 0..w.snaps.len() {
            for i in 0..3 {
                let name = format!("seed{i}");
                w.write(s, &name, &body(3000, s as u8 * 8 + i as u8));
            }
        }
        w.f.meta.sync().unwrap();
        w.f.store.sync().unwrap();

        for (step, op) in ops.iter().enumerate() {
            let here = format!("step {step} {op:?}");
            match op {
                Op::Write(s, i, n) => {
                    let s = usize::from(*s) % w.snaps.len();
                    let name = if w.names[s].is_empty() {
                        format!("w{step}")
                    } else {
                        w.names[s][usize::from(*i) % w.names[s].len()].clone()
                    };
                    let seed = (step as u8).wrapping_mul(31).wrapping_add(*i);
                    w.write(s, &name, &body(*n, seed));
                }
                Op::Fork(k) => {
                    let parent = usize::from(*k) % w.snaps.len();
                    let forked = w.snaps[parent].fork(&format!("f{step}")).unwrap();
                    let id = forked.id().0;
                    // The fork starts with the parent's files, so the model inherits them.
                    let files = w
                        .model
                        .snaps
                        .get(&w.snaps[parent].id().0)
                        .cloned()
                        .unwrap_or_default();
                    w.model.snaps.insert(id, files);
                    w.names.push(w.names[parent].clone());
                    w.snaps.push(forked);
                }
                Op::Unlink(s, i) => {
                    let s = usize::from(*s) % w.snaps.len();
                    if w.names[s].is_empty() {
                        continue;
                    }
                    let name = w.names[s][usize::from(*i) % w.names[s].len()].clone();
                    w.unlink(s, &name);
                }
                Op::Remove(s) => {
                    if w.snaps.len() == 1 {
                        continue;
                    }
                    let s = usize::from(*s) % w.snaps.len();
                    let id = w.snaps[s].id().0;
                    w.f.meta.remove_snapshot(cowfs_meta::SnapshotId(id)).unwrap();
                    w.f.meta.reap_all().unwrap();
                    w.model.remove(id);
                    w.snaps.remove(s);
                    w.names.remove(s);
                }
                Op::Collect => {
                    let r = w.f.gc.collect(Some(&*roots)).unwrap();
                    prop_assert!(r.errors.is_empty(), "{here}: {:?}", r.errors);
                    prop_assert!(!w.f.store.recovery().has_corruption(), "{here}: corruption");
                }
                Op::Reopen => {
                    let live = w.model.live();
                    let bytes: HashMap<BlockId, Vec<u8>> = live
                        .iter()
                        .map(|b| (*b, w.model.bytes[b].clone()))
                        .collect();
                    w = w.reopen();
                    for (b, want) in bytes {
                        match w.f.store.get(b) {
                            Ok(got) => prop_assert!(
                                got == want,
                                "{here}: block {} after reopen has the wrong bytes",
                                &b.to_string()[..8]
                            ),
                            Err(e) => prop_assert!(false, "{here}: block {} lost on reopen: {e}", &b.to_string()[..8]),
                        }
                    }
                }
            }
            w.check(&here);
        }
        let r = w.f.gc.collect(Some(&*roots)).unwrap();
        prop_assert!(r.errors.is_empty(), "final collect: {:?}", r.errors);
        w.check("final");
        // A second collect after everything is dead must still find the live set intact.
        let r2 = w.f.gc.collect(Some(&*roots)).unwrap();
        prop_assert!(r2.errors.is_empty(), "second collect: {:?}", r2.errors);
        w.check("after a second collect");
    }
}
