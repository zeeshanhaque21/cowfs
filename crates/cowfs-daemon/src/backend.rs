//! The backend seam: what the daemon serves, and the snapshot namespace above it.
//!
//! `CoreBackend` is the real backend: `cowfs-core`'s `Vfs` over the block store, whose root lists
//! the snapshots and whose control plane is the snapshot namespace. `PathBackend` serves a
//! directory of a native filesystem and exists for the tests that need no store; `MemBackend`
//! keeps everything in memory.
//!
//! A snapshot set answers with tree operations, not a `Vfs`: a snapshot is not one filesystem,
//! it is one entry in a namespace whose root the mount shows. That is why the mount's root needs
//! no synthetic layer here: the entries are already there.

use cowfs_core::{Core, Ingested};
use cowfs_ctl::{BaseMeta, CtlError, CtlResult, ErrorCode, SnapshotInfo};
use cowfs_vfs::Vfs;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// The snapshot namespace a daemon serves.
pub trait Snapshots: Send + Sync + fmt::Debug {
    /// Every snapshot name.
    fn list(&self) -> io::Result<Vec<String>>;

    /// Creates `name`, an O(1) clone of `from` or the empty tree.
    fn create(&self, name: &str, from: Option<&str>) -> io::Result<SnapshotInfo>;

    /// Removes `name` and everything under it.
    fn remove(&self, name: &str) -> io::Result<()>;

    /// Replaces `name` with a fresh clone of `from`.
    fn swap(&self, name: &str, from: &str) -> io::Result<SnapshotInfo>;

    /// Renames a snapshot.
    fn rename(&self, from: &str, to: &str) -> io::Result<()>;

    /// Turns a clone into a base. Idempotent.
    fn promote(&self, name: &str) -> io::Result<SnapshotInfo>;

    /// The `SnapshotInfo` of an existing snapshot, without changing anything.
    fn create_meta(&self, name: &str) -> io::Result<SnapshotInfo>;
}

/// What a daemon serves: a mount tree plus the snapshot namespace above it.
pub trait Backend: Send + Sync + fmt::Debug {
    /// The tree the default mount shows. Its root lists the snapshots as directories.
    fn root(&self) -> io::Result<Arc<dyn Vfs>>;

    /// The tree snapshot `name` shows on its own, for an export at a client-chosen path.
    fn snapshot(&self, name: &str) -> io::Result<Arc<dyn Vfs>>;

    /// The store directory: refused as an export target, reported by `status`.
    fn store_path(&self) -> &Path;

    /// The namespace operations a control handler needs.
    fn snapshots(&self) -> &dyn Snapshots;

    /// Bytes and blocks to report, or `None` when the backend has no block store and the caller
    /// counts the tree itself.
    fn usage(&self) -> io::Result<Option<Usage>> {
        Ok(None)
    }

    /// Makes everything durable and releases whatever the store holds. Called after the mount is
    /// gone, so a backend that locks its store releases the lock here and not at drop.
    fn close(&self) -> io::Result<()> {
        Ok(())
    }

    /// True when a directory can be copied in as a snapshot, which only a backend whose snapshots
    /// are directories can do. A content-addressed backend needs a writer, so it says no rather
    /// than writing a tree where its store cannot read it back.
    fn ingests_directories(&self) -> bool {
        false
    }

    /// Re-hashes every block, or `None` when the backend has no block store to check.
    fn fsck(&self) -> io::Result<Option<cowfs_store::FsckReport>> {
        Ok(None)
    }

    /// Ingests `from` as a new snapshot `name` through the backend's own writer, then verifies it
    /// byte for byte and only then makes the name visible. `None` when this backend has no writer
    /// for a directory, which is the passthrough backend: it copies the directory itself instead.
    fn ingest(
        &self,
        _from: &Path,
        _name: &str,
        _progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> CtlResult<Option<Ingested>> {
        Ok(None)
    }
}

/// What a block store holds, for `status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    /// Unique blocks in the store.
    pub blocks: u64,
    /// Sum of the uncompressed sizes of those blocks.
    pub logical_bytes: u64,
    /// Bytes those blocks take on disk, headers and compression included.
    pub stored_bytes: u64,
}

/// The handle every backend operation shares, so `close` can take the last one.
type CoreSlot = Arc<Mutex<Option<Core>>>;

/// A backend over `cowfs-core`: the `Vfs` over the block store, whose root lists the snapshots
/// and whose control plane is the snapshot namespace.
///
/// `Core` sits behind a lock in an `Option`, not held directly, because [`Core::close`] needs the
/// last handle and every method here takes `&self`.
pub struct CoreBackend {
    core: CoreSlot,
    snaps: CoreSnapshots,
    store: PathBuf,
}

impl fmt::Debug for CoreBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoreBackend")
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl CoreBackend {
    /// Opens the store and metadata under `store`, created if absent.
    ///
    /// `cowfs-core` refuses to open a store that reports damage to data a completed sync made
    /// durable. That refusal is surfaced as it stands: the loss is not acknowledged for the
    /// operator, because accepting it is their decision.
    pub fn open(store: impl AsRef<Path>, opts: cowfs_core::Options) -> io::Result<Self> {
        let store = store.as_ref().to_owned();
        let core = Arc::new(Mutex::new(Some(Core::open(&store, opts).map_err(|e| {
            io::Error::other(format!(
                "{}: {e}. cowfs did not acknowledge the loss; the store needs an operator's \
                     decision (docs/v1-store.md)",
                store.display()
            ))
        })?)));
        Ok(Self {
            snaps: CoreSnapshots {
                core: Arc::clone(&core),
                bases: Mutex::new(Default::default()),
            },
            core,
            store,
        })
    }
}

/// The snapshot namespace of a `Core`. Every method is the core's own control plane: a clone is a
/// fork, a reset is a staged swap, and no tree is ever copied.
///
/// `base` is the one thing the core does not track, so the daemon keeps it, as `PathSnapshots`
/// does: `snapshot_promote` says how a snapshot is used, not what is in it.
#[derive(Debug)]
pub struct CoreSnapshots {
    core: CoreSlot,
    bases: Mutex<std::collections::BTreeSet<String>>,
}

/// Runs `f` against the core in `slot`, or fails once the backend has been closed.
fn with_core<T>(slot: &CoreSlot, f: impl FnOnce(&Core) -> io::Result<T>) -> io::Result<T> {
    let guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
    match guard.as_ref() {
        Some(core) => f(core),
        None => Err(io::Error::other("the core backend is closed")),
    }
}

fn missing(name: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("snapshot {name:?} does not exist"),
    )
}

impl CoreSnapshots {
    fn with<T>(&self, f: impl FnOnce(&Core) -> io::Result<T>) -> io::Result<T> {
        with_core(&self.core, f)
    }

    fn info(&self, name: &str) -> io::Result<SnapshotInfo> {
        let is_base = self.is_base(name);
        self.with(|c| {
            let all = CoreSnapshots::names(c)?;
            let entry = all
                .iter()
                .find(|e| e.name == name)
                .ok_or_else(|| missing(name))?;
            Ok(core_info(entry, &all, is_base))
        })
    }

    fn is_base(&self, name: &str) -> bool {
        self.bases
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(name)
    }

    /// The core's `list_snapshots`.
    fn names(core: &Core) -> io::Result<Vec<cowfs_core::SnapshotEntry>> {
        core.list_snapshots().map_err(control_io)
    }
}

/// A control-plane error as an `io::Error`, keeping which refusal it was: the handler maps the
/// kind to a protocol error code, so `already_exists` and `busy` must not become one code.
/// Maps an ingest failure onto the protocol's codes. A cancelled import is `cancelled`, a refused
/// source or an entry the core cannot hold is `invalid_params`, and a verification mismatch is an
/// `io_error` that says which path differs.
fn ingest_error(e: cowfs_core::ImportError) -> CtlError {
    use cowfs_core::ControlError as E;
    use cowfs_core::ImportError as I;
    match e {
        I::Cancelled => CtlError::cancelled(),
        I::Invalid(m) => CtlError::invalid(m),
        I::Mismatch { path, reason } => CtlError::new(
            ErrorCode::IoError,
            format!("the imported tree is not the source tree: {path}: {reason}"),
        ),
        I::Core(E::Exists) => CtlError::new(
            ErrorCode::AlreadyExists,
            "that snapshot name is already taken".to_owned(),
        ),
        I::Core(E::InvalidName(why)) => CtlError::invalid(format!("invalid snapshot name: {why}")),
        I::Core(e) => CtlError::new(ErrorCode::IoError, e.to_string()),
    }
}

fn control_io(e: cowfs_core::ControlError) -> io::Error {
    let kind = match &e {
        cowfs_core::ControlError::InvalidName(_) => io::ErrorKind::InvalidInput,
        cowfs_core::ControlError::Exists => io::ErrorKind::AlreadyExists,
        cowfs_core::ControlError::NotFound => io::ErrorKind::NotFound,
        cowfs_core::ControlError::Busy => io::ErrorKind::WouldBlock,
        cowfs_core::ControlError::Fs(e) => match e {
            cowfs_vfs::Error::NotFound => io::ErrorKind::NotFound,
            cowfs_vfs::Error::Exists => io::ErrorKind::AlreadyExists,
            cowfs_vfs::Error::PermissionDenied => io::ErrorKind::PermissionDenied,
            cowfs_vfs::Error::NotSupported => io::ErrorKind::Unsupported,
            cowfs_vfs::Error::InvalidArgument => io::ErrorKind::InvalidInput,
            _ => io::ErrorKind::Other,
        },
    };
    io::Error::new(kind, e)
}

/// The `SnapshotInfo` of one core snapshot entry, with the parent's name resolved from its id.
fn core_info(
    entry: &cowfs_core::SnapshotEntry,
    all: &[cowfs_core::SnapshotEntry],
    is_base: bool,
) -> SnapshotInfo {
    SnapshotInfo {
        name: entry.name.clone(),
        parent: entry
            .parent
            .and_then(|id| all.iter().find(|n| n.id == id))
            .map(|n| n.name.clone()),
        base: is_base.then_some(BaseMeta {
            repo: None,
            git_ref: None,
            commit: None,
        }),
        created_unix_ms: u64::try_from(entry.created.secs)
            .unwrap_or(0)
            .saturating_mul(1000)
            .saturating_add(u64::from(entry.created.nanos / 1_000_000)),
    }
}

impl Backend for CoreBackend {
    fn root(&self) -> io::Result<Arc<dyn Vfs>> {
        // The core's root already lists the snapshots, so the default mount needs no wrapper.
        with_core(&self.core, |c| Ok(Arc::new(c.clone()) as Arc<dyn Vfs>))
    }

    fn snapshot(&self, name: &str) -> io::Result<Arc<dyn Vfs>> {
        with_core(&self.core, |c| {
            Ok(Arc::new(c.snapshot_view(name).map_err(control_io)?) as Arc<dyn Vfs>)
        })
    }

    fn store_path(&self) -> &Path {
        &self.store
    }

    fn snapshots(&self) -> &dyn Snapshots {
        &self.snaps
    }

    fn usage(&self) -> io::Result<Option<Usage>> {
        with_core(&self.core, |c| {
            let s = c.store().stats();
            Ok(Some(Usage {
                blocks: s.blocks,
                logical_bytes: s.uncompressed_bytes,
                stored_bytes: s.stored_bytes,
            }))
        })
    }

    /// `base_refresh` still copies a git worktree into the store directory, which means nothing
    /// for a backend whose snapshots are trees, so it is refused here. `import` does not come
    /// through this flag: it goes through [`Backend::ingest`].
    fn ingests_directories(&self) -> bool {
        false
    }

    fn ingest(
        &self,
        from: &Path,
        name: &str,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> CtlResult<Option<Ingested>> {
        // `with_core` speaks `io::Error`, so the ingest error is carried through the message and
        // re-mapped by the one call that can afford to hold both types.
        let mut out: Result<Ingested, CtlError> =
            Err(CtlError::new(ErrorCode::IoError, "the ingest did not run"));
        with_core(&self.core, |c| {
            let mut hooks = cowfs_core::Hooks {
                progress: &mut *progress,
            };
            match cowfs_core::ingest(c, from, name, &mut hooks) {
                Ok(i) => {
                    out = Ok(i);
                    Ok(())
                }
                Err(e) => {
                    out = Err(ingest_error(e));
                    Ok(())
                }
            }
        })
        .map_err(|e| {
            CtlError::new(
                ErrorCode::IoError,
                format!("cannot ingest into the core: {e}"),
            )
        })?;
        out.map(Some)
    }

    fn fsck(&self) -> io::Result<Option<cowfs_store::FsckReport>> {
        with_core(&self.core, |c| {
            c.fsck()
                .map(Some)
                .map_err(|e| io::Error::other(e.to_string()))
        })
    }

    fn close(&self) -> io::Result<()> {
        let taken = self
            .core
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match taken {
            Some(core) => core
                .close()
                .map_err(|e| io::Error::other(format!("closing the core: {e}"))),
            None => Ok(()),
        }
    }
}

impl Snapshots for CoreSnapshots {
    fn list(&self) -> io::Result<Vec<String>> {
        self.with(|c| Ok(Self::names(c)?.into_iter().map(|e| e.name).collect()))
    }

    fn create(&self, name: &str, from: Option<&str>) -> io::Result<SnapshotInfo> {
        self.with(|c| {
            let entry = match from {
                None => c.create_snapshot(name),
                Some(from) => c.fork_snapshot(from, name),
            };
            entry.map_err(control_io).map(|_| ())
        })?;
        self.info(name)
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        self.with(|c| c.remove_snapshot(name).map_err(control_io).map(|_| ()))
    }

    fn swap(&self, name: &str, from: &str) -> io::Result<SnapshotInfo> {
        // Replacing `name` with a clone of `from` is what the core's staged swap does for a base:
        // one fork and one rename, with an intent record, so a crash mid-way is finished on the
        // next open rather than losing the old snapshot.
        self.with(|c| {
            // `promote_base` creates its target when it is absent, which is what a base wants and
            // a reset does not: `snapshot_reset` replaces a snapshot, so a missing one is
            // `not_found` rather than a new snapshot that was never asked for.
            for n in [name, from] {
                Self::names(c)?
                    .into_iter()
                    .any(|e| e.name == n)
                    .then_some(())
                    .ok_or_else(|| missing(n))?;
            }
            c.promote_base(from, name).map_err(control_io).map(|_| ())
        })?;
        let mut info = self.info(name)?;
        // The core forks twice: `from` into a staging name, then the staging name into `name`. So
        // the parent it records is the staging snapshot, which the swap then removes. The
        // protocol's `parent` is the snapshot this one was cloned from, which is `from`.
        info.parent = Some(from.to_owned());
        Ok(info)
    }

    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        self.with(|c| c.rename_snapshot(from, to).map_err(control_io).map(|_| ()))
    }

    fn promote(&self, name: &str) -> io::Result<SnapshotInfo> {
        self.with(|c| {
            Self::names(c)?
                .into_iter()
                .any(|e| e.name == name)
                .then_some(())
                .ok_or_else(|| missing(name))
        })?;
        self.bases
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_owned());
        self.info(name)
    }

    fn create_meta(&self, name: &str) -> io::Result<SnapshotInfo> {
        self.info(name)
    }
}

/// A backend over a directory of a native filesystem. It has no block store, so `status`
/// reports no blocks and the long operations are `unsupported`.
#[derive(Clone, Debug)]
pub struct PathBackend {
    store: PathBuf,
    snaps: PathSnapshots,
}

impl PathBackend {
    /// Serves the directory `store`, created if missing.
    pub fn open(store: impl AsRef<Path>) -> io::Result<Self> {
        std::fs::create_dir_all(store.as_ref())?;
        let store = std::fs::canonicalize(store.as_ref())?;
        Ok(Self {
            snaps: PathSnapshots::new(store.clone()),
            store,
        })
    }
}

impl Backend for PathBackend {
    fn ingests_directories(&self) -> bool {
        true
    }

    fn root(&self) -> io::Result<Arc<dyn Vfs>> {
        Ok(Arc::new(cowfs_vfs_path::PathVfs::new(&self.store)?))
    }

    fn snapshot(&self, name: &str) -> io::Result<Arc<dyn Vfs>> {
        let dir = self.store.join(name);
        if !std::fs::metadata(&dir)?.is_dir() {
            return Err(io::Error::other(format!("{dir:?} is not a directory")));
        }
        Ok(Arc::new(cowfs_vfs_path::PathVfs::new(dir)?))
    }

    fn store_path(&self) -> &Path {
        &self.store
    }

    fn snapshots(&self) -> &dyn Snapshots {
        &self.snaps
    }
}

/// Snapshots as directories under a store. A clone copies the tree: this backend is a
/// passthrough for the seam, not the O(1) store, and `cowfs-core` replaces the copy with a
/// root pointer swap. A `swap` therefore builds the new tree beside the old one and renames
/// it into place, so a failure leaves the old tree whole.
#[derive(Clone, Debug)]
pub struct PathSnapshots {
    store: PathBuf,
    bases: Arc<std::sync::Mutex<std::collections::BTreeSet<String>>>,
}

impl PathSnapshots {
    fn new(store: PathBuf) -> Self {
        Self {
            store,
            bases: Arc::default(),
        }
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.store.join(name)
    }

    fn exists(&self, name: &str) -> bool {
        self.dir(name).is_dir()
    }

    fn info(&self, name: &str, parent: Option<String>) -> SnapshotInfo {
        let base = self
            .bases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(name)
            .then_some(BaseMeta {
                repo: None,
                git_ref: None,
                commit: None,
            });
        SnapshotInfo {
            name: name.to_owned(),
            parent,
            base,
            created_unix_ms: now_ms(),
        }
    }
}

/// Snapshot names that are not directories are ignored: the store is a native filesystem
/// and something else may live in it.
fn snapshot_dirs(store: &Path) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(store)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if cowfs_ctl::validate_snapshot_name(&name).is_ok() && entry.file_type()?.is_dir() {
            out.push(name);
        }
    }
    out.sort();
    Ok(out)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&src, &dst)?;
        } else if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(&src)?, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

impl Snapshots for PathSnapshots {
    fn list(&self) -> io::Result<Vec<String>> {
        snapshot_dirs(&self.store)
    }

    fn create(&self, name: &str, from: Option<&str>) -> io::Result<SnapshotInfo> {
        if self.exists(name) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "name is taken",
            ));
        }
        match from {
            None => std::fs::create_dir(self.dir(name))?,
            Some(from) => {
                if !self.exists(from) {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("snapshot {from:?} does not exist"),
                    ));
                }
                copy_tree(&self.dir(from), &self.dir(name))?;
            }
        }
        Ok(self.info(name, from.map(str::to_owned)))
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        if !self.exists(name) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("snapshot {name:?} does not exist"),
            ));
        }
        cowfs_vfs_path::force_remove_dir_all(&self.dir(name));
        Ok(())
    }

    fn swap(&self, name: &str, from: &str) -> io::Result<SnapshotInfo> {
        for n in [name, from] {
            if !self.exists(n) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("snapshot {n:?} does not exist"),
                ));
            }
        }
        if name == from {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot swap a snapshot with itself",
            ));
        }
        // Build beside the old tree, then swap by rename, so a failed copy changes nothing.
        let staging = self.store.join(format!(".cowfs-swap-{name}"));
        let retired = self.store.join(format!(".cowfs-retired-{name}"));
        cowfs_vfs_path::force_remove_dir_all(&staging);
        cowfs_vfs_path::force_remove_dir_all(&retired);
        copy_tree(&self.dir(from), &staging)?;
        std::fs::rename(self.dir(name), &retired)?;
        std::fs::rename(&staging, self.dir(name))?;
        cowfs_vfs_path::force_remove_dir_all(&retired);
        Ok(self.info(name, Some(from.to_owned())))
    }

    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        if !self.exists(from) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("snapshot {from:?} does not exist"),
            ));
        }
        if self.exists(to) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "name is taken",
            ));
        }
        std::fs::rename(self.dir(from), self.dir(to))
    }

    fn promote(&self, name: &str) -> io::Result<SnapshotInfo> {
        if !self.exists(name) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("snapshot {name:?} does not exist"),
            ));
        }
        self.bases
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(name.to_owned());
        Ok(self.info(name, None))
    }

    fn create_meta(&self, name: &str) -> io::Result<SnapshotInfo> {
        if !self.exists(name) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("snapshot {name:?} does not exist"),
            ));
        }
        Ok(self.info(name, None))
    }
}

/// A backend that keeps everything in memory, for tests that must not touch a disk. It has
/// one flat namespace with no snapshots under it, so `snapshot` and every `Snapshots` method
/// that names one is `unsupported`.
pub struct MemBackend {
    root: Arc<dyn Vfs>,
    snaps: MemSnapshots,
}

impl std::fmt::Debug for MemBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemBackend").finish_non_exhaustive()
    }
}

impl MemBackend {
    /// An empty in-memory filesystem.
    pub fn open() -> io::Result<Self> {
        Ok(Self {
            root: Arc::new(cowfs_vfs_test::MemVfs::new()),
            snaps: MemSnapshots,
        })
    }
}

impl Backend for MemBackend {
    fn root(&self) -> io::Result<Arc<dyn Vfs>> {
        Ok(Arc::clone(&self.root))
    }

    fn snapshot(&self, name: &str) -> io::Result<Arc<dyn Vfs>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("the in-memory backend has no snapshot {name:?}"),
        ))
    }

    fn store_path(&self) -> &Path {
        Path::new(":memory:")
    }

    fn snapshots(&self) -> &dyn Snapshots {
        &self.snaps
    }
}

/// Bytes a file of `n` bytes of that pattern holds, so a write through the core can be checked
/// against what it should be without keeping the pattern in the test.
#[cfg(test)]
fn pattern(n: u64) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A core over a temp dir, with its store closed by the end of the test whatever happens.
    fn core() -> (tempfile::TempDir, CoreBackend) {
        let dir = tempfile::tempdir().unwrap();
        let backend = CoreBackend::open(dir.path(), cowfs_core::Options::default())
            .expect("a core over a fresh store");
        (dir, backend)
    }

    #[test]
    fn the_core_namespace_adds_clones_resets_renames_and_removes() {
        let (_d, b) = core();
        let s = b.snapshots();
        assert_eq!(s.list().unwrap(), Vec::<String>::new());
        assert_eq!(s.create("base", None).unwrap().name, "base");
        assert_eq!(
            s.create("slot", Some("base")).unwrap().parent.as_deref(),
            Some("base"),
            "a clone knows its parent"
        );
        assert_eq!(s.list().unwrap(), ["base", "slot"]);
        assert_eq!(
            s.swap("slot", "base").unwrap().parent.as_deref(),
            Some("base"),
            "a reset is a clone of the other snapshot"
        );
        assert_eq!(s.list().unwrap(), ["base", "slot"]);
        s.remove("slot").unwrap();
        assert_eq!(s.list().unwrap(), ["base"]);
    }

    #[test]
    fn a_clone_of_the_core_is_o1_and_independent() {
        let (_d, b) = core();
        let s = b.snapshots();
        s.create("base", None).unwrap();
        s.create("slot", Some("base")).unwrap();
        // Written through the core's own Vfs, not through the mount: this is the filesystem the
        // adapters serve, and a write into one clone must not reach the other.
        let base = b.snapshot("base").unwrap();
        let slot = b.snapshot("slot").unwrap();
        let root = cowfs_vfs::ROOT_INO;
        let file = base.create(root, b"only-base", 0o644).unwrap();
        let h = base.open(file.ino).unwrap();
        assert_eq!(base.write(file.ino, 0, b"one").unwrap(), 3);
        base.release(h).unwrap();
        assert!(slot.lookup(root, b"only-base").is_err(), "the clone leaked");
        assert_eq!(base.read(file.ino, 0, 8).unwrap(), b"one");
        s.remove("slot").unwrap();
        s.remove("base").unwrap();
    }

    #[test]
    fn every_refusal_is_the_error_code_the_protocol_uses() {
        let (_d, b) = core();
        let s = b.snapshots();
        s.create("base", None).unwrap();
        assert_eq!(
            s.create("base", None).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        // The framework validates the name before the call, so what is left here is the core's
        // own rule: no slash, no leading dot.
        assert_eq!(
            s.create("bad/name", None).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            s.remove("nosuch").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            s.swap("nosuch", "base").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            s.create("slot", Some("nosuch")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn a_promoted_snapshot_reports_itself_as_a_base_and_keeps_doing_so() {
        let (_d, b) = core();
        let s = b.snapshots();
        s.create("base", None).unwrap();
        assert!(s.create_meta("base").unwrap().base.is_none());
        let first = s.promote("base").unwrap();
        assert!(first.base.is_some(), "{first:?}");
        assert!(s.promote("base").unwrap().base.is_some(), "idempotent");
        assert!(s.create_meta("base").unwrap().base.is_some());
        assert_eq!(
            s.promote("nosuch").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn usage_reports_the_blocks_the_store_holds() {
        let (_d, b) = core();
        b.snapshots().create("base", None).unwrap();
        // The core's own root is a read-only namespace of snapshots, so the bytes go into one.
        let base = b.snapshot("base").unwrap();
        let file = base.create(cowfs_vfs::ROOT_INO, b"payload", 0o644).unwrap();
        let h = base.open(file.ino).unwrap();
        assert_eq!(base.write(file.ino, 0, &pattern(3 << 20)).unwrap(), 3 << 20);
        base.fsync(file.ino, false).unwrap();
        base.release(h).unwrap();
        let u = b.usage().unwrap().expect("the core has a store");
        assert!(u.blocks > 0, "{u:?}");
        assert!(
            u.stored_bytes > 0 && u.stored_bytes <= u.logical_bytes,
            "{u:?}"
        );
    }

    #[test]
    fn close_releases_the_store_so_the_next_open_is_not_locked_out() {
        let dir = tempfile::tempdir().unwrap();
        let b = CoreBackend::open(dir.path(), cowfs_core::Options::default()).unwrap();
        b.snapshots().create("base", None).unwrap();
        b.close().expect("the core closes");
        let again = CoreBackend::open(dir.path(), cowfs_core::Options::default())
            .expect("the store reopens after a close");
        assert_eq!(again.snapshots().list().unwrap(), ["base"]);
    }

    #[test]
    fn a_store_that_is_still_open_is_not_closed_behind_another_handles_back() {
        let dir = tempfile::tempdir().unwrap();
        let b = CoreBackend::open(dir.path(), cowfs_core::Options::default()).unwrap();
        let live = b.snapshots().create("base", None).unwrap();
        let core_again = CoreBackend::open(dir.path(), cowfs_core::Options::default());
        assert!(
            core_again.is_err(),
            "the second open must fail while the first holds the store"
        );
        assert_eq!(live.name, "base");
        b.close().unwrap();
        CoreBackend::open(dir.path(), cowfs_core::Options::default())
            .expect("the store is free again");
    }
}

#[derive(Debug)]
struct MemSnapshots;

fn unsupported(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, what.to_owned())
}

impl Snapshots for MemSnapshots {
    fn list(&self) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn create(&self, _name: &str, _from: Option<&str>) -> io::Result<SnapshotInfo> {
        Err(unsupported("the in-memory backend has no snapshots"))
    }
    fn remove(&self, _name: &str) -> io::Result<()> {
        Err(unsupported("the in-memory backend has no snapshots"))
    }
    fn swap(&self, _name: &str, _from: &str) -> io::Result<SnapshotInfo> {
        Err(unsupported("the in-memory backend has no snapshots"))
    }
    fn rename(&self, _from: &str, _to: &str) -> io::Result<()> {
        Err(unsupported("the in-memory backend has no snapshots"))
    }
    fn promote(&self, _name: &str) -> io::Result<SnapshotInfo> {
        Err(unsupported("the in-memory backend has no snapshots"))
    }
    fn create_meta(&self, _name: &str) -> io::Result<SnapshotInfo> {
        Err(unsupported("the in-memory backend has no snapshots"))
    }
}
#[test]
fn probe_alias_release_makes_a_held_number_stale() {
    let dir = tempfile::tempdir().unwrap();
    let b = CoreBackend::open(dir.path(), cowfs_core::Options::default()).unwrap();
    b.snapshots().create("base", None).unwrap();
    let v = b.snapshot("base").unwrap();
    let r = cowfs_vfs::ROOT_INO;
    let d = v.mkdir(r, b"d", 0o755).unwrap();
    eprintln!("mkdir d -> {:#x}", d.ino);
    // The NFS adapter forgets the reference the moment it hands the attribute out.
    v.forget(d.ino, 1);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    eprintln!(
        "forgotten+flushed: getattr {:#x} -> {:?}",
        d.ino,
        v.getattr(d.ino).err()
    );
    let held = v.mkdir(r, b"e", 0o755).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    eprintln!(
        "reference held: getattr {:#x} -> {:?}",
        held.ino,
        v.getattr(held.ino).err()
    );
}
