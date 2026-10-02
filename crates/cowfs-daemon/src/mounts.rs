//! The platform mount adapter: NFS loopback on macOS, FUSE on Linux.
//!
//! One type per platform behind the three calls the daemon needs: mount a `Vfs` at a path,
//! unmount it, and announce a change made behind the mount so the kernel drops what it cached.
//! `health` is Linux only, where the adapter has lanes that can wedge. The platform is
//! chosen with `#[cfg]`, not `cfg!`, so only the adapter for this build is typechecked.

use cowfs_vfs::Vfs;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(target_os = "macos")]
type Handle = cowfs_nfs::Mount;
#[cfg(target_os = "linux")]
type Handle = cowfs_fuse::Mount;

/// What a mount adapter reports about itself, for `mount_info` and the daemon's log line.
pub fn adapter_name() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "nfs"
    }
    #[cfg(target_os = "linux")]
    {
        "fuse"
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        "none"
    }
}

/// True when this build has a mount adapter that works here.
pub fn available() -> bool {
    #[cfg(target_os = "macos")]
    {
        cowfs_nfs::mount_nfs_available()
    }
    #[cfg(target_os = "linux")]
    {
        Path::new("/dev/fuse").exists()
            && std::process::Command::new("fusermount3")
                .arg("--version")
                .output()
                .is_ok()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

/// True if the kernel's mount table lists `path`.
pub fn is_mounted(path: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        let Ok(out) = std::process::Command::new("/sbin/mount").output() else {
            return false;
        };
        cowfs_nfs::is_listed(&String::from_utf8_lossy(&out.stdout), path)
    }
    #[cfg(target_os = "linux")]
    {
        // `cowfs-fuse` mounts as the `cowfs` source with the `fuse.cowfs` type, and an export
        // is one of ours for the same reason, so both match on the type rather than on the
        // device, which is a different node per mount.
        const FSTYPE: &str = "fuse.cowfs";
        let Ok(text) = std::fs::read_to_string("/proc/mounts") else {
            return false;
        };
        let want = path.display().to_string();
        text.lines().any(|l| {
            let mut f = l.split_whitespace();
            let (Some(_source), Some(where_), Some(fstype)) = (f.next(), f.next(), f.next()) else {
                return false;
            };
            fstype == FSTYPE && where_ == want
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = path;
        false
    }
}

/// Clears mounts of ours whose server is gone, under `prefix`. On macOS this is what stops a
/// killed daemon from hanging every `ls` on its mount for twenty seconds or more.
pub fn sweep_stale(prefix: &Path) -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        cowfs_nfs::sweep_stale_mounts(prefix).unwrap_or_else(|e| {
            eprintln!("cowfs-daemon: cannot sweep stale mounts: {e}");
            Vec::new()
        })
    }
    #[cfg(target_os = "linux")]
    {
        cowfs_fuse::sweep_stale_mounts(prefix)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = prefix;
        Vec::new()
    }
}

/// Unmounts this process's mounts on SIGTERM, SIGINT and SIGHUP. Call once, early.
pub fn install_signal_cleanup() -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        cowfs_nfs::install_signal_cleanup()
    }
    #[cfg(target_os = "linux")]
    {
        cowfs_fuse::Mount::install_signal_cleanup()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Ok(())
    }
}

/// A live mount. Dropping it unmounts; `unmount` does it explicitly and reports the failure.
pub struct Mounted {
    handle: Option<Handle>,
    path: PathBuf,
}

impl fmt::Debug for Mounted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mounted").field("path", &self.path).finish()
    }
}

impl Mounted {
    /// Mounts `vfs` at `path`, which is created if missing. `mount_snapshot` accepts a path
    /// that does not exist yet, so creating it here is what makes an absent target work on
    /// both adapters: the NFS one creates it itself, and FUSE needs it to exist.
    pub fn mount(vfs: Arc<dyn Vfs>, path: &Path) -> io::Result<Mounted> {
        std::fs::create_dir_all(path)?;
        #[cfg(target_os = "macos")]
        {
            let handle = cowfs_nfs::Mount::new(vfs, path, cowfs_nfs::MountOptions::default())
                .map_err(|e| io::Error::other(e.to_string()))?;
            Ok(Mounted {
                handle: Some(handle),
                path: path.to_owned(),
            })
        }
        #[cfg(target_os = "linux")]
        {
            let handle = cowfs_fuse::Mount::new(vfs, path, cowfs_fuse::MountOptions::default())
                .map_err(|e| io::Error::other(e.to_string()))?;
            Ok(Mounted {
                handle: Some(handle),
                path: path.to_owned(),
            })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (vfs, path);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "cowfs-daemon has no mount adapter on this platform",
            ))
        }
    }

    /// A mount that is not a mount, so a handler can be exercised on a machine whose adapter
    /// is unusable. `unmount` has nothing to do and it never reports itself alive.
    pub fn no_mount(path: impl AsRef<Path>) -> io::Result<Mounted> {
        Ok(Mounted {
            handle: None,
            path: path.as_ref().to_owned(),
        })
    }

    /// Where it is mounted.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Unmounts and stops the server, synchronously. The mount stays in the kernel's table
    /// if this fails, so the error is returned rather than logged and dropped.
    pub fn unmount(mut self) -> Result<(), String> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        #[cfg(target_os = "macos")]
        {
            handle.unmount().map_err(|e| e.to_string())
        }
        #[cfg(target_os = "linux")]
        {
            handle.unmount().map(|_| ()).map_err(|e| e.to_string())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            drop(handle);
            Ok(())
        }
    }

    /// True while the mount is in the kernel's table and the adapter has not failed.
    pub fn is_alive(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.handle.is_some() && is_mounted(&self.path)
        }
        #[cfg(target_os = "linux")]
        {
            self.handle
                .as_ref()
                .is_some_and(cowfs_fuse::Mount::is_alive)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            self.handle.is_some()
        }
    }

    /// Tells the kernel that the tree changed behind the mount. Linux only: the NFS client
    /// re-reads on its own `actimeo`, and the default shared FUSE mode caches for a second,
    /// so the invalidator is what makes that second a bound instead of a guess.
    pub fn invalidate_all(&self) {
        #[cfg(target_os = "linux")]
        if let Some(mount) = &self.handle {
            if let Err(e) = mount.invalidator().invalidate_all() {
                eprintln!("cowfs-daemon: cannot invalidate the kernel cache: {e}");
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = self;
    }

    /// Whether the adapter is healthy. Linux only, and meaning only where the adapter has
    /// lanes that can wedge; elsewhere `Ok`.
    #[allow(unused_variables)]
    pub fn health(&self) -> Result<(), String> {
        #[cfg(target_os = "linux")]
        {
            match self.handle.as_ref().map(cowfs_fuse::Mount::health) {
                None | Some(cowfs_fuse::Health::Ok) => Ok(()),
                Some(cowfs_fuse::Health::Wedged(lane)) => Err(format!("lane {lane} is wedged")),
                Some(cowfs_fuse::Health::Failed) => Err("the mount is failed".into()),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(())
        }
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        // `unmount` takes `self` by value, so it cannot be called from `drop`; the handle is
        // taken out and dropped here, which is what unmounts it.
        drop(self.handle.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_adapter_name_and_availability_match_the_build() {
        let name = adapter_name();
        assert!(["nfs", "fuse", "none"].contains(&name), "{name}");
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert!(
                !name.is_empty() && name != "none",
                "this build has an adapter"
            );
        }
    }

    #[test]
    fn a_placeholder_mount_is_not_a_mount() {
        let m = Mounted::no_mount("/nowhere/at/all").unwrap();
        assert_eq!(m.path(), Path::new("/nowhere/at/all"));
        assert!(!m.is_alive());
        assert_eq!(m.health(), Ok(()));
        m.invalidate_all();
        assert_eq!(m.unmount(), Ok(()), "there is nothing to unmount");
    }

    #[test]
    fn a_directory_nobody_mounted_is_not_mounted() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_mounted(dir.path()));
        assert!(sweep_stale(&dir.path().join("under")).is_empty());
    }
}
