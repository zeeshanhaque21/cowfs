mod common;

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Arc, Mutex};

use common::{compressible, index_bytes, install_wm, opts, pack_path, random, record, PACK_HEADER};
use cowfs_store::{BlockId, Error, Op, Options, Store, Trace};

const P0: &str = "pack-00000000.cpk";
const P1: &str = "pack-00000001.cpk";

fn trace() -> Trace {
    Arc::new(Mutex::new(Vec::new()))
}

fn take(t: &Trace) -> Vec<Op> {
    std::mem::take(&mut *t.lock().unwrap())
}

fn sync(n: &str) -> Op {
    Op::Sync(n.into())
}

fn dir_sync(n: &str) -> Op {
    Op::DirSync(n.into())
}

fn create(n: &str) -> Op {
    Op::Create(n.into())
}

fn name(p: &std::path::Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}

fn assert_in_order(ops: &[Op], want: &[Op]) {
    assert_in_order_msg(ops, want, "wrong op order")
}

fn assert_in_order_msg(ops: &[Op], want: &[Op], msg: &str) {
    let mut it = ops.iter();
    for w in want {
        assert!(
            it.any(|o| o == w),
            "{msg}: {w:?} missing or out of order in {ops:#?}"
        );
    }
}

#[test]
fn a_fresh_store_syncs_every_new_directory_entry_and_file_before_use() {
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("store");
    let t = trace();
    let _s = Store::open_traced(&dir, opts(), Arc::clone(&t)).unwrap();
    let ops = take(&t);
    assert_in_order(
        &ops,
        &[
            create("store"),
            dir_sync("store"),
            dir_sync(&name(base.path())),
            create("packs"),
            dir_sync("packs"),
            dir_sync("store"),
            create("LOCK"),
            dir_sync("store"),
            create("SYNCED"),
            sync("SYNCED"),
            dir_sync("store"),
            create(P0),
            sync(P0),
            dir_sync("packs"),
            sync("SYNCED"),
        ],
    );
}

#[test]
fn sync_makes_data_durable_before_the_watermark_and_is_free_when_idle() {
    let dir = tempfile::tempdir().unwrap();
    let t = trace();
    let s = Store::open_traced(dir.path(), opts(), Arc::clone(&t)).unwrap();
    take(&t);
    s.put(b"hello").unwrap();
    assert_eq!(take(&t), vec![], "put must not sync");
    s.sync().unwrap();
    assert_eq!(take(&t), vec![sync(P0), sync("SYNCED")]);
    s.sync().unwrap();
    assert_eq!(take(&t), vec![], "nothing new to sync");
}

#[test]
fn rolling_syncs_the_sealed_pack_then_creates_and_syncs_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let t = trace();
    let o = Options {
        max_pack_size: 200,
        ..opts()
    };
    let s = Store::open_traced(dir.path(), o, Arc::clone(&t)).unwrap();
    take(&t);
    s.put(&random(1, 100)).unwrap();
    assert_eq!(take(&t), vec![]);
    s.put(&random(2, 100)).unwrap();
    assert_eq!(
        take(&t),
        vec![sync(P0), create(P1), sync(P1), dir_sync("packs")]
    );
    s.sync().unwrap();
    assert_eq!(take(&t), vec![sync(P1), sync("SYNCED")]);
}

#[test]
fn checkpoint_orders_pack_watermark_index_rename_dir() {
    let dir = tempfile::tempdir().unwrap();
    let t = trace();
    let s = Store::open_traced(dir.path(), opts(), Arc::clone(&t)).unwrap();
    s.put(b"hello").unwrap();
    take(&t);
    s.checkpoint().unwrap();
    let ops = take(&t);
    assert_in_order(
        &ops,
        &[
            sync(P0),
            sync("SYNCED"),
            create("index.cix.tmp"),
            sync("index.cix.tmp"),
            Op::Rename("index.cix.tmp".into(), "index.cix".into()),
            dir_sync(&name(dir.path())),
        ],
    );
    assert_eq!(ops.last(), Some(&dir_sync(&name(dir.path()))), "{ops:?}");
}

#[test]
fn recovery_syncs_what_it_scanned_and_what_it_truncated() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        s.put(b"hello").unwrap();
        s.sync().unwrap();
    }
    let t = trace();
    drop(Store::open_traced(dir.path(), opts(), Arc::clone(&t)).unwrap());
    assert_in_order_msg(
        &take(&t),
        &[sync(P0), sync("SYNCED")],
        "a rebuild syncs the pack it read, then the watermark it advanced",
    );

    OpenOptions::new()
        .append(true)
        .open(pack_path(dir.path(), 0))
        .unwrap()
        .write_all(&[7u8; 30])
        .unwrap();
    let t2 = Arc::clone(&t);
    let s = Store::open_traced(dir.path(), opts(), t2).unwrap();
    // The tail is copied to a sidecar and the pack is labelled before it is cut, so a cut that is
    // interrupted is recognizable next time. The watermark itself does not move: the cut lands
    // exactly on it.
    assert_in_order(
        &take(&t),
        &[
            create("pack-00000000.cpk.torn-0"),
            sync("pack-00000000.cpk.torn-0"),
            Op::DirSync("packs".into()),
            create("pack-00000000.cpk.cut.tmp"),
            sync("pack-00000000.cpk.cut.tmp"),
            Op::Rename(
                "pack-00000000.cpk.cut.tmp".into(),
                "pack-00000000.cpk.cut".into(),
            ),
            Op::DirSync("packs".into()),
            Op::Truncate(P0.into()),
            sync(P0),
        ],
    );
    assert_eq!(s.recovery().truncated_bytes, 30);
    assert_eq!(s.recovery().torn_sidecars.len(), 1);
}

#[test]
fn a_damaged_last_pack_is_never_appended_to() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        s.put(&random(1, 500)).unwrap();
        s.sync().unwrap();
    }
    let mut bytes = std::fs::read(pack_path(dir.path(), 0)).unwrap();
    let n = bytes.len();
    bytes[n - 5] ^= 1;
    std::fs::write(pack_path(dir.path(), 0), &bytes).unwrap();
    let t = trace();
    let s = Store::open_traced(dir.path(), opts(), Arc::clone(&t)).unwrap();
    assert!(s.recovery().has_corruption());
    assert_in_order(&take(&t), &[create(P1), sync(P1), dir_sync("packs")]);
    s.put(b"more").unwrap();
    assert_eq!(std::fs::read(pack_path(dir.path(), 0)).unwrap(), bytes);
}

/// A record whose checksum is valid but whose data does not hash to its id can only be caught by the hash.
fn wrong_hash_store(compressed: bool) -> (tempfile::TempDir, BlockId, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let real = if compressed {
        compressible(2, 4000)
    } else {
        random(2, 1000)
    };
    let id = BlockId::of(&real);
    let stored = if compressed {
        compressible(3, 4000)
    } else {
        random(3, 1000)
    };
    let (codec, payload) = if compressed {
        (1u8, zstd::bulk::compress(&stored, 3).unwrap())
    } else {
        (0u8, stored)
    };
    let ulen = if compressed { 4000 } else { 1000 };
    let rec = record(codec, ulen, *id.as_bytes(), &payload);
    let mut pack = PACK_HEADER.to_vec();
    pack.extend_from_slice(&rec);
    let ix = index_bytes(
        &[(0, pack.len() as u64)],
        &[(
            id,
            [0, PACK_HEADER.len() as u32, payload.len() as u32, ulen],
        )],
    );
    install_wm(
        dir.path(),
        &[(0, &pack)],
        Some(&ix),
        Some((0, pack.len() as u64)),
    );
    (dir, id, real)
}

#[test]
fn get_checks_the_hash_even_when_the_checksum_is_valid() {
    for compressed in [false, true] {
        let (dir, id, real) = wrong_hash_store(compressed);
        let s = Store::open_unsynced(dir.path(), opts()).unwrap();
        assert!(s.recovery().index_loaded);
        assert!(
            matches!(s.get(id), Err(Error::HashMismatch(x)) if x == id),
            "compressed {compressed}"
        );
        assert_eq!(s.put(&real).unwrap(), id, "put repairs by rewriting");
        assert_eq!(s.get(id).unwrap(), real);
    }
}

#[test]
fn rebuild_quarantines_a_checksum_valid_record_with_the_wrong_hash() {
    let (dir, id, real) = wrong_hash_store(false);
    std::fs::remove_file(dir.path().join("index.cix")).unwrap();
    let s = Store::open_unsynced(dir.path(), opts()).unwrap();
    assert!(!s.contains(id));
    assert_eq!(s.recovery().gaps.len(), 1);
    assert_eq!(s.put(&real).unwrap(), id);
    assert_eq!(s.get(id).unwrap(), real);
}

#[test]
fn a_record_with_nonzero_padding_and_a_valid_checksum_is_quarantined() {
    let dir = tempfile::tempdir().unwrap();
    let d = random(9, 300);
    let id = BlockId::of(&d);
    let mut rec = record(0, 300, *id.as_bytes(), &d);
    rec[5] = 1;
    let hcrc = crc32c::crc32c(&rec[..48]);
    rec[48..52].copy_from_slice(&hcrc.to_le_bytes());
    let rcrc = crc32c::crc32c_append(hcrc, &d);
    rec[52..56].copy_from_slice(&rcrc.to_le_bytes());
    let mut pack = PACK_HEADER.to_vec();
    pack.extend_from_slice(&rec);
    install_wm(
        dir.path(),
        &[(0, &pack)],
        None,
        Some((0, pack.len() as u64)),
    );
    let s = Store::open_unsynced(dir.path(), opts()).unwrap();
    assert!(!s.contains(id));
    assert!(s.recovery().has_corruption());
}
