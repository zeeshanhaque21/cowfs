//! Getting rid of mounts whose server is gone. A mount whose server died hangs every `ls` and
//! `stat` on it until it is force-unmounted, so a host that is killed must clean up first, and a
//! host that starts after one was killed must sweep what is left.
use std::io;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, Once, PoisonError};
use std::time::Duration;

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

use crate::mount::{checked, unmount_path, MountError};

const NFSSTAT: &str = "/usr/bin/nfsstat";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);

static MOUNTS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static INSTALLED: Once = Once::new();

pub(crate) fn register(path: &Path) {
    MOUNTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(path.to_path_buf());
}

pub(crate) fn unregister(path: &Path) {
    MOUNTS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|p| p != path);
}

/// Unmounts every live [`Mount`](crate::Mount) of this process (`umount`, then `umount -f`) on
/// SIGTERM, SIGINT and SIGHUP, then exits with status 128 plus the signal number. Call it once,
/// early. It cannot help against SIGKILL or a crash: use [`sweep_stale_mounts`] at startup for that.
pub fn install_signal_cleanup() -> io::Result<()> {
    let mut result = Ok(());
    INSTALLED.call_once(|| match Signals::new([SIGTERM, SIGINT, SIGHUP]) {
        Err(e) => result = Err(e),
        Ok(mut signals) => {
            result = std::thread::Builder::new()
                .name("cowfs-nfs-signals".into())
                .spawn(move || {
                    if let Some(sig) = signals.forever().next() {
                        let paths: Vec<PathBuf> = MOUNTS
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .clone();
                        for p in paths {
                            if let Err(e) = unmount_path(&p, COMMAND_TIMEOUT) {
                                eprintln!("cowfs-nfs: {e}");
                            }
                        }
                        std::process::exit(128 + sig);
                    }
                })
                .map(drop);
        }
    });
    result
}

/// Mounts of a cowfs export under `prefix` in the output of `nfsstat -m`, with the port they talk
/// to. Anything else is another file system's mount and is left alone.
fn localhost_mounts(nfsstat: &str, prefix: &Path) -> Vec<(PathBuf, u16)> {
    let mut out = Vec::new();
    let mut current: Option<PathBuf> = None;
    for line in nfsstat.lines() {
        if !line.starts_with(char::is_whitespace) {
            current = line
                .rsplit_once(" from ")
                .filter(|(_, server)| crate::mount::is_our_export(server))
                .map(|(path, _)| PathBuf::from(path))
                .filter(|p| p.starts_with(prefix));
        } else if let Some(path) = &current {
            let port = line
                .trim()
                .strip_prefix("NFS parameters:")
                .filter(|params| params.contains("vers=3"))
                .and_then(|params| {
                    params
                        .split(',')
                        .find_map(|o| o.trim().strip_prefix("port="))
                        .and_then(|p| p.parse().ok())
                });
            if let Some(port) = port {
                out.push((path.clone(), port));
                current = None;
            }
        }
    }
    out
}

fn answers(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

/// Force-unmounts the NFS mounts of `localhost:/` under `prefix` whose server no longer answers,
/// and returns their paths. Mounts of live servers, of other hosts and outside `prefix` are left
/// alone. Call it at startup with the directory under which the host puts its mount points.
pub fn sweep_stale_mounts(prefix: &Path) -> Result<Vec<PathBuf>, MountError> {
    let prefix = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.to_path_buf());
    let table = checked(Command::new(NFSSTAT).arg("-m"), COMMAND_TIMEOUT)?;
    let mut swept = Vec::new();
    for (path, port) in localhost_mounts(&table, &prefix) {
        if answers(port) {
            continue;
        }
        unmount_path(&path, COMMAND_TIMEOUT)?;
        swept.push(path);
    }
    Ok(swept)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COWFS: &str = "localhost:/cowfs-0123456789abcdef0123456789abcdef";
    const SAMPLE: &str = "\
/Users/z/OrbStack from OrbStack:/OrbStack
  -- Original mount options:
     NFS parameters: vers=4.0,port=63709,soft
/tmp/cowfs/a from COWFS
  -- Original mount options:
     General mount flags: 0x0
     NFS parameters: vers=3,tcp,port=11111,mountport=11111,locallocks,rsize=131072
/tmp/cowfs/b from localhost:/cowfs-ffffffffffffffffffffffffffffffff
  -- Original mount options:
     NFS parameters: locallocks,vers=3,tcp,rsize=1,wsize=1,actimeo=1,port=4711,mountport=4711
/tmp/other/c from localhost:/
  -- Original mount options:
     NFS parameters: vers=3,tcp,port=5
/tmp/cowfs/d from localhost:/cowfs-nothex
  -- Original mount options:
     NFS parameters: vers=3,tcp,port=7
/tmp/cowfs/e from otherhost:/cowfs-0123456789abcdef0123456789abcdef
  -- Original mount options:
     NFS parameters: vers=3,tcp,port=8
";

    #[test]
    fn only_localhost_v3_mounts_under_the_prefix_are_candidates() {
        let got = localhost_mounts(&SAMPLE.replace("COWFS", COWFS), Path::new("/tmp/cowfs"));
        assert_eq!(
            got,
            vec![
                (PathBuf::from("/tmp/cowfs/a"), 11111),
                (PathBuf::from("/tmp/cowfs/b"), 4711)
            ],
            "only mounts of a cowfs export, never localhost:/ of another tool"
        );
        assert!(localhost_mounts(SAMPLE, Path::new("/nowhere")).is_empty());
        assert!(localhost_mounts("", Path::new("/")).is_empty());
    }

    #[test]
    fn a_listening_port_answers_and_a_closed_one_does_not() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(answers(l.local_addr().unwrap().port()));
        drop(l);
        // Another process may grab a freed port on a busy machine: most of several must refuse.
        let refused = (0..7)
            .filter(|_| {
                let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let port = l.local_addr().unwrap().port();
                drop(l);
                !answers(port)
            })
            .count();
        assert!(refused >= 5, "only {refused} of 7 closed ports refused");
    }
}
