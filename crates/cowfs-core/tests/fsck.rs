//! `Core::fsck` must detect a durable snapshot that references a block the store no longer has.
//!
//! Regression for #84: an old reader unlinked a block a surviving fork needed. The store
//! acknowledged the loss and reported no corruption, so `Store::fsck` said clean while the file
//! could not be read back. The check is filesystem-level: it has to walk the committed trees, not
//! just re-hash the records that are still there.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use cowfs_core::{Core, Options};
use cowfs_store::Damage;
use cowfs_vfs::{Error, Vfs};

/// Store options with small packs, so one file lands in its own pack and can be removed whole.
fn small_pack_opts() -> Options {
    Options {
        store: cowfs_store::Options {
            max_pack_size: 128 << 10,
            checkpoint_on_drop: true,
            ..cowfs_store::Options::default()
        },
        background: false,
        file_flush_bytes: 1 << 16,
        ..Options::default()
    }
}

fn pack_files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir.join("store/packs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "cpk" || e == "pack")
        })
        .collect();
    v.sort();
    v
}

/// Repro #84 end to end: a durable snapshot references a block that is gone, the store reports no
/// corruption, a read through the mount fails, and `fsck` must not report the filesystem clean.
#[test]
fn missing_live_block_is_not_reported_clean() {
    let dir = tempfile::tempdir().unwrap();
    let data = pattern(300_000, 42);

    // A durable snapshot with one file, closed cleanly so the watermark names its pack.
    {
        let c = Core::open(dir.path(), small_pack_opts()).unwrap();
        c.create_snapshot("s").unwrap();
        let r = root_entry(&c, "s").ino;
        let _a = mkfile(&c, r, "f", &data);
        c.sync().unwrap();
        c.close().unwrap();
    }

    // Fixture-owned corruption: remove the pack that holds the block. Verify before mutation that
    // the pack exists and is non-empty.
    let packs = pack_files(dir.path());
    assert!(!packs.is_empty(), "no pack to remove");
    let victim = packs.last().unwrap().clone();
    assert!(
        std::fs::metadata(&victim).unwrap().len() > 0,
        "victim pack is empty"
    );
    std::fs::remove_file(&victim).unwrap();

    // Acknowledge the loss as the old reader's cache upgrade did, so the store stops reporting
    // corruption. This is what made `Store::fsck` say clean while a live file was unreadable.
    {
        let s = cowfs_store::Store::open(dir.path().join("store"), small_pack_opts().store).unwrap();
        assert!(
            s.recovery().has_corruption(),
            "removing the pack was not seen as a loss"
        );
        s.acknowledge_corruption().unwrap();
    }

    // Reopen the filesystem: the store is clean, the committed tree still names the gone block.
    let c = Core::open(dir.path(), small_pack_opts()).unwrap();
    let r = root_entry(&c, "s").ino;
    let a = c.lookup(r, b"f").unwrap().ino;

    let read_err = c.read(a, 0, 1 << 20).unwrap_err();
    assert!(
        matches!(read_err, Error::Io(_) | Error::Corrupt(_)),
        "the missing block must fail a read with EIO, got {read_err:?}"
    );

    let report = c.fsck().unwrap();
    assert!(
        !report.is_clean(),
        "fsck reported clean while a live file referenced a missing block"
    );
    assert!(
        report
            .damage
            .iter()
            .any(|d| matches!(d, Damage::MissingLiveBlock { .. })),
        "fsck did not name the missing live block: {:?}",
        report.damage
    );
}

/// A clean store with no missing references still reports clean: no false positive.
#[test]
fn clean_store_with_live_files_is_clean() {
    let dir = tempfile::tempdir().unwrap();
    let data = pattern(300_000, 7);
    let c = Core::open(dir.path(), small_pack_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    let a = mkfile(&c, r, "f", &data).ino;
    c.sync().unwrap();
    assert_eq!(read_all(&c, a), data, "readback differs from source");
    let report = c.fsck().unwrap();
    assert!(report.is_clean(), "false positive on a clean store: {report:?}");
    assert_eq!(
        report.damage.iter().filter(|d| matches!(d, Damage::MissingLiveBlock { .. })).count(),
        0
    );
}

/// Hash corruption is still detected: the live-reference check must not mask the record check.
#[test]
fn hash_corruption_is_still_detected() {
    let dir = tempfile::tempdir().unwrap();
    let data = pattern(300_000, 3);
    let c = Core::open(dir.path(), small_pack_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    let _a = mkfile(&c, r, "f", &data).ino;
    c.sync().unwrap();
    c.drop_caches();

    // Flip a byte inside the first record's payload, past the pack header and record header.
    let pack = pack_files(dir.path()).pop().unwrap();
    let mut bytes = std::fs::read(&pack).unwrap();
    let idx = 16 + 52 + 1000;
    bytes[idx] ^= 0x5a;
    std::fs::write(&pack, &bytes).unwrap();
    c.drop_caches();

    let report = c.fsck().unwrap();
    assert!(!report.is_clean(), "flipped byte went unnoticed");
    assert!(
        report
            .damage
            .iter()
            .any(|d| !matches!(d, Damage::MissingLiveBlock { .. })),
        "expected record-level damage, got {:?}",
        report.damage
    );
}
