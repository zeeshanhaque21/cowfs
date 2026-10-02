//! The backend seam: what the daemon serves, and the snapshot namespace above it.
//!
//! `PathBackend` is the real backend today: a directory of a native filesystem, every
//! snapshot a directory under it. `MemBackend` keeps everything in memory. `cowfs-core`
//! plugs in later by implementing the same two traits, and nothing above this line changes.
//!
//! A snapshot set answers with tree operations, not a `Vfs`: a snapshot is not one
//! filesystem, it is one entry in a namespace whose root the mount shows. That is why the
//! mount's root needs no synthetic layer here: the directories are already there.

use cowfs_ctl::{BaseMeta, SnapshotInfo};
use cowfs_vfs::Vfs;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
