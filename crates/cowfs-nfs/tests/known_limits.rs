mod common;

use std::sync::Arc;

use cowfs_nfs::{MountOptions, Server};
use cowfs_vfs_test::MemVfs;
use nfsserve::nfs::{nfsstat3, sattr3, set_mode3};
use nfsserve::xdr::XDR;

#[test]
#[ignore = "single-user v1: the NFS server ignores AUTH_UNIX caller identity; mountpoint permissions are the boundary"]
fn readonly_nonowner_rpc_write_is_denied() {
    assert_eq!(nonowner_write(), nfsstat3::NFS3ERR_ACCES as u32);
}

#[test]
fn single_user_v1_accepts_nonowner_rpc_write() {
    assert_eq!(nonowner_write(), common::OK);
}

fn nonowner_write() -> u32 {
    let server = Server::start(
        Arc::new(MemVfs::new()),
        &MountOptions::default(),
        Some((501, 20)),
    )
    .unwrap();
    let mut nfs = common::Nfs::connect(server.port(), server.export_name());
    let root = nfs.root.clone();
    let (status, handle, _) = nfs.create(
        &root,
        "readonly",
        1,
        sattr3 {
            mode: set_mode3::mode(0o444),
            ..sattr3::default()
        },
        [0; 8],
    );
    assert_eq!(status, common::OK);
    let handle = handle.unwrap();
    let attr = nfs.attrs(&handle);
    assert_eq!((attr.uid, attr.mode), (501, 0o444));
    let mut request = Vec::new();
    for word in [99u32, 0, 2, 100_003, 3, 7, 1, 20, 0, 0, 502, 20, 0, 0, 0] {
        word.serialize(&mut request).unwrap();
    }
    handle.serialize(&mut request).unwrap();
    0u64.serialize(&mut request).unwrap();
    3u32.serialize(&mut request).unwrap();
    2u32.serialize(&mut request).unwrap();
    b"bad".to_vec().serialize(&mut request).unwrap();
    nfs.send(&request);
    let mut reply = std::io::Cursor::new(nfs.recv());
    assert_eq!(common::dec::<u32>(&mut reply), 99);
    for expected in [1u32, 0, 0, 0, 0] {
        assert_eq!(common::dec::<u32>(&mut reply), expected);
    }
    common::dec::<u32>(&mut reply)
}

/// pjdfstest `unlink/14.t` #4 (#109): an open file that is unlinked reports nlink 1 on the macOS
/// NFS mount, where POSIX and APFS say 0.
///
/// Client divergence, not a server defect (docs/verification/evidence/nfs109-unlink-open-nlink.md).
/// The client never sends REMOVE for a file it holds open: it sends RENAME to `.nfs.*`, so the
/// server sees a file with one link and answers 1. NFSv3 has no open state, so once REMOVE does
/// arrive the handle is STALE (RFC 1813) and nlink 0 can never be reported. This pins the RPC
/// sequence the client sends and the answers the server gives to it.
#[test]
fn silly_renamed_file_keeps_one_link_then_goes_stale_on_remove() {
    let (_s, mut c) = common::serve(common::memfs(), MountOptions::default());
    let root = c.root.clone();
    let f = c.create_file(&root, "f");
    assert_eq!(c.rename(&root, "f", &root, ".nfs.silly"), common::OK);
    assert_eq!(c.attrs(&f).nlink, 1, "renamed, not removed: one link");
    assert_eq!(c.lookup(&root, "f").0, common::NOENT);
    assert_eq!(c.remove(&root, ".nfs.silly"), common::OK);
    assert_eq!(c.getattr(&f).0, common::STALE, "no open state: removed means stale");
}
