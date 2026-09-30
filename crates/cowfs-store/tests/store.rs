mod common;

use std::fs;
use std::io::Cursor;

use common::{compressible, copy_store, index_path, opts, pack_ids, pack_path, parse_pack, random};
use cowfs_store::{BlockId, Error, Store, MAX_BLOCK_LEN, MAX_CHUNK_LEN, MIN_CHUNK_LEN};

fn open(dir: &std::path::Path) -> Store {
    Store::open(dir, opts()).unwrap()
}

#[test]
fn put_get_round_trip_at_edge_sizes() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path());
    for len in [
        0,
        1,
        MIN_CHUNK_LEN - 1,
        MIN_CHUNK_LEN,
        MIN_CHUNK_LEN + 1,
        MAX_CHUNK_LEN - 1,
        MAX_CHUNK_LEN,
    ] {
        for data in [random(len as u64, len), compressible(len as u64, len)] {
            let id = s.put(&data).unwrap();
            assert_eq!(id, BlockId::of(&data));
            assert!(s.contains(id));
            assert_eq!(s.get(id).unwrap(), data, "len {len}");
        }
    }
}

#[test]
fn put_rejects_oversized_block() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path());
    let err = s.put(&vec![0u8; MAX_BLOCK_LEN + 1]).unwrap_err();
    assert!(matches!(err, Error::BlockTooLarge(n) if n == MAX_BLOCK_LEN + 1));
}

#[test]
fn get_unknown_id_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path());
    let id = BlockId::of(b"nope");
    assert!(!s.contains(id));
    assert!(matches!(s.get(id), Err(Error::NotFound(x)) if x == id));
}

#[test]
fn ingest_round_trips_multi_mib_and_max_plus_one() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path());
    for len in [0, 1, MAX_CHUNK_LEN + 1, 5 << 20, (9 << 20) + 12_345] {
        let data = if len % 2 == 0 {
            random(len as u64, len)
        } else {
            compressible(len as u64, len)
        };
        let refs = s.ingest_bytes(&data).unwrap();
        let mut joined = Vec::new();
        for r in &refs {
            let b = s.get(r.id).unwrap();
            assert_eq!(b.len(), r.len as usize);
            joined.extend_from_slice(&b);
        }
        assert_eq!(joined, data, "len {len}");
        let streamed = s.ingest(Cursor::new(&data)).unwrap();
        assert_eq!(streamed, refs, "stream and slice ingest agree at {len}");
    }
}

#[test]
fn ingest_reader_with_short_reads() {
    struct Drip<'a>(&'a [u8]);
    impl std::io::Read for Drip<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.0.len()).min(777);
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path());
    let data = random(9, 3 << 20);
    assert_eq!(
        s.ingest(Drip(&data)).unwrap(),
        s.ingest_bytes(&data).unwrap()
    );
}

#[test]
fn small_file_is_one_block() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path());
    let refs = s.ingest_bytes(&random(1, 1000)).unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].len, 1000);
    assert!(s.ingest_bytes(&[]).unwrap().is_empty());
}

#[test]
fn same_data_twice_is_stored_once() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path());
    let data = random(7, 3 << 20);
    let a = s.ingest_bytes(&data).unwrap();
    let after_first = s.stats();
    let b = s.ingest_bytes(&data).unwrap();
    let after_second = s.stats();
    assert_eq!(a, b);
    assert_eq!(after_first.blocks, a.len() as u64);
    assert_eq!(after_second.blocks, after_first.blocks);
    assert_eq!(after_second.pack_bytes, after_first.pack_bytes);
    assert_eq!(after_second.stored_bytes, after_first.stored_bytes);
    assert_eq!(after_second.uncompressed_bytes, data.len() as u64);
    assert_eq!(after_second.put_calls, 2 * a.len() as u64);
    assert_eq!(after_second.put_bytes, 2 * data.len() as u64);
    assert_eq!(after_second.dedup_hits, a.len() as u64);
    assert_eq!(after_second.dedup_bytes, data.len() as u64);
}

#[test]
fn shifted_copy_dedups_most_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path());
    let data = random(21, 8 << 20);
    s.ingest_bytes(&data).unwrap();
    let before = s.stats();
    let mut shifted = vec![1u8, 2, 3];
    shifted.extend_from_slice(&data);
    s.ingest_bytes(&shifted).unwrap();
    let after = s.stats();
    let added = after.uncompressed_bytes - before.uncompressed_bytes;
    assert!(added < (data.len() as u64) / 10, "added {added}");
}

#[test]
fn compressible_blocks_shrink_and_random_blocks_stay_raw() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path());
    let c = compressible(1, MAX_CHUNK_LEN);
    let r = random(1, MAX_CHUNK_LEN);
    s.put(&c).unwrap();
    let after_c = s.stats().stored_bytes;
    assert!(after_c < (MAX_CHUNK_LEN as u64) / 4, "stored {after_c}");
    s.put(&r).unwrap();
    let raw_cost = s.stats().stored_bytes - after_c;
    assert_eq!(raw_cost, 52 + MAX_CHUNK_LEN as u64);
}

#[test]
fn reopen_after_sync_reads_everything() {
    let dir = tempfile::tempdir().unwrap();
    let mut ids = Vec::new();
    {
        let s = open(dir.path());
        for i in 0..50u64 {
            let d = random(i, (i as usize) * 997);
            ids.push((s.put(&d).unwrap(), d));
        }
        s.sync().unwrap();
    }
    let s = open(dir.path());
    assert!(!s.recovery().index_loaded);
    assert_eq!(s.recovery().truncated_bytes, 0);
    for (id, d) in &ids {
        assert_eq!(&s.get(*id).unwrap(), d);
    }
    assert!(s.fsck().unwrap().is_clean());
}

#[test]
fn checkpoint_is_used_and_new_records_after_it_are_scanned() {
    let dir = tempfile::tempdir().unwrap();
    let a = random(1, 5000);
    let b = random(2, 6000);
    let (ia, ib);
    {
        let s = open(dir.path());
        ia = s.put(&a).unwrap();
        s.checkpoint().unwrap();
        ib = s.put(&b).unwrap();
        s.sync().unwrap();
    }
    let s = open(dir.path());
    assert!(s.recovery().index_loaded);
    assert_eq!(s.recovery().records_scanned, 1);
    assert_eq!(s.get(ia).unwrap(), a);
    assert_eq!(s.get(ib).unwrap(), b);
}

#[test]
fn checkpoint_on_drop_writes_index() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), Default::default()).unwrap();
        s.put(b"hello").unwrap();
    }
    assert!(index_path(dir.path()).exists());
    let s = open(dir.path());
    assert!(s.recovery().index_loaded);
    assert_eq!(s.recovery().records_scanned, 0);
    assert_eq!(s.get(BlockId::of(b"hello")).unwrap(), b"hello");
}

#[test]
fn index_loss_and_index_damage_never_lose_data() {
    let dir = tempfile::tempdir().unwrap();
    let mut ids = Vec::new();
    {
        let s = Store::open(
            dir.path(),
            cowfs_store::Options {
                max_pack_size: 200_000,
                ..Default::default()
            },
        )
        .unwrap();
        for i in 0..40u64 {
            let d = random(i, 30_000 + i as usize);
            ids.push((s.put(&d).unwrap(), d));
        }
        s.sync().unwrap();
    }
    assert!(pack_ids(dir.path()).len() > 2);
    let idx = index_path(dir.path());
    let good = fs::read(&idx).unwrap();

    let check = |label: &str| {
        let s = open(dir.path());
        assert!(!s.recovery().index_loaded, "{label}");
        for (id, d) in &ids {
            assert_eq!(&s.get(*id).unwrap(), d, "{label}");
        }
        assert!(s.fsck().unwrap().is_clean(), "{label}");
    };

    fs::remove_file(&idx).unwrap();
    check("deleted");

    fs::write(&idx, &good[..good.len() / 2]).unwrap();
    check("truncated");

    let mut flipped = good.clone();
    flipped[good.len() / 2] ^= 0x10;
    fs::write(&idx, &flipped).unwrap();
    check("bit flipped");

    fs::write(&idx, b"garbage").unwrap();
    check("garbage");
}

#[test]
fn stale_index_pointing_past_a_shorter_pack_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let d = random(1, 40_000);
    let id;
    {
        let s = open(dir.path());
        s.put(&random(0, 1000)).unwrap();
        s.sync().unwrap();
        s.checkpoint().unwrap();
        id = s.put(&d).unwrap();
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    let p = pack_path(dir.path(), 0);
    let len = fs::metadata(&p).unwrap().len();
    fs::OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .set_len(len - 10)
        .unwrap();
    let s = open(dir.path());
    assert!(!s.recovery().index_loaded);
    assert!(!s.contains(id));
    assert!(s.recovery().truncated_bytes > 0);
    s.put(&d).unwrap();
    assert_eq!(s.get(id).unwrap(), d);
}

#[test]
fn packs_roll_over_and_all_blocks_stay_readable() {
    let dir = tempfile::tempdir().unwrap();
    let mut ids = Vec::new();
    {
        let s = Store::open(
            dir.path(),
            cowfs_store::Options {
                max_pack_size: 100_000,
                checkpoint_on_drop: false,
            },
        )
        .unwrap();
        for i in 0..30u64 {
            let d = random(i, 20_000);
            ids.push((s.put(&d).unwrap(), d));
        }
        let st = s.stats();
        assert!(st.packs >= 6, "{st:?}");
        assert_eq!(st.blocks, 30);
        for (id, d) in &ids {
            assert_eq!(&s.get(*id).unwrap(), d);
        }
        s.sync().unwrap();
    }
    for id in pack_ids(dir.path()) {
        let bytes = fs::read(pack_path(dir.path(), id)).unwrap();
        parse_pack(&bytes);
    }
    let s = open(dir.path());
    for (id, d) in &ids {
        assert_eq!(&s.get(*id).unwrap(), d);
    }
    assert_eq!(s.iter_ids().count(), 30);
}

#[test]
fn second_open_of_same_dir_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let _a = open(dir.path());
    assert!(matches!(
        Store::open(dir.path(), opts()),
        Err(Error::Locked(_))
    ));
}

#[test]
fn fsck_counts_records_and_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path());
    for i in 0..10u64 {
        s.put(&random(i, 1000)).unwrap();
    }
    s.sync().unwrap();
    let r = s.fsck().unwrap();
    assert!(r.is_clean());
    assert_eq!(r.records, 10);
    assert_eq!(r.blocks_verified, 10);
    assert_eq!(r.duplicate_records, 0);
    assert_eq!(r.packs, 1);
}

#[test]
fn copy_of_live_store_files_opens_read_only_view() {
    let dir = tempfile::tempdir().unwrap();
    let s = open(dir.path());
    let id = s.put(&random(1, 5000)).unwrap();
    s.sync().unwrap();
    let copy = tempfile::tempdir().unwrap();
    copy_store(dir.path(), copy.path());
    let t = open(copy.path());
    assert_eq!(t.get(id).unwrap(), random(1, 5000));
}
