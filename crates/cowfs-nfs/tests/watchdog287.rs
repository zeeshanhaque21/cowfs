//! Issue 287: a handler that never completes must not leave its xid unanswered for good. The
//! client (a hard mount) retransmits the same xid and must get an answer or a reset.
mod common;

use std::time::Duration;

use common::hang::HangVfs;
use common::*;
use cowfs_nfs::MountOptions;
use nfsserve::tcp::Limits;

const WATCHDOG: Duration = Duration::from_millis(500);
/// The watchdog plus generous slack for a loaded machine.
const BOUND: Duration = Duration::from_secs(10);

fn watchdog() -> MountOptions {
    opts(Limits {
        handler_timeout: WATCHDOG,
        ..Limits::default()
    })
}

fn named(root: &nfsserve::nfs::nfs_fh3, n: &str) -> Args {
    Args::new().put(&dirop(root, n))
}

#[test]
fn a_retransmitted_lookup_that_never_completes_is_answered() {
    let vfs = HangVfs::new();
    let (_server, mut c) = serve(vfs.clone(), watchdog());
    let root = c.root.clone();
    let x = c.send_nfs(3, named(&root, "hang"));
    c.set_next_xid(x);
    c.send_nfs(3, named(&root, "hang"));
    let first = c.recv_status_within(BOUND);
    let second = c.recv_status_within(BOUND);
    vfs.release();
    assert_eq!(first, Some((x, JUKEBOX)), "the original is answered");
    assert_eq!(second, Some((x, JUKEBOX)), "so is the retransmission");
    let (st, _) = c.getattr(&root);
    assert_eq!(st, OK, "and the connection still serves");
}

#[test]
fn a_hung_remove_is_answered_and_its_retry_is_not_a_cached_jukebox() {
    let vfs = HangVfs::new();
    let (_server, mut c) = serve(vfs.clone(), watchdog());
    let root = c.root.clone();
    let x = c.send_nfs(12, named(&root, "hang"));
    // The retransmission of a running non-idempotent call is dropped, the original is answered.
    c.set_next_xid(x);
    c.send_nfs(12, named(&root, "hang"));
    assert_eq!(c.recv_status_within(BOUND), Some((x, JUKEBOX)));
    assert_eq!(
        c.recv_status_within(Duration::from_secs(1)),
        None,
        "the dropped retransmission gets nothing of its own"
    );
    // The client retries after JUKEBOX: it must run again, not replay the cancelled answer.
    c.set_next_xid(x);
    c.send_nfs(12, named(&root, "hang"));
    vfs.release();
    let (_, st) = c.recv_status_within(BOUND).expect("the retry is answered");
    assert_ne!(st, JUKEBOX, "the retry ran once the file system came back");
}

#[test]
fn fast_calls_are_untouched_by_the_watchdog() {
    let (_server, mut c) = serve(memfs(), watchdog());
    let root = c.root.clone();
    let (st, fh, _) = c.create(&root, "x", 1, sattr_mode(0o644), [0; 8]);
    assert_eq!((st, fh.is_some()), (OK, true));
    assert_eq!(c.lookup(&root, "x").0, OK);
}

/// Pins the documented hazard (`Limits::handler_timeout`): the blocking work outlives the
/// watchdog and completes, the cancelled call was not cached, so the client's retry re-executes
/// against the already-applied change and reports NOENT for a REMOVE that did succeed.
#[test]
fn a_slow_remove_that_finishes_after_the_timeout_is_not_replayed_on_retry() {
    let vfs = HangVfs::new();
    let (_server, mut c) = serve(vfs.clone(), watchdog());
    let root = c.root.clone();
    let (st, _, _) = c.create(&root, "hang", 1, sattr_mode(0o644), [0; 8]);
    assert_eq!(st, OK);
    let x = c.send_nfs(12, named(&root, "hang"));
    assert_eq!(c.recv_status_within(BOUND), Some((x, JUKEBOX)));
    // The file system comes back: the abandoned blocking unlink completes.
    vfs.release();
    let deadline = std::time::Instant::now() + BOUND;
    while c.lookup(&root, "hang").0 != NOENT {
        assert!(
            std::time::Instant::now() < deadline,
            "the unlink never completed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    c.set_next_xid(x);
    c.send_nfs(12, named(&root, "hang"));
    let (_, st) = c.recv_status_within(BOUND).expect("the retry is answered");
    assert_eq!(
        st, NOENT,
        "re-executed, not replayed as OK: the documented hazard"
    );
}
