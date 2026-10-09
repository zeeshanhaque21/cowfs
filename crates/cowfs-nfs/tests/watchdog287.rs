//! Issue 287: a handler that never completes must not leave its xid unanswered for good. The
//! client (a hard mount) retransmits the same xid and must get an answer or a reset.
mod common;

use std::time::Duration;

use common::hang::HangVfs;
use common::*;
use cowfs_nfs::MountOptions;

const BOUND: Duration = Duration::from_secs(5);

fn lookup_args(root: &nfsserve::nfs::nfs_fh3, n: &str) -> Args {
    Args::new().put(&dirop(root, n))
}

#[test]
fn a_retransmitted_lookup_that_never_completes_is_answered() {
    let vfs = HangVfs::new();
    let (_server, mut c) = serve(vfs.clone(), MountOptions::default());
    let root = c.root.clone();
    let x = c.send_nfs(3, lookup_args(&root, "hang"));
    c.set_next_xid(x);
    c.send_nfs(3, lookup_args(&root, "hang"));
    let got = c.recv_status_within(BOUND);
    vfs.release();
    assert!(got.is_some(), "no reply to a retransmitted LOOKUP within {BOUND:?}");
}
