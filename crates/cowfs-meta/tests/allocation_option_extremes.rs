//! Issue #40 M5: `Options::ino_block` at its extremes.
//!
//! `ino_block` is declared `u64` but is a durable reservation step size, and `init` clamps it to
//! `1..=INO_LIMIT` when the file is created. Before that clamp, `ino_block = u64::MAX` panicked in
//! debug and wrapped in release, and a caller could pass any value. This covers the two ends of
//! the domain through the public API only:
//!
//! - `ino_block = 0` is clamped up to 1, so a reservation is never a silent no-op,
//! - `ino_block = u64::MAX` is clamped down to `INO_LIMIT`, so the step never wraps or panics,
//! - both extremes behave exactly like the value they clamp to, and a plain reopen keeps the
//!   stored block governing, so nothing already handed out is reissued.
//!
//! The observables are the durable floor (`Meta::health().ino_floor`), the ranges handed back, and
//! `check()`, not the internal field. `ino_block` is only consulted on create, so each case uses
//! its own fresh store, and the max case must not allocate a large range: the point is the clamp,
//! not volume.

use cowfs_meta::{Error, Meta, Options, INO_LIMIT, ROOT_INO};

fn opts_with_block(ino_block: u64) -> Options {
    Options {
        node_size: 512,
        sync_every_ops: 1,
        background: false,
        ino_block,
        ..Options::default()
    }
}

/// `ino_block = 0` clamps to 1. A reservation must still advance the floor and hand out numbers,
/// which is the behaviour a literal zero block would have destroyed.
#[test]
fn a_zero_block_is_clamped_to_one_and_still_reserves() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");

    let reserved = {
        let m = Meta::open(&path, opts_with_block(0)).unwrap();
        // One reservation. With a literal zero block the durable floor either would not move or
        // the arithmetic would be a no-op; the clamp to 1 makes this an ordinary reservation.
        let r = m.reserve_inodes(5).unwrap();
        assert_eq!(r.len(), 5);
        assert!(r.start().0 > ROOT_INO.0, "never includes the root");
        assert_eq!(
            m.health().ino_floor,
            r.end().0,
            "the floor is durable when the reservation returns"
        );
        m.check().expect("check after a zero-block reservation");
        m.sync().unwrap();
        r
    };

    // Nothing reserved comes back, on the same store after a reopen.
    let m = Meta::open(&path, opts_with_block(0)).unwrap();
    let after = m.reserve_inodes(5).unwrap();
    assert!(
        after.start().0 >= reserved.end().0,
        "reopen must resume at or above {}: got {}",
        reserved.end().0,
        after.start().0
    );
    assert!(
        !after.iter().any(|i| reserved.contains(i)),
        "no number from the first reservation is reissued"
    );
    m.check().expect("check after reopen");
}

/// `ino_block = u64::MAX` clamps to `INO_LIMIT`. It must not panic (debug) or wrap (release), must
/// hand out numbers with a floor that never exceeds the limit, and must stay correct across a
/// reopen with the stored block governing.
///
/// This is the case the audit found missing: the old failure was an arithmetic overflow on this
/// value, and nothing drove it.
#[test]
fn a_u64_max_block_is_clamped_and_never_overflows_or_panics() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");

    let reserved = {
        // The clamp runs at create. If it did not, this call or the reservation below would
        // overflow or panic; that is exactly what this case rejects.
        let m = Meta::open(&path, opts_with_block(u64::MAX)).unwrap();

        let floor_before = m.health().ino_floor;
        let r = m.reserve_inodes(3).unwrap();
        assert_eq!(r.len(), 3, "a normal small request is honoured");
        assert!(r.start().0 > ROOT_INO.0, "never includes the root");

        let floor = m.health().ino_floor;
        assert!(
            floor <= INO_LIMIT,
            "the floor must never exceed INO_LIMIT ({INO_LIMIT}), got {floor}"
        );
        assert!(
            floor >= r.end().0,
            "the floor covers the reserved end {}: got {floor}",
            r.end().0
        );
        assert!(
            floor >= floor_before,
            "the floor only moves up: {floor_before} -> {floor}"
        );
        m.check().expect("check after a u64::MAX-block reservation");
        m.sync().unwrap();
        r
    };

    // The stored block governs on reopen: a caller passing a small block now must not change the
    // allocator, and no number already handed out comes back.
    let m = Meta::open(&path, opts_with_block(4)).unwrap();
    assert!(
        m.health().recoveries == 0,
        "no rollback happened, so no recovery was recorded"
    );
    let after = m.reserve_inodes(1).unwrap();
    assert!(
        after.start().0 >= reserved.end().0,
        "reopen must resume at or above {}: got {}",
        reserved.end().0,
        after.start().0
    );
    assert!(
        after.start().0 <= INO_LIMIT,
        "no number at or above INO_LIMIT is ever handed out"
    );
    m.check()
        .expect("check after reopen with a different block");
}

/// A request larger than the space left below `INO_LIMIT` is refused, not clamped to a fraction and
/// not allowed to wrap, under a max-clamped block. This pins the interaction between the extreme
/// block and the `LimitExceeded` guard the ordinary tests already cover at a small block.
#[test]
fn a_max_clamped_block_still_refuses_a_request_past_the_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let m = Meta::open(&path, opts_with_block(u64::MAX)).unwrap();

    // Ask for more than the whole domain. It must be refused and write nothing, not panic.
    let e = m.reserve_inodes(u64::MAX).unwrap_err();
    assert!(
        matches!(e, Error::LimitExceeded(_)),
        "a request past the limit must be LimitExceeded, got {e:?}"
    );
    assert_eq!(
        m.health().ino_floor,
        2,
        "a refused request leaves the fresh floor untouched"
    );

    // A normal request right after still works, so the refusal did not wedge the allocator.
    let r = m.reserve_inodes(2).unwrap();
    assert_eq!(r.len(), 2);
    m.check().expect("check after a refused max request");
}

/// The default block is untouched by the clamping cases: it behaves like an ordinary step, so the
/// clamp is not accidentally collapsing every value to one end. This is the control for the two
/// extreme cases above.
#[test]
fn the_default_block_is_a_normal_step() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let default_block = Options::default().ino_block;
    assert!(
        (1..=INO_LIMIT).contains(&default_block),
        "the default must already be inside the clamp: {default_block}"
    );

    let m = Meta::open(&path, opts_with_block(default_block)).unwrap();
    let first = m.reserve_inodes(1).unwrap();
    assert!(first.start().0 > ROOT_INO.0);

    let second = m.reserve_inodes(1).unwrap();
    assert!(
        second.start().0 >= first.end().0,
        "no reuse under the default block"
    );
    m.check().expect("check under the default block");
}
