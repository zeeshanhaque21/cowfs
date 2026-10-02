//! F3: one damaged block must not wedge its snapshot.
//!
//! A write that only partly covers a damaged chunk is EIO at the write; the file is then poisoned
//! (every operation on it reports the error), the rest of the snapshot's queue still commits, other
//! files' data is still durable after their own `fsync`, and the mount keeps working.

mod common;

use common::*;
use cowfs_core::{Core, Options};
use cowfs_vfs::{Error, SetAttr, Vfs};

fn one_pack(dir: &std::path::Path) -> std::path::PathBuf {
    let mut v: Vec<_> = std::fs::read_dir(dir.join("store/packs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    v.sort();
    v.remove(0)
}

/// The end offset of the chunk that contains file offset 70_000, read from meta.
fn chunk_end(c: &Core, ino: u64) -> u64 {
    let m = cowfs_meta::Ino(c.meta_inode(ino).expect("no meta inode behind the number"));
    let refs = c.meta().snapshot("s").unwrap().chunks(m).unwrap();
    let mut acc = 0u64;
    for r in refs {
        if acc + u64::from(r.len) > 70_000 {
            return acc + u64::from(r.len);
        }
        acc += u64::from(r.len);
    }
    acc
}

fn damage(dir: &std::path::Path) {
    let pack = one_pack(dir);
    let mut bytes = std::fs::read(&pack).unwrap();
    bytes[16 + 52 + 70_000] ^= 0x55;
    std::fs::write(&pack, &bytes).unwrap();
}

/// F3: the critic's a3 repro, as assertions.
#[test]
fn a_damaged_chunk_fails_its_own_file_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(
        dir.path(),
        Options {
            background: false,
            file_flush_bytes: 1 << 20,
            ..Options::default()
        },
    )
    .unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    let data = pattern(600_000, 7);
    let f = mkfile(&c, r, "f", &data).ino;
    let other = mkfile(&c, r, "keep", b"keep me").ino;
    c.sync().unwrap();
    c.drop_caches();
    damage(dir.path());
    // a full-chunk overwrite does not need the old bytes, so it is allowed
    assert!(c.write(f, 0, &pattern(600_000, 1)).is_ok());
    // a partial overwrite inside the damaged chunk is EIO now, not later
    let e = c.write(f, 70_000, b"xxxx").unwrap_err();
    assert_eq!(e.errno(), Error::Io(String::new()).errno(), "{e:?}");
    assert!(matches!(e, Error::Corrupt(_)), "{e:?}");
    // an unrelated file commits and becomes durable
    let g = c.create(r, b"g", 0o644).unwrap().ino;
    c.write(g, 0, b"important").unwrap();
    c.fsync(g, false).expect("fsync of an unrelated file");
    assert_eq!(c.read(g, 0, 9).unwrap(), b"important");
    // the namespace is not stuck either
    assert_eq!(c.readdir(r, 0, 100).unwrap().entries.len(), 3);
    let d = c.mkdir(r, b"d", 0o755).unwrap().ino;
    c.mkdir(d, b"e", 0o755).unwrap();
    assert_eq!(c.readdir(d, 0, 10).unwrap().entries.len(), 1);
    assert!(c.fork_snapshot("s", "t").is_ok());
    assert!(c.merkle_root("s").is_ok());
    assert!(c.sync().is_ok());
    // the damaged file is poisoned: every operation on it reports the error
    assert!(matches!(c.read(f, 70_000, 4), Err(Error::Corrupt(_))));
    assert!(matches!(
        c.write(f, 70_000, b"yyyy"),
        Err(Error::Corrupt(_))
    ));
    assert!(matches!(c.fsync(f, false), Err(Error::Corrupt(_))));
    assert!(matches!(Vfs::flush(&c, f), Err(Error::Corrupt(_))));
    assert!(matches!(
        c.setattr(
            f,
            SetAttr {
                size: Some(10),
                ..SetAttr::default()
            }
        ),
        Err(Error::Corrupt(_))
    ));
    let s = c.stats();
    assert!(s.poisoned >= 1, "{s:?}");
    assert_eq!(
        s.flush_errors, 0,
        "a poisoned file must not fail the snapshot: {s:?}"
    );
    assert_eq!(read_all(&c, other), b"keep me");
    assert_eq!(c.read(g, 0, 9).unwrap(), b"important");
    // removing the damaged file clears the poison and its bytes
    c.unlink(r, b"f").unwrap();
    c.forget(f, 1);
    c.flush().expect("flush after the unlink");
    assert_eq!(c.stats().dirty_bytes, 0, "{:?}", c.stats());
    c.check().unwrap();
    drop(c);
    // other files' data survived: the damaged one is unreadable, the rest is not
    let c = Core::open(dir.path(), test_opts()).expect("reopen");
    let r = root_entry(&c, "s").ino;
    assert_eq!(read_all(&c, c.lookup(r, b"keep").unwrap().ino), b"keep me");
    assert_eq!(read_all(&c, c.lookup(r, b"g").unwrap().ino), b"important");
    assert!(c.lookup(r, b"f").is_err());
    c.check().unwrap();
    // the store is deliberately damaged here, so fsck reports it; the metadata is consistent
    assert!(!c.fsck().unwrap().is_clean());
}

/// F3: a damage that only a truncate can find is reported by the truncate, and the file is
/// poisoned so the queue still moves.
#[test]
fn a_damaged_chunk_found_by_a_truncate_poisons_only_that_file() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(
        dir.path(),
        Options {
            background: false,
            file_flush_bytes: 1 << 20,
            ..Options::default()
        },
    )
    .unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    mkfile(&c, r, "f", &pattern(600_000, 7));
    mkfile(&c, r, "g", b"other");
    c.sync().unwrap();
    drop(c);
    damage(dir.path());
    // the damaged bytes are at file offset 70_000, so shrink to a size inside that chunk
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let r = root_entry(&c, "s").ino;
    let f = c.lookup(r, b"f").unwrap().ino;
    let g = c.lookup(r, b"g").unwrap().ino;
    let end = chunk_end(&c, f);
    assert!(
        end > 70_000,
        "the damaged bytes should be inside chunk 0, which ends at {end}"
    );
    let e = c
        .setattr(
            f,
            SetAttr {
                size: Some(end - 1),
                ..SetAttr::default()
            },
        )
        .unwrap_err();
    assert!(matches!(e, Error::Corrupt(_)), "{e:?}");
    assert!(matches!(c.read(f, 0, 10), Err(Error::Corrupt(_))));
    c.fsync(g, false).expect("unrelated fsync");
    assert_eq!(read_all(&c, g), b"other");
    assert!(c.flush().is_ok());
    assert!(c.check().is_ok());
}

/// F3: a healthy file's data is durable after its own fsync even when another file is poisoned.
#[test]
fn a_healthy_file_is_durable_after_its_fsync_while_another_is_poisoned() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(
        dir.path(),
        Options {
            background: false,
            file_flush_bytes: 1 << 20,
            ..Options::default()
        },
    )
    .unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    let f = mkfile(&c, r, "f", &pattern(600_000, 7)).ino;
    let g = c.create(r, b"g", 0o644).unwrap().ino;
    c.sync().unwrap();
    c.drop_caches();
    damage(dir.path());
    let _ = c.write(f, 70_000, b"xxxx");
    c.write(g, 0, b"durable payload").unwrap();
    c.fsync(g, false).expect("fsync of g");
    drop(c);
    // the store holds g's bytes and they are inside the durable watermark
    let st = cowfs_store::Store::open(dir.path().join("store"), cowfs_store::Options::default())
        .expect("store opens");
    let mut found = false;
    for id in st.iter_ids() {
        if st.get(id).ok().as_deref() == Some(b"durable payload") {
            found = true;
        }
    }
    assert!(found, "the fsynced bytes are not in the store");
    assert!(
        dir.path().join("store/SYNCED").exists(),
        "no durable watermark"
    );
    drop(st);

    let c = Core::open(dir.path(), test_opts()).expect("reopen");
    let r = root_entry(&c, "s").ino;
    assert_eq!(
        read_all(&c, c.lookup(r, b"g").unwrap().ino),
        b"durable payload"
    );
}
