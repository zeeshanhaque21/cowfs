use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cowfs_vfs::{Ino, Vfs, ROOT_INO};
use fuser::{MountOption, Session};

use crate::fs::{mounter, Fs, Shared};
use crate::lifecycle;
use crate::mounts;
use crate::options::MountOptions;
use crate::{MountError, Unmounted};

fn session(
    vfs: Arc<dyn Vfs>,
    mountpoint: &Path,
    opts: MountOptions,
    shared: Arc<Shared>,
) -> Result<Session<Fs>, MountError> {
    let needs_other = opts.allow_other || opts.auto_unmount;
    if needs_other && mounter().0 != 0 {
        let conf = fs::read_to_string("/etc/fuse.conf").unwrap_or_default();
        if !mounts::fuse_conf_allows_other(&conf) {
            return Err(MountError::NeedsAllowOther);
        }
    }
    let mut m = vec![
        MountOption::FSName(opts.fs_name.clone()),
        MountOption::Subtype("cowfs".into()),
    ];
    let flags = [
        (opts.read_only, MountOption::RO),
        (opts.default_permissions, MountOption::DefaultPermissions),
        (opts.allow_other, MountOption::AllowOther),
        (opts.auto_unmount, MountOption::AutoUnmount),
    ];
    m.extend(flags.into_iter().filter(|f| f.0).map(|f| f.1));
    let se = Session::new(Fs::new(vfs, opts, shared.clone()), mountpoint, &m)?;
    let _ = shared.notifier.set(se.notifier());
    Ok(se)
}

/// Mounts `vfs` at `mountpoint` and serves it on the calling thread until the filesystem is
/// unmounted from outside (`fusermount3 -u`).
pub fn run(
    vfs: Arc<dyn Vfs>,
    mountpoint: impl AsRef<Path>,
    opts: MountOptions,
) -> Result<(), MountError> {
    let mut se = session(vfs, mountpoint.as_ref(), opts, Arc::new(Shared::new()))?;
    Ok(se.run()?)
}

/// Tells the kernel to drop what it cached, for changes made to the tree by any route other
/// than this mount (snapshot, gc and control operations). Cheap to clone, usable from any thread.
///
/// Never call it from inside a `Vfs` method that the adapter is running: invalidating names
/// takes directory locks that a syscall waiting on this very request may hold, and the mount
/// deadlocks. Call it from the control plane, after the change is visible through the `Vfs`.
///
/// The kernel ignores invalidation of a cached "no such name" answer (verified on Linux 7.0),
/// so a name created behind the mount, such as a new snapshot directory at the mount root,
/// appears after at most `negative_ttl`.
#[derive(Clone, Debug)]
pub struct Invalidator(Arc<Shared>);

impl Invalidator {
    fn notifier(&self) -> io::Result<&fuser::Notifier> {
        self.0
            .notifier
            .get()
            .ok_or_else(|| io::Error::other("mount not started"))
    }

    /// Drops the cached attributes and file pages of `ino`.
    pub fn invalidate_inode(&self, ino: Ino) -> io::Result<()> {
        self.notifier()?.inval_inode(ino, 0, 0)
    }

    /// Drops the cached lookup of the existing name `name` in `parent`.
    pub fn invalidate_entry(&self, parent: Ino, name: &OsStr) -> io::Result<()> {
        self.notifier()?.inval_entry(parent, name)
    }

    /// Drops every cached name under `parent` that the adapter has handed to the kernel, the
    /// inodes behind them, and `parent`'s own attributes (its mtime and link count). It cannot
    /// reach names beyond the eight hardlinks per inode the adapter tracks. Not recursive.
    pub fn invalidate_children(&self, parent: Ino) -> io::Result<()> {
        let kids = self.0.table().children(parent);
        let n = self.notifier()?;
        let mut first = None;
        for (name, ino) in kids {
            for r in [
                n.inval_entry(parent, OsStr::from_bytes(&name)),
                n.inval_inode(ino, 0, 0),
            ] {
                first = first.or(r.err());
            }
        }
        first = first.or(n.inval_inode(parent, 0, 0).err());
        first.map_or(Ok(()), Err)
    }

    /// Drops everything the kernel holds that the adapter knows of: every referenced inode and
    /// every name, plus the root. Costs one notification per cached inode and name (a few
    /// microseconds each), which is bounded by what the kernel currently caches. Keeps going
    /// after an error and returns the first one.
    pub fn invalidate_all(&self) -> io::Result<()> {
        let (inos, names) = self.0.table().snapshot();
        let n = self.notifier()?;
        let mut first = None;
        for (parent, name) in names {
            first = first.or(n.inval_entry(parent, OsStr::from_bytes(&name)).err());
        }
        for ino in inos.into_iter().chain([ROOT_INO]) {
            first = first.or(n.inval_inode(ino, 0, 0).err());
        }
        first.map_or(Ok(()), Err)
    }

    /// Starts a new epoch: counts it and runs `invalidate_all`. Returns the new epoch number.
    pub fn bump_epoch(&self) -> io::Result<u64> {
        let e = self.0.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.invalidate_all()?;
        Ok(e)
    }

    /// The number of `bump_epoch` calls so far.
    pub fn epoch(&self) -> u64 {
        self.0.epoch.load(Ordering::Acquire)
    }
}

/// A live FUSE mount served on background threads. Dropping it unmounts.
#[derive(Debug)]
pub struct Mount {
    path: PathBuf,
    shared: Arc<Shared>,
    unmount_timeout: Duration,
    registered: u64,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl Mount {
    /// Mounts `vfs` at `mountpoint`, which must be an existing directory.
    pub fn new(
        vfs: Arc<dyn Vfs>,
        mountpoint: impl AsRef<Path>,
        opts: MountOptions,
    ) -> Result<Self, MountError> {
        let path = mountpoint.as_ref().to_owned();
        let unmount_timeout = opts.unmount_timeout;
        let shared = Arc::new(Shared::new());
        let mut se = session(vfs, &path, opts, shared.clone())?;
        let thread = std::thread::Builder::new()
            .name("cowfs-fuse".into())
            .spawn(move || se.run())?;
        let registered = lifecycle::register(&path, unmount_timeout);
        Ok(Self {
            path,
            shared,
            unmount_timeout,
            registered,
            thread: Some(thread),
        })
    }

    /// A handle for announcing changes made behind the mount.
    pub fn invalidator(&self) -> Invalidator {
        Invalidator(self.shared.clone())
    }

    /// True while the request loop runs, the mount is in `/proc/mounts` and no `Vfs` panic
    /// budget was exhausted.
    pub fn is_alive(&self) -> bool {
        !self.failed()
            && self.thread.as_ref().is_some_and(|t| !t.is_finished())
            && lifecycle::is_mounted(&self.path)
    }

    /// True once `Vfs` panics reached `max_panics`. The mount then answers `ENOTCONN` to
    /// everything until it is unmounted, so a failed host never leaves an empty directory that
    /// looks like a valid empty tree.
    pub fn failed(&self) -> bool {
        self.shared.failed()
    }

    /// Unmounts and reports how. A busy mount is retried for `unmount_timeout`, then unmounted
    /// lazily and reported as [`Unmounted::Lazy`]. Fails if neither works or the request loop
    /// does not stop.
    pub fn unmount(mut self) -> Result<Unmounted, MountError> {
        self.finish()
    }

    /// Unmounts the registered mounts on SIGTERM, SIGINT and SIGHUP, then lets the signal take
    /// its default action, so a terminated host does not leave a mount answering `ENOTCONN`.
    /// Call once at startup; later calls do nothing. `kill -9` cannot be handled: clear that
    /// with [`sweep_stale_mounts`](crate::sweep_stale_mounts) at the next start.
    pub fn install_signal_cleanup() -> io::Result<()> {
        lifecycle::install_signal_cleanup()
    }

    fn finish(&mut self) -> Result<Unmounted, MountError> {
        let Some(thread) = self.thread.take() else {
            return Ok(Unmounted::Clean);
        };
        lifecycle::deregister(self.registered);
        let how = lifecycle::unmount(&self.path, self.unmount_timeout)?;
        if how == Unmounted::Lazy {
            return Ok(how);
        }
        let deadline = Instant::now() + self.unmount_timeout.max(Duration::from_secs(1));
        while !thread.is_finished() {
            if Instant::now() >= deadline {
                return Err(MountError::Unmount("request loop did not stop".into()));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        match thread.join() {
            Ok(r) => r.map(|()| how).map_err(Into::into),
            Err(_) => Err(MountError::Unmount("request loop panicked".into())),
        }
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        if let Err(e) = self.finish() {
            log::error!("{e}");
        }
    }
}
