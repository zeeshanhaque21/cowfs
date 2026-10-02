//! `cowfs-core`: the `Vfs` over the block store and the metadata tree, with a write-back layer.
//!
//! Design: `docs/v1-core.md`. [`Core`] is the mount (its root lists the snapshots) and the
//! control plane (snapshot operations, sync, checks). [`SnapshotView`] shows one snapshot as a
//! plain filesystem.

mod blocks;
mod dcache;
mod error;
mod file;
mod import;
mod inner;
mod ino;
mod io;
mod node;
mod ns;
mod queue;
mod snapname;
mod swap;
mod util;
mod vfs_impl;
mod view;

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
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

/// Test seams for the durability calls the design argument depends on. Not a stable API.
#[doc(hidden)]
pub mod fsops;
pub use crate::import::{ingest, Hooks, ImportError, Ingested};
pub use crate::inner::{FileHealth, Health, LaneHealth};
pub use crate::inner::{Options, Stats};
pub use crate::ino::VIRT_COUNTER_MASK;
pub use crate::snapname::{name_key, validate_snapshot_name, validate_snapshot_name_bytes};
pub use crate::view::SnapshotView;
pub use cowfs_vfs::NAME_MAX;

/// The most one `read` call allocates. The trait says "reads up to `size` bytes", so a larger
/// request is answered in pieces; without a cap a caller can make the mount fault in 4 GiB per call.
pub const MAX_READ_BYTES: u64 = 8 << 20;

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

/// The sync hook that orders a store sync before every durable meta commit.
///
/// It holds the store weakly, so a closed store is not kept alive by the hook: a commit after
/// [`Core::close`] fails instead of writing to a store that no longer reports its state.
pub fn store_sync_hook(store: &Arc<Store>) -> SyncHook {
    let store = Arc::downgrade(store);
    Arc::new(move || {
        let store = store
            .upgrade()
            .ok_or_else(|| std::io::Error::other("the block store is closed"))?;
        store.sync().map_err(|e| match e {
            cowfs_store::Error::Io(e) => e,
            e => std::io::Error::other(e.to_string()),
        })
    })
}

struct Guard {
    inner: Arc<Inner>,
    thread: Mutex<Option<JoinHandle<()>>>,
    done: AtomicBool,
}

impl Guard {
    /// Stops the background flusher and makes everything durable, reporting what went wrong.
    /// Runs at most once, so a drop after an explicit close does not repeat the work.
    fn shutdown(&self) -> Result<(), Error> {
        if self.done.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        *self.inner.bg.0.lk() = true;
        self.inner.bg.1.notify_all();
        if let Some(t) = self.thread.lk().take() {
            let _ = t.join();
        }
        self.inner.sync_all()?;
        self.inner.meta.close().map_err(from_meta)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// An open cowfs: the mount and its control plane. Cloning shares the same filesystem; the last
/// clone to drop commits everything and makes it durable.
#[derive(Clone)]
pub struct Core {
    pub(crate) inner: Arc<Inner>,
    _guard: Arc<Guard>,
}

/// One snapshot's namespace and flush locks, as a test can observe them.
#[doc(hidden)]
pub struct SnapshotLockProbe(Arc<crate::queue::SnapCtx>);

#[doc(hidden)]
impl SnapshotLockProbe {
    /// True when neither lock is held right now.
    pub fn free(&self) -> bool {
        self.0.ns.try_lk().is_some() && self.0.flush.try_lk().is_some()
    }
}

impl std::fmt::Debug for SnapshotLockProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotLockProbe").finish_non_exhaustive()
    }
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core").finish_non_exhaustive()
    }
}

impl Core {
    /// Opens (creating if needed) the store and metadata under `dir`.
    ///
    /// The store sync hook is wired in [`Core::open_with_meta`], the one place that does it, so no
    /// entry point can forget it.
    pub fn open(dir: impl AsRef<Path>, opts: Options) -> Result<Core, Error> {
        Self::open_with_meta(dir, opts, |dir, o| Meta::open(dir.join("meta.redb"), o))
    }

    /// Like [`Core::open`], but the metadata database is built by `make_meta`, which receives the
    /// directory and the options with the store sync hook already set.
    ///
    /// The hook is wired here, in the production path, so a test that needs a different metadata
    /// backend (a recording one, for crash injection) cannot forget it.
    pub fn open_with_meta(
        dir: impl AsRef<Path>,
        opts: Options,
        make_meta: impl FnOnce(&Path, cowfs_meta::Options) -> cowfs_meta::Result<Meta>,
    ) -> Result<Core, Error> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir).map_err(|e| from_io(&e))?;
        let store = Arc::new(Store::open(dir.join("store"), opts.store).map_err(from_store)?);
        let mut mopts = opts.meta.clone();
        mopts.before_sync = Some(store_sync_hook(&store));
        let meta = make_meta(dir, mopts).map_err(from_meta)?;
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
        let (mark, mark_warning) =
            ino::read_virt_mark(dir).counter(!meta.snapshots().map_err(from_meta)?.is_empty());
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
            next_virt: AtomicU64::new(mark),
            virt_reserved: AtomicU64::new(mark),
            virt_lock: Mutex::new(()),
            dirty_bytes: AtomicUsize::new(0),
            uid: md.uid(),
            gid: md.gid(),
            root_time: Mutex::new(Timestamp::now()),
            ctr: Counters::default(),
            pressure: Mutex::new(()),
            bg: (Mutex::new(false), Condvar::new()),
            unsynced: Mutex::new(None),
            last_error: Mutex::new(mark_warning),
            capacity_blocks: (avail / 4096).max(1 << 20).min(total / 4096 + 1),
            base_pack_bytes,
            root: dir.to_path_buf(),
            swap_fault: AtomicU8::new(0),
            load_node_contention: Mutex::new(None),
            flush_fault: Mutex::new(HashMap::new()),
            opts,
        });
        for info in inner.meta.snapshots().map_err(from_meta)? {
            if info.id.0 >= MAX_SNAP {
                // meta hands out ids below its own SNAPSHOT_LIMIT, which is the same limit, so this
                // is unreachable; skipping the snapshot silently would hide it
                return Err(Error::Corrupt(format!(
                    "snapshot id {} does not fit in an inode number (limit {MAX_SNAP})",
                    info.id.0
                )));
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
                done: AtomicBool::new(false),
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
        self.swap_snapshot(old, Some(old), new)
    }

    /// Makes `base` a clone of `src`, replacing an existing `base`.
    ///
    /// Crash-safe and error-safe like [`Core::rename_snapshot`]: if anything fails, the old `base`
    /// is still there.
    pub fn promote_base(&self, src: &str, base: &str) -> Result<SnapshotEntry, ControlError> {
        self.inner.snap_by_name(src)?;
        self.swap_snapshot(src, None, base)
    }

    /// All snapshots in id order.
    /// The snapshots in id order. A staging snapshot of an interrupted swap is not listed: its name
    /// is reserved and it is only reachable through an intent file (see `src/swap.rs`).
    pub fn list_snapshots(&self) -> Result<Vec<SnapshotEntry>, ControlError> {
        let mut out = Vec::new();
        for info in self.inner.meta.snapshots().map_err(control_meta)? {
            if !swap::is_staging(&info.name) && self.inner.snaps.rd().by_id.contains_key(&info.id.0)
            {
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

    /// Shuts the mount down: stops the background flusher, makes everything durable, then closes
    /// the metadata database and the block store, releasing the store lock.
    ///
    /// Dropping the last [`Core`] does the same work but cannot report it. This is the way to
    /// release the lock and learn whether the final flush worked. It needs the last handle, and
    /// reports `Error::Stale` while another clone is still open.
    pub fn close(self) -> Result<(), Error> {
        let Core { inner, _guard } = self;
        if Arc::strong_count(&_guard) > 1 {
            return Err(Error::Stale);
        }
        let guard = Arc::try_unwrap(_guard).map_err(|_| Error::Stale)?;
        guard.shutdown()?;
        // the guard holds its own reference to the shared state, and it must go before the store
        // inside it can be taken out
        drop(guard);
        let mut inner = Arc::try_unwrap(inner).map_err(|_| Error::Stale)?;
        let store = inner.blocks.take_store();
        drop(inner);
        let store = Arc::try_unwrap(store).map_err(|_| Error::Stale)?;
        store.close().map_err(from_store)
    }

    /// Re-hashes every block in the store.
    pub fn fsck(&self) -> Result<FsckReport, Error> {
        self.inner.blocks.store().fsck().map_err(from_store)
    }

    /// Counters and cache sizes.
    pub fn stats(&self) -> Stats {
        self.inner.stats()
    }

    /// The most recent error a background flush hit, of any kind, if any.
    pub fn last_flush_error(&self) -> Option<String> {
        self.inner.last_error.lk().clone()
    }

    /// Blocks that only memory names: chunks of open unlinked files and of files whose chunk list
    /// is not committed yet. Garbage collection must treat them as live.
    /// What is broken, for an operator: files that are dead, files that are only temporarily
    /// unwritable, the lanes still holding pending data, and the last error. Pollable.
    pub fn health(&self) -> Health {
        self.inner.health()
    }

    /// Repairs a file whose data cannot currently be stored, after the cause is gone (space freed,
    /// a descriptor problem fixed, a block rewritten). The bytes are still in memory; this puts the
    /// file back in the dirty set so the next flush stores them. `NotFound` for an inode the mount
    /// does not know.
    pub fn unpoison(&self, ino: Ino) -> Result<(), Error> {
        self.inner.unpoison(ino)
    }

    /// Test seam: a handle on one snapshot's own locks, kept after the snapshot leaves the table. The
    /// liveness property is that nothing holds those locks across a wait for meta's writer lock, and
    /// that is not observable from outside without holding the handle first.
    #[doc(hidden)]
    pub fn snapshot_lock_probe(&self, name: &str) -> Option<SnapshotLockProbe> {
        self.inner.snap_by_name(name).ok().map(SnapshotLockProbe)
    }

    /// Test seam: the bytes the session alias table holds, and how many entries it has.
    #[doc(hidden)]
    pub fn alias_table(&self) -> (usize, usize) {
        (
            self.inner.aliases.rd().len(),
            self.inner.aliases.rd().bytes(),
        )
    }

    /// Test seam: the meta inode number behind `ino`, for a test that has to read meta's own view
    /// of a file. A number a session created is virtual, so it is not the packed meta number.
    #[doc(hidden)]
    pub fn meta_inode(&self, ino: Ino) -> Option<u64> {
        self.inner.meta_of(ino)
    }

    /// Test seam: make the next `tries` node-table insertions lose their race, so `load_node` takes
    /// the retry path instead of relying on a real race to reach it. `ino` 0 arms every inode, which
    /// is what a caller that only knows the alias needs.
    #[doc(hidden)]
    pub fn set_load_node_contention(&self, ino: Ino, tries: usize) {
        *self.inner.load_node_contention.lk() = (tries > 0).then_some((ino, tries));
    }

    /// Test seam: make the next `times` flushes of `ino` fail, `kind` 1 transient (out of space),
    /// 2 corruption. Only `tests/` uses it; there is no other way to make a store fail on demand.
    #[doc(hidden)]
    pub fn set_flush_fault(&self, ino: Ino, kind: u8, times: u32) {
        let mut f = self.inner.flush_fault.lk();
        if times == 0 {
            f.remove(&ino);
        } else {
            f.insert(ino, (kind, times));
        }
    }

    /// The blocks that only memory names right now: those of an open orphan (unlinked, with a
    /// handle or a lookup reference still held) and those of a chunk list that is not committed yet.
    ///
    /// GC (#10) must treat every id here as live. The answer is exact for the state at the end of
    /// the call, or `ControlError::Busy` if a node lock could not be taken within two seconds; it is
    /// never partial. A block first referenced after the call is not in it, so the store's own mark
    /// must protect blocks written during a mark phase.
    pub fn pinned_blocks(&self) -> Result<Vec<BlockId>, ControlError> {
        self.inner.pinned_blocks()
    }

    /// Every block a snapshot's committed tree references, with holes removed.
    ///
    /// `cowfs-meta` yields the all-zero hole ref of a sparse file, which is not a block; this
    /// filters it, so every id here is in the store. GC (#10) should use this, not the walker
    /// directly, until `ChunkRef` has a hole flag (see `docs/v1-core.md`).
    /// Subtrees are skipped if the caller reuses one `Marker` across snapshots.
    pub fn live_blocks(
        &self,
        name: &str,
        marker: &mut cowfs_meta::Marker,
    ) -> Result<Vec<BlockId>, ControlError> {
        let sc = self.inner.snap_by_name(name)?;
        self.inner.flush_snapshot(&sc)?;
        let mut out = Vec::new();
        let walk = sc.snap.live_blocks(marker).map_err(control_meta)?;
        for r in walk {
            let id = r.map_err(control_meta)?;
            if id != file::HOLE {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// The block store.
    pub fn store(&self) -> &Store {
        self.inner.blocks.store()
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

    /// A snapshot by name. A staging name (reserved for an interrupted swap) is not reachable.
    fn snap_by_name(&self, name: &str) -> Result<Arc<SnapCtx>, ControlError> {
        if swap::is_staging(name) {
            return Err(ControlError::NotFound);
        }
        self.snap_by_name_raw(name)
    }

    /// A snapshot by name including a staging name, for the swap itself.
    pub(crate) fn snap_by_name_raw(&self, name: &str) -> Result<Arc<SnapCtx>, ControlError> {
        let s = self.snaps.rd();
        let id = s.by_name.get(name).ok_or(ControlError::NotFound)?;
        s.by_id.get(id).cloned().ok_or(ControlError::NotFound)
    }

    /// A name must be free, and must not alias an existing name on a case-insensitive or
    /// normalising mount (see `snapname::name_key`).
    fn check_new_name(&self, name: &str) -> Result<(), ControlError> {
        self.check_new_name_except(name, None)
    }

    /// [`Inner::check_new_name`], ignoring `except`, the snapshot whose name a swap is replacing.
    /// Staging snapshots are never counted: their names are reserved and invisible.
    fn check_new_name_except(&self, name: &str, except: Option<&str>) -> Result<(), ControlError> {
        if swap::is_staging(name) {
            return Err(ControlError::InvalidName(
                "the name is reserved for an interrupted snapshot swap",
            ));
        }
        let key = crate::snapname::name_key(name);
        let s = self.snaps.rd();
        for n in s.by_name.keys() {
            if swap::is_staging(n) || Some(n.as_str()) == except {
                continue;
            }
            if crate::snapname::name_key(n) == key {
                return Err(ControlError::Exists);
            }
        }
        Ok(())
    }

    fn register(&self, snap: cowfs_meta::Snapshot) -> Result<SnapshotEntry, ControlError> {
        let info = snap.info().map_err(control_meta)?;
        if info.id.0 >= MAX_SNAP {
            *self.last_error.lk() = Some(format!(
                "snapshot id {} does not fit in an inode number (limit {MAX_SNAP}); it is NOT \
                 registered and NOT removed",
                info.id.0
            ));
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
        {
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
                    for _ in 0..64 {
                        match n.st.try_read() {
                            Ok(st) => {
                                freed += st.dirty_bytes();
                                break;
                            }
                            Err(_) => std::thread::yield_now(),
                        }
                    }
                    false
                } else {
                    true
                }
            });
            self.dirty_bytes.fetch_sub(freed, Ordering::AcqRel);
            self.dents.purge_snapshot(sc.id);
            self.aliases.wr().purge_snapshot(sc.id);
            *self.root_time.lk() = Timestamp::now();
        }
        // `removed` blocks every new operation on this snapshot and its caches are empty, so the
        // meta commit can run without the namespace and flush locks: it can wait for another
        // snapshot's store fsync, and holding those locks across that wait is what wedges a mount.
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
            transient: self.ctr.transient.load(Ordering::Relaxed),
            poisoned: self.ctr.poisoned.load(Ordering::Relaxed),
            aliases_dropped: self.ctr.aliases_dropped.load(Ordering::Relaxed),
            dentry_hits: self.ctr.dhit.load(Ordering::Relaxed),
            dentry_misses: self.ctr.dmiss.load(Ordering::Relaxed),
            nodes: self.nodes.len(),
            dentries: self.dents.len(),
            aliases: self.aliases.rd().len(),
            dirty_bytes: self.dirty_bytes.load(Ordering::Relaxed),
            pending_ops: self.all_snaps().iter().map(|sc| sc.q.lk().op_count()).sum(),
        }
    }

    fn pinned_blocks(&self) -> Result<Vec<BlockId>, ControlError> {
        let flushed = self.flushed_of();
        let mut out = HashSet::new();
        let mut nodes = Vec::new();
        self.nodes.retain(|_, n| {
            nodes.push(n.clone());
            true
        });
        for n in nodes {
            let fl = ino::snap_of(n.ino).and_then(|s| flushed.get(&s)).copied();
            // exact or Busy, never a partial answer. No other lock is held here, so waiting for a
            // node lock cannot invert the documented order.
            let Some(st) = n.try_read_for(Duration::from_secs(2)) else {
                return Err(ControlError::Busy);
            };
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
        Ok(out.into_iter().collect())
    }

    /// Everything an operator needs after a failure: which files are dead, which are only
    /// temporarily unwritable, and the last error. Cheap enough to poll.
    fn health(&self) -> Health {
        let mut files = Vec::new();
        self.nodes.retain(|ino, n| {
            let poisoned = n.poison_reason();
            let degraded = n.degraded().map(|e| e.to_string());
            if poisoned.is_some() || degraded.is_some() {
                files.push(FileHealth {
                    ino: *ino,
                    snapshot: ino::snap_of(*ino),
                    poisoned: poisoned.is_some(),
                    reason: poisoned.or(degraded),
                });
            }
            true
        });
        files.sort_by_key(|f| f.ino);
        let mut lanes = Vec::new();
        for sc in self.all_snaps() {
            let stuck = sc.q.lk().dirty_file_count();
            if stuck != 0 {
                lanes.push(LaneHealth {
                    snapshot: sc.name.clone(),
                    id: sc.id,
                    files_stuck: stuck,
                });
            }
        }
        Health {
            files,
            lanes,
            last_error: self.last_error.lk().clone(),
        }
    }

    /// Clears a file's poison and its transient failure, and puts it back in the dirty set so the
    /// next flush tries again. The bytes are still in memory, so this is a repair, not a recovery.
    fn unpoison(&self, ino: Ino) -> Result<(), Error> {
        let n = self.live(ino)?;
        let sc = self.snapctx(ino)?;
        n.unpoison();
        n.clear_degraded();
        sc.q.lk().add_dirty_file(ino);
        Ok(())
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
