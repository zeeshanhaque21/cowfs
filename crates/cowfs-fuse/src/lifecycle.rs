//! Unmounting, clearing stale mounts and cleaning up on signals.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use crate::mounts;
use crate::MountError;

/// How a mount ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unmounted {
    /// The mount is gone and its request loop has stopped.
    Clean,
    /// A process still had a file open or a working directory inside, so the mount was only
    /// detached from the file system tree (`fusermount3 -u -z`). It keeps serving those
    /// processes and is destroyed when they let go.
    Lazy,
}

fn mounts_text() -> String {
    fs::read_to_string("/proc/mounts").unwrap_or_default()
}

pub(crate) fn is_mounted(path: &Path) -> bool {
    mounts::is_mounted(&mounts_text(), path)
}

/// Runs `fusermount3 -u` (or `fusermount`), lazily with `-z`. Returns its error text on failure.
fn fusermount(path: &Path, lazy: bool) -> Result<(), String> {
    let mut last = "neither fusermount3 nor fusermount could be run".to_owned();
    for bin in ["fusermount3", "fusermount"] {
        let mut c = Command::new(bin);
        c.arg("-u");
        if lazy {
            c.arg("-z");
        }
        match c.arg("--").arg(path).output() {
            Ok(o) if o.status.success() => return Ok(()),
            Ok(o) => return Err(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
            Err(e) => last = format!("{bin}: {e}"),
        }
    }
    Err(last)
}

/// Unmounts `path`. A busy mount is retried for `timeout` and then unmounted lazily.
pub(crate) fn unmount(path: &Path, timeout: Duration) -> Result<Unmounted, MountError> {
    let deadline = Instant::now() + timeout;
    loop {
        match fusermount(path, false) {
            Ok(()) => return Ok(Unmounted::Clean),
            Err(_) if !is_mounted(path) => return Ok(Unmounted::Clean),
            Err(e) if Instant::now() >= deadline => {
                log::warn!("{} is busy ({e}); unmounting lazily", path.display());
                break;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    match fusermount(path, true) {
        Ok(()) => Ok(Unmounted::Lazy),
        Err(_) if !is_mounted(path) => Ok(Unmounted::Clean),
        Err(e) => Err(MountError::Unmount(format!("{}: {e}", path.display()))),
    }
}

/// True when `path` is a mount whose server is gone: `stat` fails with `ENOTCONN`. A mount
/// that does not answer within two seconds is treated as alive.
fn is_dead(path: &Path) -> bool {
    let (tx, rx) = mpsc::channel();
    let p = path.to_owned();
    std::thread::spawn(move || {
        let _ = tx.send(fs::metadata(p));
    });
    matches!(
        rx.recv_timeout(Duration::from_secs(2)),
        Ok(Err(e)) if e.raw_os_error() == Some(libc::ENOTCONN)
    )
}

/// Lazily unmounts every cowfs mount at or below `prefix` whose server process is gone, for
/// example after `kill -9` of the host. Call it at startup. Returns the mount points cleared.
pub fn sweep_stale_mounts(prefix: impl AsRef<Path>) -> Vec<PathBuf> {
    mounts::cowfs_mounts(&mounts_text(), prefix.as_ref())
        .into_iter()
        .filter(|p| is_dead(p))
        .filter(|p| match fusermount(p, true) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("cannot clear stale mount {}: {e}", p.display());
                false
            }
        })
        .collect()
}

static REGISTRY: Mutex<Vec<(u64, PathBuf, Duration)>> = Mutex::new(Vec::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static INSTALLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn register(path: &Path, timeout: Duration) -> u64 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((id, path.to_owned(), timeout));
    id
}

pub(crate) fn deregister(id: u64) {
    REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|e| e.0 != id);
}

/// See `Mount::install_signal_cleanup`.
pub(crate) fn install_signal_cleanup() -> io::Result<()> {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP]).inspect_err(|_| {
        INSTALLED.store(false, Ordering::SeqCst);
    })?;
    std::thread::Builder::new()
        .name("cowfs-signal".into())
        .spawn(move || {
            let Some(sig) = signals.forever().next() else {
                return;
            };
            let mounts = std::mem::take(&mut *REGISTRY.lock().unwrap_or_else(|e| e.into_inner()));
            for (_, path, timeout) in mounts {
                if let Err(e) = unmount(&path, timeout) {
                    log::error!("{e}");
                }
            }
            let _ = signal_hook::low_level::emulate_default_handler(sig);
            std::process::exit(128 + sig);
        })?;
    Ok(())
}
