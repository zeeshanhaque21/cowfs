//! A connection flood must not grow the server without bound. Own test binary: it counts the
//! threads of the whole process.

mod common;

use common::*;
use cowfs_ctl::*;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

#[test]
fn connection_flood_keeps_threads_bounded_and_the_server_responsive() {
    let fx = start(stub());
    let base = thread_count();
    let mut held = Vec::new();
    for _ in 0..100 {
        held.push(UnixStream::connect(&fx.path).unwrap());
    }
    std::thread::sleep(Duration::from_millis(800));
    let grown = thread_count().saturating_sub(base);
    assert!(
        grown <= 64 + 8,
        "{grown} extra threads for 100 idle connections"
    );
    drop(held);

    let started = Instant::now();
    let (mut ok, mut refused, mut peak) = (0, 0, 0);
    for i in 0..3000 {
        match UnixStream::connect(&fx.path) {
            Ok(s) => {
                ok += 1;
                drop(s);
            }
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => refused += 1,
            Err(e) => panic!("{e}"),
        }
        if i % 500 == 0 {
            peak = peak.max(thread_count().saturating_sub(base));
        }
    }
    // How many connects get through depends on the listen backlog, which is a kernel setting.
    // What must hold is the service: the thread count stays bounded and a real client is served.
    eprintln!("flood: ok={ok} refused={refused}");
    // At most one thread per admitted connection (64) plus the accept thread, the request
    // threads of at most a couple of clients, and the harness itself. 3000 connects must not
    // multiply that.
    let cap = 64 + 32;
    assert!(
        peak <= cap,
        "{peak} extra threads during a 3000 connect flood, cap {cap}"
    );
    let mut c = bounded_connect(&fx.path);
    assert!(c.call(Request::Ping(Empty {})).is_ok());
    assert!(started.elapsed() < Duration::from_secs(60));
    assert!(thread_count().saturating_sub(base) <= cap);
}

fn bounded_connect(path: &std::path::Path) -> Client {
    let p = path.to_owned();
    bounded(10, move || {
        for _ in 0..50 {
            if let Ok(c) = Client::connect(&p) {
                return c;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("server never answered a legitimate client");
    })
}
