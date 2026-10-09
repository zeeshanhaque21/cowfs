//! Ingesting a directory into a new snapshot.
//!
//! The bytes go into a staging snapshot whose name is reserved, so a crash or a kill -9 leaves
//! nothing visible: the name the caller asked for appears only after the whole tree is written,
//! made durable, and read back through the `Vfs` and compared with the source byte for byte. The
//! switch is then one fork of the staging snapshot into the real name, so it is a single root
//! write like any other snapshot operation.
//!
//! Peak extra disk is bounded by construction: the source is never written to, no uncompressed
//! copy is ever staged on disk, and one 64 KiB buffer is in flight per file. The bytes the store
//! keeps are its own compressed, deduplicated blocks, reported as `stored_bytes`.
//!
//! Symlinks are kept as symlinks and never followed, so the imported tree hashes the same as the
//! source under `cowfs_ctl::hash_tree`. A fifo, a socket or a device node is imported as the same
//! kind of node (`Vfs::mknod`), with its permission bits and, for a device, its device number
//! converted from the host's `st_rdev` encoding to the cowfs one; nothing is created on the host
//! and no privilege is needed, because the store only records the node. The read-back compares
//! kind, mode and device number, never content. Any other kind of entry is refused, because
//! silently dropping it would make the imported tree differ from the source. No directory is
//! special-cased, `.git` included: the source is copied as it is found.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};

use cowfs_vfs::{Attr, FileKind, Ino, Vfs, ROOT_INO};

use crate::{control_meta, swap, ControlError, Core, SnapshotView};

/// Bytes one read or one write carries, and the most the ingest holds in memory at once.
pub const CHUNK: usize = 64 << 10;

/// One entry of a directory on either side of the comparison.
/// Kind, inode, permission bits, size, device number.
type Entry = (FileKind, Ino, u32, u64, u64);

/// What an ingest did to the store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ingested {
    /// Regular files ingested.
    pub files: u64,
    /// Bytes of regular file content read from the source.
    pub bytes: u64,
    /// Unique blocks the store did not already hold, which is how many this ingest added.
    pub blocks: u64,
    /// Bytes those blocks take on disk, headers and compression included.
    pub stored_bytes: u64,
}

/// Why an ingest stopped. No snapshot named as requested exists unless this is `Ok`.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// The caller cancelled, or stopped reading progress.
    #[error("the import was cancelled")]
    Cancelled,
    /// The source cannot be ingested at all: not a directory, or an entry the core cannot hold.
    #[error("{0}")]
    Invalid(String),
    /// The imported tree is not the source tree, reported with the path that differs.
    #[error("{path}: {reason}")]
    Mismatch {
        /// Path relative to the source root.
        path: String,
        /// What differs.
        reason: String,
    },
    /// The core refused, for example because the name is taken.
    #[error(transparent)]
    Core(#[from] ControlError),
}

/// Progress and cancellation for one ingest.
pub struct Hooks<'a> {
    /// Called with the bytes ingested so far and the total the source held when it began.
    /// Returning false stops the ingest, which is what a caller whose peer went away wants.
    pub progress: &'a mut dyn FnMut(u64, u64) -> bool,
}

impl std::fmt::Debug for Hooks<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks").finish_non_exhaustive()
    }
}

impl Hooks<'_> {
    fn report(&mut self, done: u64, total: u64) -> bool {
        (self.progress)(done, total)
    }
}

fn io(what: String, e: std::io::Error) -> ImportError {
    ImportError::Invalid(format!("{what}: {e}"))
}

fn fs_err(what: &str, path: &Path, e: std::io::Error) -> ImportError {
    io(format!("{what} {}", path.display()), e)
}

fn core(e: cowfs_vfs::Error) -> ImportError {
    ImportError::Core(ControlError::Fs(e))
}

/// Ingests `from` as a new snapshot `name`, then makes the name visible.
///
/// The name must be free: an import never replaces a snapshot, so a second call with a name that
/// exists fails rather than writing a second copy of the same content.
pub fn ingest(
    c: &Core,
    from: &Path,
    name: &str,
    hooks: &mut Hooks<'_>,
) -> Result<Ingested, ImportError> {
    ingest_with(c, from, name, hooks, false)
}

/// Like [`ingest`], but a snapshot already called `name` is replaced.
///
/// The new tree is staged under the reserved hidden name and fully verified first, so a failed
/// ingest leaves the old snapshot exactly as it was. The old one goes only once the swap's intent
/// record is on disk, so a crash after that is rolled forward on the next open.
pub fn ingest_replacing(
    c: &Core,
    from: &Path,
    name: &str,
    hooks: &mut Hooks<'_>,
) -> Result<Ingested, ImportError> {
    ingest_with(c, from, name, hooks, true)
}

fn ingest_with(
    c: &Core,
    from: &Path,
    name: &str,
    hooks: &mut Hooks<'_>,
    replace: bool,
) -> Result<Ingested, ImportError> {
    let md = fs::metadata(from).map_err(|e| fs_err("cannot read", from, e))?;
    if !md.is_dir() {
        return Err(ImportError::Invalid(format!(
            "{} is not a directory",
            from.display()
        )));
    }
    crate::validate_snapshot_name(name)?;
    // A swap a failed earlier call left pending for this name is finished before anything below
    // removes its staging snapshot, the only copy of that call's tree (issue 177).
    c.recover_target(name)?;
    let victim = (replace && c.inner.snap_by_name(name).is_ok()).then_some(name);
    c.inner.check_new_name_except(name, victim)?;
    let total = plan(from);
    let staged = swap::staging_name(name);
    // A leftover staging snapshot of this name holds blocks nothing points at: a pending intent was
    // finished above, and `Core::open` removes the orphans a crash left.
    if let Ok(leftover) = c.inner.snap_by_name_raw(&staged) {
        let _ = c.inner.unregister(&leftover);
    }
    let snap = c.inner.meta.new_snapshot(&staged).map_err(control_meta)?;
    let entry = c.inner.register(snap)?;
    let view = SnapshotView::new(c.clone(), entry.ino);
    let before = c.store().stats();
    let mut n = Counters {
        total,
        ..Counters::default()
    };
    let outcome = (|| {
        write_tree(&view, ROOT_INO, from, hooks, &mut n)?;
        let sc = c.inner.snap_by_name_raw(&staged)?;
        c.inner.fsync_snapshot(&sc).map_err(core)?;
        verify(&view, from)?;
        Ok(())
    })();
    let (files, bytes) = match outcome {
        Ok(()) => (n.files, n.bytes),
        Err(e) => {
            // Nothing named `name` exists yet, so dropping the staging snapshot leaves the store
            // exactly as the call found it.
            if let Ok(sc) = c.inner.snap_by_name_raw(&staged) {
                let _ = c.inner.unregister(&sc);
            }
            return Err(e);
        }
    };
    let installed = if victim.is_some() {
        c.replace_with_staged(&staged, name)
    } else {
        c.finish_swap(&staged, name)
    };
    if let Err(e) = installed {
        // On the replacing path the old target may already be gone and `staged` is the only copy of
        // the new tree, which `Core::open` rolls forward from the intent file, so it is kept.
        if victim.is_none() {
            if let Ok(sc) = c.inner.snap_by_name_raw(&staged) {
                let _ = c.inner.unregister(&sc);
            }
        }
        return Err(e.into());
    }
    let after = c.store().stats();
    Ok(Ingested {
        files,
        bytes,
        blocks: after.blocks - before.blocks,
        stored_bytes: after.stored_bytes - before.stored_bytes,
    })
}

/// The bytes of regular files under `dir`, counted before anything is written so progress has a
/// total. A file that vanishes between this walk and the write is the write walk's problem.
fn plan(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries.flatten().fold(0u64, |sum, e| {
        let path = e.path();
        let Ok(md) = fs::symlink_metadata(&path) else {
            return sum;
        };
        if md.is_dir() {
            sum + plan(&path)
        } else if md.is_file() {
            sum.saturating_add(md.size())
        } else {
            sum
        }
    })
}

/// The counters one ingest threads through its walk, so no call needs more than a path and a
/// destination.
#[derive(Debug, Default)]
struct Counters {
    files: u64,
    bytes: u64,
    done: u64,
    /// What the source held when the ingest began, so progress has a total.
    total: u64,
}

/// Writes the tree at `dir` into `parent` in `view`.
fn write_tree(
    view: &SnapshotView,
    parent: Ino,
    dir: &Path,
    hooks: &mut Hooks<'_>,
    n: &mut Counters,
) -> Result<(), ImportError> {
    for (name, path, md) in entries(dir)? {
        let key = name.as_bytes();
        let mode = md.mode() & 0o7777;
        if md.is_dir() {
            let attr = view.mkdir(parent, key, mode).map_err(core)?;
            write_tree(view, attr.ino, &path, hooks, n)?;
        } else if md.is_symlink() {
            let target =
                fs::read_link(&path).map_err(|e| fs_err("cannot read the link", &path, e))?;
            view.symlink(parent, key, target.as_os_str().as_bytes())
                .map_err(core)?;
        } else if md.is_file() {
            n.bytes += write_file(view, parent, key, &path, mode, hooks, n)?;
        } else if let Some(kind) = special_kind(&md) {
            let rdev = if kind.is_device() {
                cowfs_rdev(md.rdev())
            } else {
                0
            };
            view.mknod(parent, key, kind, mode, rdev).map_err(core)?;
        } else {
            return Err(ImportError::Invalid(format!(
                "{} is not a regular file, a directory, a symlink, a fifo, a socket or a device, so it cannot be ingested",
                path.display()
            )));
        }
        if !hooks.report(n.done, n.total) {
            return Err(ImportError::Cancelled);
        }
    }
    Ok(())
}

/// The special kind of a source entry, if it is one.
fn special_kind(md: &std::fs::Metadata) -> Option<FileKind> {
    let t = md.file_type();
    if t.is_fifo() {
        Some(FileKind::Fifo)
    } else if t.is_socket() {
        Some(FileKind::Socket)
    } else if t.is_char_device() {
        Some(FileKind::CharDevice)
    } else if t.is_block_device() {
        Some(FileKind::BlockDevice)
    } else {
        None
    }
}

/// The cowfs device number of a host `st_rdev`: macOS packs an 8 bit major above a 24 bit minor,
/// Linux the glibc layout with a 12 bit major and a 20 bit minor spread over 64 bits.
#[cfg(target_os = "macos")]
fn cowfs_rdev(rdev: u64) -> u64 {
    let rdev = rdev & 0xffff_ffff;
    cowfs_vfs::makedev(((rdev >> 24) & 0xff) as u32, (rdev & 0xff_ffff) as u32)
}

#[cfg(not(target_os = "macos"))]
fn cowfs_rdev(rdev: u64) -> u64 {
    let major = ((rdev >> 8) & 0xfff) | ((rdev >> 32) & !0xfff);
    let minor = (rdev & 0xff) | ((rdev >> 12) & !0xff);
    cowfs_vfs::makedev(major as u32, minor as u32)
}

/// The entries of `dir`, sorted by name bytes so an import of unchanged content does the same
/// writes in the same order every time.
fn entries(dir: &Path) -> Result<Vec<(OsString, PathBuf, std::fs::Metadata)>, ImportError> {
    let read = fs::read_dir(dir).map_err(|e| fs_err("cannot read", dir, e))?;
    let mut out = Vec::new();
    for entry in read {
        let entry = entry.map_err(|e| fs_err("cannot read", dir, e))?;
        let path = entry.path();
        let md = fs::symlink_metadata(&path).map_err(|e| fs_err("cannot stat", &path, e))?;
        out.push((entry.file_name(), path, md));
    }
    out.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(out)
}

/// Streams one file into the snapshot and returns its length. The file is never held whole.
fn write_file(
    view: &SnapshotView,
    parent: Ino,
    name: &[u8],
    src: &Path,
    mode: u32,
    hooks: &mut Hooks<'_>,
    n: &mut Counters,
) -> Result<u64, ImportError> {
    let md = fs::symlink_metadata(src).map_err(|e| fs_err("cannot stat", src, e))?;
    let attr = view.create(parent, name, mode).map_err(core)?;
    let handle = view.open(attr.ino).map_err(core)?;
    let result = stream_into(view, attr.ino, src, hooks, n);
    view.flush(attr.ino).map_err(core)?;
    view.release(handle).map_err(core)?;
    result?;
    n.files += 1;
    Ok(md.size())
}

fn stream_into(
    view: &SnapshotView,
    ino: Ino,
    src: &Path,
    hooks: &mut Hooks<'_>,
    n: &mut Counters,
) -> Result<(), ImportError> {
    let mut file = fs::File::open(src).map_err(|e| fs_err("cannot read", src, e))?;
    let mut buf = vec![0u8; CHUNK];
    let mut off = 0u64;
    loop {
        let got = file
            .read(&mut buf)
            .map_err(|e| fs_err("cannot read", src, e))?;
        if got == 0 {
            return Ok(());
        }
        let mut put = 0;
        while put < got {
            let wrote = view
                .write(ino, off + put as u64, &buf[put..got])
                .map_err(core)? as usize;
            if wrote == 0 {
                return Err(ImportError::Invalid(format!(
                    "{} accepted no bytes at offset {}",
                    src.display(),
                    off + put as u64
                )));
            }
            put += wrote;
        }
        off += got as u64;
        n.done += got as u64;
        if !hooks.report(n.done, n.total) {
            return Err(ImportError::Cancelled);
        }
    }
}

/// Reads the imported tree back through the `Vfs` and compares it with the source: names, kinds,
/// permission bits, sizes, every byte of every file, and every symlink target.
fn verify(view: &SnapshotView, from: &Path) -> Result<(), ImportError> {
    compare_dir(view, ROOT_INO, from)
}

fn compare_dir(view: &SnapshotView, parent: Ino, dir: &Path) -> Result<(), ImportError> {
    let mut imported: BTreeMap<Vec<u8>, Entry> = BTreeMap::new();
    let mut cookie = 0u64;
    loop {
        let page = view.readdir(parent, cookie, 64).map_err(core)?;
        for e in &page.entries {
            let attr: Attr = view.getattr(e.ino).map_err(core)?;
            imported.insert(
                e.name.clone(),
                (attr.kind, e.ino, attr.mode, attr.size, attr.rdev),
            );
        }
        if page.eof {
            break;
        }
        match page.entries.last() {
            Some(last) => cookie = last.cookie,
            // A page with no entries and no eof would spin, so stop and say so.
            None => {
                return Err(ImportError::Invalid(format!(
                    "the imported {} listed no entry and did not report the end",
                    dir.display()
                )))
            }
        }
    }
    for e in imported.values() {
        view.forget(e.1, 1);
    }
    for (name, path, md) in entries(dir)? {
        let Some(&(kind, ino, mode, size, rdev)) = imported.get(name.as_bytes()) else {
            return Err(ImportError::Mismatch {
                path: path.display().to_string(),
                reason: "missing from the imported snapshot".into(),
            });
        };
        let want = if md.is_dir() {
            FileKind::Directory
        } else if md.is_symlink() {
            FileKind::Symlink
        } else if let Some(k) = special_kind(&md) {
            k
        } else {
            FileKind::Regular
        };
        if kind != want {
            return Err(ImportError::Mismatch {
                path: path.display().to_string(),
                reason: format!("imported as {kind:?}, the source is {want:?}"),
            });
        }
        // A symlink's permission bits are not its own: the core gives every symlink 0o777 and a
        // kernel reports whatever it likes (0o755 on macOS), so only real entries are compared.
        if kind != FileKind::Symlink && mode != md.mode() & 0o7777 {
            return Err(ImportError::Mismatch {
                path: path.display().to_string(),
                reason: format!(
                    "imported with mode {mode:o}, the source has {:o}",
                    md.mode() & 0o7777
                ),
            });
        }
        if kind.is_device() && rdev != cowfs_rdev(md.rdev()) {
            return Err(ImportError::Mismatch {
                path: path.display().to_string(),
                reason: format!(
                    "imported with device number {rdev:x}, the source has {:x}",
                    cowfs_rdev(md.rdev())
                ),
            });
        }
        if size != md.len() && want == FileKind::Regular {
            return Err(ImportError::Mismatch {
                path: path.display().to_string(),
                reason: format!("imported {} bytes, the source has {}", size, md.len()),
            });
        }
        match kind {
            FileKind::Directory => compare_dir(view, ino, &path)?,
            FileKind::Symlink => {
                let target =
                    fs::read_link(&path).map_err(|e| fs_err("cannot read the link", &path, e))?;
                let got = view.readlink(ino).map_err(core)?;
                if got != target.as_os_str().as_bytes() {
                    return Err(ImportError::Mismatch {
                        path: path.display().to_string(),
                        reason: "the symlink target differs".into(),
                    });
                }
            }
            FileKind::Regular => compare_bytes(view, ino, &path)?,
            _ => {}
        }
    }
    Ok(())
}

fn compare_bytes(view: &SnapshotView, ino: Ino, src: &Path) -> Result<(), ImportError> {
    let mut file = fs::File::open(src).map_err(|e| fs_err("cannot read", src, e))?;
    let mut buf = vec![0u8; CHUNK];
    let mut off = 0u64;
    loop {
        let want = file
            .read(&mut buf)
            .map_err(|e| fs_err("cannot read", src, e))?;
        if want == 0 {
            return Ok(());
        }
        let got = view.read(ino, off, want as u32).map_err(core)?;
        if got != buf[..want] {
            return Err(ImportError::Mismatch {
                path: src.display().to_string(),
                reason: format!("content differs at offset {off}"),
            });
        }
        off += want as u64;
    }
}
