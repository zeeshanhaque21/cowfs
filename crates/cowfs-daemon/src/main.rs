//! `cowfs-daemon serve`: the daemon as its own binary, so it can run without the `cowfs` CLI
//! and so the end-to-end tests can start and kill a real process.
//!
//! `cowfs serve` is the documented entry point and calls the same library.

use clap::Parser;
use cowfs_ctl::default_socket_path;
use cowfs_daemon::{prepare_platform, Daemon, DaemonConfig};
use signal_hook::consts::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

#[derive(Debug, Parser)]
#[command(
    name = "cowfs-daemon",
    version,
    about = "Serve a cowfs store over a mount and the control API"
)]
struct Cli {
    /// Block store directory; one subdirectory per snapshot under the path backend
    #[arg(long)]
    store: PathBuf,
    /// Which backend serves the store: core is the real one
    #[arg(long, value_enum, default_value_t = cowfs_daemon::BackendKind::Core)]
    backend: cowfs_daemon::BackendKind,
    /// Where the default mount goes
    #[arg(long)]
    mount: PathBuf,
    /// Control socket path (default: per-user, as for cowfs)
    #[arg(long)]
    socket: Option<PathBuf>,
    /// A directory a client may export a snapshot inside. Repeatable; without one,
    /// mount_snapshot refuses every path
    #[arg(long = "export-root")]
    export_root: Vec<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut config = DaemonConfig::new(
        &cli.store,
        &cli.mount,
        cli.socket.clone().unwrap_or_else(default_socket_path),
    );
    for root in &cli.export_root {
        config = config.with_export_root(root);
    }
    config = config.with_backend(cli.backend);
    // Before anything is mounted: a mount left by a killed daemon hangs every `ls` on it for
    // twenty seconds or more on macOS 26, so it is swept first.
    prepare_platform(&config.mount);
    let daemon = match Daemon::start(&config) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cowfs-daemon: {e}");
            return ExitCode::from(1);
        }
    };
    eprintln!(
        "cowfs-daemon: serving {} on {} with the {} adapter, socket {}",
        config.mount.display(),
        daemon.mount_path().display(),
        cowfs_daemon::mounts::adapter_name(),
        daemon.socket().display()
    );
    let second = Arc::new(AtomicBool::new(false));
    if let Err(e) = watch_signals(&daemon, Arc::clone(&second)) {
        eprintln!("cowfs-daemon: cannot watch signals: {e}");
        return ExitCode::from(1);
    }
    let problems = daemon.run();
    for p in problems {
        eprintln!("cowfs-daemon: {p}");
    }
    ExitCode::from(0)
}

/// Unmounts in order on the first signal, then asks the control server to stop. A second
/// signal exits at once: the operator asked twice, and a daemon that will not stop is worse
/// than one that stops untidily.
fn watch_signals(daemon: &Arc<Daemon>, second: Arc<AtomicBool>) -> std::io::Result<()> {
    let mut signals = Signals::new([SIGINT, SIGTERM])?;
    let daemon = Arc::clone(daemon);
    thread::Builder::new()
        .name("cowfs-daemon-signals".into())
        .spawn(move || {
            for sig in signals.forever() {
                if second.swap(true, Ordering::SeqCst) {
                    eprintln!("cowfs-daemon: second signal {sig}, exiting now");
                    std::process::exit(130);
                }
                eprintln!("cowfs-daemon: signal {sig}, shutting down");
                daemon.stop();
            }
        })?;
    Ok(())
}
