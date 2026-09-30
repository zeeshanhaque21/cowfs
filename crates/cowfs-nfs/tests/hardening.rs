//! Security, resource-bound, retransmission and inode-reuse tests at the protocol level.
mod common;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use common::reuse::ReusingVfs;
use common::*;
use cowfs_nfs::MountOptions;
use nfsserve::nfs::{nfs_fh3, nfsstat3};
use nfsserve::tcp::Limits;

const MNT_ACCES: u32 = 13;

fn opts(limits: Limits) -> MountOptions {
    MountOptions {
        limits,
        ..MountOptions::default()
    }
}

fn connect_raw(port: u16) -> TcpStream {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s
}

/// True if the server closes `s` within `secs`.
fn closed_within(s: &mut TcpStream, secs: u64) -> bool {
    let t = Instant::now();
    s.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut b = [0u8; 16];
    while t.elapsed() < Duration::from_secs(secs) {
        match s.read(&mut b) {
            Ok(0) => return true,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => return true,
            Ok(_) | Err(_) => {}
        }
    }
    false
}

fn fd_limit() -> usize {
    let out = std::process::Command::new("/bin/sh")
        .args(["-c", "ulimit -n"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(256)
}

// ---- mount gate and handle MACs -------------------------------------------------------------

#[test]
fn only_the_first_mnt_gets_the_root_handle() {
    let (s, mut first) = serve(memfs(), MountOptions::default());
    let root = first.root.clone();
    let mut other = Nfs::attach(s.port(), nfs_fh3::default());
    assert_eq!(other.mount().0, MNT_ACCES, "a second process cannot mount");
    assert_eq!(
        first.mount().0,
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
        again.mount().0,
        0,
        "a deliberate remount is possible after rearm"
    );
    assert_eq!(other.mount().0, MNT_ACCES, "and closes the gate behind it");
}

#[test]
fn a_gate_off_server_lets_everyone_mount() {
    let o = MountOptions {
        one_shot_mount: false,
        ..MountOptions::default()
    };
    let (s, _c) = serve(memfs(), o);
    assert_eq!(Nfs::attach(s.port(), nfs_fh3::default()).mount().0, 0);
}

#[test]
fn forged_and_guessed_handles_are_refused() {
    let (s, mut legit) = serve(memfs(), MountOptions::default());
    let root = legit.root.clone();
    let secret = legit.create_file(&root, "secret");
    legit.write(&secret, 0, b"top secret", 2);
    let mut attacker = Nfs::attach(s.port(), nfs_fh3::default());

    let mut wrong_ino = secret.data.clone();
    wrong_ino[8..16].copy_from_slice(&1u64.to_le_bytes());
    let mut wrong_mac = secret.data.clone();
    wrong_mac[39] ^= 0xff;
    let mut zero_mac = secret.data.clone();
    zero_mac[24..].fill(0);
    let mut plain = secret.data[..24].to_vec();
    plain.extend_from_slice(&[0; 16]);
    for (why, data) in [
        ("another inode, old MAC", wrong_ino),
        ("flipped MAC", wrong_mac),
        ("zero MAC", zero_mac),
        ("no MAC", plain),
        ("short", secret.data[..24].to_vec()),
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
        d[24..].copy_from_slice(&guess.to_le_bytes().repeat(2));
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

// ---- resource bounds -------------------------------------------------------------------------

#[test]
fn oversized_frames_close_the_connection_before_any_allocation() {
    let (s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    let before = rss_bytes();
    let mut socks: Vec<TcpStream> = (0..20)
        .map(|_| {
            let mut b = connect_raw(s.port());
            b.write_all(&((8u32 << 20) | 1 << 31).to_be_bytes())
                .unwrap();
            b
        })
        .collect();
    for b in &mut socks {
        assert!(closed_within(b, 5), "an 8 MiB frame is refused");
    }
    assert!(rss_bytes().saturating_sub(before) < 30 << 20);
    assert_eq!(c.getattr(&root).0, OK);
}

#[test]
fn half_sent_frames_keep_memory_bounded() {
    let want = 200usize;
    let n = want.min((fd_limit().saturating_sub(120)) / 2);
    let limits = Limits {
        max_connections: 1000,
        frame_timeout: Duration::from_secs(60),
        ..Limits::default()
    };
    let (s, mut c) = serve(memfs(), opts(limits));
    let root = c.root.clone();
    let before = rss_bytes();
    let socks: Vec<TcpStream> = (0..n)
        .map(|_| {
            let mut b = connect_raw(s.port());
            b.write_all(&((1u32 << 20) | 1 << 31).to_be_bytes())
                .unwrap();
            b.write_all(&[0; 8]).unwrap();
            b
        })
        .collect();
    std::thread::sleep(Duration::from_millis(500));
    let grown = rss_bytes().saturating_sub(before);
    println!("{n} half-sent 1 MiB frames: RSS grew {} MiB", grown >> 20);
    assert!(grown < 60 << 20, "RSS grew by {} MiB", grown >> 20);
    assert_eq!(c.getattr(&root).0, OK, "the server still answers");
    drop(socks);
}

#[test]
fn a_connection_flood_is_capped_and_the_server_recovers() {
    let n = 10_000usize.min(fd_limit().saturating_sub(120) / 2);
    let (s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    let mut socks = Vec::new();
    for _ in 0..n {
        match TcpStream::connect(("127.0.0.1", s.port())) {
            Ok(x) => socks.push(x),
            Err(_) => break,
        }
    }
    println!(
        "flood: {} connections opened (fd limit {}), cap {}",
        socks.len(),
        fd_limit(),
        Limits::default().max_connections
    );
    std::thread::sleep(Duration::from_millis(500));
    let mut open = 0;
    for x in &mut socks {
        x.set_read_timeout(Some(Duration::from_millis(1))).unwrap();
        let mut b = [0u8; 1];
        if !matches!(x.read(&mut b), Ok(0)) {
            open += 1;
        }
    }
    assert!(
        open <= Limits::default().max_connections,
        "{open} connections held open"
    );
    drop(socks);
    std::thread::sleep(Duration::from_millis(300));
    let mut c2 = Nfs::attach(s.port(), root.clone());
    assert_eq!(
        c2.getattr(&root).0,
        OK,
        "a new client is served after the flood"
    );
    assert_eq!(c.getattr(&root).0, OK);
}

#[test]
fn slow_and_idle_connections_time_out() {
    let limits = Limits {
        frame_timeout: Duration::from_millis(400),
        idle_timeout: Duration::from_millis(900),
        ..Limits::default()
    };
    let (s, mut c) = serve(memfs(), opts(limits));
    let root = c.root.clone();

    let mut silent = connect_raw(s.port());
    assert!(closed_within(&mut silent, 4), "no request at all");
    let mut slow = connect_raw(s.port());
    slow.write_all(&[0x80, 0, 0]).unwrap();
    assert!(closed_within(&mut slow, 4), "three of four header bytes");
    let mut trickle = connect_raw(s.port());
    trickle
        .write_all(&(100u32 | 1 << 31).to_be_bytes())
        .unwrap();
    trickle.write_all(&[0; 10]).unwrap();
    assert!(closed_within(&mut trickle, 4), "a body that stops");

    let mut idle = Nfs::attach(s.port(), root.clone());
    assert_eq!(idle.getattr(&root).0, OK);
    std::thread::sleep(Duration::from_millis(1800));
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

#[test]
fn the_reply_cache_stays_small_under_a_long_run() {
    let (_s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    let before = rss_bytes();
    for i in 0..20_000u32 {
        c.set_next_xid(i + 1);
        c.create(&root, "f", 0, sattr_mode(0o644), [0; 8]);
    }
    let grown = rss_bytes().saturating_sub(before);
    println!("20000 cached calls: RSS grew {} MiB", grown >> 20);
    assert!(grown < 40 << 20);
    let _ = nfsstat3::NFS3_OK;
}
