//! Child-process fixture for #77. A stuck handler that ignores cancellation, one client, and an
//! immediate `process::exit(0)` after `Server::wait()` returns. `cowfs serve` exits as soon as
//! `wait()` returns, so this reproduces the window where a detached abandon worker loses the race
//! against process exit and the client sees a bare EOF instead of `shutting_down`.
//!
//! Invoked by `tests/progress_shutdown.rs::terminal_frame_survives_process_exit_after_wait`.
//!
//! Args: <socket-path> <deadline-ms> <mode>
//!   mode `stuck`   : handler ignores cancellation (client is reading, write lock is free)
//!   mode `blocked` : handler streams huge progress to an unreading client (write lock held)

use cowfs_ctl::*;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

struct Stuck;

impl ControlHandler for Stuck {
    fn fsck(&self, _ctx: &OpContext<'_>) -> CtlResult<FsckReport> {
        println!("entered");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

struct Flood;

impl ControlHandler for Flood {
    fn gc(&self, _: GcParams, ctx: &OpContext<'_>) -> CtlResult<GcReport> {
        println!("entered");
        std::io::stdout().flush().unwrap();
        loop {
            ctx.progress(ProgressEvent {
                phase: "mark".into(),
                done: 0,
                total: None,
                unit: Unit::Items,
                message: Some("x".repeat(1 << 20)),
            })?;
        }
    }
}

fn main() {
    let mut a = std::env::args().skip(1);
    let sock = a.next().unwrap();
    let deadline: u64 = a.next().unwrap().parse().unwrap();
    let mode = a.next().unwrap_or_else(|| "stuck".into());
    let handler: Arc<dyn ControlHandler> = if mode == "blocked" {
        Arc::new(Flood)
    } else {
        Arc::new(Stuck)
    };
    let server = Server::start(
        Path::new(&sock),
        handler,
        ServerOptions {
            shutdown_deadline: Duration::from_millis(deadline),
            write_timeout: Duration::from_secs(4),
            ..ServerOptions::default()
        },
    )
    .unwrap();
    println!("ready");
    std::io::stdout().flush().unwrap();
    let handle = server.handle();
    std::thread::spawn(move || {
        let mut l = String::new();
        std::io::stdin().read_line(&mut l).ok();
        handle.shutdown();
    });
    server.wait();
    // `cowfs serve` returns from `wait()` and the process exits at once. Any frame not already on
    // the wire is lost.
    std::process::exit(0);
}
