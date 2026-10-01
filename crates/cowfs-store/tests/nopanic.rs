mod common;

use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use common::{
    fixture, index_path, install, opts, pack_path, random, Fixture, PACK_HEADER, REC_HDR,
};
use cowfs_store::{BlockId, Store};
use proptest::prelude::*;

fn base() -> &'static Fixture {
    static FX: OnceLock<Fixture> = OnceLock::new();
    FX.get_or_init(|| fixture(&[(0, false), (40, false), (90, true), (300, true)], 1 << 20))
}

/// Open whatever is in `dir`. It may fail, but it must not panic, serve wrong data, or refuse writes once open.
fn exercise(dir: &Path) {
    let Ok(s) = Store::open_unsynced(dir, opts()) else {
        return;
    };
    let ids: Vec<_> = s.iter_ids().collect();
    for id in ids {
        if let Ok(d) = s.get(id) {
            assert_eq!(BlockId::of(&d), id);
        }
    }
    let _ = s.fsck();
    let _ = s.stats();
    let d = random(5, 500);
    let id = s.put(&d).unwrap();
    assert_eq!(s.get(id).unwrap(), d);
    s.sync().unwrap();
}

fn record(codec: u8, ulen: u32, id: [u8; 32], payload: &[u8]) -> Vec<u8> {
    let mut r = b"CWRB".to_vec();
    r.extend_from_slice(&[codec, 0, 0, 0]);
    r.extend_from_slice(&ulen.to_le_bytes());
    r.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    r.extend_from_slice(&id);
    let crc = crc32c::crc32c_append(crc32c::crc32c(&r), payload);
    r.extend_from_slice(&crc.to_le_bytes());
    r.extend_from_slice(payload);
    r
}

fn index_bytes(packs: &[(u32, u64)], entries: &[(BlockId, [u32; 4])]) -> Vec<u8> {
    let mut b = b"COWIDX01".to_vec();
    b.extend_from_slice(&(packs.len() as u32).to_le_bytes());
    b.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for (id, len) in packs {
        b.extend_from_slice(&id.to_le_bytes());
        b.extend_from_slice(&len.to_le_bytes());
    }
    for (id, loc) in entries {
        b.extend_from_slice(id.as_bytes());
        for v in loc {
            b.extend_from_slice(&v.to_le_bytes());
        }
    }
    let crc = crc32c::crc32c(&b);
    b.extend_from_slice(&crc.to_le_bytes());
    b
}

fn crafted_record() -> impl Strategy<Value = Vec<u8>> {
    (
        0u8..4,
        prop_oneof![
            0u32..400,
            Just(262_144u32),
            Just(262_145u32),
            Just(u32::MAX)
        ],
        any::<[u8; 32]>(),
        prop::collection::vec(any::<u8>(), 0..400),
    )
        .prop_map(|(codec, ulen, id, payload)| record(codec, ulen, id, &payload))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn arbitrary_bytes_as_a_pack(bytes in prop::collection::vec(any::<u8>(), 0..3000)) {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &[(0, &bytes)], None);
        exercise(dir.path());
    }

    #[test]
    fn arbitrary_bytes_after_a_valid_pack_header(bytes in prop::collection::vec(any::<u8>(), 0..3000)) {
        let dir = tempfile::tempdir().unwrap();
        let mut b = PACK_HEADER.to_vec();
        b.extend_from_slice(&bytes);
        install(dir.path(), &[(0, &b)], None);
        exercise(dir.path());
    }

    #[test]
    fn crafted_records_with_valid_checksums(
        recs in prop::collection::vec(crafted_record(), 0..6),
        tail in prop::collection::vec(any::<u8>(), 0..100),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut b = PACK_HEADER.to_vec();
        for r in &recs {
            b.extend_from_slice(r);
        }
        b.extend_from_slice(&tail);
        install(dir.path(), &[(0, &b)], None);
        exercise(dir.path());
    }

    #[test]
    fn mutated_valid_pack(
        edits in prop::collection::vec((any::<usize>(), any::<u8>()), 0..8),
        cut in any::<usize>(),
        insert in prop::collection::vec(any::<u8>(), 0..40),
        at in any::<usize>(),
    ) {
        let mut b = base().packs[0].clone();
        for (pos, v) in edits {
            let n = b.len();
            b[pos % n] = v;
        }
        let at = at % (b.len() + 1);
        b.splice(at..at, insert);
        b.truncate(cut % (b.len() + 1));
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &[(0, &b)], None);
        exercise(dir.path());
    }

    #[test]
    fn arbitrary_bytes_as_an_index(bytes in prop::collection::vec(any::<u8>(), 0..400)) {
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &[(0, &base().packs[0])], Some(&bytes));
        exercise(dir.path());
    }

    #[test]
    fn crafted_index_with_a_valid_checksum(
        locs in prop::collection::vec((any::<[u8; 32]>(), any::<u32>(), any::<u32>(), any::<u32>()), 0..10),
        claimed_len in any::<u64>(),
    ) {
        let plen = base().packs[0].len() as u64;
        let entries: Vec<_> = locs
            .into_iter()
            .map(|(id, a, b, ulen)| {
                let offset = PACK_HEADER.len() as u32 + a % (plen as u32 - REC_HDR as u32 - PACK_HEADER.len() as u32);
                let slen = b % (plen as u32 - offset - REC_HDR as u32 + 1);
                (BlockId::from_bytes(id), [0, offset, slen, ulen % 300])
            })
            .collect();
        let lens = if claimed_len % 4 == 0 { claimed_len } else { plen };
        let ix = index_bytes(&[(0, lens)], &entries);
        let dir = tempfile::tempdir().unwrap();
        install(dir.path(), &[(0, &base().packs[0])], Some(&ix));
        exercise(dir.path());
    }
}

#[test]
fn index_pointing_at_a_missing_pack_or_a_directory_is_ignored() {
    let fx = base();
    let dir = tempfile::tempdir().unwrap();
    install(dir.path(), &[(0, &fx.packs[0])], None);
    fs::create_dir(index_path(dir.path())).unwrap();
    let s = Store::open_unsynced(dir.path(), opts()).unwrap();
    assert!(!s.recovery().index_loaded);
    for (id, d) in &fx.blocks {
        assert_eq!(&s.get(*id).unwrap(), d);
    }
    drop(s);
    fs::remove_dir(index_path(dir.path())).unwrap();

    let ix = index_bytes(&[(7, 100)], &[]);
    install(dir.path(), &[(0, &fx.packs[0])], Some(&ix));
    let s = Store::open_unsynced(dir.path(), opts()).unwrap();
    assert!(!s.recovery().index_loaded);
    assert_eq!(s.iter_ids().count(), fx.blocks.len());
}

#[test]
fn stray_files_in_the_store_are_ignored() {
    let fx = base();
    let dir = tempfile::tempdir().unwrap();
    install(dir.path(), &[(0, &fx.packs[0])], None);
    fs::write(dir.path().join("packs").join("notes.txt"), b"hi").unwrap();
    fs::write(pack_path(dir.path(), 0).with_extension("tmp"), b"x").unwrap();
    let s = Store::open_unsynced(dir.path(), opts()).unwrap();
    assert_eq!(s.iter_ids().count(), fx.blocks.len());
}
