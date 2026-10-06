//! Issue #40 M5: `Options::ino_block` at its extremes, through ordinary creation.
//!
//! `ino_block` is the step the ordinary allocator grows the durable inode floor by, and the
//! historical defect lived in that allocator, not in `reserve_inodes`. `Tx::alloc`
//! (`crates/cowfs-meta/src/tx.rs`) computes `(a.next + a.block.max(1)).min(INO_LIMIT)`, and it is
//! reached by every ordinary `create`/`mkdir`/`symlink`. With `ino_block = u64::MAX` and `next = 2`
//! the addition overflowed: debug panicked, release wrapped. `init` now clamps the stored block
//! with `opts.ino_block.clamp(1, INO_LIMIT)` (`crates/cowfs-meta/src/db.rs`), and that clamped value
//! seeds `InoAlloc.block` on open.
//!
//! These cases drive the clamp through the real allocator, which `reserve_inodes` never consults:
//! it does `s.ino.next + n` and reads no block. So a reservation-only test passes with the clamp
//! removed. Here the observable is the inode an ordinary `create` actually receives, the readback
//! of that inode, the durable floor, and `check()`.
//!
//! Bounds, not volume: `ino_block` is only read at create, so each case uses its own fresh store,
//! and the maximum case intentionally exhausts the id space after the first create rather than
//! allocating a range or iterating.

use cowfs_meta::{Error, FileType, Meta, Options, INO_LIMIT, ROOT_INO};

fn opts_with_block(ino_block: u64) -> Options {
    Options {
        node_size: 512,
        sync_every_ops: 1,
        background: false,
        ino_block,
        ..Options::default()
    }
}

/// `ino_block = u64::MAX` clamps to `INO_LIMIT` at create, and the clamped value is what the
/// ordinary allocator uses. This is the historical overflow: `alloc()` with `next = 2` and an
/// unclamped `u64::MAX` block computed `2 + u64::MAX`, panicking in debug and wrapping in release.
/// Nothing here touches `reserve_inodes`, so a clamp-removal mutant is caught by the first create.
#[test]
fn a_u64_max_block_is_clamped_in_the_ordinary_allocator() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");

    let (created, floor) = {
        let m = Meta::open(&path, opts_with_block(u64::MAX)).unwrap();
        let s = m.new_snapshot("s0").unwrap();

        // The ordinary allocator path. An unclamped u64::MAX block overflows exactly here.
        let f = s
            .create(ROOT_INO, b"f", 0o644)
            .expect("ordinary create under a u64::MAX block must not overflow");

        assert_eq!(f.ino.0, 2, "the first ordinary inode after the root");
        assert_eq!(f.kind, FileType::File, "a regular file was created");
        assert!(f.ino.0 > ROOT_INO.0, "never the root");

        // Read the inode back through the public lookup, so this is a real created inode and not
        // just a successful return.
        let back = s.lookup(ROOT_INO, b"f").unwrap();
        assert_eq!(back.ino, f.ino, "lookup returns the created inode");
        assert_eq!(back.kind, FileType::File);
        assert_eq!(back.mode & 0o777, 0o644);

        // The clamped block reserves the whole id space in one step. That is the honest
        // consequence of an extreme block: the floor is at the limit, not wrapped past it.
        let floor = m.health().ino_floor;
        assert_eq!(
            floor, INO_LIMIT,
            "the clamped block reserves up to INO_LIMIT, not past it"
        );
        m.check()
            .expect("check after an ordinary create under u64::MAX");
        m.sync().unwrap();
        (f.ino, floor)
    };

    // Drop every handle, then reopen: the floor is persisted, and the id space the extreme block
    // reserved is genuinely gone, so the next ordinary create is refused honestly.
    let m = Meta::open(&path, opts_with_block(4)).unwrap();
    assert_eq!(
        m.health().ino_floor,
        floor,
        "the reserved floor survives a reopen"
    );
    let s = m.new_snapshot("s1").unwrap();
    let e = s
        .create(ROOT_INO, b"g", 0o644)
        .expect_err("a fully reserved id space must be refused, not wrapped into");
    assert!(
        matches!(e, Error::LimitExceeded(_)),
        "space exhaustion is LimitExceeded, got {e:?}"
    );

    // The first inode is not reissued and remains readable.
    let back = s.lookup(ROOT_INO, b"f").unwrap();
    assert_eq!(back.ino, created, "the created inode is not reused");
    m.check().expect("check after reopen");
}

/// `ino_block = 0` must not break ordinary creation. Note `alloc()` already writes
/// `block.max(1)`, so a literal zero is bounded locally even without the `Options` clamp; this case
/// is branch and bounds coverage for the zero end, not a load-bearing clamp-removal claim.
#[test]
fn a_zero_block_still_creates_with_a_valid_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");

    let first = {
        let m = Meta::open(&path, opts_with_block(0)).unwrap();
        let s = m.new_snapshot("s0").unwrap();
        let f = s
            .create(ROOT_INO, b"f", 0o644)
            .expect("ordinary create under a zero block");
        assert_eq!(f.ino.0, 2);
        assert_eq!(f.kind, FileType::File);

        let back = s.lookup(ROOT_INO, b"f").unwrap();
        assert_eq!(back.ino, f.ino);
        assert_eq!(back.kind, FileType::File);
        m.check().expect("check after a zero-block create");
        m.sync().unwrap();
        f.ino
    };

    // Reopen: the second ordinary inode is fresh, not a collision with the first.
    let m = Meta::open(&path, opts_with_block(0)).unwrap();
    let s = m.new_snapshot("s1").unwrap();
    let f2 = s.create(ROOT_INO, b"g", 0o644).unwrap();
    assert!(
        f2.ino.0 > first.0,
        "a new ordinary inode must not collide: got {} after {}",
        f2.ino.0,
        first.0
    );
    m.check().expect("check after a zero-block reopen");
}

/// The default block is the control: an ordinary create yields a valid file, the next id is not a
/// collision, and the floor persists across a reopen. This shows the extreme cases are the two ends
/// of a range, not the whole behaviour collapsing to one end.
#[test]
fn the_default_block_creates_valid_files_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let default_block = Options::default().ino_block;
    assert!(
        (1..=INO_LIMIT).contains(&default_block),
        "the default must already be inside the clamp: {default_block}"
    );

    let first = {
        let m = Meta::open(&path, opts_with_block(default_block)).unwrap();
        let s = m.new_snapshot("s0").unwrap();
        let f = s.create(ROOT_INO, b"f", 0o644).unwrap();
        assert_eq!(f.ino.0, 2);
        assert_eq!(f.kind, FileType::File);
        assert_eq!(s.lookup(ROOT_INO, b"f").unwrap().ino, f.ino);
        assert!(
            m.health().ino_floor >= f.ino.0,
            "the floor covers the created inode"
        );
        m.check().expect("check under the default block");
        m.sync().unwrap();
        f.ino
    };

    let m = Meta::open(&path, opts_with_block(default_block)).unwrap();
    let s = m.new_snapshot("s1").unwrap();
    let f2 = s.create(ROOT_INO, b"g", 0o644).unwrap();
    assert!(
        f2.ino.0 > first.0,
        "no collision across a reopen: got {} after {}",
        f2.ino.0,
        first.0
    );
    m.check()
        .expect("check under the default block after reopen");
}

/// A single ordinary create must never hand back `INO_LIMIT` or the root, under either extreme.
/// This pins the boundary the allocator's `.min(INO_LIMIT)` and the root exclusion enforce, so an
/// extreme block cannot leak an illegal id into ordinary use.
#[test]
fn ordinary_creation_never_returns_the_root_or_the_limit() {
    for block in [0u64, 1, 4, u64::MAX] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.redb");
        let m = Meta::open(&path, opts_with_block(block)).unwrap();
        let s = m.new_snapshot("s0").unwrap();
        let f = s.create(ROOT_INO, b"f", 0o644).unwrap();
        assert!(
            f.ino.0 > ROOT_INO.0,
            "block {block}: never the root, got {}",
            f.ino.0
        );
        assert!(
            f.ino.0 < INO_LIMIT,
            "block {block}: never INO_LIMIT, got {}",
            f.ino.0
        );
        m.check().expect("check under an extreme block");
    }
}
