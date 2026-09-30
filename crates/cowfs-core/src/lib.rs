//! `cowfs-core`: the `Vfs` over the block store and the metadata tree, with a write-back layer.
//!
//! Design: `docs/v1-core.md`. [`Core`] is the mount (its root lists the snapshots) and the
//! control plane (snapshot operations, sync, checks). [`SnapshotView`] shows one snapshot as a
//! plain filesystem.

mod blocks;
mod dcache;
mod error;
mod file;
mod inner;
mod ino;
mod io;
mod node;
mod ns;
mod queue;
mod swap;
mod util;
mod vfs_impl;
mod view;

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use cowfs_meta::{Meta, NodeId, SnapshotId, SnapshotInfo, SyncHook};
use cowfs_store::{BlockId, FsckReport, Store};
use cowfs_vfs::{Error, Ino, Timestamp};

use crate::blocks::Blocks;
use crate::dcache::DCache;
use crate::error::{from_io, from_meta, from_store};
use crate::inner::{Counters, Inner, Snaps};
use crate::ino::{pack, Aliases, MAX_SNAP};
use crate::queue::SnapCtx;
use crate::util::{MutexExt, RwExt, ShardMap};

pub use crate::inner::{Options, Stats};
pub use crate::view::SnapshotView;

/// Errors from the control plane.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ControlError {
    /// The snapshot name breaks the naming rules.
    #[error("invalid snapshot name: {0}")]
    InvalidName(&'static str),
    /// A snapshot with that name exists.
    #[error("snapshot exists")]
    Exists,
    /// No snapshot with that name.
    #[error("no such snapshot")]
    NotFound,
    /// A handle is open in the snapshot.
    #[error("snapshot is busy: a file is open in it")]
    Busy,
    /// A filesystem or storage error.
    #[error(transparent)]
    Fs(#[from] Error),
}

/// One snapshot as listed by [`Core::list_snapshots`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotEntry {
    /// Snapshot name, the directory name under the mount root.
    pub name: String,
    /// Snapshot id (never reused).
    pub id: u64,
    /// Inode number of the snapshot's root directory.
    pub ino: Ino,
    /// Merkle root of the committed tree.
    pub root: NodeId,
    /// Creation time.
    pub created: Timestamp,
    /// Snapshot this one was cloned from.
    pub parent: Option<u64>,
}

/// Checks a snapshot name: 1 to 255 bytes, no `/` or NUL, not `.` or `..`, and not starting
/// with `._` (AppleDouble) or `.nfs` (NFS silly rename).
pub fn validate_snapshot_name(name: &str) -> Result<(), ControlError> {
    if name.is_empty() {
        return Err(ControlError::InvalidName("empty"));
    }
    if name.len() > cowfs_vfs::NAME_MAX {
        return Err(ControlError::InvalidName("longer than 255 bytes"));
    }
    if name == "." || name == ".." {
        return Err(ControlError::InvalidName("dot names are reserved"));
    }
    if name.contains('/') || name.contains('\0') {
        return Err(ControlError::InvalidName("contains / or NUL"));
    }
    if name.starts_with("._") || name.starts_with(".nfs") {
        return Err(ControlError::InvalidName(
            "names starting with ._ or .nfs are reserved for the OS",
        ));
    }
    Ok(())
}

/// The sync hook that orders a store sync before every durable meta commit.
pub fn store_sync_hook(store: &Arc<Store>) -> SyncHook {
    let store = store.clone();
    Arc::new(move || {
        store.sync().map_err(|e| match e {
            cowfs_store::Error::Io(e) => e,
            e => std::io::Error::other(e.to_string()),
        })
    })
}

struct Guard {
    inner: Arc<Inner>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        *self.inner.bg.0.lk() = true;
        self.inner.bg.1.notify_all();
        if let Some(t) = self.thread.lk().take() {
            let _ = t.join();
        }
        let _ = self.inner.sync_all();
        let _ = self.inner.meta.close();
    }
}

/// An open cowfs: the mount and its control plane. Cloning shares the same filesystem; the last
/// clone to drop commits everything and makes it durable.
#[derive(Clone)]
pub struct Core {
    pub(crate) inner: Arc<Inner>,
    _guard: Arc<Guard>,
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core").finish_non_exhaustive()
    }
}

impl Core {
    /// Opens (creating if needed) the store and metadata under `dir`.
    pub fn open(dir: impl AsRef<Path>, mut opts: Options) -> Result<Core, Error> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir).map_err(|e| from_io(&e))?;
        let store = Arc::new(Store::open(dir.join("store"), opts.store).map_err(from_store)?);
        opts.meta.before_sync = Some(store_sync_hook(&store));
        let meta = Meta::open(dir.join("meta.redb"), opts.meta.clone()).map_err(from_meta)?;
        Self::from_parts(store, meta, opts, dir)
    }

    /// Builds a `Core` from an opened store and metadata. `meta` must have been opened with
    /// [`store_sync_hook`] as its `before_sync`, or durability ordering is lost.
    pub fn from_parts(
        store: Arc<Store>,
        meta: Meta,
        opts: Options,
        dir: &Path,
    ) -> Result<Core, Error> {
        let rec = store.recovery();
        if rec.has_corruption() {
            return Err(Error::Corrupt(format!(
                "block store lost durable data to corruption: {:?}",
                rec.corrupt_synced
            )));
        }
        let md = std::fs::metadata(dir).map_err(|e| from_io(&e))?;
        let total = fs2::total_space(dir).unwrap_or(1 << 40);
        let avail = fs2::available_space(dir).unwrap_or(total);
        let base_pack_bytes = store.stats().pack_bytes;
        let inner = Arc::new(Inner {
            meta,
            blocks: Blocks::new(store, opts.block_cache_bytes),
            snaps: RwLock::new(Snaps::default()),
            nodes: ShardMap::new(),
            dents: DCache::new(opts.dentry_cache),
            aliases: RwLock::new(Aliases::default()),
            handles: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
            next_virt: AtomicU64::new(0),
            dirty_bytes: AtomicUsize::new(0),
            uid: md.uid(),
            gid: md.gid(),
            root_time: Mutex::new(Timestamp::now()),
            ctr: Counters::default(),
            pressure: Mutex::new(()),
            bg: (Mutex::new(false), Condvar::new()),
            unsynced: Mutex::new(None),
            last_error: Mutex::new(None),
            capacity_blocks: (avail / 4096).max(1 << 20).min(total / 4096 + 1),
            base_pack_bytes,
            root: dir.to_path_buf(),
            swap_fault: AtomicU8::new(0),
            opts,
        });
        for info in inner.meta.snapshots().map_err(from_meta)? {
            if info.id.0 >= MAX_SNAP {
                continue;
            }
            let snap = inner.meta.snapshot_by_id(info.id).map_err(from_meta)?;
            inner.add_snap(&info, snap);
        }
        let thread = if inner.opts.background {
            let i = inner.clone();
            let tick = (i.opts.flush_interval / 2).max(Duration::from_millis(5));
            Some(
                std::thread::Builder::new()
                    .name("cowfs-flusher".into())
                    .spawn(move || loop {
                        {
                            let g = i.bg.0.lk();
                            if *g {
                                return;
                            }
                            let (g, _) =
                                i.bg.1
                                    .wait_timeout(g, tick)
                                    .unwrap_or_else(|e| e.into_inner());
                            if *g {
                                return;
                            }
                        }
                        i.tick();
                    })
                    .map_err(|e| from_io(&e))?,
            )
        } else {
            None
        };
        let core = Core {
            _guard: Arc::new(Guard {
                inner: inner.clone(),
                thread: Mutex::new(thread),
            }),
            inner,
        };
        swap::recover(&core);
        Ok(core)
    }

    /// A `Vfs` whose root is the root directory of snapshot `name`.
    pub fn snapshot_view(&self, name: &str) -> Result<SnapshotView, ControlError> {
        let sc = self.inner.snap_by_name(name)?;
        Ok(SnapshotView::new(
            self.clone(),
            pack(sc.id, cowfs_meta::ROOT_INO.0)?,
        ))
    }

    /// Creates an empty snapshot.
    pub fn create_snapshot(&self, name: &str) -> Result<SnapshotEntry, ControlError> {
        validate_snapshot_name(name)?;
        self.inner.check_new_name(name)?;
        let snap = self.inner.meta.new_snapshot(name).map_err(control_meta)?;
        self.inner.register(snap)
    }

    /// Clones snapshot `src` into a new writable snapshot `name` in O(1).
    pub fn fork_snapshot(&self, src: &str, name: &str) -> Result<SnapshotEntry, ControlError> {
        validate_snapshot_name(name)?;
        let sc = self.inner.snap_by_name(src)?;
        self.inner.check_new_name(name)?;
        self.inner.flush_snapshot(&sc)?;
        let snap = sc.snap.fork(name).map_err(control_meta)?;
        self.inner.register(snap)
    }

    /// Removes a snapshot, discarding uncommitted work in it. Refused while a handle is open.
    pub fn remove_snapshot(&self, name: &str) -> Result<(), ControlError> {
        let sc = self.inner.snap_by_name(name)?;
        self.inner.unregister(&sc)
    }

    /// Renames a snapshot. Crash-safe and error-safe (see `src/swap.rs`): a failure before the
    /// old name is removed leaves it untouched, and a crash in between is finished on the next
    /// open. Its snapshot id and every inode number in it change.
    pub fn rename_snapshot(&self, old: &str, new: &str) -> Result<SnapshotEntry, ControlError> {
        self.inner.snap_by_name(old)?;
        self.swap_snapshot(old, Some(old), new, false)
    }

    /// Makes `base` a clone of `src`, replacing an existing `base`.
    ///
    /// Crash-safe and error-safe like [`Core::rename_snapshot`]: if anything fails, the old `base`
    /// is still there.
    pub fn promote_base(&self, src: &str, base: &str) -> Result<SnapshotEntry, ControlError> {
        self.inner.snap_by_name(src)?;
        self.swap_snapshot(src, None, base, true)
    }

    /// All snapshots in id order.
    pub fn list_snapshots(&self) -> Result<Vec<SnapshotEntry>, ControlError> {
        let mut out = Vec::new();
        for info in self.inner.meta.snapshots().map_err(control_meta)? {
            if self.inner.snaps.rd().by_id.contains_key(&info.id.0) {
                out.push(entry(&info)?);
            }
        }
        Ok(out)
    }

    /// The Merkle root of a snapshot after committing everything pending in it.
    pub fn merkle_root(&self, name: &str) -> Result<NodeId, ControlError> {
        let sc = self.inner.snap_by_name(name)?;
        self.inner.flush_snapshot(&sc)?;
        sc.snap.root().map_err(control_meta)
    }

    /// Commits everything pending and makes it durable.
    pub fn sync(&self) -> Result<(), Error> {
        self.inner.sync_all()
    }

    /// Commits everything pending without forcing it to disk.
    pub fn flush(&self) -> Result<(), Error> {
        for sc in self.inner.all_snaps() {
            self.inner.flush_snapshot(&sc)?;
        }
        Ok(())
    }

    /// Commits everything pending, then verifies every metadata invariant.
    pub fn check(&self) -> Result<(), Error> {
        self.flush()?;
        self.inner.meta.check().map_err(from_meta)
    }

    /// Re-hashes every block in the store.
    pub fn fsck(&self) -> Result<FsckReport, Error> {
        self.inner.blocks.store.fsck().map_err(from_store)
    }

    /// Counters and cache sizes.
    pub fn stats(&self) -> Stats {
        self.inner.stats()
    }

    /// The most recent error a background flush hit, if any.
    pub fn last_flush_error(&self) -> Option<String> {
        self.inner.last_error.lk().clone()
    }

    /// Blocks that only memory names: chunks of open unlinked files and of files whose chunk list
    /// is not committed yet. Garbage collection must treat them as live.
    pub fn pinned_blocks(&self) -> Vec<BlockId> {
        self.inner.pinned_blocks()
    }

    /// The block store.
    pub fn store(&self) -> &Arc<Store> {
        &self.inner.blocks.store
    }

    /// The metadata database.
    pub fn meta(&self) -> &Meta {
        &self.inner.meta
    }

    /// Drops every cached dentry, node and block that is not needed for correctness. For tests.
    pub fn drop_caches(&self) {
        self.inner.drop_caches();
    }
}

fn control_meta(e: cowfs_meta::Error) -> ControlError {
    match e {
        cowfs_meta::Error::SnapshotExists => ControlError::Exists,
        cowfs_meta::Error::NoSuchSnapshot => ControlError::NotFound,
        e => ControlError::Fs(from_meta(e)),
    }
}

fn entry(info: &SnapshotInfo) -> Result<SnapshotEntry, Error> {
    Ok(SnapshotEntry {
        name: info.name.clone(),
        id: info.id.0,
        ino: pack(info.id.0, cowfs_meta::ROOT_INO.0)?,
        root: info.root,
        created: Timestamp {
            secs: info.created.secs,
            nanos: info.created.nanos,
        },
        parent: info.parent.map(|p| p.0),
    })
}

impl Inner {
    fn add_snap(&self, info: &SnapshotInfo, snap: cowfs_meta::Snapshot) -> Arc<SnapCtx> {
        let sc = Arc::new(SnapCtx::new(info.id.0, info.name.clone(), snap));
        let mut s = self.snaps.wr();
        s.by_id.insert(info.id.0, sc.clone());
        s.by_name.insert(info.name.clone(), info.id.0);
        sc
    }

    fn snap_by_name(&self, name: &str) -> Result<Arc<SnapCtx>, ControlError> {
        let s = self.snaps.rd();
        let id = s.by_name.get(name).ok_or(ControlError::NotFound)?;
        s.by_id.get(id).cloned().ok_or(ControlError::NotFound)
    }

    fn check_new_name(&self, name: &str) -> Result<(), ControlError> {
        if self.snaps.rd().by_name.contains_key(name) {
            return Err(ControlError::Exists);
        }
        Ok(())
    }

    fn register(&self, snap: cowfs_meta::Snapshot) -> Result<SnapshotEntry, ControlError> {
        let info = snap.info().map_err(control_meta)?;
        if info.id.0 >= MAX_SNAP {
            let _ = self.meta.remove_snapshot(info.id);
            return Err(ControlError::Fs(Error::NoSpace));
        }
        self.add_snap(&info, snap);
        *self.root_time.lk() = Timestamp::now();
        Ok(entry(&info)?)
    }

    fn unregister(&self, sc: &Arc<SnapCtx>) -> Result<(), ControlError> {
        if sc.open_handles.load(Ordering::Acquire) > 0 {
            return Err(ControlError::Busy);
        }
        let _ns = sc.ns.lk();
        let _fl = sc.flush.lk();
        sc.removed.store(true, Ordering::Release);
        {
            let mut s = self.snaps.wr();
            s.by_id.remove(&sc.id);
            s.by_name.remove(&sc.name);
        }
        *sc.q.lk() = queue::Queue::default();
        let mut freed = 0usize;
        self.nodes.retain(|ino, n| {
            if ino::snap_of(*ino) == Some(sc.id) {
                freed += n.st.try_read().map_or(0, |st| st.dirty_bytes());
                false
            } else {
                true
            }
        });
        self.dirty_bytes.fetch_sub(freed, Ordering::AcqRel);
        self.dents.purge_snapshot(sc.id);
        self.aliases.wr().purge_snapshot(sc.id);
        *self.root_time.lk() = Timestamp::now();
        self.meta
            .remove_snapshot(SnapshotId(sc.id))
            .map_err(control_meta)
    }

    fn stats(&self) -> Stats {
        Stats {
            batches: self.ctr.batches.load(Ordering::Relaxed),
            ops_committed: self.ctr.ops.load(Ordering::Relaxed),
            elided: self.ctr.elided.load(Ordering::Relaxed),
            barriers: self.ctr.barriers.load(Ordering::Relaxed),
            forget_underflows: self.ctr.underflows.load(Ordering::Relaxed),
            flush_errors: self.ctr.flush_errors.load(Ordering::Relaxed),
            dentry_hits: self.ctr.dhit.load(Ordering::Relaxed),
            dentry_misses: self.ctr.dmiss.load(Ordering::Relaxed),
            nodes: self.nodes.len(),
            dentries: self.dents.len(),
            aliases: self.aliases.rd().len(),
            dirty_bytes: self.dirty_bytes.load(Ordering::Relaxed),
            pending_ops: self.all_snaps().iter().map(|sc| sc.q.lk().op_count()).sum(),
        }
    }

    fn pinned_blocks(&self) -> Vec<BlockId> {
        let flushed = self.flushed_of();
        let mut out = HashSet::new();
        let mut nodes = Vec::new();
        self.nodes.retain(|_, n| {
            nodes.push(n.clone());
            true
        });
        for n in nodes {
            let fl = ino::snap_of(n.ino).and_then(|s| flushed.get(&s)).copied();
            let Ok(st) = n.st.try_read() else { continue };
            let uncommitted = fl.is_none_or(|fl| n.seq.load(Ordering::Acquire) > fl);
            if let Some(f) = &st.file {
                if st.is_orphan() || uncommitted {
                    out.extend(
                        f.chunks
                            .refs
                            .iter()
                            .map(|r| r.id)
                            .filter(|id| *id != file::HOLE),
                    );
                }
            }
        }
        out.into_iter().collect()
    }

    fn drop_caches(&self) {
        let flushed = self.flushed_of();
        self.nodes.retain(|ino, n| {
            let fl = ino::snap_of(*ino)
                .and_then(|s| flushed.get(&s))
                .copied()
                .unwrap_or(0);
            n.pinned()
                || n.seq.load(Ordering::Acquire) > fl
                || n.ns_seq.load(Ordering::Acquire) > fl
                || n.st.try_read().map_or(true, |st| st.dirty_bytes() > 0)
        });
        self.dents.shrink_all(&|d| {
            ino::snap_of(d)
                .and_then(|s| flushed.get(&s))
                .copied()
                .unwrap_or(u64::MAX)
        });
        self.blocks.clear();
    }
}
