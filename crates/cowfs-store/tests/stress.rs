//! Eight threads hitting one store for `C7C_SECS` seconds (20 by default, 180 for a long run):
//! puts, gets, syncs, checkpoints, fsck, salvage and acknowledgements at once.
mod common;
use std::collections::HashMap;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use common::{compressible, random};
use cowfs_store::{BlockId, Options, Store};

#[test]
fn stress() {
    let secs: u64 = std::env::var("C7C_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let dir = tempfile::tempdir().unwrap();
    let o = Options {
        max_pack_size: 50_000,
        max_open_packs: 2,
        checkpoint_on_drop: true,
        ..Options::default()
    };
    let model: Mutex<HashMap<BlockId, Vec<u8>>> = Mutex::new(HashMap::new());
    let errors = AtomicU64::new(0);
    let ops = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    {
        let s = Store::open(dir.path(), o).unwrap();
        let start = Instant::now();
        std::thread::scope(|sc| {
            for t in 0..8u64 {
                let (s, model, errors, ops, stop) = (&s, &model, &errors, &ops, &stop);
                sc.spawn(move || {
                    let mut x = 0x9E37_79B9u64.wrapping_mul(t + 1);
                    let mut next = || {
                        x ^= x << 13;
                        x ^= x >> 7;
                        x ^= x << 17;
                        x
                    };
                    while !stop.load(Relaxed) {
                        ops.fetch_add(1, Relaxed);
                        match next() % 100 {
                            0..=44 => {
                                let seed = next() % 400;
                                let len = 300 + (next() % 12_000) as usize;
                                let d = if seed % 3 == 0 {
                                    compressible(seed, len)
                                } else {
                                    random(seed, len)
                                };
                                match s.put(&d) {
                                    Ok(id) => {
                                        model.lock().unwrap().insert(id, d);
                                    }
                                    Err(e) => {
                                        errors.fetch_add(1, Relaxed);
                                        eprintln!("put err {e:?}");
                                    }
                                }
                            }
                            45..=79 => {
                                let pick = {
                                    let m = model.lock().unwrap();
                                    if m.is_empty() {
                                        None
                                    } else {
                                        let i = (next() as usize) % m.len();
                                        m.iter().nth(i).map(|(k, v)| (*k, v.clone()))
                                    }
                                };
                                if let Some((id, d)) = pick {
                                    match s.get(id) {
                                        Ok(g) if g == d => {}
                                        r => {
                                            errors.fetch_add(1, Relaxed);
                                            eprintln!("get mismatch/err {:?}", r.map(|v| v.len()));
                                        }
                                    }
                                }
                            }
                            80..=86 => {
                                if let Err(e) = s.sync() {
                                    errors.fetch_add(1, Relaxed);
                                    eprintln!("sync {e:?}");
                                }
                            }
                            87..=90 => {
                                if let Err(e) = s.checkpoint() {
                                    errors.fetch_add(1, Relaxed);
                                    eprintln!("ckpt {e:?}");
                                }
                            }
                            91..=93 => match s.verify_all() {
                                Ok(f) if f.is_clean() => {}
                                Ok(f) => {
                                    errors.fetch_add(1, Relaxed);
                                    eprintln!("fsck dirty {:?}", f.damage.len());
                                }
                                Err(e) => {
                                    errors.fetch_add(1, Relaxed);
                                    eprintln!("fsck err {e:?}");
                                }
                            },
                            94..=96 => {
                                if let Err(e) = s.salvage() {
                                    errors.fetch_add(1, Relaxed);
                                    eprintln!("salvage {e:?}");
                                }
                            }
                            97 => {
                                if let Err(e) = s.acknowledge_corruption() {
                                    errors.fetch_add(1, Relaxed);
                                    eprintln!("ack {e:?}");
                                }
                            }
                            _ => {
                                let st = s.stats();
                                if st.pack_bytes < st.stored_bytes {
                                    errors.fetch_add(1, Relaxed);
                                    eprintln!("stats pack_bytes<stored {st:?}");
                                }
                            }
                        }
                    }
                });
            }
            while start.elapsed() < Duration::from_secs(secs) {
                std::thread::sleep(Duration::from_millis(200));
            }
            stop.store(true, Relaxed);
        });
        s.sync().unwrap();
        let st = s.stats();
        let m = model.lock().unwrap();
        let full: usize = s.iter_ids().count();
        let disk: u64 = fs::read_dir(dir.path().join("packs"))
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        let npacks = fs::read_dir(dir.path().join("packs")).unwrap().count() as u64;
        let f = s.fsck().unwrap();
        println!("STRESS ops={} errors={} model={} stats.blocks={} iter_ids={} fsck.verified={} fsck.dupes={} stats.packs={} disk.packs={} stats.pack_bytes={} disk.bytes={} fds_limit_ok", ops.load(Relaxed), errors.load(Relaxed), m.len(), st.blocks, full, f.blocks_verified, f.duplicate_records, st.packs, npacks, st.pack_bytes, disk);
        assert!(f.is_clean());
        for (id, d) in m.iter() {
            assert_eq!(&s.get(*id).unwrap(), d);
        }
    }
    let s = Store::open(dir.path(), o).unwrap();
    let m = model.lock().unwrap();
    for (id, d) in m.iter() {
        assert_eq!(&s.get(*id).unwrap(), d, "after reopen");
    }
    println!(
        "STRESS reopen ok, corruption={} torn={}",
        s.recovery().has_corruption(),
        s.recovery().torn_tail_discarded
    );
    assert_eq!(errors.load(Relaxed), 0);
}
