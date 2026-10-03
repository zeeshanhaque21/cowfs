//! Resource-bound tests that measure a process-wide resource: the descriptor table or the resident
//! set. The server runs in the test process, so a neighbouring test thread opening sockets or
//! allocating shows up in the measurement. These tests therefore live in a binary of their own and
//! take `exclusive()`, so nothing else in the process runs while one of them measures.
mod common;

use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use common::*;
use cowfs_nfs::MountOptions;
use nfsserve::tcp::Limits;

/// Held for the whole test. A failed test poisons the lock, which must not fail the next one.
fn exclusive() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What to assume when the shell cannot report the descriptor limit.
const FALLBACK_FD_LIMIT: usize = 256;
/// Descriptors kept free for the child processes that read the limit and the resident set.
const SPAWN_RESERVE: usize = 32;

fn parse_limit(printed: &str) -> Option<usize> {
    match printed.trim() {
        "unlimited" => Some(1 << 20),
        n => n.parse().ok(),
    }
}

/// The soft descriptor limit, read once. Never panics: a limit that cannot be read is reported and
/// replaced by `FALLBACK_FD_LIMIT`, so "small limit" and "could not tell" both end in a smaller flood.
fn fd_limit() -> usize {
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        let read = Command::new("/bin/sh")
            .args(["-c", "ulimit -n"])
            .output()
            .map_err(|e| format!("cannot spawn sh: {e}"))
            .and_then(|out| {
                let printed = String::from_utf8_lossy(&out.stdout).into_owned();
                parse_limit(&printed).ok_or_else(|| format!("unparsable output {printed:?}"))
            });
        read.unwrap_or_else(|why| {
            eprintln!("fd limit unreadable ({why}); assuming {FALLBACK_FD_LIMIT}");
            FALLBACK_FD_LIMIT
        })
    })
}

fn open_fds() -> usize {
    std::fs::read_dir("/dev/fd").expect("list /dev/fd").count()
}

/// How many in-process connections (a client and a server descriptor each) fit in the descriptors
/// that are still free, up to `want`.
fn affordable(want: usize) -> usize {
    let free = fd_limit().saturating_sub(open_fds() + SPAWN_RESERVE);
    want.min(free / 2)
}

#[test]
fn the_limit_reader_accepts_what_ulimit_prints() {
    assert_eq!(parse_limit("256\n"), Some(256));
    assert_eq!(parse_limit("unlimited\n"), Some(1 << 20));
    assert_eq!(parse_limit(""), None);
    assert_eq!(parse_limit("sh: ulimit: error\n"), None);
}

#[test]
fn oversized_frames_close_the_connection_before_any_allocation() {
    let _x = exclusive();
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
    let grown = rss_bytes().saturating_sub(before);
    println!("20 oversized frames: RSS grew {} MiB", grown >> 20);
    assert!(grown < 30 << 20, "RSS grew by {} MiB", grown >> 20);
    assert_eq!(c.getattr(&root).0, OK);
}

#[test]
fn half_sent_frames_keep_memory_bounded() {
    let _x = exclusive();
    let limits = Limits {
        max_connections: 1000,
        frame_timeout: Duration::from_secs(60),
        ..Limits::default()
    };
    let (s, mut c) = serve(memfs(), opts(limits));
    let root = c.root.clone();
    let n = affordable(200);
    assert!(
        n >= 32,
        "fd limit {} leaves room for {n} half-sent frames, too few to see per-connection growth; raise it (ulimit -n)",
        fd_limit()
    );
    let declared: u64 = 1 << 20;
    let before = rss_bytes();
    let socks: Vec<TcpStream> = (0..n)
        .map(|_| {
            let mut b = connect_raw(s.port());
            b.write_all(&((declared as u32) | 1 << 31).to_be_bytes())
                .unwrap();
            b.write_all(&[0; 8]).unwrap();
            b
        })
        .collect();
    // The peak over the settling window counts, so a server that buffers late is still seen.
    let mut grown = 0;
    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(100));
        grown = grown.max(rss_bytes().saturating_sub(before));
    }
    let bound = n as u64 * (declared / 4);
    println!(
        "{n} half-sent 1 MiB frames: RSS grew {} KiB ({} KiB per connection, bound {} KiB)",
        grown >> 10,
        (grown / n as u64) >> 10,
        bound >> 10
    );
    assert!(
        grown < bound,
        "RSS grew by {} KiB for {n} connections, over a quarter of each declared frame",
        grown >> 10
    );
    assert_eq!(c.getattr(&root).0, OK, "the server still answers");
    drop(socks);
}

#[test]
fn a_connection_flood_is_capped_and_the_server_recovers() {
    let _x = exclusive();
    let cap = Limits::default().max_connections;
    let (s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    let n = affordable(10_000);
    assert!(
        n > cap,
        "fd limit {} leaves room for {n} flood connections, not more than the cap {cap}; raise it (ulimit -n)",
        fd_limit()
    );
    let mut socks = Vec::new();
    for _ in 0..n {
        match TcpStream::connect(("127.0.0.1", s.port())) {
            Ok(x) => socks.push(x),
            Err(_) => break,
        }
    }
    println!(
        "flood: {} of {n} connections opened (fd limit {}), cap {cap}",
        socks.len(),
        fd_limit(),
    );
    assert!(
        socks.len() > cap,
        "only {} connections opened, not more than the cap {cap}, so the cap was never exercised",
        socks.len()
    );
    std::thread::sleep(Duration::from_millis(500));
    let mut open = 0;
    for x in &mut socks {
        let _ = x.set_read_timeout(Some(Duration::from_millis(1)));
        let mut b = [0u8; 1];
        // A kicked connection is closed or reset, both of which count as not open.
        let alive = match x.read(&mut b) {
            Ok(0) => false,
            Err(e)
                if e.kind() == ErrorKind::ConnectionReset || e.kind() == ErrorKind::BrokenPipe =>
            {
                false
            }
            Ok(_) | Err(_) => true,
        };
        open += usize::from(alive);
    }
    assert!(open <= cap, "{open} connections held open");
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
fn the_reply_cache_stays_small_under_a_long_run() {
    let _x = exclusive();
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
}
