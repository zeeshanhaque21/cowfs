//! pjdfstest `unlink/14.t` #4 over raw NFSv3 (#109).
mod common;

use common::*;
use cowfs_nfs::MountOptions;

/// An open file that is unlinked reports nlink 1 on the macOS NFS mount, where POSIX and APFS say 0.
///
/// Client divergence, not a server defect (docs/verification/evidence/nfs109-unlink-open-nlink.md).
/// The client never sends REMOVE for a file it holds open: it sends RENAME to `.nfs.*`, so the
/// server sees a file with one link and answers 1. NFSv3 has no open state, so once REMOVE does
/// arrive the handle is STALE (RFC 1813) and nlink 0 can never be reported. This pins the RPC
/// sequence the client sends and the answers the server gives to it.
#[test]
fn silly_renamed_file_keeps_one_link_then_goes_stale_on_remove() {
    let (_s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    let f = c.create_file(&root, "f");
    assert_eq!(c.rename(&root, "f", &root, ".nfs.silly"), OK);
    assert_eq!(c.attrs(&f).nlink, 1, "renamed, not removed: one link");
    assert_eq!(c.lookup(&root, "f").0, NOENT);
    assert_eq!(c.remove(&root, ".nfs.silly"), OK);
    assert_eq!(c.getattr(&f).0, STALE, "no open state: removed means stale");
}
