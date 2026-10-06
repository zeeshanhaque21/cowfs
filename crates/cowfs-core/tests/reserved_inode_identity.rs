//! #42 request 4 (Core consumer): a created file's identity must be a real,
//! durable, reservation-backed inode number, not a session-local virtual alias.
//!
//! # The requirement, end to end, the way a user hits it
//!
//! PR140 adds `Meta::reserve_inodes(n)`, which hands out inode numbers without
//! creating inodes. Issue #42 request 4 is the Core consumer: a file created at
//! a number drawn from that reservation must keep that same number durably, and
//! must survive a crash and a reopen with the same identity and bytes.
//!
//! Today Core does not do that. `Inner::alloc_virt` mints a *virtual* number
//! (top bit `VIRT = 1 << 63` set) from Core's own mark files, create queues an
//! `Op::Create` carrying that virtual number, and only the metadata `Tx` picks a
//! real meta number. Core then records the bridge in an in-memory alias table.
//! The virtual number is a session-local alias: it is not the durable identity,
//! it is not reservation-backed, and it is gone after a reopen.
//!
//! This test pins the *consumer contract* with the existing public Core API:
//! the number a caller holds for a created file must be the same durable
//! number before a flush and after a fresh `Core::open`, and that number must
//! be a real reservation-backed identity rather than a virtual alias.
//!
//! OLD (current tree): the first-session number carries the `VIRT` bit, and
//! after a reopen the file resolves to a *different* (packed meta) number, so
//! the two ends of the path disagree. The assertion at the end fails on OLD.
//!
//! NEW (the seam this test is written to drive): create draws its number from a
//! real reservation, the number is durable before the flush, and the same
//! number comes back after the reopen. No `VIRT` bit is involved.
//!
//! This is TEST-FIRST: it must fail on the current tree with a real assertion
//! about identity, not a compile error about a missing API. It uses only public
//! Core API and the existing public `Core::meta_inode` test seam.

mod common;

use common::*;
use cowfs_core::Core;
use cowfs_vfs::{Error, Vfs, ROOT_INO};

/// Top bit Core sets on a number handed out before meta assigned a real one.
/// Same constant as `crates/cowfs-core/src/ino.rs`; a test must not reach into
/// the crate, so it is restated here as the public property it is.
const VIRT: u64 = 1 << 63;

/// The end-to-end regression: one created file, followed from the uncommitted
/// create through an explicit flush to a fresh open, must keep one identity.
#[test]
fn a_created_file_keeps_one_durable_identity_across_a_flush_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = b"reserved-inode-identity-payload";

    // --- Arrange: a snapshot to create in. ---------------------------------
    // Gather the whole path first, then assert, so on OLD every observation is
    // captured (the assertion that fires is about identity, not about an early
    // short circuit).
    let created;
    let meta_before;
    let meta_after_flush;
    {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.create_snapshot("s").unwrap();
        let r = root_entry(&c, "s").ino;

        // --- Act 1: create, then look at the number while it is UNCOMMITTED. -
        // `create` queues an `Op::Create` and returns the number the caller
        // must use; at this instant no metadata commit has run, so the number
        // is whatever Core hands out on the create path.
        let a = c.create(r, b"f", 0o644).unwrap();
        created = a.ino;
        meta_before = c.meta_inode(created);

        // --- Act 2: make it durable, then read it back within the session. ---
        Vfs::flush(&c, created).unwrap();
        c.sync().unwrap();
        meta_after_flush = c.meta_inode(created);

        // The bytes are readable through the very number the caller holds.
        write_all(&c, created, 0, bytes);
        c.sync().unwrap();
        // `c` drops here: the last clone commits and closes the meta db.
    }

    // --- Assert: a fresh Core must resolve the file to the SAME identity. ----
    let c = Core::open(dir.path(), test_opts()).unwrap();
    let r = root_entry(&c, "s").ino;
    let a = c.lookup(r, b"f").unwrap();
    let meta_after_reopen = c.meta_inode(a.ino);

    // The contract in one sentence: the caller's number IS the durable
    // identity. On OLD this is false at every clause. The first session held a
    // session-local virtual number (`created` has the VIRT bit), meta never
    // named it, and a fresh session resolves the file to the packed meta
    // number instead, so the two ends disagree.
    let report = format!(
        "created(in-session)={created:#x} created_meta_before_flush={meta_before:?} \
         created_meta_after_flush={meta_after_flush:?} reopen=(ino={:#x}, meta={meta_after_reopen:?})",
        a.ino
    );

    // 1. The number the caller holds is a real inode number, not a virtual alias.
    assert_eq!(
        created & VIRT,
        0,
        "created file got a session-local virtual number: {report}"
    );
    // 2. A reservation-backed number is one meta can name before the flush.
    assert!(
        meta_before.is_some(),
        "no meta inode behind the created number before the flush: {report}"
    );
    // 3. The same durable identity survives the reopen.
    assert_eq!(
        a.ino, created,
        "the file's inode number changed across a reopen: {report}"
    );
    assert_eq!(
        meta_after_reopen, meta_after_flush,
        "the durable meta identity behind the file changed across the reopen: {report}"
    );

    // Preserved bytes, read back from the real store through the durable number.
    assert_eq!(read_all(&c, a.ino), bytes);
    c.check().unwrap();
}

/// Negative control: the identity a caller holds must not be mistakable for the
/// virtual alias shape. A number with the `VIRT` bit set is a session-local
/// alias by construction, and meta must have no inode behind it: the mount root
/// is the one number that is neither a meta inode nor virtual, and it must stay
/// distinct from any file the session creates.
#[test]
fn a_created_number_is_never_the_virtual_alias_shape_or_the_root() {
    let dir = tempfile::tempdir().unwrap();
    let c = Core::open(dir.path(), test_opts()).unwrap();
    c.create_snapshot("s").unwrap();
    let r = root_entry(&c, "s").ino;
    let a = c.create(r, b"f", 0o644).unwrap();
    Vfs::flush(&c, a.ino).unwrap();
    c.sync().unwrap();

    assert_ne!(
        a.ino, ROOT_INO,
        "a created file reused the mount root number"
    );
    assert_eq!(
        a.ino & VIRT,
        0,
        "a created file number {:#x} carries the virtual alias bit",
        a.ino
    );
    assert!(
        c.meta_inode(a.ino).is_some(),
        "a created file number {:#x} has no meta inode behind it",
        a.ino
    );
}

/// Negative control for the refusal path: a virtual alias-shaped number the
/// fresh session never handed out is `Stale`, not another file's bytes.
///
/// This tests the alias *shape*, not any created file's real identity. It must
/// not assert on a created file's number: under the NEW reservation design a
/// created file's number is durable, so a fresh `getattr` on it must succeed,
/// and testing `Stale` on the created number would contradict the positive case
/// above. The `VIRT` bit is what makes a number an alias, and a real
/// reservation-backed number never sets it, so the tagged number here is a
/// distinct, non-admitted identity by construction.
#[test]
fn a_virtual_alias_number_is_stale_after_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let virt_alias = {
        let c = Core::open(dir.path(), test_opts()).unwrap();
        c.create_snapshot("s").unwrap();
        let r = root_entry(&c, "s").ino;
        let a = c.create(r, b"f", 0o644).unwrap();
        write_all(&c, a.ino, 0, b"x");
        c.sync().unwrap();
        // The legacy alias shape: top bit set, tagged virtual on purpose. On a
        // real reservation this number is never handed out, so it stays stale.
        a.ino | VIRT
    };
    assert_ne!(
        virt_alias & VIRT,
        0,
        "the control number is not alias-shaped: {virt_alias:#x}"
    );
    let c = Core::open(dir.path(), test_opts()).unwrap();
    assert_eq!(
        c.getattr(virt_alias),
        Err(Error::Stale),
        "a virtual alias number is not stale after a reopen: {virt_alias:#x}"
    );
}
