//! `PathVfs`: a [`cowfs_vfs::Vfs`] over a directory on a real filesystem, using only `std` and
//! libc calls, so the conformance suite in `cowfs-vfs-test` can run on a native filesystem
//! as a control and through a mounted cowfs adapter.
//!
//! The trait is inode based and a filesystem is path based, so `PathVfs` keeps its own inode
//! table. An `Ino` is a number it allocates. Behind it sits the file's identity on the backing
//! filesystem (`st_dev`, `st_ino`), which is why every hardlinked name reports the same `Ino`,
//! plus the `(parent, name)` pairs it was reached by. A file is reopened through one of those
//! names when needed, checking the identity, and never through a symlink. A file that loses its
//! last name while a reference or handle remains keeps a descriptor open, so it stays usable.
//! Reads and writes go through descriptors, not paths.
//!
//! Everything else is left to the backing filesystem: `nlink` (a directory's is whatever the
//! filesystem reports), timestamps, block counts, xattr support, name rules. That is the
//! point: a check that fails here is either a bug in `PathVfs`, an assumption the suite makes
//! that a real filesystem does not meet, or a quirk of that filesystem. `FINDINGS.md` in this
//! crate classifies every check for each environment it was run on.
//!
//! # Running the controls and the mount runs
//!
//! All runs print the suite's table and are `#[ignore]`d. From the repository root:
//!
//! ```text
//! # Native control on this machine (APFS on the Mac, the temp dir's filesystem elsewhere).
//! # Set COWFS_PATHVFS_NATIVE_DIR to run on a specific filesystem (a case-sensitive volume,
//! # a loop-mounted ext4, ...). Add COWFS_CONFORMANCE_HEAVY=1 for the 50,000 entry and 8 MiB checks.
//! cargo test -p cowfs-vfs-path --test native -j4 -- --ignored --nocapture
//!
//! # Through a mount: mount a cowfs adapter over some Vfs, then point the test at the mount.
//! COWFS_PATHVFS_MOUNT=/path/to/mountpoint \
//!   cargo test -p cowfs-vfs-path --test mount -j4 -- --ignored --nocapture
//! ```
//!
//! `FINDINGS.md` has the FUSE and NFS driver recipes, the case-sensitive APFS and loop-mounted
//! ext4 setups, and the full matrix.
//!
//! Each check gets a fresh empty subdirectory of the chosen directory and removes it afterwards.
//! `COWFS_CONFORMANCE_FILTER=text` runs only the checks whose `category::name` contains `text`.
//!
//! # Limits
//!
//! * One lock guards the inode table, so namespace operations are serialised. Reads and writes
//!   run outside it, on their own descriptors.
//! * A file with no known name and no descriptor cannot be found again, so unlinking the last
//!   known name of a file whose descriptor cannot be opened (a file with mode 000, say) while a
//!   reference remains makes the inode `Stale` earlier than the trait requires.
//! * A single `read` call returns at most 64 MiB.
//! * Symlink extended attributes on Linux go through `/proc/self/fd`.

use std::any::Any;
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use cowfs_vfs::{
    validate_name, Attr, DirEntry, Error, FileHandle, FileKind, Ino, ReadDir, RenameFlags, Result,
    SetAttr, SetTime, StatFs, Vfs, XattrFlags, MODE_MASK, ROOT_INO,
};

mod cookies;
#[allow(unsafe_code)]
mod sys;
mod table;

use sys::{TimeSpec, XTarget};
use table::{io_err, kind_of, open_from, Loc, State};

const READ_MAX: u32 = 64 << 20;

/// A `Vfs` over a directory of a real filesystem. See the crate docs.
pub struct PathVfs {
    state: Mutex<State>,
    _keep: Option<Box<dyn Any + Send + Sync>>,
}

impl std::fmt::Debug for PathVfs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PathVfs").finish_non_exhaustive()
    }
}

impl PathVfs {
    /// Serves the directory `root`, which becomes `ROOT_INO`. Everything under it is fair game.
    pub fn new(root: impl AsRef<Path>) -> io::Result<Self> {
        sys::raise_nofile_limit();
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY)
            .open(root)?;
        Ok(Self {
            state: Mutex::new(State::new(OwnedFd::from(file))?),
            _keep: None,
        })
    }

    /// Ties `guard` to this filesystem's lifetime; it is dropped after every descriptor is closed.
    #[must_use]
    pub fn keep_alive(mut self, guard: impl Any + Send + Sync) -> Self {
        self._keep = Some(Box::new(guard));
        self
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn with_xattr<R>(&self, ino: Ino, f: impl FnOnce(&XTarget<'_>) -> io::Result<R>) -> Result<R> {
        let mut s = self.lock();
        #[cfg(target_os = "linux")]
        if s.node(ino)?.kind == FileKind::Symlink {
            return match s.locate(ino)? {
                Loc::Named(dir, name) => {
                    let t = XTarget::link(dir.as_raw_fd(), &name).map_err(io_err)?;
                    f(&t).map_err(io_err)
                }
                Loc::Fd(_) => Err(Error::NotSupported),
            };
        }
        let o = s.open_fd(ino)?;
        f(&XTarget::Fd(o.file.as_fd())).map_err(io_err)
    }
}

fn time_spec(t: Option<SetTime>) -> TimeSpec {
    match t {
        None => TimeSpec::Omit,
        Some(SetTime::Now) => TimeSpec::Now,
        Some(SetTime::At(t)) => TimeSpec::At(t.secs, t.nanos),
    }
}

impl Vfs for PathVfs {
    fn lookup(&self, parent: Ino, name: &[u8]) -> Result<Attr> {
        validate_name(name)?;
        let mut s = self.lock();
        let dir = s.dir_fd(parent)?;
        let st = sys::fstatat(dir.file.as_fd(), name).map_err(io_err)?;
        let ino = s.register(parent, &dir.file, name, &st)?;
        s.add_ref(ino);
        s.attr_of(ino, &st)
    }

    fn forget(&self, ino: Ino, count: u64) {
        self.lock().forget(ino, count);
    }

    fn getattr(&self, ino: Ino) -> Result<Attr> {
        self.lock().attr(ino)
    }

    fn setattr(&self, ino: Ino, changes: SetAttr) -> Result<Attr> {
        let mut s = self.lock();
        let kind = s.node(ino)?.kind;
        if changes.size.is_some() {
            match kind {
                FileKind::Directory => return Err(Error::IsDir),
                FileKind::Symlink => return Err(Error::InvalidArgument),
                FileKind::Regular => {}
            }
        }
        if let Some(size) = changes.size {
            s.open_rw(ino)?.file.set_len(size).map_err(io_err)?;
        }
        if changes.atime.is_some() || changes.mtime.is_some() {
            let (a, m) = (time_spec(changes.atime), time_spec(changes.mtime));
            match s.locate(ino)? {
                Loc::Named(dir, name) => sys::utimensat(dir.as_fd(), &name, a, m),
                Loc::Fd(o) => sys::utimens_fd(o.file.as_fd(), a, m, kind == FileKind::Symlink),
            }
            .map_err(io_err)?;
        }
        if let Some(mode) = changes.mode {
            let mode = mode & MODE_MASK;
            match s.locate(ino)? {
                Loc::Named(dir, name) => {
                    sys::fchmodat(dir.as_fd(), &name, mode, kind == FileKind::Symlink)
                }
                Loc::Fd(o) => sys::fchmod(o.file.as_fd(), mode),
            }
            .map_err(io_err)?;
        }
        s.attr(ino)
    }

    fn readlink(&self, ino: Ino) -> Result<Vec<u8>> {
        let s = self.lock();
        s.node(ino)?.target.clone().ok_or(Error::InvalidArgument)
    }

    fn create(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        validate_name(name)?;
        let mut s = self.lock();
        let dir = s.dir_fd(parent)?;
        s.invalidate(parent);
        let flags = libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW;
        let fd = sys::openat(dir.file.as_fd(), name, flags, 0o600).map_err(io_err)?;
        // Set the mode explicitly: the creation mode is filtered by the umask.
        sys::fchmod(fd.as_fd(), mode & MODE_MASK).map_err(io_err)?;
        let open = open_from(fd, true);
        let st = sys::fstat(open.file.as_fd()).map_err(io_err)?;
        let ino = s.register(parent, &dir.file, name, &st)?;
        s.remember(ino, open);
        s.add_ref(ino);
        s.attr_of(ino, &st)
    }

    fn mkdir(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        validate_name(name)?;
        let mut s = self.lock();
        let dir = s.dir_fd(parent)?;
        s.invalidate(parent);
        sys::mkdirat(dir.file.as_fd(), name, 0o700).map_err(io_err)?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;
        let fd = sys::openat(dir.file.as_fd(), name, flags, 0).map_err(io_err)?;
        sys::fchmod(fd.as_fd(), mode & MODE_MASK).map_err(io_err)?;
        let open = open_from(fd, false);
        let st = sys::fstat(open.file.as_fd()).map_err(io_err)?;
        let ino = s.register(parent, &dir.file, name, &st)?;
        s.remember(ino, open);
        s.add_ref(ino);
        s.attr_of(ino, &st)
    }

    fn symlink(&self, parent: Ino, name: &[u8], target: &[u8]) -> Result<Attr> {
        validate_name(name)?;
        let mut s = self.lock();
        let dir = s.dir_fd(parent)?;
        s.invalidate(parent);
        sys::symlinkat(target, dir.file.as_fd(), name).map_err(io_err)?;
        let st = sys::fstatat(dir.file.as_fd(), name).map_err(io_err)?;
        let ino = s.register(parent, &dir.file, name, &st)?;
        s.add_ref(ino);
        s.attr_of(ino, &st)
    }

    fn link(&self, ino: Ino, new_parent: Ino, new_name: &[u8]) -> Result<Attr> {
        validate_name(new_name)?;
        let mut s = self.lock();
        let (kind, id, names) = {
            let n = s.node(ino)?;
            (n.kind, n.id, n.names.clone())
        };
        let new_dir = s.dir_fd(new_parent)?;
        s.invalidate(new_parent);
        if kind == FileKind::Directory {
            return Err(Error::PermissionDenied);
        }
        let mut linked = false;
        for (parent, name) in names {
            let Ok(dir) = s.dir_fd(parent) else {
                continue;
            };
            match sys::fstatat(dir.file.as_fd(), &name) {
                Ok(st) if (st.dev, st.ino) == id => {}
                _ => continue,
            }
            sys::linkat(dir.file.as_fd(), &name, new_dir.file.as_fd(), new_name).map_err(io_err)?;
            linked = true;
            break;
        }
        if !linked {
            return Err(Error::NotFound);
        }
        let st = sys::fstatat(new_dir.file.as_fd(), new_name).map_err(io_err)?;
        let linked_ino = s.register(new_parent, &new_dir.file, new_name, &st)?;
        s.add_ref(linked_ino);
        s.attr_of(linked_ino, &st)
    }

    fn unlink(&self, parent: Ino, name: &[u8]) -> Result<()> {
        validate_name(name)?;
        let mut s = self.lock();
        let dir = s.dir_fd(parent)?;
        s.invalidate(parent);
        let st = sys::fstatat(dir.file.as_fd(), name).map_err(io_err)?;
        if st.file_type() == u32::from(libc::S_IFDIR) {
            return Err(Error::IsDir);
        }
        let known = s.node_by_id((st.dev, st.ino));
        if let Some(i) = known {
            if s.node(i)?.names.len() <= 1 {
                s.pin(i);
            }
        }
        if let Err(e) = sys::unlinkat(dir.file.as_fd(), name, false) {
            if let Some(i) = known {
                s.settle(i);
            }
            return Err(io_err(e));
        }
        if let Some(i) = known {
            s.name_removed(i, parent, name, st.nlink.saturating_sub(1));
        }
        Ok(())
    }

    fn rmdir(&self, parent: Ino, name: &[u8]) -> Result<()> {
        validate_name(name)?;
        let mut s = self.lock();
        let dir = s.dir_fd(parent)?;
        s.invalidate(parent);
        let st = sys::fstatat(dir.file.as_fd(), name).map_err(io_err)?;
        if st.file_type() != u32::from(libc::S_IFDIR) {
            return Err(Error::NotDir);
        }
        let known = s.node_by_id((st.dev, st.ino));
        if let Some(i) = known {
            s.pin(i);
        }
        if let Err(e) = sys::unlinkat(dir.file.as_fd(), name, true) {
            if let Some(i) = known {
                s.settle(i);
            }
            return Err(match io_err(e) {
                Error::Exists => Error::NotEmpty,
                other => other,
            });
        }
        if let Some(i) = known {
            s.name_removed(i, parent, name, 0);
        }
        Ok(())
    }

    fn rename(
        &self,
        parent: Ino,
        name: &[u8],
        new_parent: Ino,
        new_name: &[u8],
        flags: RenameFlags,
    ) -> Result<()> {
        validate_name(name)?;
        validate_name(new_name)?;
        let mut s = self.lock();
        let from_dir = s.dir_fd(parent)?;
        let to_dir = s.dir_fd(new_parent)?;
        s.invalidate(parent);
        s.invalidate(new_parent);
        let src = sys::fstatat(from_dir.file.as_fd(), name).map_err(io_err)?;
        let dst = match sys::fstatat(to_dir.file.as_fd(), new_name) {
            Ok(st) => Some(st),
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => None,
            Err(e) => return Err(io_err(e)),
        };
        let same = dst.is_some_and(|d| (d.dev, d.ino) == (src.dev, src.ino));
        let moved = s.node_by_id((src.dev, src.ino));
        let replaced = match dst {
            Some(d) if !same => s.node_by_id((d.dev, d.ino)),
            _ => None,
        };
        if let Some(i) = replaced {
            if s.node(i)?.names.len() <= 1 {
                s.pin(i);
            }
        }
        if let Err(e) = sys::renameat(
            from_dir.file.as_fd(),
            name,
            to_dir.file.as_fd(),
            new_name,
            flags.no_replace,
        ) {
            if let Some(i) = replaced {
                s.settle(i);
            }
            return Err(match io_err(e) {
                Error::Exists if !flags.no_replace => Error::NotEmpty,
                other => other,
            });
        }
        if same {
            return Ok(());
        }
        if let Some(i) = moved {
            s.rename_name(i, (parent, name), (new_parent, new_name));
        }
        if let (Some(i), Some(d)) = (replaced, dst) {
            let remaining = if d.file_type() == u32::from(libc::S_IFDIR) {
                0
            } else {
                d.nlink.saturating_sub(1)
            };
            s.name_removed(i, new_parent, new_name, remaining);
        }
        Ok(())
    }

    fn open(&self, ino: Ino) -> Result<FileHandle> {
        self.lock().open_handle(ino).map(FileHandle)
    }

    fn release(&self, handle: FileHandle) -> Result<()> {
        self.lock().release_handle(handle.0)
    }

    fn read(&self, ino: Ino, offset: u64, size: u32) -> Result<Vec<u8>> {
        let file = {
            let mut s = self.lock();
            match s.node(ino)?.kind {
                FileKind::Directory => return Err(Error::IsDir),
                FileKind::Symlink => return Err(Error::InvalidArgument),
                FileKind::Regular => {}
            }
            s.open_fd(ino)?.file
        };
        if offset > i64::MAX as u64 - u64::from(READ_MAX) {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; size.min(READ_MAX) as usize];
        let mut got = 0;
        while got < buf.len() {
            match file.read_at(&mut buf[got..], offset + got as u64) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(io_err(e)),
            }
        }
        buf.truncate(got);
        Ok(buf)
    }

    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> Result<u32> {
        let file = {
            let mut s = self.lock();
            match s.node(ino)?.kind {
                FileKind::Directory => return Err(Error::IsDir),
                FileKind::Symlink => return Err(Error::InvalidArgument),
                FileKind::Regular => {}
            }
            s.open_rw(ino)?.file
        };
        let len = u32::try_from(data.len()).map_err(|_| Error::InvalidArgument)?;
        file.write_all_at(data, offset).map_err(io_err)?;
        Ok(len)
    }

    fn flush(&self, ino: Ino) -> Result<()> {
        self.lock().node(ino).map(|_| ())
    }

    fn fsync(&self, ino: Ino, data_only: bool) -> Result<()> {
        let open = {
            let mut s = self.lock();
            if s.node(ino)?.kind == FileKind::Symlink {
                return Ok(());
            }
            s.open_fd(ino)?
        };
        let r = if data_only {
            open.file.sync_data()
        } else {
            open.file.sync_all()
        };
        r.map_err(io_err)
    }

    fn readdir(&self, dir: Ino, cookie: u64, max: usize) -> Result<ReadDir> {
        let mut s = self.lock();
        let open = s.dir_fd(dir)?;
        if cookie == 0 || s.node(dir)?.listing.is_none() {
            let names = sys::list_dir(open.file.as_fd()).map_err(io_err)?;
            let n = s.node_mut(dir)?;
            n.listing = Some(n.cookies.sync(names));
        }
        let listing = s.node_mut(dir)?.listing.take().unwrap_or_default();
        let rest = &listing[listing.partition_point(|(c, _)| *c <= cookie)..];
        let mut entries = Vec::new();
        let mut consumed = 0;
        for (c, name) in rest {
            if entries.len() >= max {
                break;
            }
            consumed += 1;
            let st = match sys::fstatat(open.file.as_fd(), name) {
                Ok(st) => st,
                Err(e) if e.raw_os_error() == Some(libc::ENOENT) => continue,
                Err(e) => return Err(io_err(e)),
            };
            let Ok(kind) = kind_of(&st) else {
                continue;
            };
            let ino = s.register(dir, &open.file, name, &st)?;
            entries.push(DirEntry {
                ino,
                kind,
                name: name.clone(),
                cookie: *c,
            });
        }
        let eof = consumed == rest.len();
        s.node_mut(dir)?.listing = Some(listing);
        Ok(ReadDir { entries, eof })
    }

    fn statfs(&self) -> Result<StatFs> {
        let open = self.lock().dir_fd(ROOT_INO)?;
        let v = sys::fstatvfs(open.file.as_fd()).map_err(io_err)?;
        Ok(StatFs {
            block_size: u32::try_from(v.block_size).unwrap_or(4096),
            blocks: v.blocks,
            blocks_free: v.blocks_free,
            blocks_available: v.blocks_available,
            files: v.files,
            files_free: v.files_free,
            name_max: u32::try_from(v.name_max).unwrap_or(u32::MAX),
        })
    }

    fn getxattr(&self, ino: Ino, name: &[u8]) -> Result<Vec<u8>> {
        self.with_xattr(ino, |t| sys::getxattr(t, name))
    }

    fn setxattr(&self, ino: Ino, name: &[u8], value: &[u8], flags: XattrFlags) -> Result<()> {
        self.with_xattr(ino, |t| {
            sys::setxattr(t, name, value, flags.create, flags.replace)
        })
    }

    fn listxattr(&self, ino: Ino) -> Result<Vec<Vec<u8>>> {
        self.with_xattr(ino, sys::listxattr)
    }

    fn removexattr(&self, ino: Ino, name: &[u8]) -> Result<()> {
        self.with_xattr(ino, |t| sys::removexattr(t, name))
    }
}

/// Removes a directory tree even when it holds entries whose mode forbids it.
pub fn force_remove_dir_all(path: &Path) {
    fn open_up(p: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::symlink_metadata(p) else {
            return;
        };
        if !meta.is_dir() {
            return;
        }
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                open_up(&e.path());
            }
        }
    }
    open_up(path);
    let _ = std::fs::remove_dir_all(path);
}

#[cfg(test)]
mod tests;
