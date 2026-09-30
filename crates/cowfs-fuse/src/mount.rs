use std::ffi::OsStr;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;

use cowfs_vfs::{Ino, Vfs};
use fuser::{MountOption, Notifier, Session, SessionUnmounter};

use crate::fs::Fs;
use crate::options::MountOptions;
use crate::MountError;

fn session(
    vfs: Arc<dyn Vfs>,
    mountpoint: &Path,
    opts: MountOptions,
) -> Result<Session<Fs>, MountError> {
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
    Ok(Session::new(Fs::new(vfs, opts), mountpoint, &m)?)
}

/// Mounts `vfs` at `mountpoint` and serves it on the calling thread until the filesystem is
/// unmounted from outside (`fusermount3 -u`).
pub fn run(
    vfs: Arc<dyn Vfs>,
    mountpoint: impl AsRef<Path>,
    opts: MountOptions,
) -> Result<(), MountError> {
    Ok(session(vfs, mountpoint.as_ref(), opts)?.run()?)
}

/// A live FUSE mount served on a background thread. Dropping it unmounts and waits for the
/// thread to finish; that wait lasts as long as any process still holds a file on the mount
/// open, so close such files first.
#[derive(Debug)]
pub struct Mount {
    unmounter: SessionUnmounter,
    notifier: Notifier,
    thread: Option<JoinHandle<io::Result<()>>>,
}

impl Mount {
    /// Mounts `vfs` at `mountpoint`, which must be an existing empty directory.
    pub fn new(
        vfs: Arc<dyn Vfs>,
        mountpoint: impl AsRef<Path>,
        opts: MountOptions,
    ) -> Result<Self, MountError> {
        let mut se = session(vfs, mountpoint.as_ref(), opts)?;
        let unmounter = se.unmount_callable();
        let notifier = se.notifier();
        let thread = std::thread::Builder::new()
            .name("cowfs-fuse".into())
            .spawn(move || se.run())?;
        Ok(Self {
            unmounter,
            notifier,
            thread: Some(thread),
        })
    }

    /// Unmounts, waits for the request loop to end and reports how it ended.
    pub fn unmount(mut self) -> Result<(), MountError> {
        self.finish()
    }

    /// Tells the kernel to forget everything cached for `ino`: attributes and file pages.
    /// Call it after changing that inode by any route other than this mount.
    pub fn invalidate_inode(&self, ino: Ino) -> io::Result<()> {
        self.notifier.inval_inode(ino, 0, 0)
    }

    /// Tells the kernel to forget the cached lookup of `name` in `parent`, negative or not.
    /// Call it after adding or removing that name by any route other than this mount.
    pub fn invalidate_entry(&self, parent: Ino, name: &OsStr) -> io::Result<()> {
        self.notifier.inval_entry(parent, name)
    }

    fn finish(&mut self) -> Result<(), MountError> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        self.unmounter.unmount()?;
        match thread.join() {
            Ok(r) => Ok(r?),
            Err(_) => Err(io::Error::other("fuse request loop panicked").into()),
        }
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        if let Err(e) = self.finish() {
            log::error!("unmount: {e}");
        }
    }
}
