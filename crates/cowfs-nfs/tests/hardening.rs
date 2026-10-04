//! Security, resource-bound, retransmission and inode-reuse tests at the protocol level.
mod common;

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use common::reuse::ReusingVfs;
use common::*;
use cowfs_nfs::{MountOptions, HANDLE_LEN};
use nfsserve::nfs::nfs_fh3;
use nfsserve::tcp::Limits;

const MNT_ACCES: u32 = 13;
const MNT_NOENT: u32 = 2;

// ---- mount gate and handle MACs -------------------------------------------------------------

#[test]
fn only_the_first_mnt_gets_the_root_handle() {
    let (s, mut first) = serve(memfs(), MountOptions::default());
    let root = first.root.clone();
    let mut other = Nfs::attach(s.port(), nfs_fh3::default());
    let own = format!("/{}", s.export_name());
    assert_eq!(
        other.mount_path(&own).0,
        MNT_ACCES,
        "a second process cannot mount"
    );
    assert_eq!(
        first.mount_path(&own).0,
        0,
        "the claiming connection may repeat itself"
    );
    assert_eq!(
        other.getattr(&root).0,
        OK,
        "handles still work for the legitimate client"
    );

    s.rearm_mount();
    let mut again = Nfs::attach(s.port(), nfs_fh3::default());
    assert_eq!(
        again.mount_path(&own).0,
        0,
        "a deliberate remount is possible after rearm"
    );
    assert_eq!(
        other.mount_path(&own).0,
        MNT_ACCES,
        "and closes the gate behind it"
    );
}

#[test]
fn only_the_servers_own_export_path_answers_mnt() {
    let (s, c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    for path in [
        "/",
        "/cowfs",
        "/cowfs-",
        "/cowfs-00000000000000000000000000000000",
        "/cowfs-000000000000000000000000000000000",
        "/cowfs-zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        "/other",
    ] {
        let mut a = Nfs::attach(s.port(), nfs_fh3::default());
        let (st, h) = a.mount_path(path);
        assert_eq!(st, MNT_NOENT, "{path} must not answer MNT");
        assert!(h.is_none());
    }
    // The one path that works is the one only mount_nfs was told.
    let mut right = Nfs::attach(s.port(), nfs_fh3::default());
    let (st, h) = right.mount_path(&format!("/{}", s.export_name()));
    assert_ne!(st, MNT_NOENT, "the real export path answers");
    let _ = (root, h);
}

#[test]
fn two_servers_never_answer_each_others_mnt() {
    let (a, _ca) = serve(memfs(), MountOptions::default());
    let (b, _cb) = serve(memfs(), MountOptions::default());
    let (_c, _cc) = serve(memfs(), MountOptions::default());
    assert_ne!(a.export_name(), b.export_name(), "the secret is per server");
    for (server, other) in [(&a, &b), (&b, &a)] {
        let mut x = Nfs::attach(server.port(), nfs_fh3::default());
        assert_eq!(
            x.mount_path(&format!("/{}", other.export_name())).0,
            MNT_NOENT
        );
    }
}

#[test]
fn a_gate_off_server_lets_everyone_mount() {
    let o = MountOptions {
        one_shot_mount: false,
        ..MountOptions::default()
    };
    let (s, _c) = serve(memfs(), o);
    let mut x = Nfs::attach(s.port(), nfs_fh3::default());
    assert_eq!(x.mount_path(&format!("/{}", s.export_name())).0, 0);
    let mut y = Nfs::attach(s.port(), nfs_fh3::default());
    assert_eq!(
        y.mount_path("/cowfs-0123456789abcdef0123456789abcdef").0,
        MNT_NOENT
    );
}

#[test]
fn forged_and_guessed_handles_are_refused() {
    let (s, mut legit) = serve(memfs(), MountOptions::default());
    let root = legit.root.clone();
    let secret = legit.create_file(&root, "secret");
    legit.write(&secret, 0, b"top secret", 2);
    let mut attacker = Nfs::attach(s.port(), nfs_fh3::default());

    // The MAC is the last 16 bytes, so everything before it is what it covers.
    let split = HANDLE_LEN - 16;
    let mut wrong_ino = secret.data.clone();
    wrong_ino[8..16].copy_from_slice(&1u64.to_le_bytes());
    let mut wrong_kind = secret.data.clone();
    wrong_kind[split - 1] ^= 1;
    let mut wrong_mac = secret.data.clone();
    wrong_mac[HANDLE_LEN - 1] ^= 0xff;
    let mut zero_mac = secret.data.clone();
    zero_mac[split..].fill(0);
    let mut plain = secret.data[..split].to_vec();
    plain.extend_from_slice(&[0; 16]);
    for (why, data) in [
        ("another inode, old MAC", wrong_ino),
        ("another kind, old MAC", wrong_kind),
        ("flipped MAC", wrong_mac),
        ("zero MAC", zero_mac),
        ("no MAC", plain),
        ("short", secret.data[..split].to_vec()),
        ("empty", vec![]),
    ] {
        let fh = nfs_fh3 { data };
        assert_eq!(attacker.getattr(&fh).0, BADHANDLE, "getattr with {why}");
        assert_eq!(attacker.read(&fh, 0, 100).0, BADHANDLE, "read with {why}");
        assert_eq!(
            attacker.write(&fh, 0, b"x", 2).0,
            BADHANDLE,
            "write with {why}"
        );
    }
    let mut brute = 0;
    for guess in 1u64..2000 {
        let mut d = secret.data.clone();
        d[8..16].copy_from_slice(&guess.to_le_bytes());
        d[split..].copy_from_slice(&guess.to_le_bytes().repeat(2));
        brute += usize::from(attacker.getattr(&nfs_fh3 { data: d }).0 == OK);
    }
    assert_eq!(brute, 0, "2000 guessed handles, none accepted");
    assert_eq!(
        attacker.read(&secret, 0, 100).1,
        b"top secret",
        "a stolen real handle works: the residual risk"
    );
}

#[test]
fn handles_of_a_previous_server_generation_are_stale() {
    let (_s1, c1) = serve(memfs(), MountOptions::default());
    std::thread::sleep(Duration::from_millis(5));
    let (_s2, mut c2) = serve(memfs(), MountOptions::default());
    assert_eq!(c2.getattr(&c1.root).0, STALE);
}

// ---- inode reuse -----------------------------------------------------------------------------

#[test]
fn a_reused_inode_number_never_serves_another_files_bytes() {
    let (_s, mut c) = serve(ReusingVfs::new(), MountOptions::default());
    let root = c.root.clone();
    let a = c.create_file(&root, "a");
    c.write(&a, 0, b"AAAA", 2);
    assert_eq!(c.remove(&root, "a"), OK);
    let b = c.create_file(&root, "b");
    c.write(&b, 0, b"BBBB", 2);
    assert_eq!(
        a.data[8..16],
        b.data[8..16],
        "the backend reused the number"
    );
    assert_ne!(a.data, b.data, "but the handles differ");
    assert_eq!(c.read(&a, 0, 10).0, STALE, "the old handle is stale");
    assert_eq!(c.getattr(&a).0, STALE);
    assert_eq!(
        c.write(&a, 0, b"zz", 2).0,
        STALE,
        "and cannot write into the new file"
    );
    assert_eq!(c.read(&b, 0, 10).1, b"BBBB");

    let c1 = c.create_file(&root, "c1");
    let c2 = c.create_file(&root, "c2");
    assert_eq!(
        c.rename(&root, "c1", &root, "c2"),
        OK,
        "rename over a file frees its number"
    );
    let c3 = c.create_file(&root, "c3");
    assert_eq!(c.getattr(&c2).0, STALE);
    assert_eq!(c.getattr(&c3).0, OK);
    assert_eq!(c.getattr(&c1).0, OK, "the renamed file keeps its handle");

    let (_, d) = c.mkdir(&root, "d");
    let d = d.unwrap();
    assert_eq!(c.rmdir(&root, "d"), OK);
    let (_, e) = c.mkdir(&root, "e");
    assert_eq!(c.getattr(&d).0, STALE);
    assert_eq!(c.getattr(&e.unwrap()).0, OK);
}

#[test]
fn a_hardlinked_file_keeps_its_handle_until_the_last_name_goes() {
    let (_s, mut c) = serve(ReusingVfs::new(), MountOptions::default());
    let root = c.root.clone();
    let f = c.create_file(&root, "f");
    assert_eq!(c.link(&f, &root, "g").0, OK);
    assert_eq!(c.remove(&root, "f"), OK);
    assert_eq!(c.getattr(&f).0, OK);
    assert_eq!(c.remove(&root, "g"), OK);
    assert_eq!(c.getattr(&f).0, STALE);
}

// ---- resource bounds and deadlines ------------------------------------------------------------
//
// Tests that measure a process-wide resource (the fd table or RSS) are in resource_bounds.rs, which
// runs them one at a time in a process of their own.

#[test]
fn slow_and_idle_connections_time_out() {
    let limits = Limits {
        frame_timeout: Duration::from_millis(2000),
        idle_timeout: Duration::from_millis(4000),
        ..Limits::default()
    };
    let (s, mut c) = serve(memfs(), opts(limits));
    let root = c.root.clone();

    let mut silent = connect_raw(s.port());
    assert!(closed_within(&mut silent, 15), "no request at all");
    let mut slow = connect_raw(s.port());
    slow.write_all(&[0x80, 0, 0]).unwrap();
    assert!(closed_within(&mut slow, 15), "three of four header bytes");
    let mut trickle = connect_raw(s.port());
    trickle
        .write_all(&(100u32 | 1 << 31).to_be_bytes())
        .unwrap();
    trickle.write_all(&[0; 10]).unwrap();
    assert!(closed_within(&mut trickle, 15), "a body that stops");

    let mut idle = Nfs::attach(s.port(), root.clone());
    assert_eq!(idle.getattr(&root).0, OK);
    std::thread::sleep(Duration::from_millis(6000));
    assert!(
        idle.try_getattr(&root).is_none(),
        "an idle connection is closed"
    );
    let mut fresh = Nfs::attach(s.port(), root.clone());
    assert_eq!(
        fresh.getattr(&root).0,
        OK,
        "and the server serves new clients"
    );
    let _ = &mut c;
}

#[test]
fn absurd_counts_get_bounded_replies() {
    let (_s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    let f = c.create_file(&root, "big");
    for i in 0..4u64 {
        assert_eq!(c.write(&f, i << 19, &vec![7u8; 1 << 19], 0).0, OK);
    }
    let (st, mut r) = c.call(6, Args::new().put(&f).put(&0u64).put(&u32::MAX));
    assert_eq!(st, OK);
    assert!(
        r.get_ref().len() <= (1 << 20) + 200,
        "READ reply of {} bytes",
        r.get_ref().len()
    );
    let _a: nfsserve::nfs::post_op_attr = dec(&mut r);
    assert_eq!(dec::<u32>(&mut r), 1 << 20);

    let (_, d) = c.mkdir(&root, "d");
    let d = d.unwrap();
    for i in 0..6000 {
        c.create_file(&d, &format!("file-with-a-longish-name-{i:05}"));
    }
    for (proc, extra) in [(16u32, false), (17, true)] {
        let mut a = Args::new().put(&d).put(&0u64).put(&[0u8; 8]).put(&u32::MAX);
        if extra {
            a = a.put(&u32::MAX);
        }
        let (st, r) = c.call(proc, a);
        assert_eq!(st, OK);
        let len = r.get_ref().len();
        println!("proc {proc} with count u32::MAX: {len} byte reply");
        assert!(len <= 300 * 1024, "reply of {len} bytes");
    }
    let mut cookie = 0;
    let mut seen = 0;
    loop {
        let (_, page, eof) = c.readdir_page(&d, cookie, true, u32::MAX / 8);
        seen += page.len();
        if eof || page.is_empty() {
            break;
        }
        cookie = page.last().unwrap().cookie;
    }
    assert_eq!(seen, 6000, "paging with huge counts still lists everything");
}

#[test]
fn a_replaced_sidecar_in_hide_mode_goes_stale() {
    let o = MountOptions {
        appledouble: cowfs_nfs::AppleDoubleMode::Hide,
        ..MountOptions::default()
    };
    let (_s, mut c) = serve(ReusingVfs::new(), o);
    let root = c.root.clone();
    c.create_file(&root, "a");
    c.create_file(&root, "._a");
    c.create_file(&root, "b");
    let old_side = c.create_file(&root, "._b");
    assert_eq!(c.rename(&root, "a", &root, "b"), OK);
    assert_eq!(
        c.getattr(&old_side).0,
        STALE,
        "the replaced sidecar's handle is stale"
    );
    let fresh = c.create_file(&root, "c");
    assert_ne!(fresh.data, old_side.data);
    assert_eq!(
        c.getattr(&old_side).0,
        STALE,
        "even when its number is reused"
    );
}

// ---- retransmission --------------------------------------------------------------------------

#[test]
fn a_retransmitted_remove_gets_the_original_reply_on_the_same_connection() {
    let (_s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    c.create_file(&root, "gone");
    c.set_next_xid(7777);
    assert_eq!(c.remove(&root, "gone"), OK);
    c.set_next_xid(7777);
    assert_eq!(
        c.remove(&root, "gone"),
        OK,
        "replayed, not re-executed into NOENT"
    );
    c.set_next_xid(7778);
    assert_eq!(c.remove(&root, "gone"), NOENT, "a new xid is a new call");
}

#[test]
fn a_retransmission_on_a_new_connection_is_replayed_too() {
    let (s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    c.create_file(&root, "gone");
    c.set_next_xid(4242);
    assert_eq!(c.remove(&root, "gone"), OK);
    drop(c);
    let mut again = Nfs::attach(s.port(), root.clone());
    again.set_next_xid(4242);
    assert_eq!(
        again.remove(&root, "gone"),
        OK,
        "the client reconnected and resent"
    );
    again.set_next_xid(4242);
    assert_eq!(
        again.remove(&root, "other"),
        NOENT,
        "same xid, different call"
    );
}

#[test]
fn every_non_idempotent_procedure_replays() {
    let (_s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    let mut xid = 9000;
    let mut twice = |c: &mut Nfs, f: &mut dyn FnMut(&mut Nfs) -> u32, what: &str| {
        xid += 1;
        c.set_next_xid(xid);
        let a = f(c);
        c.set_next_xid(xid);
        let b = f(c);
        assert_eq!((a, b), (OK, OK), "{what}");
    };
    twice(
        &mut c,
        &mut |c| c.create(&root, "f", 1, sattr_mode(0o644), [0; 8]).0,
        "guarded CREATE",
    );
    twice(&mut c, &mut |c| c.mkdir(&root, "d").0, "MKDIR");
    twice(&mut c, &mut |c| c.symlink(&root, "l", "t").0, "SYMLINK");
    let f = c.must_lookup(&root, "f");
    twice(&mut c, &mut |c| c.link(&f, &root, "h").0, "LINK");
    twice(&mut c, &mut |c| c.rename(&root, "f", &root, "f2"), "RENAME");
    twice(&mut c, &mut |c| c.remove(&root, "h"), "REMOVE");
    twice(&mut c, &mut |c| c.rmdir(&root, "d"), "RMDIR");
    twice(
        &mut c,
        &mut |c| c.setattr(&f, sattr_mode(0o600)).0,
        "SETATTR",
    );
}

// ---- connection budget and record deadlines (round 2) ----------------------------------------

/// A NULL call: well formed, needs no handle, and gets an answer.
fn null_frame() -> Vec<u8> {
    let mut m = Vec::new();
    for w in [1u32, 0, 2, 100_003, 3, 0, 0, 0, 0, 0] {
        m.extend_from_slice(&w.to_be_bytes());
    }
    let mut f = (m.len() as u32 | 1 << 31).to_be_bytes().to_vec();
    f.extend_from_slice(&m);
    f
}

fn answered(s: &mut TcpStream) -> bool {
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut h = [0u8; 4];
    s.read_exact(&mut h).is_ok()
}

#[test]
fn served_but_silent_connections_do_not_lock_the_client_out() {
    let limits = Limits {
        max_connections: 4,
        ..Limits::default()
    };
    let (s, c) = serve(memfs(), opts(limits));
    let root = c.root.clone();
    let mut hold = vec![];
    for _ in 0..3 {
        let mut x = connect_raw(s.port());
        x.write_all(&null_frame()).unwrap();
        assert!(answered(&mut x), "the silent connection is served");
        hold.push(x);
    }
    let mut late = connect_raw(s.port());
    late.write_all(&null_frame()).unwrap();
    assert!(
        answered(&mut late),
        "a silent local process locked the client out of a reconnect"
    );
    drop(hold);
    let mut after = Nfs::attach(s.port(), root.clone());
    assert_eq!(
        after.getattr(&root).0,
        OK,
        "and the server still serves new clients"
    );
}

#[test]
fn empty_fragments_do_not_extend_the_record_deadline() {
    let limits = Limits {
        frame_timeout: Duration::from_secs(2),
        ..Limits::default()
    };
    let (s, _c) = serve(memfs(), opts(limits));
    let mut x = connect_raw(s.port());
    let start = Instant::now();
    for _ in 0..8 {
        // A closed connection answers with a reset, which is the answer being tested for.
        if x.write_all(&0u32.to_be_bytes()).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(900));
    }
    // Nothing to read and no error means the connection is still open and waiting.
    let _ = x.set_read_timeout(Some(Duration::from_millis(200)));
    let mut b = [0u8; 8];
    let alive = matches!(
        x.read(&mut b),
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
    );
    eprintln!(
        "empty fragments: alive after {:?}: {alive}",
        start.elapsed()
    );
    assert!(
        !alive,
        "frame_timeout is 2 s but empty fragments kept it open"
    );
}

#[test]
fn a_record_may_not_exceed_the_cap_across_fragments() {
    let limits = Limits {
        max_frame: 512 * 1024,
        ..Limits::default()
    };
    let (s, mut c) = serve(memfs(), opts(limits));
    let root = c.root.clone();
    let mut x = connect_raw(s.port());
    // 400 KiB fragments, none of them over the cap on its own. A reset mid-write is the answer
    // being tested for, so it is not an error here.
    let chunk = vec![0u8; 400 * 1024];
    for _ in 0..8 {
        if x.write_all(&(400u32 * 1024).to_be_bytes()).is_err() {
            break;
        }
        if x.write_all(&chunk).is_err() {
            break;
        }
    }
    assert!(
        closed_within(&mut x, 10),
        "the cap is for the whole record, not for one fragment"
    );
    assert_eq!(c.getattr(&root).0, OK, "the server is unharmed");
}
