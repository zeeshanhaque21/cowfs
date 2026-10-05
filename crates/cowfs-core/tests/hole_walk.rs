//! The walk in `cowfs-meta` is safe on its own: it never hands out a hole.
//!
//! This fixture uses no `ChunkRef` literal and no flag, so it compiles and runs against the tree
//! before the flag existed as well as against the tree with it.
//! The sparse file is made through the public `Core` API, and the walk is the metadata walk itself,
//! so what this measures is what a collector would see.
//!
//! What it does not claim: anything about `Core::live_blocks`, which filtered holes before the flag
//! existed and is still the entry point the collector uses.

use cowfs_core::{Core, Options};
use cowfs_vfs::{Vfs, ROOT_INO};

fn open(dir: &std::path::Path) -> Core {
    Core::open(
        dir,
        Options {
            background: false,
            ..Default::default()
        },
    )
    .expect("a store opens")
}

fn meta_walk(c: &Core) -> Vec<cowfs_store::BlockId> {
    let mut marker = cowfs_meta::Marker::default();
    let sid = c.list_snapshots().unwrap()[0].id;
    let snap = c
        .meta()
        .snapshot_by_id(cowfs_meta::SnapshotId(sid))
        .unwrap();
    let mut out: Vec<_> = snap
        .live_blocks(&mut marker)
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();
    out.sort();
    out
}

#[test]
fn the_metadata_walk_of_a_sparse_file_yields_only_stored_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let c = open(dir.path());
    c.create_snapshot("s").unwrap();
    let v = c.snapshot_view("s").unwrap();

    // a file with a hole in the middle: truncate past the end, then write a little way in
    let sparse = v.create(ROOT_INO, b"sparse", 0o644).unwrap().ino;
    v.setattr(
        sparse,
        cowfs_vfs::SetAttr {
            size: Some(8 << 20),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(v.write(sparse, 1 << 20, b"in the middle").unwrap(), 13);
    // and a file with no hole at all, so the walk has something it must keep
    let full = v.create(ROOT_INO, b"full", 0o644).unwrap().ino;
    let body = vec![b'z'; 300_000];
    assert_eq!(v.write(full, 0, &body).unwrap() as usize, body.len());
    c.sync().unwrap();
    c.check().expect("check");

    let got = meta_walk(&c);
    assert!(
        !got.is_empty(),
        "the walk must find the real blocks: {got:?}"
    );
    assert!(
        !got.iter().any(|id| id.as_bytes() == &[0u8; 32]),
        "the metadata walk handed a hole to the caller as if it were a block: {got:?}"
    );
    for id in &got {
        assert!(
            c.store().contains(*id),
            "the walk yielded a block the store does not hold: {id}"
        );
    }
}

#[test]
fn the_metadata_walk_and_the_stored_blocks_agree() {
    // the survivor readback: every block the walk names is a block the store holds and can return,
    // so filtering the hole did not cost the data
    let dir = tempfile::tempdir().unwrap();
    let c = open(dir.path());
    c.create_snapshot("s").unwrap();
    let v = c.snapshot_view("s").unwrap();
    let sparse = v.create(ROOT_INO, b"sparse", 0o644).unwrap().ino;
    v.setattr(
        sparse,
        cowfs_vfs::SetAttr {
            size: Some(8 << 20),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(v.write(sparse, 4 << 20, b"tail bytes").unwrap(), 10);
    c.sync().unwrap();

    let got = meta_walk(&c);
    assert_eq!(got.len(), 1, "one written region is one block: {got:?}");
    // the block the walk names is the block the store holds: the walk and the store agree
    assert!(
        c.store().contains(got[0]),
        "the walked block is not in the store"
    );
    assert!(
        c.store().get(got[0]).is_ok(),
        "the walked block cannot be read back: {:?}",
        c.store().get(got[0]).err()
    );

    // and the bytes the mount hands back across the hole are the ones written
    let back = v.read(sparse, 4 << 20, 10).unwrap();
    assert_eq!(back, b"tail bytes");
    let gap = v.read(sparse, 1 << 20, 16).unwrap();
    assert_eq!(gap, vec![0u8; 16], "the hole reads as zeros");
}

#[test]
fn a_walk_after_a_reopen_still_yields_no_hole() {
    let dir = tempfile::tempdir().unwrap();
    let first = {
        let c = open(dir.path());
        c.create_snapshot("s").unwrap();
        let v = c.snapshot_view("s").unwrap();
        let f = v.create(ROOT_INO, b"sparse", 0o644).unwrap().ino;
        v.setattr(
            f,
            cowfs_vfs::SetAttr {
                size: Some(4 << 20),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(v.write(f, 2 << 20, b"middle").unwrap(), 6);
        c.sync().unwrap();
        meta_walk(&c)
    };
    let c = open(dir.path());
    let again = meta_walk(&c);
    assert_eq!(again, first, "a reopened store must walk the same way");
    assert!(
        !again.iter().any(|id| id.as_bytes() == &[0u8; 32]),
        "the hole sentinel came back after a reopen: {again:?}"
    );
}
