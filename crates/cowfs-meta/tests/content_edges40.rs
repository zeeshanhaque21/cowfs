use cowfs_meta::{BlockId, ChunkRef, Error, Meta, Options, ROOT_INO};

#[test]
fn an_empty_splice_inside_a_chunk_is_refused_without_changing_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("meta.redb");
    let m = Meta::open(&path, Options::default()).unwrap();
    let s = m.new_snapshot("s").unwrap();
    let f = s.create(ROOT_INO, b"f", 0o644).unwrap();
    let chunk = ChunkRef::block(BlockId::of(b"abcdefgh"), 8);
    s.batch(|tx| tx.set_content(f.ino, &[chunk], 8)).unwrap();
    let version = s.content_version(f.ino).unwrap();
    let attrs = s.getattr(f.ino).unwrap();
    let result = s.batch(|tx| tx.splice_content(f.ino, version, 3, 3, &[], 8));
    assert!(
        matches!(result, Err(Error::Invalid(_))),
        "an empty splice inside a chunk must be refused: {result:?}"
    );
    assert_eq!(s.content_version(f.ino).unwrap(), version);
    assert_eq!(s.getattr(f.ino).unwrap(), attrs);
    assert_eq!(s.chunks(f.ino).unwrap(), vec![chunk]);
    m.check().unwrap();
    drop(s);
    m.close().unwrap();
    drop(m);
    let m = Meta::open(&path, Options::default()).unwrap();
    let s = m.snapshot("s").unwrap();
    assert_eq!(s.content_version(f.ino).unwrap(), version);
    assert_eq!(s.chunks(f.ino).unwrap(), vec![chunk]);
    s.batch(|tx| tx.splice_content(f.ino, version, 8, 8, &[], 8))
        .expect("an empty splice at the covered-end boundary stays legal");
    m.check().unwrap();
}

#[test]
fn a_zero_length_ref_before_a_real_splice_ref_cannot_silently_collapse() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("meta.redb");
    let m = Meta::open(&path, Options::default()).unwrap();
    let s = m.new_snapshot("s").unwrap();
    let f = s.create(ROOT_INO, b"f", 0o644).unwrap();
    let chunk = ChunkRef::block(BlockId::of(b"abcdefgh"), 8);
    s.batch(|tx| tx.set_content(f.ino, &[chunk], 8)).unwrap();
    let version = s.content_version(f.ino).unwrap();
    let attrs = s.getattr(f.ino).unwrap();
    for zero in [
        ChunkRef::block(BlockId::of(b"invalid"), 0),
        ChunkRef::hole(0),
    ] {
        let result = s.batch(|tx| tx.splice_content(f.ino, version, 0, 8, &[zero, chunk], 8));
        assert!(
            matches!(result, Err(Error::Invalid(_))),
            "a zero-length splice ref must be refused before offset deduplication: {result:?}"
        );
        assert_eq!(s.content_version(f.ino).unwrap(), version);
        assert_eq!(s.getattr(f.ino).unwrap(), attrs);
        assert_eq!(s.chunks(f.ino).unwrap(), vec![chunk]);
        let result = s.batch(|tx| tx.set_content(f.ino, &[zero, chunk], 8));
        assert!(matches!(result, Err(Error::Invalid(_))), "{result:?}");
        assert_eq!(s.content_version(f.ino).unwrap(), version);
        assert_eq!(s.chunks(f.ino).unwrap(), vec![chunk]);
    }
    m.check().unwrap();
    drop(s);
    m.close().unwrap();
    drop(m);
    let m = Meta::open(&path, Options::default()).unwrap();
    let s = m.snapshot("s").unwrap();
    assert_eq!(s.content_version(f.ino).unwrap(), version);
    assert_eq!(s.chunks(f.ino).unwrap(), vec![chunk]);
    s.batch(|tx| tx.splice_content(f.ino, version, 0, 8, &[chunk], 8))
        .expect("the same replacement without the invalid ref stays legal");
    m.check().unwrap();
}
