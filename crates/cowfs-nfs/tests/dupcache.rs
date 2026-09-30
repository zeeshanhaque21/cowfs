//! The duplicate request cache must tell a retransmission from an xid collision between the
//! several connections one client keeps open.
mod common;

use common::*;
use cowfs_nfs::MountOptions;

#[test]
fn identical_xid_and_call_on_a_second_connection_is_executed() {
    let (server, mut a) = serve(memfs(), MountOptions::default());
    let root = a.root.clone();
    let mut b = Nfs::attach(server.port(), root.clone());
    a.set_next_xid(5);
    let (st, fh, _) = a.create(&root, "x", 1, sattr_mode(0o644), [0; 8]);
    assert_eq!((st, fh.is_some()), (OK, true), "A creates x with xid 5");
    a.set_next_xid(6);
    assert_eq!(a.remove(&root, "x"), OK, "A removes x with xid 6");
    assert_eq!(a.lookup(&root, "x").0, NOENT, "x is gone");
    b.set_next_xid(5);
    let (st, _, _) = b.create(&root, "x", 1, sattr_mode(0o644), [0; 8]);
    assert_eq!(st, OK);
    assert_eq!(
        b.lookup(&root, "x").0,
        OK,
        "B was told CREATE succeeded but x does not exist"
    );
    assert_eq!(
        b.remove(&root, "x"),
        OK,
        "and the file B was handed is removable"
    );
}

#[test]
fn every_connection_gets_its_own_xid_space() {
    let (server, mut a) = serve(memfs(), MountOptions::default());
    let root = a.root.clone();
    let mut b = Nfs::attach(server.port(), root.clone());
    let mut c = Nfs::attach(server.port(), root.clone());
    a.set_next_xid(1);
    let (n, _, _) = a.create(&root, "a", 1, sattr_mode(0o644), [0; 8]);
    assert_eq!(n, OK);
    b.set_next_xid(1);
    let (n, _, _) = b.create(&root, "b", 1, sattr_mode(0o644), [0; 8]);
    assert_eq!(n, OK);
    c.set_next_xid(1);
    let (n, _, _) = c.create(&root, "c", 1, sattr_mode(0o644), [0; 8]);
    assert_eq!(n, OK);
    for name in ["a", "b", "c"] {
        assert_eq!(c.lookup(&root, name).0, OK, "{name} exists");
    }
    for (i, name) in ["d", "e", "f"].iter().enumerate() {
        a.set_next_xid(2 + i as u32);
        let (n, fh, _) = a.create(&root, name, 1, sattr_mode(0o644), [0; 8]);
        assert_eq!((n, fh.is_some()), (OK, true), "A's own calls still run");
    }
    for name in ["d", "e", "f"] {
        assert_eq!(c.lookup(&root, name).0, OK, "{name} exists");
    }
}

#[test]
fn a_retransmit_after_a_reconnect_is_not_executed_again() {
    let (server, mut a) = serve(memfs(), MountOptions::default());
    let root = a.root.clone();
    a.create_file(&root, "x");
    a.set_next_xid(20);
    assert_eq!(
        a.remove(&root, "x"),
        OK,
        "A removed x with xid 20, its last call"
    );
    drop(a);

    // The reply to xid 20 never arrived, so another connection re-created x and the client came
    // back and sent the same call again.
    let mut c = Nfs::attach(server.port(), root.clone());
    c.create_file(&root, "x");
    let mut b = Nfs::attach(server.port(), root.clone());
    b.set_next_xid(20);
    assert_eq!(b.remove(&root, "x"), OK, "replayed, not executed");
    assert_eq!(b.lookup(&root, "x").0, OK, "the re-created file survives");
}

#[test]
fn a_call_the_client_had_answered_is_executed_on_another_connection() {
    let (server, mut a) = serve(memfs(), MountOptions::default());
    let root = a.root.clone();
    a.set_next_xid(20);
    assert_eq!(a.remove(&root, "x"), NOENT, "A removed x with xid 20");
    a.set_next_xid(21);
    a.create_file(&root, "x");
    let mut b = Nfs::attach(server.port(), root.clone());
    b.set_next_xid(20);
    assert_eq!(
        b.remove(&root, "x"),
        OK,
        "same call, new connection: a new call"
    );
    assert_eq!(
        b.lookup(&root, "x").0,
        NOENT,
        "so it ran and really removed the file"
    );
}

#[test]
fn a_retransmitted_remove_never_takes_a_recreated_file() {
    let (server, mut a) = serve(memfs(), MountOptions::default());
    let root = a.root.clone();
    a.create_file(&root, "x");
    a.set_next_xid(31);
    assert_eq!(
        a.remove(&root, "x"),
        OK,
        "A removed x with xid 31, its last call"
    );
    drop(a);

    // Something re-creates x, then the client comes back and resends the call it never saw the
    // answer to. The recreated file must survive.
    let mut c = Nfs::attach(server.port(), root.clone());
    c.create_file(&root, "x");
    let mut b = Nfs::attach(server.port(), root.clone());
    b.set_next_xid(31);
    assert_eq!(b.remove(&root, "x"), OK, "replayed, not executed");
    assert_eq!(
        b.lookup(&root, "x").0,
        OK,
        "the recreated file is untouched"
    );

    // On the same connection the rule is unconditional.
    let mut d = Nfs::attach(server.port(), root.clone());
    d.set_next_xid(9);
    d.create_file(&root, "y");
    d.set_next_xid(10);
    assert_eq!(d.remove(&root, "y"), OK);
    d.create_file(&root, "y");
    d.set_next_xid(10);
    assert_eq!(
        d.remove(&root, "y"),
        OK,
        "the retransmit replays the original reply"
    );
    assert_eq!(
        d.lookup(&root, "y").0,
        OK,
        "and the recreated file is untouched"
    );
}

#[test]
fn the_same_xid_with_other_arguments_never_replays() {
    let (_s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    c.create_file(&root, "one");
    c.create_file(&root, "two");
    c.set_next_xid(77);
    assert_eq!(c.remove(&root, "one"), OK);
    c.set_next_xid(77);
    assert_eq!(
        c.remove(&root, "two"),
        OK,
        "another file, same xid: a different call"
    );
    assert_eq!(c.lookup(&root, "two").0, NOENT, "it really ran");
    c.set_next_xid(77);
    assert_eq!(
        c.remove(&root, "one"),
        OK,
        "the first call is still remembered"
    );
    assert_eq!(c.lookup(&root, "one").0, NOENT, "and ran exactly once");
}
