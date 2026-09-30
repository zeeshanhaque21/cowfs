#![allow(dead_code)]

use cowfs_ctl::*;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Aborts the whole test binary when a test runs longer than `secs`, so a hang fails CI.
pub struct Watchdog(mpsc::Sender<()>);

impl Watchdog {
    pub fn start(secs: u64) -> Watchdog {
        let (tx, rx) = mpsc::channel::<()>();
        let name = thread::current().name().unwrap_or("?").to_owned();
        thread::spawn(move || {
            if let Err(mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(Duration::from_secs(secs))
            {
                eprintln!("WATCHDOG: test {name} exceeded {secs}s, aborting the test binary");
                std::process::abort();
            }
        });
        Watchdog(tx)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

/// Runs `f` on its own thread and fails the test when it takes longer than `secs`.
pub fn bounded<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)));
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(Ok(v)) => v,
        Ok(Err(p)) => std::panic::resume_unwind(p),
        Err(_) => panic!("timed out after {secs}s"),
    }
}

pub fn private_tempdir() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    dir
}

pub struct Fixture {
    pub _dir: TempDir,
    pub path: PathBuf,
    pub server: Option<Server>,
    _watchdog: Watchdog,
}

impl Fixture {
    pub fn server(&self) -> &Server {
        self.server.as_ref().unwrap()
    }
}

pub fn start_with(handler: impl ControlHandler + 'static, opts: ServerOptions) -> Fixture {
    let watchdog = Watchdog::start(120);
    let dir = private_tempdir();
    let path = dir.path().join("c.sock");
    let server = Server::start(&path, Arc::new(handler), opts).unwrap();
    Fixture {
        _dir: dir,
        path,
        server: Some(server),
        _watchdog: watchdog,
    }
}

/// A fixture from an already-boxed handler, so a test can keep a handle to it.
pub fn start_arc(handler: Arc<dyn ControlHandler>, opts: ServerOptions) -> Fixture {
    let watchdog = Watchdog::start(120);
    let dir = private_tempdir();
    let path = dir.path().join("c.sock");
    let server = Server::start(&path, handler, opts).unwrap();
    Fixture {
        _dir: dir,
        path,
        server: Some(server),
        _watchdog: watchdog,
    }
}

pub fn start(handler: impl ControlHandler + 'static) -> Fixture {
    start_with(handler, ServerOptions::default())
}

pub fn stub() -> StubHandler {
    StubHandler::new("/store", "/mount")
}

pub struct Raw {
    pub stream: UnixStream,
    pub reader: BufReader<UnixStream>,
}

impl Raw {
    pub fn connect(path: &Path) -> Raw {
        let stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        Raw { stream, reader }
    }

    pub fn hello(path: &Path) -> Raw {
        let mut r = Raw::connect(path);
        r.send(r#"{"type":"hello","versions":[1]}"#);
        assert_eq!(r.recv()["type"], "hello");
        r
    }

    pub fn send(&mut self, line: &str) {
        self.stream.write_all(line.as_bytes()).unwrap();
        self.stream.write_all(b"\n").unwrap();
    }

    pub fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).unwrap();
        assert!(n > 0, "unexpected EOF");
        serde_json::from_str(&line).unwrap()
    }

    /// The first frame that is not progress.
    pub fn recv_final(&mut self) -> Value {
        loop {
            let f = self.recv();
            if f["type"] != "progress" {
                return f;
            }
        }
    }

    pub fn assert_eof(&mut self) {
        let mut line = String::new();
        assert_eq!(
            self.reader.read_line(&mut line).unwrap(),
            0,
            "expected EOF, got {line:?}"
        );
    }
}

pub fn wait_for(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

pub fn code(err: ClientError) -> ErrorCode {
    match err {
        ClientError::Server(e) => e.code,
        other => panic!("expected a server error, got {other:?}"),
    }
}

/// Threads in this process, from the OS.
pub fn thread_count() -> usize {
    let pid = std::process::id().to_string();
    if cfg!(target_os = "linux") {
        std::fs::read_dir("/proc/self/task").map_or(0, Iterator::count)
    } else {
        let out = std::process::Command::new("ps")
            .args(["-M", "-p", &pid])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .count()
            .saturating_sub(1)
    }
}
