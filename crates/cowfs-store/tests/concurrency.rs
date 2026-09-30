mod common;

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::thread;

use common::{compressible, opts, random};
use cowfs_store::{BlockId, Options, Store};

const THREADS: usize = 8;
const POOL: usize = 300;

fn block(i: usize) -> Vec<u8> {
    let len = 1000 + (i * 7919) % 40_000;
    if i.is_multiple_of(4) {
        compressible(i as u64, len)
    } else {
        random(i as u64, len)
    }
}

#[test]
fn many_threads_put_overlapping_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let pool: Vec<Vec<u8>> = (0..POOL).map(block).collect();
    let done = AtomicBool::new(false);
    {
        let s = Store::open(
            dir.path(),
            Options {
                max_pack_size: 1 << 20,
                checkpoint_on_drop: false,
            },
        )
        .unwrap();
        thread::scope(|scope| {
            let workers: Vec<_> = (0..THREADS)
                .map(|t| {
                    let (s, pool) = (&s, &pool);
                    scope.spawn(move || {
                        for k in 0..POOL {
                            let d = &pool[(k + t * 37) % POOL];
                            let a = s.put(d).unwrap();
                            let b = s.put(d).unwrap();
                            assert_eq!(a, b);
                            assert_eq!(&s.get(a).unwrap(), d);
                            if t == 0 && k % 25 == 0 {
                                s.sync().unwrap();
                            }
                            if t == 1 && k % 60 == 0 {
                                s.checkpoint().unwrap();
                            }
                        }
                    })
                })
                .collect();
            let watcher = scope.spawn(|| {
                while !done.load(Relaxed) {
                    let _ = s.stats();
                    let ids: Vec<_> = s.iter_ids().take(20).collect();
                    for id in ids {
                        assert_eq!(BlockId::of(&s.get(id).unwrap()), id);
                    }
                    assert!(s.fsck().unwrap().is_clean());
                }
            });
            for w in workers {
                w.join().unwrap();
            }
            done.store(true, Relaxed);
            watcher.join().unwrap();
        });

        let st = s.stats();
        assert_eq!(st.blocks, POOL as u64);
        assert_eq!(st.put_calls, (THREADS * POOL * 2) as u64);
        assert_eq!(st.dedup_hits, st.put_calls - POOL as u64);
        let unique: u64 = pool.iter().map(|d| d.len() as u64).sum();
        assert_eq!(st.uncompressed_bytes, unique);
        let r = s.fsck().unwrap();
        assert!(r.is_clean());
        assert_eq!(r.records, POOL as u64);
        assert_eq!(r.duplicate_records, 0);
        s.sync().unwrap();
    }
    let s = Store::open(dir.path(), opts()).unwrap();
    for d in &pool {
        assert_eq!(&s.get(BlockId::of(d)).unwrap(), d);
    }
}

#[test]
fn many_threads_ingest_the_same_file() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), opts()).unwrap();
    let data = random(5, 6 << 20);
    let lists: Vec<_> = thread::scope(|scope| {
        let hs: Vec<_> = (0..THREADS)
            .map(|_| scope.spawn(|| s.ingest_bytes(&data).unwrap()))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(lists.windows(2).all(|w| w[0] == w[1]));
    let st = s.stats();
    assert_eq!(st.blocks, lists[0].len() as u64);
    assert_eq!(st.uncompressed_bytes, data.len() as u64);
    assert_eq!(s.fsck().unwrap().duplicate_records, 0);
}
