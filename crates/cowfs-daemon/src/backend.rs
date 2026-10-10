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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

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

    /// Records where the base `name` was built from, durably, before this returns.
    ///
    /// Separate from [`Snapshots::promote`] because the two answer different questions: `promote`
    /// says how a snapshot is used, this says what is in it. A refresh publishes a base by calling
    /// both, and a caller that reconnects must be told the same commit, so this cannot be
    /// satisfied by returning the record in a response.
    fn set_base_meta(&self, name: &str, meta: &BaseMeta) -> io::Result<()>;

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
    ///
    /// `progress(done, total)` is called often while it works and returns false to stop the check,
    /// which then fails; a caller throttles.
    fn fsck(
        &self,
        _progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> io::Result<Option<cowfs_store::FsckReport>> {
        Ok(None)
    }

    /// One garbage-collection cycle, or `None` when the backend has no block store to collect.
    ///
    /// `progress` returns false to ask the cycle to stop, and `cancelled` is polled while the cycle
    /// is quiet, so a cancel does not wait for the next event. A dry run changes nothing.
    fn collect_garbage(
        &self,
        _dry_run: bool,
        _progress: GcProgress<'_>,
        _cancelled: &dyn Fn() -> bool,
    ) -> CtlResult<Option<GcOutcome>> {
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

    /// Like [`Backend::ingest`], but a snapshot already called `name` is replaced, only once the
    /// new tree is staged and verified. `None` when the backend has no writer.
    fn ingest_replacing(
        &self,
        _from: &Path,
        _name: &str,
        _progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> CtlResult<Option<Ingested>> {
        Ok(None)
    }
}

/// What one garbage-collection cycle did, with the block counts around it.
#[derive(Clone, Debug)]
pub struct GcOutcome {
    /// What the collector reported.
    pub report: cowfs_gc::GcReport,
    /// Blocks the store held before the cycle.
    pub blocks_before: u64,
    /// Blocks the store held after it.
    pub blocks_after: u64,
}

/// What one garbage-collection cycle is told by its caller: `progress` gets every event and says
/// whether to go on, `cancelled` is polled between events.
pub type GcProgress<'a> = &'a mut dyn FnMut(&cowfs_gc::Progress) -> bool;

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
    gc_opts: cowfs_gc::Options,
    gc: GcSlot,
}

/// The one running collection, so `close` can stop it and wait for it before it takes the core.
#[derive(Default)]
struct GcSlot {
    state: Mutex<GcState>,
    idle: Condvar,
}

#[derive(Default)]
struct GcState {
    running: bool,
    closing: bool,
    cancel: Option<Arc<AtomicBool>>,
}

/// Marks the collection finished, whatever way it ends. Declared before the collector's `Core`
/// clone is moved away, so the clone is gone by the time `close` is woken.
struct GcRun<'a>(&'a GcSlot);

impl Drop for GcRun<'_> {
    fn drop(&mut self) {
        let mut g = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        g.running = false;
        g.cancel = None;
        self.0.idle.notify_all();
    }
}

/// How long `close` waits for a cancelled collection to stop.
const GC_STOP_PATIENCE: Duration = Duration::from_secs(120);

fn gc_error(e: cowfs_gc::Error) -> CtlError {
    CtlError::new(ErrorCode::IoError, format!("garbage collection: {e}"))
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
        Self::open_with_gc(store, opts, cowfs_gc::Options::default())
    }

    /// Like [`CoreBackend::open`], with the collector's thresholds chosen by the caller.
    pub fn open_with_gc(
        store: impl AsRef<Path>,
        opts: cowfs_core::Options,
        gc_opts: cowfs_gc::Options,
    ) -> io::Result<Self> {
        let store = store.as_ref().to_owned();
        let core = Arc::new(Mutex::new(Some(Core::open(&store, opts).map_err(|e| {
            io::Error::other(format!(
                "{}: {e}. cowfs did not acknowledge the loss; the store needs an operator's \
                     decision (docs/v1-store.md)",
                store.display()
            ))
        })?)));
        // Same rule as the path backend: a store whose base records cannot be read is not one
        // this backend may serve.
        let bases = crate::base_meta::BaseMetaStore::open(&store).map_err(|e| {
            io::Error::other(format!(
                "cannot read the base records in {}: {e}",
                store.display()
            ))
        })?;
        Ok(Self {
            snaps: CoreSnapshots {
                core: Arc::clone(&core),
                bases,
            },
            core,
            store,
            gc_opts,
            gc: GcSlot::default(),
        })
    }

    fn run_gc(
        &self,
        core: Core,
        cancel: &AtomicBool,
        dry_run: bool,
        progress: GcProgress<'_>,
        cancelled: &dyn Fn() -> bool,
    ) -> CtlResult<GcOutcome> {
        let blocks_before = core.store().stats().blocks;
        let collector = core
            .collector(cowfs_gc::Options {
                dry_run,
                ..self.gc_opts
            })
            .map_err(gc_error)?;
        let (tx, rx) = mpsc::channel::<cowfs_gc::Progress>();
        collector.gc().set_progress(move |p| {
            let _ = tx.send(*p);
        });
        progress(&cowfs_gc::Progress::default());
        let report = std::thread::scope(|s| {
            let worker = s.spawn(|| collector.collect());
            let mut stopping = false;
            loop {
                if !stopping && (cancelled() || cancel.load(Ordering::Acquire)) {
                    collector.gc().cancel();
                    stopping = true;
                }
                if let Ok(p) = rx.recv_timeout(Duration::from_millis(50)) {
                    if !progress(&p) && !stopping {
                        collector.gc().cancel();
                        stopping = true;
                    }
                }
                if worker.is_finished() {
                    break;
                }
            }
            while let Ok(p) = rx.try_recv() {
                progress(&p);
            }
            worker.join()
        });
        let report = match report {
            Ok(r) => r.map_err(gc_error)?,
            Err(_) => {
                return Err(CtlError::new(
                    ErrorCode::IoError,
                    "garbage collection: the collector panicked",
                ))
            }
        };
        let blocks_after = core.store().stats().blocks;
        Ok(GcOutcome {
            report,
            blocks_before,
            blocks_after,
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
    bases: crate::base_meta::BaseMetaStore,
}

/// Runs `f` against the core in `slot`, or fails once the backend has been closed.
fn with_core<T>(slot: &CoreSlot, f: impl FnOnce(&Core) -> io::Result<T>) -> io::Result<T> {
    let guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
    match guard.as_ref() {
        Some(core) => f(core),
        None => Err(io::Error::other("the core backend is closed")),
    }
}

/// Makes `name` a base whose provenance is unknown, if it was a base at all.
///
/// A swap replaces the tree under a name, so the record of the tree that was there describes bytes
/// that are gone. Reporting that record would be a claim about the new tree that nothing supports.
/// Invalidating it keeps the base designation and drops every field, which is the state
/// `base_meta` already documents as "provenance unknown" and which `base status` reports as stale
/// rather than fresh.
///
/// A name with no record is not a base and does not become one here: a plain snapshot must not
/// acquire a record that nothing asked for.
fn io_err_ctl(what: &str, e: io::Error) -> CtlError {
    CtlError::new(ErrorCode::IoError, format!("{what}: {e}"))
}

fn invalidate_base_record(
    root: &Path,
    records: &mut std::collections::BTreeMap<String, crate::base_meta::Record>,
    name: &str,
) -> io::Result<()> {
    if records.get(name).is_none() {
        return Ok(());
    }
    crate::base_meta::write_locked(
        root,
        records,
        name,
        &crate::base_meta::Record::promoted_unknown(),
    )
}

/// Puts a base record back where it was after a snapshot rename failed, and says so if even that
/// fails, because a record left under the new name would describe a snapshot that is not there.
fn rollback_base(
    root: &Path,
    records: &mut std::collections::BTreeMap<String, crate::base_meta::Record>,
    moved_to: &str,
    was: &str,
    cause: io::Error,
) -> io::Error {
    match crate::base_meta::rename_locked(root, records, moved_to, was) {
        Ok(()) => cause,
        Err(rollback) => io::Error::new(
            cause.kind(),
            format!("{cause}; and the base record could not be moved back: {rollback}"),
        ),
    }
}

/// A test-only point inside a backend operation, so two threads can be interleaved at a chosen
/// instruction instead of by racing. Compiled only into the test binary: there is no production hook
/// here, and no production path reaches this module.
#[cfg(test)]
pub(crate) mod test_hook {
    use std::sync::{Arc, Mutex, PoisonError};

    type Hook = Arc<dyn Fn(&str) + Send + Sync>;
    static HOOK: Mutex<Option<Hook>> = Mutex::new(None);

    /// Removes the hook when it goes out of scope, so a test that panics with one installed cannot
    /// leave it behind to park an unrelated test inside a lock it will never be given.
    pub(crate) struct Guard(());

    impl Guard {
        pub(crate) fn install(hook: Hook) -> Guard {
            *HOOK.lock().unwrap_or_else(PoisonError::into_inner) = Some(hook);
            Guard(())
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            *HOOK.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
    }

    /// Announces that `point` has been reached. The store's own lock is released first, so a hook may
    /// block here without holding anything but whatever the test itself is waiting on.
    pub(crate) fn reach(point: &str) {
        let hook = HOOK.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(hook) = hook {
            hook(point);
        }
    }
}

/// Where a create has decided the name is free and has not yet touched the record.
#[cfg(test)]
const AFTER_PRECHECK: &str = "create:after-precheck";

/// The refusal for a name that is taken. One helper so both backends report a duplicate the same way,
/// whichever one serves the store.
fn name_taken() -> io::Error {
    io::Error::new(io::ErrorKind::AlreadyExists, "name is taken")
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
        let base = self.bases.get(name);
        self.with(|c| {
            let all = CoreSnapshots::names(c)?;
            let entry = all
                .iter()
                .find(|e| e.name == name)
                .ok_or_else(|| missing(name))?;
            Ok(core_info(entry, &all, base))
        })
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
    base: Option<BaseMeta>,
) -> SnapshotInfo {
    SnapshotInfo {
        name: entry.name.clone(),
        parent: entry
            .parent
            .and_then(|id| all.iter().find(|n| n.id == id))
            .map(|n| n.name.clone()),
        base,
        created_unix_ms: u64::try_from(entry.created.secs)
            .unwrap_or(0)
            .saturating_mul(1000)
            .saturating_add(u64::from(entry.created.nanos / 1_000_000)),
    }
}

impl CoreBackend {
    /// Runs one of the core's ingests and maps its error onto the protocol's codes.
    fn ingest_with(
        &self,
        from: &Path,
        name: &str,
        progress: &mut dyn FnMut(u64, u64) -> bool,
        ingest: fn(
            &Core,
            &Path,
            &str,
            &mut cowfs_core::Hooks<'_>,
        ) -> Result<Ingested, cowfs_core::ImportError>,
    ) -> CtlResult<Option<Ingested>> {
        // `with_core` speaks `io::Error`, so the ingest error is carried through a local and
        // re-mapped by the one call that can afford to hold both types.
        let mut out: Result<Ingested, CtlError> =
            Err(CtlError::new(ErrorCode::IoError, "the ingest did not run"));
        with_core(&self.core, |c| {
            let mut hooks = cowfs_core::Hooks {
                progress: &mut *progress,
            };
            match ingest(c, from, name, &mut hooks) {
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

    /// Snapshots here are trees, not directories, so nothing copies a directory into the store.
    /// `import` and `base_refresh` go through [`Backend::ingest`] instead.
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
        self.ingest_with(from, name, progress, cowfs_core::ingest)
    }

    fn ingest_replacing(
        &self,
        from: &Path,
        name: &str,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> CtlResult<Option<Ingested>> {
        // The old record describes the tree about to be replaced, so it is cleared first: once the
        // core rolls forward, no error can say whether the swap happened. A cancelled or refused
        // ingest fails before the core touches the old tree, so for those the record goes back.
        // Any other error leaves it cleared, and the base reports stale rather than a commit it may
        // no longer have.
        let saved = self
            .snaps
            .bases
            .exclusive(|records| {
                let saved = records.get(name).cloned();
                invalidate_base_record(self.snaps.bases.root(), records, name).map(|()| saved)
            })
            .map_err(|e| io_err_ctl("cannot clear the old base record", e))?;
        let out = self.ingest_with(from, name, progress, cowfs_core::ingest_replacing);
        if let (Err(e), Some(saved)) = (&out, saved) {
            if matches!(e.code, ErrorCode::Cancelled | ErrorCode::InvalidParams) {
                let _ = self.snaps.bases.exclusive(|records| {
                    crate::base_meta::write_locked(self.snaps.bases.root(), records, name, &saved)
                });
            }
        }
        out
    }

    fn collect_garbage(
        &self,
        dry_run: bool,
        progress: GcProgress<'_>,
        cancelled: &dyn Fn() -> bool,
    ) -> CtlResult<Option<GcOutcome>> {
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut g = self.gc.state.lock().unwrap_or_else(PoisonError::into_inner);
            if g.closing {
                return Err(CtlError::new(
                    ErrorCode::Busy,
                    "the daemon is shutting down",
                ));
            }
            if g.running {
                return Err(CtlError::new(
                    ErrorCode::Busy,
                    "a garbage collection is already running",
                ));
            }
            g.running = true;
            g.cancel = Some(Arc::clone(&cancel));
        }
        let _run = GcRun(&self.gc);
        // A clone, so the slot lock is not held for the length of a sweep. It is moved into
        // `run_gc` and dropped there, before `GcRun` wakes a waiting `close`. It is taken after the
        // registration above, so a `close` that does not see this run cannot take the core first.
        let core = with_core(&self.core, |c| Ok(c.clone()))
            .map_err(|e| CtlError::new(ErrorCode::IoError, e.to_string()))?;
        self.run_gc(core, &cancel, dry_run, progress, cancelled)
            .map(Some)
    }

    fn fsck(
        &self,
        progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> io::Result<Option<cowfs_store::FsckReport>> {
        with_core(&self.core, |c| {
            c.fsck_with(progress)
                .map(Some)
                .map_err(|e| io::Error::other(e.to_string()))
        })
    }

    fn close(&self) -> io::Result<()> {
        {
            let mut g = self.gc.state.lock().unwrap_or_else(PoisonError::into_inner);
            g.closing = true;
            if let Some(c) = &g.cancel {
                c.store(true, Ordering::Release);
            }
            let deadline = Instant::now() + GC_STOP_PATIENCE;
            while g.running {
                if Instant::now() >= deadline {
                    return Err(io::Error::other(
                        "a garbage collection did not stop when it was asked to",
                    ));
                }
                g = self
                    .gc
                    .idle
                    .wait_timeout(g, Duration::from_secs(1))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
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
        // The name is checked before anything destructive, the way the path backend checks it. The core
        // does refuse a duplicate itself, but it refuses it after this point, so clearing the record
        // first cost a refused duplicate its base: the snapshot was left with no record on disk and none
        // in the map, live and after a reopen.
        //
        // The check, the clear and the creation are one critical section, so a name another thread takes
        // in between cannot have the record it has just published deleted by this create. All four
        // namespace mutations of a name go through one of these methods and each takes the same section,
        // so nothing can slip between the decision and the record change.
        //
        // Every failure inside the section is safe because of what the check proved. If the record cannot
        // be cleared this returns an error and no snapshot is created, so nothing half-exists. If it was
        // cleared and the core then refuses, the record that went belonged to a name with no snapshot
        // behind it.
        self.bases.exclusive(|records| {
            if self.with(|c| Ok(CoreSnapshots::names(c)?.iter().any(|e| e.name == name)))? {
                return Err(name_taken());
            }
            #[cfg(test)]
            test_hook::reach(AFTER_PRECHECK);
            // A new snapshot is never a base, so any record left under this name by a snapshot that has
            // since gone is cleared before it exists. Without this, a record orphaned by an interrupted
            // rename would attach itself to the next snapshot created with the same name and report a
            // commit that has nothing to do with its contents.
            crate::base_meta::remove_locked(self.bases.root(), records, name)?;
            self.with(|c| {
                let entry = match from {
                    None => c.create_snapshot(name),
                    Some(from) => c.fork_snapshot(from, name),
                };
                entry.map_err(control_io).map(|_| ())
            })
        })?;
        self.info(name)
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        // One critical section, for the same reason as create: the tree going and its record going are
        // one step, so a reader never sees a snapshot without its base, or a base without its snapshot.
        self.bases.exclusive(|records| {
            self.with(|c| c.remove_snapshot(name).map_err(control_io).map(|_| ()))?;
            crate::base_meta::remove_locked(self.bases.root(), records, name)
        })
    }

    fn swap(&self, name: &str, from: &str) -> io::Result<SnapshotInfo> {
        // Replacing `name` with a clone of `from` is what the core's staged swap does for a base:
        // one fork and one rename, with an intent record, so a crash mid-way is finished on the
        // next open rather than losing the old snapshot.
        //
        // One critical section, for the same reason as create, remove and rename: the tree going and
        // its record going are one step. The old record cannot survive the swap, because it
        // describes the tree that is being replaced, and a base that keeps it would report a commit
        // that did not produce the tree now under its name.
        self.bases.exclusive(|records| {
            // Refusals that are decided before anything is touched, while the old tree and the old
            // record are both still there: a source or target that does not exist, and a swap with
            // itself. `promote_base` would refuse these too, but only after the record was cleared,
            // and a refusal is not a replacement, so it must leave the record alone. Holding this
            // section across the check and the swap is what makes that safe: create, remove, rename
            // and promote all mutate the namespace inside this same section, so nothing can appear
            // or disappear between the check and the swap.
            //
            // Existence is checked before the same-name refusal, because that is the order the core
            // itself used: `promote_base` looks the source up first, so a swap of a name with itself
            // reported `not_found` before anything else ran. Checking existence first keeps that
            // answer, and keeps the two backends agreeing on it.
            self.with(|c| {
                for n in [name, from] {
                    Self::names(c)?
                        .into_iter()
                        .any(|e| e.name == n)
                        .then_some(())
                        .ok_or_else(|| missing(n))?;
                }
                Ok::<(), io::Error>(())
            })?;
            if name == from {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "cannot swap a snapshot with itself",
                ));
            }

            // Past this point the swap may have replaced the tree, so the record is cleared and
            // stays cleared.
            //
            // An `Err` out of the staged swap does not mean the old tree is still there. Past its
            // point of no return the core rolls forward and only reports an error when the
            // roll-forward itself fails, which leaves the intent file for the next `Core::open` to
            // finish (`crates/cowfs-core/src/swap.rs`). The error alone cannot tell a rollback from
            // a failed roll-forward, so restoring the old record on `Err` would put `commit-AAA`
            // back over a tree the pending intent is about to install, which is the stale
            // provenance this swap exists to prevent.
            //
            // So an error from here reports the failure and leaves the provenance invalidated.
            // On a swap that really did roll back that loses provenance that was still true, and
            // the base reports itself stale rather than fresh, which is the honest answer when this
            // code cannot tell which happened.
            invalidate_base_record(self.bases.root(), records, name)?;
            self.with(|c| c.promote_base(from, name).map_err(control_io).map(|_| ()))
        })?;
        let mut info = self.info(name)?;
        // The core forks `from` into a staging name and renames it to `name`. Only the daemon
        // reports `parent` as `from` explicitly: that is the protocol's meaning, whatever the core
        // records.
        info.parent = Some(from.to_owned());
        Ok(info)
    }

    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        // One critical section: the record move, the tree move and the rollback share it, so a create
        // of `to` cannot decide it is free between a record that has moved and a tree that has not.
        self.bases.exclusive(|records| {
            // The record moves first, so the commit is under the new name before the snapshot does, and a
            // snapshot that cannot move puts the record back where it was. The other order would leave
            // the commit behind under a name that no longer exists.
            crate::base_meta::rename_locked(self.bases.root(), records, from, to)?;
            if let Err(e) =
                self.with(|c| c.rename_snapshot(from, to).map_err(control_io).map(|_| ()))
            {
                return Err(rollback_base(self.bases.root(), records, to, from, e));
            }
            Ok(())
        })
    }

    fn promote(&self, name: &str) -> io::Result<SnapshotInfo> {
        // One critical section with the create that could be clearing this name's record, so a promote
        // cannot publish a base and be deleted by a create that is deciding the name is free.
        self.bases.exclusive(|records| {
            self.with(|c| {
                Self::names(c)?
                    .into_iter()
                    .any(|e| e.name == name)
                    .then_some(())
                    .ok_or_else(|| missing(name))
            })?;
            if records.get(name).is_none() {
                crate::base_meta::write_locked(
                    self.bases.root(),
                    records,
                    name,
                    &crate::base_meta::Record::promoted_unknown(),
                )?;
            }
            Ok::<(), io::Error>(())
        })?;
        self.info(name)
    }

    fn set_base_meta(&self, name: &str, meta: &BaseMeta) -> io::Result<()> {
        if self.info(name).is_err() {
            return Err(missing(name));
        }
        self.bases.set(name, meta)
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
            snaps: PathSnapshots::new(store.clone())?,
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
    bases: crate::base_meta::BaseMetaStore,
}

impl PathSnapshots {
    fn new(store: PathBuf) -> io::Result<Self> {
        Ok(Self {
            // A store whose base records cannot be read is not a store this backend may serve:
            // it would report every base as unknown and let a refresh publish a second base
            // under a name that already has one.
            bases: crate::base_meta::BaseMetaStore::open(&store).map_err(|e| {
                io::Error::other(format!(
                    "cannot read the base records in {}: {e}",
                    store.display()
                ))
            })?,
            store,
        })
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.store.join(name)
    }

    fn exists(&self, name: &str) -> bool {
        self.dir(name).is_dir()
    }

    fn info(&self, name: &str, parent: Option<String>) -> SnapshotInfo {
        SnapshotInfo {
            name: name.to_owned(),
            parent,
            base: self.bases.get(name),
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
        // Same order, the same critical section and the same reason as the core backend: refusing a
        // duplicate must cost the caller nothing, and a name another thread takes in between must not
        // lose the record it has just published.
        self.bases.exclusive(|records| {
            if self.exists(name) {
                return Err(name_taken());
            }
            #[cfg(test)]
            test_hook::reach(AFTER_PRECHECK);
            // A new snapshot is never a base, so a record left under this name by a snapshot that has
            // since gone is cleared before this one exists, or it would report a commit that has nothing
            // to do with these contents.
            crate::base_meta::remove_locked(self.bases.root(), records, name)?;
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
            Ok(())
        })?;
        Ok(self.info(name, from.map(str::to_owned)))
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        if !self.exists(name) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("snapshot {name:?} does not exist"),
            ));
        }
        // One critical section, as on the core: the tree going and its record going are one step.
        self.bases.exclusive(|records| {
            cowfs_vfs_path::force_remove_dir_all(&self.dir(name));
            crate::base_meta::remove_locked(self.bases.root(), records, name)
        })
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
        // One critical section, as on the core: the tree going and its record going are one step, so
        // a reader never sees a base whose record describes the tree it replaced.
        self.bases.exclusive(|records| {
            // Cleared before the tree is touched, and not put back afterwards, for the same reason
            // as on the core: past this point the swap may have replaced the tree, and an error
            // from the renames cannot say whether it did.
            invalidate_base_record(self.bases.root(), records, name)?;
            // Build beside the old tree, then swap by rename, so a failed copy changes nothing.
            let staging = self.store.join(format!(".cowfs-swap-{name}"));
            let retired = self.store.join(format!(".cowfs-retired-{name}"));
            cowfs_vfs_path::force_remove_dir_all(&staging);
            cowfs_vfs_path::force_remove_dir_all(&retired);
            let result = (|| {
                copy_tree(&self.dir(from), &staging)?;
                std::fs::rename(self.dir(name), &retired)?;
                std::fs::rename(&staging, self.dir(name))?;
                Ok::<(), io::Error>(())
            })();
            match result {
                Ok(()) => cowfs_vfs_path::force_remove_dir_all(&retired),
                Err(e) => {
                    // The old tree is only at `retired` if it got that far, and the target has none.
                    // Put it back where it was. If that cannot be done the retired copy is left
                    // alone: it is the only remaining copy of the tree the target had, and deleting
                    // it would destroy work to tidy a directory.
                    cowfs_vfs_path::force_remove_dir_all(&staging);
                    if retired.is_dir() {
                        return Err(match std::fs::rename(&retired, self.dir(name)) {
                            Ok(()) => e,
                            Err(back) => io::Error::new(
                                e.kind(),
                                format!(
                                    "{e}; and the replaced tree could not be put back, and is left \
                                     at {retired:?} rather than deleted: {back}"
                                ),
                            ),
                        });
                    }
                    return Err(e);
                }
            }
            Ok::<(), io::Error>(())
        })?;
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
            return Err(name_taken());
        }
        // One critical section: the record move, the tree move and the rollback share it, as on the core.
        self.bases.exclusive(|records| {
            // The record moves first, so the commit is under the new name before the snapshot does, and a
            // snapshot that cannot move puts the record back where it was.
            crate::base_meta::rename_locked(self.bases.root(), records, from, to)?;
            std::fs::rename(self.dir(from), self.dir(to))
                .map_err(|e| rollback_base(self.bases.root(), records, to, from, e))?;
            Ok(())
        })
    }

    fn promote(&self, name: &str) -> io::Result<SnapshotInfo> {
        // One critical section with the create that could be clearing this name's record, as on the core.
        self.bases.exclusive(|records| {
            if !self.exists(name) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("snapshot {name:?} does not exist"),
                ));
            }
            if records.get(name).is_none() {
                crate::base_meta::write_locked(
                    self.bases.root(),
                    records,
                    name,
                    &crate::base_meta::Record::promoted_unknown(),
                )?;
            }
            Ok(())
        })?;
        Ok(self.info(name, None))
    }

    fn set_base_meta(&self, name: &str, meta: &BaseMeta) -> io::Result<()> {
        if !self.exists(name) {
            return Err(missing(name));
        }
        self.bases.set(name, meta)
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

    /// A path backend over a temp dir, closed by the end of the test whatever happens.
    fn path() -> (tempfile::TempDir, PathBackend) {
        let dir = tempfile::tempdir().unwrap();
        let backend = PathBackend::open(dir.path().join("store")).expect("a path backend");
        (dir, backend)
    }

    /// A `CoreSnapshots` over `dir`, holding a `Core` whose k-th durable commit fails.
    ///
    /// This is the production struct and the production `swap`. `CoreBackend` exposes no
    /// meta-options hook, so the fault cannot be injected through it; the two fields are private,
    /// which is why this test lives in this module rather than in `tests/`.
    fn core_snaps_with_failing_commit(dir: &Path, fail_at: usize) -> CoreSnapshots {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let seen = AtomicUsize::new(0);
        // `open_with_meta` wires the store sync hook into the options it hands `make_meta`, so the
        // hook is wrapped here rather than replaced, and the store's own durability is kept.
        let core = cowfs_core::Core::open_with_meta(
            dir,
            cowfs_core::Options {
                background: false,
                ..Default::default()
            },
            move |d, mut o| {
                let inner = o.before_sync.take();
                o.before_sync = Some(std::sync::Arc::new(move || {
                    let k = seen.fetch_add(1, SeqCst) + 1;
                    if let Some(h) = inner.clone() {
                        h()?;
                    }
                    if k == fail_at {
                        return Err(std::io::Error::other("injected meta sync failure"));
                    }
                    Ok(())
                }) as cowfs_meta::SyncHook);
                cowfs_meta::Meta::open(d.join("meta.redb"), o)
            },
        )
        .expect("a core with an injectable sync hook");
        CoreSnapshots {
            core: Arc::new(Mutex::new(Some(core))),
            bases: crate::base_meta::BaseMetaStore::open(dir).expect("the base records"),
        }
    }

    /// A promoted base built from `srcA` with its provenance, and a distinct `srcB`, through
    /// whichever backend is handed in, so a path store is never seeded with core structures.
    fn seeded_pair(b: &dyn Backend) {
        let s = b.snapshots();
        for (name, text) in [
            ("srcA", &b"AAAA-from-srcA"[..]),
            ("srcB", &b"BBBB-from-srcB"[..]),
        ] {
            s.create(name, None)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let v = b.snapshot(name).unwrap();
            let f = v.create(cowfs_vfs::ROOT_INO, b"only", 0o644).unwrap();
            v.write(f.ino, 0, text).unwrap();
            v.fsync(f.ino, false).unwrap();
        }
        s.create("base", Some("srcA")).unwrap();
        s.promote("base").unwrap();
        s.set_base_meta("base", &meta("/repoA", "refs/heads/main", "commit-AAA"))
            .unwrap();
        assert_eq!(
            commit_of(&s.create_meta("base").unwrap()),
            Some("commit-AAA")
        );
    }

    /// #124: a swap whose replace commit fails must not leave the old commit describing a tree
    /// that is not under the name.
    ///
    /// The fault is the second durable commit, which replaces the target by the staging snapshot
    /// (the first is the staging fork). The hook fails before that commit writes, so the core
    /// re-reads the file, sees the replace did not land and rolls back: the old tree stays and no
    /// intent file is left. (A commit that landed but returned `Err` keeps the intent and rolls
    /// forward; the core's `swap` unit tests cover that.) Either way the swap reports the failure
    /// and the record must read unknown, because the error alone cannot tell the two apart.
    ///
    /// On the unsafe code (the old record restored after an `Err`) this fails on the record still
    /// naming `commit-AAA`. The original #124 case, where the tree ends up NEW while the swap
    /// returns `Err`, needs a seam reachable from this crate and has no test here yet.
    #[test]
    fn a_core_swap_whose_replace_commit_fails_never_restores_the_old_commit() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().to_owned();
        {
            let seed = CoreBackend::open(&store, cowfs_core::Options::default()).unwrap();
            seeded_pair(&seed);
        }

        let s = core_snaps_with_failing_commit(&store, 2);
        let res = s.swap("base", "srcB");
        assert!(
            res.is_err(),
            "the second commit must fail, so the swap reports the failure"
        );
        drop(s);

        // Reopen through the production backend, so `swap::recover` runs as it would for a caller.
        let reopened = CoreBackend::open(&store, cowfs_core::Options::default()).unwrap();
        let info = reopened.snapshots().create_meta("base").unwrap();
        let v = reopened.snapshot("base").unwrap();
        let a = v.lookup(cowfs_vfs::ROOT_INO, b"only").unwrap();
        let content = String::from_utf8_lossy(&v.read(a.ino, 0, 4096).unwrap()).into_owned();
        assert_eq!(
            content, "AAAA-from-srcA",
            "the replace commit never wrote, so the swap rolled back and the old tree is still there"
        );
        assert!(
            !store.join("swap-base").exists(),
            "a rolled back swap leaves no intent file"
        );
        assert_eq!(
            commit_of(&info),
            None,
            "the record still names a commit that did not produce this tree: {info:?}"
        );
        assert_eq!(
            info.base.as_ref().and_then(|b| b.repo.clone()),
            None,
            "and the rest of the record went with it: {info:?}"
        );
        assert_eq!(
            info.base.as_ref().and_then(|b| b.git_ref.clone()),
            None,
            "and the ref: {info:?}"
        );
    }

    /// The refusal order is the one the core used, so the two backends answer a name-with-itself
    /// swap of a snapshot that does not exist the same way.
    #[test]
    fn a_swap_of_a_nonexistent_name_with_itself_is_not_found_on_both_backends() {
        let dir = tempfile::tempdir().unwrap();
        // A store per backend, seeded before either is opened: a core store holds a lock, so a
        // second open of the same store, or a store inside another one, is refused outright.
        let core_store = dir.path().join("corestore");
        let path_store = dir.path().join("pathstore");
        seeded_pair(&CoreBackend::open(&core_store, cowfs_core::Options::default()).unwrap());
        seeded_pair(&PathBackend::open(&path_store).unwrap());
        let core = CoreBackend::open(&core_store, cowfs_core::Options::default()).unwrap();
        let path = PathBackend::open(&path_store).unwrap();
        for (which, b) in [
            ("core", &core as &dyn Backend),
            ("path", &path as &dyn Backend),
        ] {
            let e = b.snapshots().swap("nosuch", "nosuch").unwrap_err();
            assert_eq!(
                e.kind(),
                std::io::ErrorKind::NotFound,
                "{which}: existence is checked before the same-name refusal, as `promote_base` did"
            );
        }
    }

    /// A refusal leaves the full old state, on both backends, for both refusal shapes.
    #[test]
    fn a_refused_swap_keeps_the_full_old_tree_and_record_on_both_backends() {
        let dir = tempfile::tempdir().unwrap();
        // A store per backend, seeded before either is opened, for the reason above.
        let core_store = dir.path().join("corestore");
        let path_store = dir.path().join("pathstore");
        seeded_pair(&CoreBackend::open(&core_store, cowfs_core::Options::default()).unwrap());
        seeded_pair(&PathBackend::open(&path_store).unwrap());
        let core = CoreBackend::open(&core_store, cowfs_core::Options::default()).unwrap();
        let path = PathBackend::open(&path_store).unwrap();
        for (which, b) in [
            ("core", &core as &dyn Backend),
            ("path", &path as &dyn Backend),
        ] {
            // `seeded_pair` already made `base` a promoted base with this provenance.
            let s = b.snapshots();
            let before = {
                let v = b.snapshot("base").unwrap();
                let a = v.lookup(cowfs_vfs::ROOT_INO, b"only").unwrap();
                String::from_utf8_lossy(&v.read(a.ino, 0, 4096).unwrap()).into_owned()
            };

            // Same name: refused, and the base is untouched.
            let e = s.swap("base", "base").unwrap_err();
            assert_eq!(
                e.kind(),
                std::io::ErrorKind::InvalidInput,
                "{which}: an existing name swapped with itself is refused"
            );
            // Missing source: refused.
            let e = s.swap("base", "nosuch").unwrap_err();
            assert_eq!(
                e.kind(),
                std::io::ErrorKind::NotFound,
                "{which}: missing source"
            );

            let kept = s.create_meta("base").unwrap();
            assert_eq!(
                commit_of(&kept),
                Some("commit-AAA"),
                "{which}: record kept: {kept:?}"
            );
            assert_eq!(
                kept.base.as_ref().and_then(|b| b.repo.clone()).as_deref(),
                Some("/repoA"),
                "{which}: repo kept"
            );
            assert_eq!(
                kept.base
                    .as_ref()
                    .and_then(|b| b.git_ref.clone())
                    .as_deref(),
                Some("refs/heads/main"),
                "{which}: ref kept"
            );
            let v = b.snapshot("base").unwrap();
            let a = v.lookup(cowfs_vfs::ROOT_INO, b"only").unwrap();
            assert_eq!(
                String::from_utf8_lossy(&v.read(a.ino, 0, 4096).unwrap()).into_owned(),
                before,
                "{which}: the tree is untouched by a refusal"
            );
        }
    }

    fn meta(repo: &str, git_ref: &str, commit: &str) -> BaseMeta {
        BaseMeta {
            repo: Some(repo.to_owned()),
            git_ref: Some(git_ref.to_owned()),
            commit: Some(commit.to_owned()),
        }
    }

    fn commit_of(info: &SnapshotInfo) -> Option<&str> {
        info.base.as_ref().and_then(|b| b.commit.as_deref())
    }

    /// The defect this is the regression for: the provenance was only ever in memory, so every
    /// caller after a reconnect or a restart was told the base had no commit and could not find it.
    #[test]
    fn a_core_base_keeps_its_provenance_across_a_reopen() {
        let (d, b) = core();
        let store = d.path().to_owned();
        let s = b.snapshots();
        s.create("warm", None).unwrap();
        s.promote("warm").unwrap();
        s.set_base_meta("warm", &meta("/r", "main", "c0ffee"))
            .unwrap();
        drop(b);

        let reopened = CoreBackend::open(&store, cowfs_core::Options::default())
            .expect("the same store opens again");
        let info = reopened.snapshots().create_meta("warm").unwrap();
        assert_eq!(commit_of(&info), Some("c0ffee"));
        assert_eq!(info.base.as_ref().unwrap().git_ref.as_deref(), Some("main"));
        assert_eq!(info.base.as_ref().unwrap().repo.as_deref(), Some("/r"));
    }

    #[test]
    fn a_path_base_keeps_its_provenance_across_a_reopen() {
        let (_d, b) = path();
        let store = b.store.clone();
        let s = b.snapshots();
        s.create("warm", None).unwrap();
        s.promote("warm").unwrap();
        s.set_base_meta("warm", &meta("/r", "main", "c0ffee"))
            .unwrap();
        drop(b);

        let reopened = PathBackend::open(&store).expect("the same store opens again");
        assert_eq!(
            commit_of(&reopened.snapshots().create_meta("warm").unwrap()),
            Some("c0ffee")
        );
    }

    /// A base whose provenance was never written is a base, and it is not fresh: the commit is
    /// unknown, not absent because nothing is built.
    #[test]
    fn a_promoted_base_with_no_provenance_reports_itself_unknown_not_fresh() {
        let (d, b) = core();
        let store = d.path().to_owned();
        b.snapshots().create("warm", None).unwrap();
        b.snapshots().promote("warm").unwrap();
        drop(b);

        let reopened = CoreBackend::open(&store, cowfs_core::Options::default()).unwrap();
        let info = reopened.snapshots().create_meta("warm").unwrap();
        assert!(info.base.is_some(), "still a base: {info:?}");
        assert_eq!(commit_of(&info), None, "and its commit is unknown");
    }

    #[test]
    fn provenance_follows_a_rename_and_is_forgotten_by_a_remove() {
        let (_d, b) = path();
        let s = b.snapshots();
        s.create("warm", None).unwrap();
        s.promote("warm").unwrap();
        s.set_base_meta("warm", &meta("/r", "main", "c0ffee"))
            .unwrap();

        s.rename("warm", "warmer").unwrap();
        assert_eq!(
            commit_of(&s.create_meta("warmer").unwrap()),
            Some("c0ffee"),
            "a rename must not lose where the base came from"
        );
        assert_eq!(
            s.create_meta("warm").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );

        s.remove("warmer").unwrap();
        assert!(!s.create_meta("warmer").is_ok());
        let store = b.store.clone();
        let reopened = PathBackend::open(&store).unwrap();
        assert!(
            crate::base_meta::BaseMetaStore::open(&reopened.store)
                .unwrap()
                .get("warmer")
                .is_none(),
            "and it stays forgotten after a reopen"
        );
    }

    /// A store whose records cannot be read is refused. Serving it would report every base as
    /// unknown and let a refresh publish a second base under a name that already has one.
    #[test]
    fn a_store_with_an_unreadable_base_record_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        let record = store.join(".cowfs-base-meta").join("warm");
        std::fs::create_dir_all(&record).unwrap();
        std::fs::write(record.join("base.json"), b"{ not json").unwrap();
        assert!(PathBackend::open(&store).is_err(), "opened anyway");
    }

    /// The setter fails closed: an error leaves the previous record in place rather than a base
    /// that claims a commit nobody can read back.
    #[test]
    fn a_provenance_write_that_fails_leaves_the_old_record_and_no_claim() {
        let (_d, b) = path();
        let s = b.snapshots();
        s.create("warm", None).unwrap();
        s.promote("warm").unwrap();
        // The record's own directory is now an ordinary file, so the write cannot land.
        let record = b.store.join(".cowfs-base-meta").join("warm");
        std::fs::remove_dir_all(&record).unwrap();
        std::fs::write(&record, b"not a directory").unwrap();

        let e = s
            .set_base_meta("warm", &meta("/r", "main", "c0ffee"))
            .unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists, "{e}");
        assert_eq!(
            commit_of(&s.create_meta("warm").unwrap()),
            None,
            "a failed write must not look like a published commit"
        );
    }

    /// P-1 at the namespace level: a removal whose record cannot be deleted is reported, and the
    /// record it could not delete is not inherited by whatever takes the name next.
    #[test]
    fn a_removal_whose_record_cannot_be_deleted_is_reported_and_loses_no_commit() {
        use std::os::unix::fs::PermissionsExt;

        let (_d, b) = path();
        let s = b.snapshots();
        s.create("warm", None).unwrap();
        std::fs::write(b.store.join("warm").join("main.rs"), b"fn main() {}\n").unwrap();
        s.promote("warm").unwrap();
        s.set_base_meta("warm", &meta("/r", "main", "abc")).unwrap();
        let record_dir = b.store.join(".cowfs-base-meta").join("warm");
        std::fs::set_permissions(&record_dir, std::fs::Permissions::from_mode(0o500)).unwrap();

        let e = s.remove("warm").unwrap_err();
        assert!(
            e.to_string().contains("cannot remove the base record"),
            "the failure names what did not happen: {e}"
        );
        assert!(
            record_dir.join("base.json").is_file(),
            "the record survived"
        );
        // The snapshot itself was asked to go, so its absence is the requested outcome, and both views
        // agree that there is no such snapshot.
        assert_eq!(
            s.create_meta("warm").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let store = b.store.clone();
        drop(b);
        let reopened = PathBackend::open(&store).unwrap();
        assert_eq!(
            reopened.snapshots().create_meta("warm").unwrap_err().kind(),
            io::ErrorKind::NotFound,
            "a live process and a restarted one must not disagree"
        );

        // And the commit that is still recorded cannot attach itself to a new snapshot of the same
        // name: that would report a base built from a commit this tree has nothing to do with.
        std::fs::set_permissions(&record_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let fresh = reopened.snapshots().create("warm", None).unwrap();
        assert_eq!(
            fresh.base, None,
            "a new snapshot inherited a commit it was not built from: {fresh:?}"
        );
    }

    /// P-2 at the namespace level: a rename whose record cannot be moved reports failure and changes
    /// nothing, rather than half-renaming the snapshot and losing the commit.
    #[test]
    fn a_rename_whose_record_cannot_be_moved_reports_failure_and_changes_nothing() {
        use std::os::unix::fs::PermissionsExt;

        let (_d, b) = path();
        let s = b.snapshots();
        s.create("warm", None).unwrap();
        std::fs::write(b.store.join("warm").join("main.rs"), b"fn main() {}\n").unwrap();
        s.promote("warm").unwrap();
        s.set_base_meta("warm", &meta("/r", "main", "abc")).unwrap();
        let before = std::fs::read(
            b.store
                .join(".cowfs-base-meta")
                .join("warm")
                .join("base.json"),
        )
        .unwrap();
        std::fs::set_permissions(
            b.store.join(".cowfs-base-meta"),
            std::fs::Permissions::from_mode(0o500),
        )
        .unwrap();

        let e = s.rename("warm", "warmer").unwrap_err();
        assert!(e.to_string().contains("warmer"), "{e}");

        // No data loss: the snapshot is where it was, with its contents.
        assert!(
            b.store.join("warm").join("main.rs").is_file(),
            "the tree survived"
        );
        assert_eq!(
            std::fs::read(b.store.join("warm").join("main.rs")).unwrap(),
            b"fn main() {}\n"
        );
        // No dangling destination: nothing was created under the new name, in the store or the records.
        assert!(!b.store.join("warmer").exists());
        assert!(!b.store.join(".cowfs-base-meta").join("warmer").exists());
        // And the source record is byte-identical, so the base keeps the commit it was published with.
        assert_eq!(
            std::fs::read(
                b.store
                    .join(".cowfs-base-meta")
                    .join("warm")
                    .join("base.json")
            )
            .unwrap(),
            before
        );
        assert_eq!(commit_of(&s.create_meta("warm").unwrap()), Some("abc"));
        std::fs::set_permissions(
            b.store.join(".cowfs-base-meta"),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }

    /// A snapshot recreated under a name whose record outlived it is not a base. Both backends, because
    /// a forged fresh base is the one failure this must never produce.
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Every core snapshot's name and Merkle root, which is the whole of its contents.
    fn core_roots(b: &CoreBackend) -> Vec<(String, String)> {
        with_core(&b.core, |c| {
            Ok(CoreSnapshots::names(c)?
                .into_iter()
                .map(|e| (e.name.clone(), hex(e.root.as_bytes())))
                .collect())
        })
        .expect("the core is open")
    }

    /// A refused duplicate must cost nothing. On the core backend the name check happened after the
    /// record was cleared, so `create warm` on a base named `warm` exited 1 and left the snapshot with
    /// no base at all, on disk and after a reopen: the record was gone. The path backend refused first
    /// and kept everything, and both now behave the same way.
    #[test]
    fn a_refused_duplicate_create_keeps_a_core_snapshot_and_its_whole_record() {
        let (d, b) = core();
        let store = d.path().to_owned();
        let s = b.snapshots();
        s.create("warm", None).unwrap();
        s.promote("warm").unwrap();
        s.set_base_meta("warm", &meta("/r", "main", "abc")).unwrap();
        let record_before = std::fs::read(crate::base_meta::record_path(&b.store, "warm")).unwrap();
        let roots_before = core_roots(&b);

        let e = s.create("warm", None).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{e}");

        assert_eq!(core_roots(&b), roots_before, "the snapshot's root changed");
        assert_eq!(
            std::fs::read(crate::base_meta::record_path(&b.store, "warm")).unwrap(),
            record_before,
            "the record is byte-identical"
        );
        assert_eq!(commit_of(&s.create_meta("warm").unwrap()), Some("abc"));
        drop(b);

        let reopened = CoreBackend::open(&store, cowfs_core::Options::default()).unwrap();
        assert_eq!(core_roots(&reopened), roots_before, "and after a reopen");
        assert_eq!(
            commit_of(&reopened.snapshots().create_meta("warm").unwrap()),
            Some("abc"),
            "a daemon that never saw this in memory still reports the base"
        );
        assert_eq!(
            std::fs::read(crate::base_meta::record_path(&reopened.store, "warm")).unwrap(),
            record_before
        );
    }

    /// What a core snapshot's tree holds, read through the backend's own Vfs. Two empty core snapshots
    /// have different Merkle roots, because each snapshot has its own inode numbers, so the root cannot
    /// serve as the identity here; the entries can.
    fn core_entries_of(b: &CoreBackend, name: &str) -> String {
        let vfs = b.snapshot(name).expect("the snapshot's vfs");
        let dir = vfs.readdir(cowfs_vfs::ROOT_INO, 0, 64).expect("readdir");
        let mut names: Vec<String> = dir
            .entries
            .iter()
            .map(|e| String::from_utf8_lossy(&e.name).into_owned())
            .collect();
        names.sort();
        names.join(",")
    }

    fn path_listing_of(b: &PathBackend, name: &str) -> String {
        let mut entries: Vec<String> = std::fs::read_dir(b.store.join(name))
            .map(|it| {
                it.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        entries.sort();
        entries.join(",")
    }

    /// Parks one create at the instant it has decided the name is free and has not touched the record,
    /// lets a second thread do a whole create-promote-publish for the same name, then lets the first
    /// one continue.
    ///
    /// The hook is a test-only point in the real `create`, so the code under test is the production
    /// order and not a reimplementation of it. It is the only synchronisation: two channels and a
    /// bounded wait, no sleeps, no iterations and no shared daemon. The bound is what makes a mistake
    /// here a test failure rather than a wedge, which is the failure the previous round's probe had.
    ///
    /// Returns whether the parked create was accepted and whether the second thread managed to publish
    /// while it was parked. Both are reported in the assertions below, neither is the assertion.
    fn interleave_create_against(
        snaps: &dyn Snapshots,
        name: &str,
        publish: impl Fn(&dyn Snapshots) + Send + Sync,
    ) -> (bool, bool) {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;

        const BOUND: Duration = Duration::from_secs(5);

        let (reached_tx, reached_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        // The hook is shared, and a receiver is `Send` but not `Sync`, so it is behind a mutex. Only the
        // parked thread ever takes it, so nothing contends for it.
        let go_rx = std::sync::Mutex::new(go_rx);
        let (done_tx, done_rx) = mpsc::channel::<()>();
        std::thread::scope(|s| {
            let a = s.spawn(|| {
                // Installed by, and scoped to, the thread being parked. The hook is a single global
                // point in `create`, so a hook that parked whichever thread arrived first would be
                // armed by any other test running in parallel: this one only parks its own thread, and
                // only that thread, and the guard removes it again when this closure returns.
                let me = std::thread::current().id();
                let hook: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |point| {
                    if point != AFTER_PRECHECK || std::thread::current().id() != me {
                        return;
                    }
                    let _ = reached_tx.send(());
                    let released = go_rx
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .recv_timeout(BOUND);
                    if released.is_err() {
                        panic!("the parked create was not released within {BOUND:?}");
                    }
                });
                let _guard = test_hook::Guard::install(hook);
                snaps.create(name, None).is_ok()
            });
            reached_rx
                .recv_timeout(BOUND)
                .expect("the first create reached the point after its name check");

            let b = s.spawn(move || {
                publish(snaps);
                let _ = done_tx.send(());
            });

            // Whether B got in while A was parked is the observation, not the claim. Against the code
            // this replaces, B completes here in microseconds. With the critical section it cannot start
            // at all, so this bounded wait is the whole cost of the fix and nothing waits twice.
            let b_ran_first = done_rx.recv_timeout(Duration::from_secs(2)).is_ok();

            let _ = go_tx.send(());
            let a_accepted = a.join().expect("the parked create thread");
            b.join().expect("the create-and-publish thread");
            (a_accepted, b_ran_first)
        })
    }

    /// What must be true whoever won the name: one snapshot called `warm`, still a base, still carrying
    /// the commit the other thread published, and still an untouched empty tree.
    fn assert_warm_is_the_published_base(snaps: &dyn Snapshots, identity: &dyn Fn(&str) -> String) {
        let names = snaps.list().expect("list");
        assert_eq!(
            names.iter().filter(|n| n.as_str() == "warm").count(),
            1,
            "exactly one snapshot named warm, got {names:?}"
        );
        let info = snaps.create_meta("warm").expect("warm exists");
        assert_eq!(
            info.base.as_ref().and_then(|b| b.commit.as_deref()),
            Some("abc"),
            "the base another thread published is gone: {info:?}"
        );
        assert_eq!(
            identity("warm"),
            identity("control"),
            "the snapshot's contents are not those of an untouched empty tree"
        );
    }

    /// The interleaving hook is one global point inside `create`, so the two arms take turns rather than
    /// run in parallel. The lock is held for one arm's whole duration, and each arm is bounded, so this
    /// cannot wedge: the holder always finishes or panics.
    static ARMS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Parks a create of `warm` at the point after its name check, lets another thread publish a base
    /// for that same name, then lets the create continue.
    fn interleave_promote(snaps: &dyn Snapshots) -> (bool, bool) {
        // A snapshot created the same way with no parent, so "the contents are untouched" has something
        // to be compared against that is not the value under test.
        snaps.create("control", None).expect("the control snapshot");
        let published = meta("/r", "main", "abc");
        interleave_create_against(snaps, "warm", move |s| {
            // The other thread's whole intent: take the name, make it a base, publish where it came from.
            // Its create may legitimately be refused, which is the case under test.
            let _ = s.create("warm", None);
            let _ = s.promote("warm");
            s.set_base_meta("warm", &published)
                .expect("publish the base's provenance");
        })
    }

    /// Parks a create of `target` while another thread renames an existing base onto that same name. The
    /// record moves before the tree does, so a create that clears the record in between takes a base's
    /// provenance with it, and the rename's own rollback has nothing left to put back. This is the case a
    /// lock inside `create` alone would not cover.
    fn interleave_rename_onto(snaps: &dyn Snapshots) -> (bool, bool) {
        snaps.create("warm", None).expect("the base to be renamed");
        snaps.promote("warm").expect("make it a base");
        snaps
            .set_base_meta("warm", &meta("/r", "main", "abc"))
            .expect("give it provenance");
        interleave_create_against(snaps, "target", move |s| {
            let _ = s.rename("warm", "target");
        })
    }

    /// Exactly one of `warm` and `target` carries the base's commit, and the name carrying it is a
    /// snapshot that exists. Either the rename went first and `target` is the base, or the create went
    /// first and the rename was refused so `warm` is. What must never happen is the record going
    /// nowhere.
    fn assert_the_base_survives_under_one_name(snaps: &dyn Snapshots) {
        let mut carriers = Vec::new();
        for name in ["warm", "target"] {
            if let Ok(info) = snaps.create_meta(name) {
                if info.base.as_ref().and_then(|b| b.commit.as_deref()) == Some("abc") {
                    carriers.push(name.to_owned());
                }
            }
        }
        assert_eq!(
            carriers.len(),
            1,
            "the base's record is not on exactly one of warm/target: {carriers:?}"
        );
        let names = snaps.list().expect("list");
        assert!(
            names.contains(&carriers[0]),
            "the record is under {} but that snapshot does not exist: {names:?}",
            carriers[0]
        );
    }

    /// Issue 115, on the core backend. A create must not cost another thread the base it published for
    /// the same name while that create was deciding the name was free.
    #[test]
    fn a_concurrent_published_base_survives_another_threads_create_on_core() {
        let _turn = ARMS.lock().unwrap_or_else(PoisonError::into_inner);
        let (d, b) = core();
        let (a_accepted, b_ran_first) = interleave_promote(b.snapshots());
        eprintln!(
            "core: parked create accepted={a_accepted}, publisher ran while parked={b_ran_first}"
        );
        assert_warm_is_the_published_base(b.snapshots(), &|n| core_entries_of(&b, n));
        drop(b);

        let reopened = CoreBackend::open(d.path(), cowfs_core::Options::default()).unwrap();
        assert_warm_is_the_published_base(reopened.snapshots(), &|n| core_entries_of(&reopened, n));
    }

    /// Issue 115, on the path backend.
    #[test]
    fn a_concurrent_published_base_survives_another_threads_create_on_path() {
        let _turn = ARMS.lock().unwrap_or_else(PoisonError::into_inner);
        let (_d, b) = path();
        let store = b.store.clone();
        let (a_accepted, b_ran_first) = interleave_promote(b.snapshots());
        eprintln!(
            "path: parked create accepted={a_accepted}, publisher ran while parked={b_ran_first}"
        );
        assert_warm_is_the_published_base(b.snapshots(), &|n| path_listing_of(&b, n));
        drop(b);

        let reopened = PathBackend::open(&store).unwrap();
        assert_warm_is_the_published_base(reopened.snapshots(), &|n| path_listing_of(&reopened, n));
    }

    /// Issue 115 with a rename onto the name instead of a create-and-promote, on the core backend.
    #[test]
    fn a_concurrent_rename_onto_a_name_keeps_the_base_on_core() {
        let _turn = ARMS.lock().unwrap_or_else(PoisonError::into_inner);
        let (d, b) = core();
        let (a_accepted, b_ran_first) = interleave_rename_onto(b.snapshots());
        eprintln!(
            "core: parked create accepted={a_accepted}, renamer ran while parked={b_ran_first}"
        );
        assert_the_base_survives_under_one_name(b.snapshots());
        drop(b);

        let reopened = CoreBackend::open(d.path(), cowfs_core::Options::default()).unwrap();
        assert_the_base_survives_under_one_name(reopened.snapshots());
    }

    /// And on the path backend.
    #[test]
    fn a_concurrent_rename_onto_a_name_keeps_the_base_on_path() {
        let _turn = ARMS.lock().unwrap_or_else(PoisonError::into_inner);
        let (_d, b) = path();
        let store = b.store.clone();
        let (a_accepted, b_ran_first) = interleave_rename_onto(b.snapshots());
        eprintln!(
            "path: parked create accepted={a_accepted}, renamer ran while parked={b_ran_first}"
        );
        assert_the_base_survives_under_one_name(b.snapshots());
        drop(b);

        let reopened = PathBackend::open(&store).unwrap();
        assert_the_base_survives_under_one_name(reopened.snapshots());
    }

    /// A create that cannot clear a stale record creates no snapshot. Both backends, because the record
    /// and the snapshot have to agree about what exists, and a half-created snapshot whose name still
    /// carries a record is worse than no snapshot.
    ///
    /// The record this leaves behind is not asserted on: with the metadata root read-only the unlink of
    /// the record succeeds and only the removal of its directory fails, so the store really does lose the
    /// record. That reconciliation is what the two removal tests in `base_meta` are for. What is
    /// asserted here is that the namespace did not advance, and that the store's own view of the record
    /// is stable across the failure rather than moving under the caller's feet.
    #[test]
    fn a_create_that_cannot_clear_a_stale_record_creates_no_snapshot() {
        use std::os::unix::fs::PermissionsExt;

        let arm =
            |backend: &str, snapshots: &dyn Snapshots, store: &Path, snapshot_exists: bool| {
                crate::base_meta::BaseMetaStore::open(store)
                    .unwrap()
                    .set("warm", &meta("/r", "main", "abc"))
                    .unwrap();
                let root = store.join(crate::base_meta::DIR);
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();
                let refused = snapshots.create("warm", None).unwrap_err();
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
                assert!(
                    refused
                        .to_string()
                        .contains("cannot remove the base record"),
                    "{backend} backend: {refused}"
                );
                assert_eq!(
                    snapshots.create_meta("warm").unwrap_err().kind(),
                    io::ErrorKind::NotFound,
                    "{backend} backend: a snapshot was created anyway"
                );
                assert!(
                    !snapshot_exists || !store.join("warm").exists(),
                    "{backend} backend: a snapshot directory was created anyway"
                );
                let view = crate::base_meta::BaseMetaStore::open(store)
                    .unwrap()
                    .get("warm");
                assert_eq!(
                    view,
                    crate::base_meta::BaseMetaStore::open(store)
                        .unwrap()
                        .get("warm"),
                    "{backend} backend: the store's view of the record is not stable"
                );
            };

        let (d, c) = core();
        arm("core", c.snapshots(), &c.store.clone(), false);
        drop(c);
        let reopened = CoreBackend::open(d.path(), cowfs_core::Options::default()).unwrap();
        assert_eq!(
            reopened.snapshots().create_meta("warm").unwrap_err().kind(),
            io::ErrorKind::NotFound,
            "core backend: the failed create is still there after a reopen"
        );

        let (_d, b) = path();
        let store = b.store.clone();
        arm("path", b.snapshots(), &store, true);
        drop(b);
        let reopened = PathBackend::open(&store).unwrap();
        assert_eq!(
            reopened.snapshots().create_meta("warm").unwrap_err().kind(),
            io::ErrorKind::NotFound,
            "path backend: the failed create is still there after a reopen"
        );
    }

    /// The same refusal on the path backend, which is the control this has to match.
    #[test]
    fn a_refused_duplicate_create_keeps_a_path_snapshot_and_its_whole_record() {
        let (_d, b) = path();
        let s = b.snapshots();
        s.create("warm", None).unwrap();
        std::fs::write(b.store.join("warm").join("main.rs"), b"fn main() {}\n").unwrap();
        s.promote("warm").unwrap();
        s.set_base_meta("warm", &meta("/r", "main", "abc")).unwrap();
        let record_before = std::fs::read(crate::base_meta::record_path(&b.store, "warm")).unwrap();
        let tree_before = std::fs::read(b.store.join("warm").join("main.rs")).unwrap();

        let e = s.create("warm", None).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists, "{e}");

        assert_eq!(
            std::fs::read(b.store.join("warm").join("main.rs")).unwrap(),
            tree_before,
            "the snapshot's bytes changed"
        );
        assert_eq!(
            std::fs::read(crate::base_meta::record_path(&b.store, "warm")).unwrap(),
            record_before,
            "the record is byte-identical"
        );
        assert_eq!(commit_of(&s.create_meta("warm").unwrap()), Some("abc"));
        let store = b.store.clone();
        drop(b);

        let reopened = PathBackend::open(&store).unwrap();
        assert_eq!(
            commit_of(&reopened.snapshots().create_meta("warm").unwrap()),
            Some("abc"),
            "a daemon that never saw this in memory still reports the base"
        );
        assert_eq!(
            std::fs::read(crate::base_meta::record_path(&reopened.store, "warm")).unwrap(),
            record_before
        );
    }

    /// A snapshot created under a name whose record outlived it is not a base. A record can outlive its
    /// snapshot when an operation is interrupted, and if `create` adopted it, the new tree would
    /// report itself a base built from a commit it has nothing to do with.
    #[test]
    fn a_recreated_snapshot_is_never_a_base_even_when_a_record_outlived_it() {
        // Path backend.
        let (_d, b) = path();
        // An orphan written straight to the store: a base record with no snapshot behind it.
        crate::base_meta::BaseMetaStore::open(&b.store)
            .unwrap()
            .set("warm", &meta("/r", "main", "abc"))
            .unwrap();
        let recreated = b.snapshots().create("warm", None).unwrap();
        assert_eq!(recreated.base, None, "path backend: {recreated:?}");

        // Core backend, the same orphan over the same kind of store.
        let (d, c) = core();
        crate::base_meta::BaseMetaStore::open(&c.store)
            .unwrap()
            .set("warm", &meta("/r", "main", "abc"))
            .unwrap();
        let store = d.path().to_owned();
        drop(c);
        let reopened = CoreBackend::open(&store, cowfs_core::Options::default()).unwrap();
        let recreated = reopened.snapshots().create("warm", None).unwrap();
        assert_eq!(recreated.base, None, "core backend: {recreated:?}");
        // And the record is gone from the store too, so a reopen agrees.
        assert_eq!(
            crate::base_meta::BaseMetaStore::open(&reopened.store)
                .unwrap()
                .get("warm"),
            None
        );
    }

    #[test]
    fn provenance_cannot_be_recorded_for_a_snapshot_that_does_not_exist() {
        let (_d, b) = path();
        assert_eq!(
            b.snapshots()
                .set_base_meta("nosuch", &meta("/r", "main", "c"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        let (_d2, c) = core();
        c.snapshots().create("warm", None).unwrap();
        assert_eq!(
            c.snapshots()
                .set_base_meta("nosuch", &meta("/r", "main", "c"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
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
    fn set_base_meta(&self, _name: &str, _meta: &BaseMeta) -> io::Result<()> {
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
