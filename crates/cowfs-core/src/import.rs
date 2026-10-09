//! Ingesting a directory into a new snapshot.
//!
//! The bytes go into a staging snapshot whose name is reserved, so a crash or a kill -9 leaves
//! nothing visible: the name the caller asked for appears only after the whole tree is written,
//! made durable, and read back through the `Vfs` and compared with the source byte for byte. The
//! switch is then a rename of the staging snapshot to the real name, so it is a single metadata
//! transaction like any other snapshot operation.
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
//!
//! Each node keeps its access and modification time to the nanosecond (`Vfs::setattr`), a
//! directory's after its children are written so adding them does not move it, and the names of
//! one hard linked file stay names of one inode (`Vfs::link`), counted within the imported tree
//! only. Build tools judge freshness by those two: a copy that drops either looks changed and is
//! rebuilt (issue 290). Times are not part of any content hash. ctime cannot be set, it is the
//! import time. The read-back compares modification times, link identity and link counts.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::fs;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};

use cowfs_vfs::{Attr, FileKind, Ino, SetAttr, SetTime, Timestamp, Vfs, ROOT_INO};

use crate::{control_meta, swap, ControlError, Core, SnapshotView};

/// Bytes one read or one write carries, and the most the ingest holds in memory at once.
pub const CHUNK: usize = 64 << 10;

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
    // Only a real replacement writes an intent file; a fresh name is installed without one.
    if victim.is_some() {
        swap::check_target_len(name)?;
    }
    let total = plan(from, hooks)?;
    let staged = swap::staging_name(name);
    // A leftover staging snapshot of this name holds blocks nothing points at: a pending intent was
    // finished above, and `Core::open` removes the orphans a crash left.
    c.clear_leftover(&staged, name)?;
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
        set_times(&view, ROOT_INO, &md, from)?;
        let sc = c.inner.snap_by_name_raw(&staged)?;
        c.inner.fsync_snapshot(&sc).map_err(core)?;
        verify(&view, from, hooks, n.total)?;
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
        // On the replacing path the swap either rolled itself back (the staging snapshot is gone)
        // or keeps `staged` pending in the intent file, which `Core::open` rolls forward.
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
///
/// It reports once per entry with a total of 0 (not known yet), so a walk of a huge tree is not
/// silent to a caller that gives up on silence.
fn plan(dir: &Path, hooks: &mut Hooks<'_>) -> Result<u64, ImportError> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(0);
    };
    let mut sum = 0u64;
    for e in entries.flatten() {
        let path = e.path();
        let Ok(md) = fs::symlink_metadata(&path) else {
            continue;
        };
        if md.is_dir() {
            sum = sum.saturating_add(plan(&path, hooks)?);
        } else if md.is_file() {
            sum = sum.saturating_add(md.size());
        }
        if !hooks.report(0, 0) {
            return Err(ImportError::Cancelled);
        }
    }
    Ok(sum)
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
    /// The imported inode of each source file that has more than one name, by source device and
    /// inode, so the next name of it is a link and not a second copy.
    links: HashMap<(u64, u64), Ino>,
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
        let id = (md.dev(), md.ino());
        if !md.is_dir() && md.nlink() > 1 {
            if let Some(&ino) = n.links.get(&id) {
                view.link(ino, parent, key).map_err(core)?;
                if md.is_file() {
                    // Counted like the tree hash counts it: once per name.
                    n.files += 1;
                    n.bytes += md.size();
                    n.done += md.size();
                }
                if !hooks.report(n.done, n.total) {
                    return Err(ImportError::Cancelled);
                }
                continue;
            }
        }
        let ino = if md.is_dir() {
            let attr = view.mkdir(parent, key, mode).map_err(core)?;
            write_tree(view, attr.ino, &path, hooks, n)?;
            attr.ino
        } else if md.is_symlink() {
            let target =
                fs::read_link(&path).map_err(|e| fs_err("cannot read the link", &path, e))?;
            view.symlink(parent, key, target.as_os_str().as_bytes())
                .map_err(core)?
                .ino
        } else if md.is_file() {
            let (ino, len) = write_file(view, parent, key, &path, mode, hooks, n)?;
            n.bytes += len;
            ino
        } else if let Some(kind) = special_kind(&md) {
            let rdev = if kind.is_device() {
                cowfs_rdev(md.rdev())
            } else {
                0
            };
            view.mknod(parent, key, kind, mode, rdev).map_err(core)?.ino
        } else {
            return Err(ImportError::Invalid(format!(
                "{} is not a regular file, a directory, a symlink, a fifo, a socket or a device, so it cannot be ingested",
                path.display()
            )));
        };
        if !md.is_dir() && md.nlink() > 1 {
            n.links.insert(id, ino);
        }
        // After the content and, for a directory, after every child: both move the times.
        set_times(view, ino, &md, &path)?;
        if !hooks.report(n.done, n.total) {
            return Err(ImportError::Cancelled);
        }
    }
    Ok(())
}

/// A source entry's modification time.
fn mtime_of(md: &std::fs::Metadata) -> Timestamp {
    Timestamp {
        secs: md.mtime(),
        nanos: u32::try_from(md.mtime_nsec()).unwrap_or(0),
    }
}

/// Gives the imported node the source's access and modification times.
fn set_times(
    view: &SnapshotView,
    ino: Ino,
    md: &std::fs::Metadata,
    path: &Path,
) -> Result<(), ImportError> {
    let atime = Timestamp {
        secs: md.atime(),
        nanos: u32::try_from(md.atime_nsec()).unwrap_or(0),
    };
    view.setattr(
        ino,
        SetAttr {
            atime: Some(SetTime::At(atime)),
            mtime: Some(SetTime::At(mtime_of(md))),
            ..SetAttr::default()
        },
    )
    .map(|_| ())
    .map_err(|e| ImportError::Invalid(format!("cannot set the times of {}: {e}", path.display())))
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
) -> Result<(Ino, u64), ImportError> {
    let md = fs::symlink_metadata(src).map_err(|e| fs_err("cannot stat", src, e))?;
    let attr = view.create(parent, name, mode).map_err(core)?;
    let handle = view.open(attr.ino).map_err(core)?;
    let result = stream_into(view, attr.ino, src, hooks, n);
    view.flush(attr.ino).map_err(core)?;
    view.release(handle).map_err(core)?;
    result?;
    n.files += 1;
    Ok((attr.ino, md.size()))
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

/// What the read-back carries from directory to directory.
struct Check<'a, 'h> {
    hooks: &'a mut Hooks<'h>,
    total: u64,
    /// Imported inode and in-tree name count of each hard linked source file, by source device
    /// and inode.
    links: HashMap<(u64, u64), (Ino, u32)>,
    /// The source file each imported inode was matched with, so two source files never share one.
    owners: HashMap<Ino, (u64, u64)>,
}

impl Check<'_, '_> {
    /// Keeps a long read-back from being silent: the write is already at its total by now.
    fn tick(&mut self) -> Result<(), ImportError> {
        if self.hooks.report(self.total, self.total) {
            Ok(())
        } else {
            Err(ImportError::Cancelled)
        }
    }
}

/// Reads the imported tree back through the `Vfs` and compares it with the source: names, kinds,
/// permission bits, modification times, sizes, hard link identity and counts, every byte of every
/// file, and every symlink target.
fn verify(
    view: &SnapshotView,
    from: &Path,
    hooks: &mut Hooks<'_>,
    total: u64,
) -> Result<(), ImportError> {
    let mut check = Check {
        hooks,
        total,
        links: HashMap::new(),
        owners: HashMap::new(),
    };
    let root = fs::metadata(from).map_err(|e| fs_err("cannot stat", from, e))?;
    let attr = view.getattr(ROOT_INO).map_err(core)?;
    same_mtime(&attr, &root, from)?;
    compare_dir(view, ROOT_INO, from, &mut check)?;
    for (ino, count) in check.links.values() {
        let attr = view.getattr(*ino).map_err(core)?;
        if attr.nlink != *count {
            return Err(ImportError::Mismatch {
                path: from.display().to_string(),
                reason: format!(
                    "a hard linked file has {} names in the import, {count} in the source",
                    attr.nlink
                ),
            });
        }
    }
    Ok(())
}

fn same_mtime(attr: &Attr, md: &std::fs::Metadata, path: &Path) -> Result<(), ImportError> {
    let want = mtime_of(md);
    if attr.mtime == want {
        return Ok(());
    }
    Err(ImportError::Mismatch {
        path: path.display().to_string(),
        reason: format!(
            "imported with mtime {}.{:09}, the source has {}.{:09}",
            attr.mtime.secs, attr.mtime.nanos, want.secs, want.nanos
        ),
    })
}

fn compare_dir(
    view: &SnapshotView,
    parent: Ino,
    dir: &Path,
    check: &mut Check<'_, '_>,
) -> Result<(), ImportError> {
    let mut imported: BTreeMap<Vec<u8>, Attr> = BTreeMap::new();
    let mut cookie = 0u64;
    loop {
        let page = view.readdir(parent, cookie, 64).map_err(core)?;
        for e in &page.entries {
            let attr: Attr = view.getattr(e.ino).map_err(core)?;
            imported.insert(e.name.clone(), attr);
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
        view.forget(e.ino, 1);
    }
    for (name, path, md) in entries(dir)? {
        let Some(&attr) = imported.get(name.as_bytes()) else {
            return Err(ImportError::Mismatch {
                path: path.display().to_string(),
                reason: "missing from the imported snapshot".into(),
            });
        };
        let (kind, ino, mode, size, rdev) = (attr.kind, attr.ino, attr.mode, attr.size, attr.rdev);
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
        same_mtime(&attr, &md, &path)?;
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
        if kind != FileKind::Directory && md.nlink() > 1 {
            let id = (md.dev(), md.ino());
            let seen = check.links.entry(id).or_insert((ino, 0));
            seen.1 += 1;
            if seen.0 != ino {
                return Err(ImportError::Mismatch {
                    path: path.display().to_string(),
                    reason: "a hard link of the source is a separate file in the import".into(),
                });
            }
        }
        if kind != FileKind::Directory {
            let id = (md.dev(), md.ino());
            if *check.owners.entry(ino).or_insert(id) != id {
                return Err(ImportError::Mismatch {
                    path: path.display().to_string(),
                    reason: "two different source files are one file in the import".into(),
                });
            }
        }
        match kind {
            FileKind::Directory => compare_dir(view, ino, &path, check)?,
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
            // A second name of a file already read back is not read again.
            FileKind::Regular
                if check
                    .links
                    .get(&(md.dev(), md.ino()))
                    .is_some_and(|l| l.1 > 1) => {}
            FileKind::Regular => compare_bytes(view, ino, &path, check)?,
            _ => {}
        }
        check.tick()?;
    }
    Ok(())
}

fn compare_bytes(
    view: &SnapshotView,
    ino: Ino,
    src: &Path,
    check: &mut Check<'_, '_>,
) -> Result<(), ImportError> {
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
        check.tick()?;
    }
}
