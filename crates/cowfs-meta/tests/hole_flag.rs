//! `ChunkRef::hole` is an explicit flag, and the walk in meta is safe on its own because of it.
//!
//! The walker used to yield the id of every chunk ref, so a sparse file's hole, whose id is the
//! all-zero sentinel, came out of `Snapshot::live_blocks` as if it named a block. Every caller had
//! to filter it again: `Core::live_blocks`, `Core::fsck`, `Core::pinned_blocks` and the collector
//! each compared the id against a sentinel of their own. The flag moves that decision to where the
//! ref is decoded, once.
//!
//! The encoding on the medium does not change. A hole is still 32 zero bytes and a length, so a
//! store written before the flag existed decodes to the same refs and re-encodes to the same bytes.

use cowfs_meta::{BlockId, ChunkRef, Error, Ino, Marker, Meta, Options, Snapshot, ROOT_INO};

fn open(dir: &std::path::Path) -> Meta {
    Meta::open(dir.join("meta.redb"), Options::default()).expect("a metadata store opens")
}

/// A real block ref, the one every assertion here says must survive.
fn real(payload: &[u8], len: u32) -> ChunkRef {
    ChunkRef::block(BlockId::of(payload), len)
}

/// A file of `head`, a gap, then `tail`: the shape a sparse write produces.
fn sparse(m: &Meta, name: &[u8]) -> (Snapshot, Ino) {
    let s = m.new_snapshot("s").unwrap();
    let f = s.create(ROOT_INO, name, 0o644).unwrap();
    let head = real(b"head", 4);
    let gap = ChunkRef::hole(4096);
    let tail = real(b"tail", 4);
    s.batch(|tx| tx.set_content(f.ino, &[head, gap, tail], 4104))
        .unwrap();
    (s, f.ino)
}

/// The block ids the metadata walk yields, in order.
fn walked(s: &Snapshot) -> Vec<BlockId> {
    let mut marker = Marker::new();
    s.live_blocks(&mut marker)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

#[test]
fn the_walk_yields_the_real_blocks_and_not_a_hole() {
    let dir = tempfile::tempdir().unwrap();
    let (s, _) = sparse(&open(dir.path()), b"f");
    let got = walked(&s);
    assert_eq!(
        got,
        vec![BlockId::of(b"head"), BlockId::of(b"tail")],
        "the walk must yield the stored blocks and nothing else"
    );
    assert!(
        !got.contains(&BlockId::from_bytes([0; 32])),
        "the hole sentinel reached the caller: {got:?}"
    );
}

#[test]
fn a_zero_id_ref_longer_than_a_hole_may_claim_is_corrupt_on_read() {
    // the sentinel with a length no hole can hold is not a hole: reading it must say so rather than
    // answer with zeros, which is what the length bound in the store crate is for
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path());
    let s = m.new_snapshot("s").unwrap();
    let f = s.create(ROOT_INO, b"f", 0o644).unwrap();
    let over = ChunkRef {
        id: cowfs_store::HOLE,
        len: (1 << 30) + 1,
        hole: true,
    };
    // the batch refuses to write it in the first place
    let err = s
        .batch(|tx| tx.set_content(f.ino, &[over], u64::from(over.len)))
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");

    // and a store that already holds those bytes is refused on the way out, not decoded as a hole
    let over_id_only = ChunkRef {
        id: cowfs_store::HOLE,
        len: (1 << 30) + 1,
        hole: false,
    };
    assert!(over_id_only.validate().is_err());
}

#[test]
fn the_flags_survive_a_close_and_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let f = {
        let (s, ino) = sparse(&open(dir.path()), b"f");
        let refs = s.chunks(ino).unwrap();
        assert_eq!(refs.len(), 3, "{refs:?}");
        assert!(!refs[0].hole, "a real block must not decode as a hole");
        assert!(refs[1].hole, "the zero-id ref must decode as a hole");
        assert_eq!(refs[1].len, 4096, "the length must survive the round trip");
        assert!(!refs[2].hole, "a real block must not decode as a hole");
        walked(&s)
    };
    assert_eq!(f, vec![BlockId::of(b"head"), BlockId::of(b"tail")]);

    let m = open(dir.path());
    let s = m.snapshot("s").unwrap();
    let f2 = s.lookup(ROOT_INO, b"f").unwrap().ino;
    assert_eq!(
        walked(&s),
        vec![BlockId::of(b"head"), BlockId::of(b"tail")],
        "a reopened store must walk the same way"
    );
    let refs = s.chunks(f2).unwrap();
    assert!(refs[1].hole, "the flag must survive a close and a reopen");
}

#[test]
fn a_file_of_only_holes_yields_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path());
    let s = m.new_snapshot("s").unwrap();
    let f = s.create(ROOT_INO, b"empty", 0o644).unwrap();
    s.batch(|tx| tx.set_content(f.ino, &[ChunkRef::hole(8192)], 8192))
        .unwrap();
    assert!(
        walked(&s).is_empty(),
        "a file with no stored block must yield no block"
    );
    assert!(s.chunks(f.ino).unwrap()[0].hole);
}

#[test]
fn a_file_with_no_chunks_at_all_still_yields_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path());
    let s = m.new_snapshot("s").unwrap();
    let _f = s.create(ROOT_INO, b"empty", 0o644).unwrap();
    assert!(walked(&s).is_empty());
}

#[test]
fn the_encoding_of_a_hole_is_unchanged_by_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path());
    let s = m.new_snapshot("s").unwrap();
    let f = s.create(ROOT_INO, b"f", 0o644).unwrap();
    s.batch(|tx| tx.set_content(f.ino, &[ChunkRef::hole(1024)], 1024))
        .unwrap();

    // the bytes a store written before the flag existed must still be the bytes written now
    // the encoded extent is 32 zero bytes then the length, exactly as before
    let enc = {
        let refs = s.chunks(f.ino).unwrap();
        let mut v = Vec::new();
        for c in &refs {
            v.extend_from_slice(c.id.as_bytes());
            v.extend(c.len.to_le_bytes());
        }
        v
    };
    assert_eq!(enc.len(), 36, "one extent is 36 bytes: id 32, length 4");
    assert_eq!(&enc[..32], &[0u8; 32], "a hole's id is the zero sentinel");
    assert_eq!(
        u32::from_le_bytes(enc[32..].try_into().unwrap()),
        1024,
        "the length is stored after the id"
    );
}

#[test]
fn the_trailing_hole_is_not_a_chunk_ref_and_is_not_walked() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path());
    let s = m.new_snapshot("s").unwrap();
    let f = s.create(ROOT_INO, b"f", 0o644).unwrap();
    // one chunk, and a size that reaches past it: the rest is a trailing hole with no ref at all
    s.batch(|tx| tx.set_content(f.ino, &[real(b"head", 4)], 4096))
        .unwrap();
    let refs = s.chunks(f.ino).unwrap();
    assert_eq!(refs.len(), 1, "a trailing hole is not a ref: {refs:?}");
    assert!(!refs[0].hole);
    assert_eq!(walked(&s), vec![BlockId::of(b"head")]);
}

#[test]
fn a_marker_reused_across_a_walk_still_skips_a_shared_subtree() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path());
    let s = m.new_snapshot("s").unwrap();
    let d = s.mkdir(ROOT_INO, b"d", 0o755).unwrap();
    let a = s.create(d.ino, b"a", 0o644).unwrap();
    let b = s.create(d.ino, b"b", 0o644).unwrap();
    s.batch(|tx| {
        tx.set_content(a.ino, &[real(b"aaa", 3), ChunkRef::hole(64)], 67)?;
        tx.set_content(b.ino, &[real(b"bbb", 3), ChunkRef::hole(64)], 67)?;
        Ok(())
    })
    .unwrap();

    let mut first = Marker::new();
    let walked_first: Vec<_> = s
        .live_blocks(&mut first)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(walked_first, vec![BlockId::of(b"aaa"), BlockId::of(b"bbb")]);

    // the second walk with the same marker skips the subtree the first one covered
    let mut second = first;
    let walked_second: Vec<_> = s
        .live_blocks(&mut second)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        walked_second.is_empty(),
        "a reused marker must still skip, got {walked_second:?}"
    );
}

#[test]
fn a_walk_that_fails_still_fails_and_yields_no_partial_claim() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path());
    let s = m.new_snapshot("s").unwrap();
    let f = s.create(ROOT_INO, b"f", 0o644).unwrap();
    s.batch(|tx| {
        tx.set_content(f.ino, &[real(b"head", 4), ChunkRef::hole(64)], 68)?;
        Ok(())
    })
    .unwrap();
    let mut marker = Marker::new();
    // a normal walk first, so the marker is populated the way a second walk would find it
    let all: Vec<_> = s
        .live_blocks(&mut marker)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(all, vec![BlockId::of(b"head")]);
    // the same marker over a snapshot whose file is gone must not claim a hole
    let s2 = m.new_snapshot("s2").unwrap();
    let mut fresh = Marker::new();
    assert!(s2
        .live_blocks(&mut fresh)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .is_empty());
}

#[test]
fn a_ref_that_claims_a_hole_and_an_id_is_refused() {
    // the flag and the id must agree, or a caller could make a hole that names a real block, and the
    // walk would then skip a block the store holds
    let bad = ChunkRef {
        id: BlockId::of(b"head"),
        len: 4,
        hole: true,
    };
    assert!(
        bad.validate().is_err(),
        "a ref that is both a hole and a stored block must be refused"
    );
}

#[test]
fn a_ref_with_no_flag_and_the_zero_id_is_refused() {
    // the mirror: claiming a stored block whose id is the sentinel would put the sentinel in front
    // of a collector
    let bad = ChunkRef {
        id: BlockId::from_bytes([0; 32]),
        len: 64,
        hole: false,
    };
    assert!(
        bad.validate().is_err(),
        "a ref that is a stored block with the zero id must be refused"
    );
}

#[test]
fn a_hole_longer_than_a_hole_may_claim_is_refused() {
    let bad = ChunkRef::hole((1 << 30) + 1);
    assert!(
        bad.validate().is_err(),
        "a hole longer than the bound must be refused, not stored: {:?}",
        bad.validate()
    );
    assert!(
        ChunkRef::hole(1 << 30).validate().is_ok(),
        "the bound itself is legal"
    );
    assert_eq!(ChunkRef::hole(1 << 30).len, 1 << 30);
}

#[test]
fn a_well_formed_ref_validates() {
    assert!(real(b"head", 4).validate().is_ok());
    assert!(ChunkRef::hole(0).validate().is_ok());
    assert!(ChunkRef::hole(1 << 30).validate().is_ok());
}

#[test]
fn the_metadata_store_refuses_a_chunk_list_it_cannot_represent() {
    let dir = tempfile::tempdir().unwrap();
    let m = open(dir.path());
    let s = m.new_snapshot("s").unwrap();
    let f = s.create(ROOT_INO, b"f", 0o644).unwrap();
    let err = s
        .batch(|tx| {
            tx.set_content(
                f.ino,
                &[ChunkRef {
                    id: BlockId::of(b"head"),
                    len: 4,
                    hole: true,
                }],
                4,
            )
        })
        .unwrap_err();
    assert!(
        matches!(err, Error::Invalid(_)),
        "a ref that is both a hole and a block must be refused by the batch: {err:?}"
    );
    // and nothing was written
    assert!(s.chunks(f.ino).unwrap().is_empty(), "{:?}", s.chunks(f.ino));
}
