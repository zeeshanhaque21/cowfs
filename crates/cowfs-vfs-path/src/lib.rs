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
//! * A file another writer replaced under a name the Vfs knows, whose inode number the
//!   filesystem then hands to another file the Vfs sees, makes the old inode `Stale`: the two
//!   files are unrelated and the new one gets an inode of its own.
//! * A single `read` call returns at most 64 MiB at a time internally and fills the caller's
//!   buffer up to the size asked for, so a short read only happens at the real end of the file.
//! * Symlink extended attributes on Linux go through `/proc/self/fd`.
//! * `open` on a directory succeeds, matching `lifecycle::open_directory_ok`.
//! * This crate is a control, not a library: it is `publish = false` through the workspace and it
//!   refuses to build anywhere but Linux and macOS.

use std::any::Any;
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use cowfs_vfs::FallocMode;
use cowfs_vfs::{
    validate_name, Attr, DirEntryPlus, Error, FileHandle, FileKind, Ino, ReadDir, ReadDirPlus,
    RenameFlags, Result, SetAttr, SetTime, StatFs, Vfs, XattrFlags, MODE_MASK, NAME_MAX, ROOT_INO,
};

mod cookies;
#[allow(unsafe_code)]
mod sys;
mod table;

use sys::{TimeSpec, XTarget};
use table::{io_err, open_from, Loc, State};

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

    /// Live inode table entries and held descriptors, for the leak regression tests.
    #[cfg(test)]
    fn live_counts(&self) -> (usize, usize) {
        self.lock().counts()
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

fn check_xattr_name(name: &[u8]) -> Result<()> {
    if name.is_empty() || name.contains(&0) {
        return Err(Error::InvalidArgument);
    }
    if name.len() > NAME_MAX {
        return Err(Error::NameTooLong);
    }
    Ok(())
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
                k if k.is_special() => return Err(Error::InvalidArgument),
                _ => return Err(Error::NotSupported),
            }
        }
        if let Some(size) = changes.size {
            s.open_rw(ino)?.file.set_len(size).map_err(io_err)?;
        }
        if changes.atime.is_some() || changes.mtime.is_some() {
            let (a, m) = (time_spec(changes.atime), time_spec(changes.mtime));
            match s.locate(ino)? {
                Loc::Named(dir, name) => sys::utimensat(dir.as_fd(), &name, a, m),
                Loc::Fd(o) => sys::utimens_fd(
                    o.file.as_fd(),
                    a,
                    m,
                    kind == FileKind::Symlink || kind.is_special(),
                ),
            }
            .map_err(io_err)?;
        }
        if let Some(mode) = changes.mode {
            let mode = mode & MODE_MASK;
            // Through a descriptor opened without following symlinks: a path based chmod could
            // land on whatever was swapped into that name.
            if kind.is_special() {
                // An O_PATH descriptor cannot be fchmod'ed: go by name, or by /proc/self/fd.
                match s.locate(ino)? {
                    Loc::Named(dir, name) => sys::fchmodat(dir.as_fd(), &name, mode),
                    #[cfg(target_os = "linux")]
                    Loc::Fd(o) => sys::chmod_fd(o.file.as_fd(), mode),
                    #[cfg(not(target_os = "linux"))]
                    Loc::Fd(_) => Err(std::io::Error::from_raw_os_error(libc::ENOTSUP)),
                }
                .map_err(io_err)?;
                return s.attr(ino);
            }
            let o = s.open_kind(ino)?;
            sys::fchmod(o.file.as_fd(), mode).map_err(io_err)?;
            if mode & 0o200 != 0 {
                s.unpin(ino);
            } else {
                s.pin(ino);
            }
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
        if mode & 0o200 == 0 {
            // Nothing can reopen this for writing later, so keep the descriptor we have.
            s.pin(ino);
        }
        s.add_ref(ino);
        s.attr_of(ino, &st)
    }

    fn mkdir(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        validate_name(name)?;
        let mut s = self.lock();
        let dir = s.dir_fd(parent)?;
        s.invalidate(parent);
        let mode = mode & MODE_MASK;
        // The mode goes in at creation, unfiltered by the umask, so no fchmod follows on a
        // descriptor opened by name (it would land on whatever was renamed over the name). The
        // owner bits are forced on so the directory can be opened. Whatever the result, the mode
        // observed on the new directory is compared with the one asked: a mismatch (no private
        // umask, setuid or setgid dropped, a default ACL or setgid parent, a mount option that
        // forces modes, a server that filters them) takes one fchmod on the descriptor.
        let exact = sys::mkdirat_exact(dir.file.as_fd(), name, mode | 0o700).map_err(io_err)?;
        if !exact {
            sys::mkdirat(dir.file.as_fd(), name, 0o700).map_err(io_err)?;
        }
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;
        let fd = sys::openat(dir.file.as_fd(), name, flags, 0).map_err(io_err)?;
        let mut st = sys::fstat(fd.as_fd()).map_err(io_err)?;
        if st.mode & MODE_MASK != mode {
            sys::fchmod(fd.as_fd(), mode).map_err(io_err)?;
            st = sys::fstat(fd.as_fd()).map_err(io_err)?;
        }
        let open = open_from(fd, false);
        let ino = s.register(parent, &dir.file, name, &st)?;
        s.remember(ino, open);
        s.add_ref(ino);
        s.attr_of(ino, &st)
    }

    fn mknod(
        &self,
        parent: Ino,
        name: &[u8],
        kind: FileKind,
        mode: u32,
        rdev: u64,
    ) -> Result<Attr> {
        validate_name(name)?;
        if !kind.is_special() || (rdev != 0 && !kind.is_device()) {
            return Err(Error::InvalidArgument);
        }
        let ty = match kind {
            FileKind::Fifo => sys::S_IFIFO,
            FileKind::Socket => sys::S_IFSOCK,
            FileKind::CharDevice => sys::S_IFCHR,
            _ => sys::S_IFBLK,
        };
        let host = if kind.is_device() {
            sys::cowfs_to_host(rdev)
        } else {
            0
        };
        let mut s = self.lock();
        let dir = s.dir_fd(parent)?;
        s.invalidate(parent);
        let mode = mode & MODE_MASK;
        // The mode goes in at creation, unfiltered by the umask: a chmod after `mknodat` can only
        // go by name, and lands on whatever another process renamed over the name in between.
        // So the chmod is left for when the observed mode differs from the one asked (no private
        // umask, a default ACL, a mount option or server that rewrites modes).
        let exact = sys::mknodat_exact(dir.file.as_fd(), name, ty | mode, host).map_err(io_err)?;
        if !exact {
            sys::mknodat(dir.file.as_fd(), name, ty | 0o600, host).map_err(io_err)?;
        }
        let mut st = sys::fstatat(dir.file.as_fd(), name).map_err(io_err)?;
        if st.mode & MODE_MASK != mode {
            sys::fchmodat(dir.file.as_fd(), name, mode).map_err(io_err)?;
            st = sys::fstatat(dir.file.as_fd(), name).map_err(io_err)?;
        }
        let ino = s.register(parent, &dir.file, name, &st)?;
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
        if st.file_type() == sys::S_IFDIR {
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
            s.name_removed(i, parent, name);
        }
        Ok(())
    }

    fn rmdir(&self, parent: Ino, name: &[u8]) -> Result<()> {
        validate_name(name)?;
        let mut s = self.lock();
        let dir = s.dir_fd(parent)?;
        s.invalidate(parent);
        let st = sys::fstatat(dir.file.as_fd(), name).map_err(io_err)?;
        if st.file_type() != sys::S_IFDIR {
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
            s.name_removed(i, parent, name);
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
        if let Some(i) = replaced {
            s.name_removed(i, new_parent, new_name);
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
                k if k.is_special() => return Err(Error::InvalidArgument),
                _ => return Err(Error::NotSupported),
            }
            s.open_fd(ino)?.file
        };
        // The loop reads at most READ_MAX bytes per call, so this keeps every offset it touches
        // inside what `off_t` can hold.
        if offset > i64::MAX as u64 - u64::from(READ_MAX) {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; size.min(READ_MAX) as usize];
        let mut got = 0;
        while got < buf.len() {
            let chunk = (buf.len() - got).min(READ_MAX as usize);
            match file.read_at(&mut buf[got..got + chunk], offset + got as u64) {
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
                k if k.is_special() => return Err(Error::InvalidArgument),
                _ => return Err(Error::NotSupported),
            }
            s.open_rw(ino)?.file
        };
        let len = u32::try_from(data.len()).map_err(|_| Error::InvalidArgument)?;
        if offset > i64::MAX as u64 - u64::from(len) {
            return Err(Error::InvalidArgument);
        }
        file.write_all_at(data, offset).map_err(io_err)?;
        Ok(len)
    }

    fn fallocate(&self, ino: Ino, mode: FallocMode, offset: u64, len: u64) -> Result<Attr> {
        // the kernel's mode bits: KEEP_SIZE 1, PUNCH_HOLE 2, ZERO_RANGE 0x10
        let bits = match mode {
            FallocMode::Allocate => 0,
            FallocMode::KeepSize => 0x01,
            FallocMode::PunchHole => 0x02 | 0x01,
            FallocMode::ZeroRange => 0x10,
            FallocMode::ZeroRangeKeepSize => 0x10 | 0x01,
            _ => return Err(Error::NotSupported),
        };
        let file = {
            let mut s = self.lock();
            match s.node(ino)?.kind {
                FileKind::Directory => return Err(Error::IsDir),
                FileKind::Regular => {}
                _ => return Err(Error::InvalidArgument),
            }
            s.open_rw(ino)?.file
        };
        if len == 0 {
            return Err(Error::InvalidArgument);
        }
        // the syscall takes signed offsets: anything that does not fit is past the largest file
        let (Ok(off), Ok(n)) = (i64::try_from(offset), i64::try_from(len)) else {
            return Err(Error::FileTooBig);
        };
        if off.checked_add(n).is_none() {
            return Err(Error::FileTooBig);
        }
        sys::fallocate(file.as_fd(), bits, off, n).map_err(io_err)?;
        self.lock().attr(ino)
    }

    fn flush(&self, ino: Ino) -> Result<()> {
        self.lock().node(ino).map(|_| ())
    }

    fn fsync(&self, ino: Ino, data_only: bool) -> Result<()> {
        let open = {
            let mut s = self.lock();
            let kind = s.node(ino)?.kind;
            // An O_PATH descriptor cannot be synced, and a symlink or special node has no data.
            if kind == FileKind::Symlink || kind.is_special() {
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
        if max == 0 {
            return Err(Error::InvalidArgument);
        }
        let (entries, eof) = self.lock().readdir_page(dir, cookie, max)?;
        Ok(ReadDir {
            entries: entries.into_iter().map(|(e, _)| e).collect(),
            eof,
        })
    }

    fn readdir_attrs(&self, dir: Ino, cookie: u64, max: usize) -> Result<ReadDirPlus> {
        if max == 0 {
            return Err(Error::InvalidArgument);
        }
        let (entries, eof) = self.lock().readdir_page(dir, cookie, max)?;
        Ok(ReadDirPlus {
            entries: entries
                .into_iter()
                .map(|(entry, attr)| DirEntryPlus { entry, attr })
                .collect(),
            eof,
        })
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
        check_xattr_name(name)?;
        self.with_xattr(ino, |t| sys::getxattr(t, name))
    }

    fn setxattr(&self, ino: Ino, name: &[u8], value: &[u8], flags: XattrFlags) -> Result<()> {
        check_xattr_name(name)?;
        self.with_xattr(ino, |t| {
            sys::setxattr(t, name, value, flags.create, flags.replace)
        })
    }

    fn listxattr(&self, ino: Ino) -> Result<Vec<Vec<u8>>> {
        self.with_xattr(ino, sys::listxattr)
    }

    fn removexattr(&self, ino: Ino, name: &[u8]) -> Result<()> {
        check_xattr_name(name)?;
        self.with_xattr(ino, |t| sys::removexattr(t, name))
    }
}

/// Whether this process may make device nodes under `dir`: a probe `mknod` of a character
/// device in a fresh scratch directory there, removed afterwards. `EPERM` means no: no
/// `CAP_MKNOD`, which uid 0 in a user namespace, a root container without that capability and
/// any non-root process all lack, whatever the owner of `/proc/self` says. Always false off
/// Linux, where `PathVfs` cannot make special files at all.
pub fn host_can_make_devices(dir: &Path) -> io::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        let scratch = dir.join(format!(".cowfs-mknod-probe-{}", std::process::id()));
        // A killed run (or a recycled pid) leaves its probe directory behind: clear it.
        force_remove_dir_all(&scratch);
        std::fs::create_dir(&scratch)?;
        let made = std::fs::File::open(&scratch).and_then(|d| {
            match sys::mknodat(
                d.as_fd(),
                b"c",
                sys::S_IFCHR | 0o600,
                sys::cowfs_to_host(cowfs_vfs::makedev(1, 3)),
            ) {
                Ok(()) => Ok(true),
                Err(e) if e.raw_os_error() == Some(libc::EPERM) => Ok(false),
                Err(e) => Err(e),
            }
        });
        force_remove_dir_all(&scratch);
        made
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = dir;
        Ok(false)
    }
}

/// Whether `mknod` sets the mode as it creates the node (Linux, when a thread may detach its own
/// umask with `unshare(CLONE_FS)`). When false it falls back to `mknodat` then a chmod by name,
/// which a node renamed over the name in between receives. A seccomp filter that refuses
/// `unshare`, as Docker's default profile does without `CAP_SYS_ADMIN`, makes it false.
#[cfg(target_os = "linux")]
pub fn mknod_mode_is_atomic() -> bool {
    sys::private_umask_available()
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
