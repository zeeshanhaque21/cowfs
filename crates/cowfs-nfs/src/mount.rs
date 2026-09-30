//! The loopback server and the `mount_nfs` / `umount` lifecycle.
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use cowfs_vfs::Vfs;
use nfsserve::tcp::{Limits, MountGate, NFSTcp, NFSTcpListener};

use crate::adapter::{AdapterOptions, AppleDoubleMode, CowNfs};
use crate::peer::same_user;

const MOUNT_NFS: &str = "/sbin/mount_nfs";
const MOUNT: &str = "/sbin/mount";
const UMOUNT: &str = "/sbin/umount";

/// What can go wrong mounting or unmounting.
#[derive(Debug, thiserror::Error)]
pub enum MountError {
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    #[error("{command} failed ({status}): {stderr}")]
    CommandFailed {
        command: String,
        status: String,
        stderr: String,
    },
    #[error("{command} did not finish within {secs}s")]
    Timeout { command: String, secs: u64 },
    #[error("{0} is already a mount point")]
    AlreadyMounted(PathBuf),
    #[error("{0} does not appear in the mount table after mount_nfs succeeded")]
    NotMounted(PathBuf),
    #[error("could not unmount {path}: {stderr}")]
    Unmount { path: PathBuf, stderr: String },
}

/// Mount and server options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountOptions {
    /// NFS read size in bytes. The server accepts up to 1 MiB.
    pub rsize: u32,
    /// NFS write size in bytes. The server accepts up to 1 MiB.
    pub wsize: u32,
    /// Client attribute cache lifetime in seconds. Changes made through another path than this
    /// mount (a control plane snapshot, say) can stay invisible for that long.
    pub actimeo: u32,
    /// What to do with the `._name` files the macOS client writes for extended attributes.
    pub appledouble: AppleDoubleMode,
    /// Answer cheap metadata calls on the network thread, see [`AdapterOptions::inline_metadata`].
    pub inline_metadata: bool,
    /// Give the root file handle to one client only: the first MNT wins and later ones are
    /// refused until [`Server::rearm_mount`].
    pub one_shot_mount: bool,
    /// Refuse MNT from a process of another user than the server's (best effort, uses `lsof`).
    pub check_peer_uid: bool,
    /// Connection and message bounds of the server.
    pub limits: Limits,
    /// Print the per-procedure latency table to stderr on SIGUSR1.
    pub stats_on_sigusr1: bool,
    /// Limit for each `mount_nfs` and `umount` invocation.
    pub command_timeout: Duration,
}

impl Default for MountOptions {
    fn default() -> Self {
        Self {
            rsize: 131_072,
            wsize: 131_072,
            actimeo: 120,
            appledouble: AppleDoubleMode::default(),
            inline_metadata: false,
            one_shot_mount: true,
            check_peer_uid: true,
            limits: Limits::default(),
            stats_on_sigusr1: false,
            command_timeout: Duration::from_secs(20),
        }
    }
}

impl MountOptions {
    /// The `-o` string for `mount_nfs`. `locallocks` is required: without it flock and fcntl
    /// fail and rustc incremental compilation aborts.
    pub fn nfs_option_string(&self, port: u16) -> String {
        format!(
            "locallocks,vers=3,tcp,rsize={},wsize={},actimeo={},port={port},mountport={port}",
            self.rsize, self.wsize, self.actimeo
        )
    }

    fn adapter(&self, owner: Option<(u32, u32)>) -> AdapterOptions {
        AdapterOptions {
            appledouble: self.appledouble,
            inline_metadata: self.inline_metadata,
            owner,
        }
    }
}

/// An NFSv3 server on 127.0.0.1 with its own runtime, serving one `Vfs`.
#[derive(Debug)]
pub struct Server {
    runtime: Option<tokio::runtime::Runtime>,
    port: u16,
    gate: Option<Arc<MountGate>>,
}

impl Server {
    /// Binds an ephemeral port on 127.0.0.1 and starts serving.
    pub fn start(
        vfs: Arc<dyn Vfs>,
        opts: &MountOptions,
        owner: Option<(u32, u32)>,
    ) -> io::Result<Server> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("cowfs-nfs")
            .enable_all()
            .build()?;
        let fs = CowNfs::new(vfs, opts.adapter(owner))?;
        let mut listener = runtime.block_on(NFSTcpListener::bind("127.0.0.1:0", fs))?;
        let port = listener.get_listen_port();
        listener.set_limits(opts.limits.clone());
        let gate = opts.one_shot_mount.then(|| Arc::new(MountGate::new()));
        if let Some(gate) = &gate {
            listener.set_mount_gate(gate.clone());
        }
        if opts.check_peer_uid {
            listener.set_peer_check(Arc::new(same_user));
        }
        runtime.spawn(async move {
            let _ = listener.handle_forever().await;
        });
        if opts.stats_on_sigusr1 {
            runtime.spawn(async {
                let kind = tokio::signal::unix::SignalKind::user_defined1();
                if let Ok(mut s) = tokio::signal::unix::signal(kind) {
                    while s.recv().await.is_some() {
                        eprint!("STATS\n{}", nfsserve::take_stats());
                    }
                }
            });
        }
        Ok(Server {
            runtime: Some(runtime),
            port,
            gate,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Lets the next MNT take the root handle again, for a deliberate remount.
    pub fn rearm_mount(&self) {
        if let Some(g) = &self.gate {
            g.rearm();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_timeout(Duration::from_secs(5));
        }
    }
}

pub(crate) struct CmdOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stderr: String,
}

fn drain(s: Option<impl Read + Send + 'static>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut s) = s {
            let _ = s.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// Runs `cmd` and kills it after `timeout` (macOS has no `timeout` binary).
pub(crate) fn run(cmd: &mut Command, timeout: Duration) -> Result<(CmdOutput, String), MountError> {
    let name = cmd.get_program().to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(MountError::Timeout {
                command: name,
                secs: timeout.as_secs(),
            });
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    Ok((CmdOutput { status, stderr }, stdout))
}

pub(crate) fn checked(cmd: &mut Command, timeout: Duration) -> Result<String, MountError> {
    let name = cmd.get_program().to_string_lossy().into_owned();
    let (out, stdout) = run(cmd, timeout)?;
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(MountError::CommandFailed {
            command: name,
            status: out.status.to_string(),
            stderr: out.stderr.trim().to_string(),
        })
    }
}

/// True if `mount_output` (the output of `mount`) lists an NFS mount on `path`.
pub fn is_listed(mount_output: &str, path: &Path) -> bool {
    let needle = format!(" on {} (nfs", path.display());
    mount_output.lines().any(|l| l.contains(&needle))
}

fn listed(path: &Path, timeout: Duration) -> Result<bool, MountError> {
    Ok(is_listed(
        &checked(&mut Command::new(MOUNT), timeout)?,
        path,
    ))
}

/// True where `mount_nfs` exists (macOS).
pub fn mount_nfs_available() -> bool {
    cfg!(target_os = "macos") && Path::new(MOUNT_NFS).exists()
}

pub(crate) fn unmount_path(path: &Path, timeout: Duration) -> Result<(), MountError> {
    let mut last = String::new();
    for force in [false, true] {
        let mut cmd = Command::new(UMOUNT);
        if force {
            cmd.arg("-f");
        }
        match checked(cmd.arg(path), timeout) {
            Ok(_) => {}
            Err(e) => last = e.to_string(),
        }
        if !listed(path, timeout)? {
            return Ok(());
        }
    }
    Err(MountError::Unmount {
        path: path.to_path_buf(),
        stderr: last,
    })
}

/// A mounted `Vfs`. Dropping it unmounts, and a mount that cannot be removed keeps its server
/// alive so it never becomes a stale mount.
#[derive(Debug)]
pub struct Mount {
    server: Option<Server>,
    mountpoint: PathBuf,
    timeout: Duration,
}

impl Mount {
    /// Starts a server for `vfs` and mounts it on `mountpoint` (created if missing).
    pub fn new(
        vfs: Arc<dyn Vfs>,
        mountpoint: &Path,
        opts: MountOptions,
    ) -> Result<Mount, MountError> {
        std::fs::create_dir_all(mountpoint)?;
        let mountpoint = std::fs::canonicalize(mountpoint)?;
        let timeout = opts.command_timeout;
        if listed(&mountpoint, timeout)? {
            return Err(MountError::AlreadyMounted(mountpoint));
        }
        let md = std::fs::metadata(&mountpoint)?;
        let server = Server::start(vfs, &opts, Some((md.uid(), md.gid())))?;
        let mounted = checked(
            Command::new(MOUNT_NFS)
                .arg("-o")
                .arg(opts.nfs_option_string(server.port()))
                .arg("localhost:/")
                .arg(&mountpoint),
            timeout,
        );
        let mut mount = Mount {
            server: Some(server),
            mountpoint,
            timeout,
        };
        if let Err(e) = mounted {
            if listed(&mount.mountpoint, timeout).unwrap_or(false) {
                let _ = mount.do_unmount();
            }
            mount.server = None;
            return Err(e);
        }
        if !listed(&mount.mountpoint, timeout)? {
            mount.server = None;
            return Err(MountError::NotMounted(mount.mountpoint.clone()));
        }
        crate::cleanup::register(&mount.mountpoint);
        std::fs::read_dir(&mount.mountpoint)?.for_each(drop);
        Ok(mount)
    }

    /// Makes the process unmount every mount on SIGTERM, SIGINT and SIGHUP before it exits,
    /// see [`crate::install_signal_cleanup`].
    pub fn install_signal_cleanup() -> io::Result<()> {
        crate::cleanup::install_signal_cleanup()
    }

    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    pub fn port(&self) -> Option<u16> {
        self.server.as_ref().map(Server::port)
    }

    fn do_unmount(&mut self) -> Result<(), MountError> {
        let Some(server) = self.server.take() else {
            return Ok(());
        };
        match unmount_path(&self.mountpoint, self.timeout) {
            Ok(()) => {
                crate::cleanup::unregister(&self.mountpoint);
                Ok(())
            }
            Err(e) => {
                self.server = Some(server);
                Err(e)
            }
        }
    }

    /// Unmounts and stops the server.
    pub fn unmount(mut self) -> Result<(), MountError> {
        self.do_unmount()
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        if let Err(e) = self.do_unmount() {
            eprintln!("cowfs-nfs: {e}; leaving the server running so the mount is not stale");
            if let Some(server) = self.server.take() {
                std::mem::forget(server);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_string_has_the_required_options() {
        let s = MountOptions::default().nfs_option_string(4711);
        assert_eq!(
            s,
            "locallocks,vers=3,tcp,rsize=131072,wsize=131072,actimeo=120,port=4711,mountport=4711"
        );
        let custom = MountOptions {
            rsize: 1 << 20,
            wsize: 65_536,
            actimeo: 0,
            ..MountOptions::default()
        };
        let s = custom.nfs_option_string(1);
        assert!(s.starts_with("locallocks,vers=3,tcp,"));
        assert!(s.contains("rsize=1048576,wsize=65536,actimeo=0,port=1,mountport=1"));
    }

    #[test]
    fn defaults_hide_appledouble_and_lock_down_the_mount() {
        let o = MountOptions::default();
        assert!(o.appledouble == AppleDoubleMode::Hide && !o.stats_on_sigusr1);
        assert!(o.one_shot_mount && o.check_peer_uid && !o.inline_metadata);
        assert_eq!(o.adapter(None).appledouble, AppleDoubleMode::Hide);
    }

    #[test]
    fn mount_table_parsing() {
        let out = "/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)\n\
                   localhost:/ on /private/tmp/m (nfs, nodev, nosuid, mounted by zee)\n\
                   localhost:/ on /private/tmp/m2 (nfs, nodev)\n";
        assert!(is_listed(out, Path::new("/private/tmp/m")));
        assert!(is_listed(out, Path::new("/private/tmp/m2")));
        assert!(!is_listed(out, Path::new("/private/tmp")));
        assert!(!is_listed(out, Path::new("/")));
    }

    #[test]
    fn a_hung_command_is_killed() {
        let started = Instant::now();
        let r = run(
            Command::new("/bin/sleep").arg("30"),
            Duration::from_millis(200),
        );
        assert!(matches!(r, Err(MountError::Timeout { .. })));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_failing_command_reports_stderr() {
        let r = checked(
            Command::new("/bin/sh").args(["-c", "echo boom >&2; exit 3"]),
            Duration::from_secs(5),
        );
        match r {
            Err(MountError::CommandFailed { stderr, .. }) => assert_eq!(stderr, "boom"),
            other => panic!("unexpected {other:?}"),
        }
    }
}
