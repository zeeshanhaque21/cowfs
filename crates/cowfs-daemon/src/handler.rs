//! The `ControlHandler` the control server dispatches to: one snapshot namespace, one mount,
//! one export registry, behind the protocol in `docs/v1-control-api.md`.
//!
//! `remove` and `swap` hold the framework's per-snapshot lock across the holder check and the
//! change, which is what `cowfs_ctl::handler_conformance` verifies. Everything a control
//! operation changes behind the mount is announced to the kernel afterwards, so the default
//! shared cache regime stays bounded instead of stale.

use crate::backend::{Backend, Snapshots};
use crate::exports::Exports;
use crate::holders;
use crate::mounts::Mounted;
use cowfs_ctl::{
    BaseRefreshParams, BaseRefreshReport, ControlHandler, CtlError, CtlResult, ErrorCode,
    FsckReport, GcParams, GcReport, HolderGuard, ImportParams, ImportReport, MountInfo,
    MountSnapshot, OpContext, ProcessInfo, ProgressEvent, SnapshotCreate, SnapshotInfo, Status,
    Unit, UnmountSnapshot,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// `mount` joined with `name`, or `None` when the name could leave the mount.
///
/// Pure path arithmetic: no syscall, no `realpath`, so it cannot be the thing that wedges on a stale
/// mount. What a symlink does after the join is settled later, by the scan, which resolves the
/// prefix under its own deadline and refuses a result that is not inside the mount.
pub(crate) fn lexical_child(mount: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    if name.is_empty() || name.starts_with('/') {
        return None;
    }
    // A leading `.` would name the mount itself, which is a scan of the whole mount: refused, while
    // a `.` further along, such as `base/./slot`, stays legal.
    let mut components = name.split('/');
    let first = components.next().unwrap_or_default();
    if first.is_empty() || first == "." || first == ".." {
        return None;
    }
    if components.any(|c| c.is_empty() || c == "..") {
        return None;
    }
    Some(mount.join(name))
}

fn io(e: std::io::Error, what: &str) -> CtlError {
    let code = match e.kind() {
        std::io::ErrorKind::NotFound => ErrorCode::NotFound,
        std::io::ErrorKind::AlreadyExists => ErrorCode::AlreadyExists,
        std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        std::io::ErrorKind::Unsupported => ErrorCode::Unsupported,
        std::io::ErrorKind::InvalidInput => ErrorCode::InvalidParams,
        std::io::ErrorKind::WouldBlock => ErrorCode::Busy,
        _ => ErrorCode::IoError,
    };
    CtlError::new(code, format!("{what}: {e}"))
}

/// The daemon's control-plane state. One of these is the whole backend.
#[derive(Debug)]
pub struct Handler {
    backend: Arc<dyn Backend>,
    exports: Exports,
    mount: std::sync::Mutex<Option<Arc<Mounted>>>,
    mount_path: PathBuf,
    store_path: PathBuf,
    started: Instant,
}

type Lock<T> = std::sync::Mutex<T>;

impl Handler {
    /// Builds the handler around an open backend and a live mount. It cannot fail, so it hands
    /// back the shared handler the server takes.
    pub fn new(backend: Arc<dyn Backend>, mount: Arc<Mounted>, exports: Exports) -> Arc<Handler> {
        Arc::new(Handler {
            mount_path: mount.path().to_owned(),
            store_path: backend.store_path().to_owned(),
            mount: Lock::new(Some(mount)),
            backend,
            exports,
            started: Instant::now(),
        })
    }

    /// The export registry, for `mount_snapshot` and `unmount_snapshot`.
    pub fn exports(&self) -> &Exports {
        &self.exports
    }

    /// Where the default mount is.
    pub fn mount_path(&self) -> &std::path::Path {
        &self.mount_path
    }

    /// The backend, for a client that wants the tree itself.
    pub fn backend(&self) -> &Arc<dyn Backend> {
        &self.backend
    }

    fn snaps(&self) -> &dyn Snapshots {
        self.backend.snapshots()
    }

    /// A change the control plane made behind the mount, announced to the kernel. In the
    /// default shared FUSE mode nothing is cached for longer than a second anyway; this is
    /// what makes the bound tight rather than eventual.
    fn changed(&self) {
        if let Some(m) = self
            .mount
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            m.invalidate_all()
        }
    }

    /// Unmounts every export and then the default mount, in that order and synchronously.
    /// Returns what could not be unmounted, so a caller can report a mount that outlived the
    /// daemon instead of leaving it silent. Idempotent, so a `shutdown` request and a signal
    /// racing each other cannot double-unmount.
    pub fn shutdown_mount_tree(&self) -> Vec<String> {
        let mut problems = self.exports.unmount_all();
        let taken = self
            .mount
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(mount) = taken {
            match Arc::try_unwrap(mount) {
                Ok(mount) => {
                    if let Err(e) = mount.unmount() {
                        problems.push(e);
                    }
                }
                Err(_) => problems
                    .push("the default mount is still shared with a live request".to_owned()),
            }
        }
        problems
    }

    fn live(&self) -> bool {
        self.mount
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|m| m.is_alive())
    }

    /// Makes the backend durable and releases what it holds, after the mount is gone. This is
    /// what releases the block store's lock, so a daemon that exits before this leaves the store
    /// refusing to open until the process is gone.
    pub fn close_backend(&self) -> Result<(), String> {
        self.backend.close().map_err(|e| e.to_string())
    }

    /// What the backend's block store holds, when it has one.
    fn usage(&self) -> Option<crate::backend::Usage> {
        self.backend.usage().ok().flatten()
    }

    /// `import` copies a directory into the store, which only means anything for
    /// a backend whose snapshots are directories. It guards only the passthrough fallback of `import`:
    /// the core ingests through its own writer (`Backend::ingest`), and `base_refresh` publishes
    /// tree-natively through `import::replace_tree` (issue 123), so neither comes here for the core.
    fn can_ingest(&self) -> CtlResult<()> {
        if self.backend.ingests_directories() {
            return Ok(());
        }
        Err(CtlError::new(
            ErrorCode::Unsupported,
            "this backend stores snapshots as trees, not as directories: copy the source into the \
             mount path instead",
        ))
    }

    /// The report for a backend that ingested and verified the tree itself. Both root hashes are
    /// recomputed here, the source on disk and the snapshot through its `Vfs`, so the caller gets
    /// the same evidence the passthrough backend reports and can check it independently.
    fn verified_report(
        &self,
        params: &ImportParams,
        ingested: cowfs_core::Ingested,
        ctx: &OpContext<'_>,
    ) -> CtlResult<ImportReport> {
        // Hashing both trees reads every byte again, which takes as long as the ingest did.
        let hashed = ingested.files;
        let tick = |phase: &'static str| {
            let mut throttle = Throttle::new();
            move || {
                !throttle.due()
                    || ctx
                        .progress(ProgressEvent {
                            phase: phase.into(),
                            done: 0,
                            total: Some(hashed),
                            unit: Unit::Items,
                            message: None,
                        })
                        .is_ok()
            }
        };
        let stopped = |e: std::io::Error| {
            ctx.check().err().unwrap_or_else(|| {
                CtlError::new(ErrorCode::IoError, format!("cannot hash the tree: {e}"))
            })
        };
        let source = PathBuf::from(&params.path);
        let source_hash =
            cowfs_ctl::hash_tree_with(&source, &mut tick("hash source")).map_err(stopped)?;
        let view = self
            .backend
            .snapshot(&params.name)
            .map_err(|e| io(e, "cannot read the imported snapshot"))?;
        let got = cowfs_ctl::hash_view_with(
            view.as_ref(),
            cowfs_vfs::ROOT_INO,
            &mut tick("hash snapshot"),
        )
        .map_err(stopped)?;
        let verified = source_hash.root == got.root;
        Ok(ImportReport {
            name: params.name.clone(),
            files: ingested.files,
            bytes: ingested.bytes,
            verified,
            hash_algorithm: cowfs_ctl::HASH_ALGORITHM.to_owned(),
            source_root_hash: source_hash.root,
            imported_root_hash: got.root,
            mismatches: if verified {
                Vec::new()
            } else {
                vec![cowfs_ctl::ImportMismatch {
                    path: ".".into(),
                    reason: "the source changed while it was ingested".into(),
                }]
            },
            mismatches_truncated: false,
            stored_bytes: Some(ingested.stored_bytes),
        })
    }
}

impl ControlHandler for Handler {
    fn status(&self) -> CtlResult<Status> {
        let names = self
            .snaps()
            .list()
            .map_err(|e| io(e, "cannot list snapshots"))?;
        let (block_count, logical, stored) = match self.usage() {
            Some(u) => (u.blocks, u.logical_bytes, u.stored_bytes),
            // A passthrough backend has no block store, so these count the tree instead.
            None => {
                let (logical, files) = walk_sizes(&self.store_path);
                (files, logical, logical)
            }
        };
        Ok(Status {
            store_path: self.store_path.display().to_string(),
            mount_path: self.mount_path.display().to_string(),
            snapshot_count: names.len() as u64,
            block_count,
            logical_bytes: logical,
            stored_bytes: stored,
            uptime_secs: self.started.elapsed().as_secs(),
        })
    }

    fn snapshot_list(&self) -> CtlResult<Vec<SnapshotInfo>> {
        let mut names = self
            .snaps()
            .list()
            .map_err(|e| io(e, "cannot list snapshots"))?;
        names.sort();
        names
            .into_iter()
            .map(|n| {
                self.snaps()
                    .create_meta(&n)
                    .map_err(|e| io(e, &format!("snapshot {n:?}")))
            })
            .collect()
    }

    fn snapshot_create(&self, params: SnapshotCreate) -> CtlResult<SnapshotInfo> {
        let info = self
            .snaps()
            .create(&params.name, params.from.as_deref())
            .map_err(|e| io(e, &format!("cannot create snapshot {:?}", params.name)))?;
        self.changed();
        Ok(info)
    }

    fn remove(&self, name: &str, guard: &HolderGuard<'_>) -> CtlResult<()> {
        // Held across the check and the change: whoever adds a holder takes this same lock, so
        // a holder cannot appear between the two.
        let _serialised = guard.lock();
        guard.check_holders()?;
        self.snaps()
            .remove(name)
            .map_err(|e| io(e, &format!("cannot remove {name:?}")))?;
        self.changed();
        Ok(())
    }

    fn swap(&self, name: &str, from: &str, guard: &HolderGuard<'_>) -> CtlResult<SnapshotInfo> {
        let _serialised = guard.lock();
        guard.check_holders()?;
        let info = self
            .snaps()
            .swap(name, from)
            .map_err(|e| io(e, &format!("cannot reset {name:?} from {from:?}")))?;
        self.changed();
        Ok(info)
    }

    fn holders(&self, snapshot: &str) -> CtlResult<Vec<ProcessInfo>> {
        // The name is resolved against the mount, so it may name a directory inside it and not only
        // a snapshot: a mode (a) treehouse slot is a worktree directory inside a snapshot, and issue
        // #20 needs a scan of exactly that directory. Which directories this daemon may be asked
        // about is a server-side rule, and its first half costs no syscall at all: a name that is
        // absolute, or climbs out with `..`, is refused here before anything touches the mount.
        let Some(dir) = lexical_child(&self.mount_path, snapshot) else {
            return Err(CtlError::new(
                ErrorCode::InvalidParams,
                format!("{snapshot:?} is not a directory inside the mount"),
            ));
        };
        if !dir.exists() {
            return Err(CtlError::not_found(format!(
                "snapshot {snapshot:?} does not exist"
            )));
        }
        // The second half, once the prefix has been resolved, runs inside the scan's own deadline:
        // resolving a path is the syscall a stale mount can wedge, and this daemon has been bitten
        // by exactly that twice, in 90c9a8f and 265fc3f.
        //
        // A platform that cannot answer is not a clear slot: reporting no holders because lsof is
        // missing, wedged, or handed back a partial answer is how a reset lands under a live writer.
        match holders::scan_mounted(&self.mount_path, &dir) {
            holders::Scan::Holders(found) => Ok(found),
            holders::Scan::Unavailable(why) => Err(CtlError::new(
                ErrorCode::Unsupported,
                format!("cannot say who holds {snapshot:?}: {why}"),
            )),
        }
    }

    fn snapshot_rename(&self, from: &str, to: &str) -> CtlResult<SnapshotInfo> {
        let before = self.snaps().create_meta(from).ok();
        self.snaps()
            .rename(from, to)
            .map_err(|e| io(e, &format!("cannot rename {from:?} to {to:?}")))?;
        self.changed();
        let after = self
            .snaps()
            .create_meta(to)
            .map_err(|e| io(e, &format!("snapshot {to:?}")))?;
        Ok(SnapshotInfo {
            parent: before.and_then(|b| b.parent),
            ..after
        })
    }

    fn snapshot_promote(&self, name: &str) -> CtlResult<SnapshotInfo> {
        let info = self
            .snaps()
            .promote(name)
            .map_err(|e| io(e, &format!("cannot promote {name:?}")))?;
        Ok(info)
    }

    fn gc(&self, params: GcParams, ctx: &OpContext<'_>) -> CtlResult<GcReport> {
        let mut progress = |p: &cowfs_gc::Progress| {
            ctx.progress(ProgressEvent {
                phase: if p.sweeping { "sweep" } else { "mark" }.into(),
                done: p.packs_done,
                total: Some(p.packs_total),
                unit: Unit::Items,
                message: Some(format!(
                    "{} bytes copied, {} bytes freed",
                    p.bytes_copied, p.freed_bytes
                )),
            })
            .is_ok()
        };
        let out = self
            .backend
            .collect_garbage(params.dry_run, &mut progress, &|| ctx.is_cancelled())?
            .ok_or_else(|| {
                CtlError::new(
                    ErrorCode::Unsupported,
                    "garbage collection needs the block store, which this backend does not have",
                )
            })?;
        ctx.check()?;
        let r = &out.report;
        if r.roots_error.is_some() && (params.dry_run || r.freed_bytes == 0) {
            return Err(CtlError::new(
                ErrorCode::Busy,
                "the writers did not let the collector hold the reference side still, so nothing \
                 was freed: try again when the mount is quieter",
            ));
        }
        // A pack whose unlink could not be made durable is counted in the figures, because the file is
        // really gone, but the cycle is not a clean success: the removal is unconfirmed. The message
        // carries the real gross, rewrite and net so the caller sees the actual numbers rather than
        // reading an unexplained success.
        if r.unlink_durability_errors > 0 {
            return Err(CtlError::new(
                ErrorCode::IoError,
                format!(
                    "garbage collection could not confirm {} unlink(s) as durable; {} removed \
                     (gross), {} rewritten, net {} reclaimed; first failure: {}",
                    r.unlink_durability_errors,
                    r.gross_removed_bytes,
                    r.rewrite_bytes,
                    r.net_reclaimed_bytes,
                    r.errors.first().map_or("none", String::as_str),
                ),
            ));
        }
        if let (Some(first), 0) = (r.errors.first(), r.freed_bytes) {
            // The cycle failed before it unlinked any pack, but it may still have written bytes
            // into a new pack. The message carries the real gross, rewrite and net so the caller
            // sees the actual cost instead of a bare "freed nothing" that hides a rewrite.
            return Err(CtlError::new(
                ErrorCode::IoError,
                format!(
                    "garbage collection freed nothing: {first}; {} removed (gross), {} rewritten, \
                     net {} reclaimed",
                    r.gross_removed_bytes, r.rewrite_bytes, r.net_reclaimed_bytes
                ),
            ));
        }
        Ok(GcReport {
            dry_run: params.dry_run,
            candidate_blocks: r.store_blocks.saturating_sub(r.live_blocks as u64),
            candidate_bytes: r.candidate_dead_bytes,
            freed_blocks: out.blocks_before.saturating_sub(out.blocks_after),
            freed_bytes: r.freed_bytes,
            gross_removed_bytes: r.gross_removed_bytes,
            rewrite_bytes: Some(r.rewrite_bytes),
            net_reclaimed_bytes: Some(r.net_reclaimed_bytes),
        })
    }

    fn fsck(&self, ctx: &OpContext<'_>) -> CtlResult<FsckReport> {
        // Re-hashing a large store takes minutes, so it reports as it goes: a client gives up on a
        // daemon that stays silent for its timeout.
        let mut throttle = Throttle::new();
        let mut progress = |done: u64, total: u64| {
            !throttle.due()
                || ctx
                    .progress(ProgressEvent {
                        phase: "verify".into(),
                        done,
                        total: Some(total),
                        unit: Unit::Bytes,
                        message: None,
                    })
                    .is_ok()
        };
        let report = self
            .backend
            .fsck(&mut progress)
            .map_err(|e| {
                // A cancel stops the check by failing it, and says why through the context.
                ctx.check()
                    .err()
                    .unwrap_or_else(|| CtlError::new(ErrorCode::IoError, format!("fsck: {e}")))
            })?
            .ok_or_else(|| {
                CtlError::new(
                    ErrorCode::Unsupported,
                    "fsck needs the block store, which this backend does not have",
                )
            })?;
        ctx.progress(cowfs_ctl::ProgressEvent {
            phase: "verify".into(),
            done: report.bytes_scanned,
            total: Some(report.bytes_scanned),
            unit: cowfs_ctl::Unit::Bytes,
            message: Some("re-hashed every block".into()),
        })?;
        Ok(FsckReport {
            ok: report.damage.is_empty(),
            blocks_checked: report.blocks_verified,
            bytes_checked: report.bytes_scanned,
            snapshots_checked: self.snaps().list().map(|n| n.len() as u64).unwrap_or(0),
            problems: report.damage.iter().map(damage).collect(),
        })
    }

    /// Ingests a directory as a new snapshot. A backend with a writer of its own (the core) does
    /// the ingest through it, staging, verifying and switching atomically; the passthrough backend
    /// copies the tree into the store and reports `stored_bytes: None`, because it has no store to
    /// compress into.
    fn import(&self, params: ImportParams, ctx: &OpContext<'_>) -> CtlResult<ImportReport> {
        let source = std::path::PathBuf::from(&params.path);
        // Called for every entry and every chunk, so most calls are skipped. A total of 0 is the
        // walk that counts the source, before there is one to show.
        let mut throttle = Throttle::new();
        let mut progress = |done: u64, total: u64| {
            !throttle.due()
                || ctx
                    .progress(ProgressEvent {
                        phase: "ingest".into(),
                        done,
                        total: (total > 0).then_some(total),
                        unit: Unit::Bytes,
                        message: None,
                    })
                    .is_ok()
        };
        if let Some(ingested) = self.backend.ingest(&source, &params.name, &mut progress)? {
            self.changed();
            return self.verified_report(&params, ingested, ctx);
        }
        self.can_ingest()?;
        crate::import::run(self.backend.as_ref(), self.snaps(), &params, ctx)
    }

    fn base_refresh(
        &self,
        params: BaseRefreshParams,
        ctx: &OpContext<'_>,
    ) -> CtlResult<BaseRefreshReport> {
        // No `can_ingest` here: a tree-native backend publishes the checkout through its own
        // writer (`import::replace_tree`), so only a backend with no way in at all is refused.
        let report =
            crate::import::base_refresh(self.backend.as_ref(), self.snaps(), &params, ctx)?;
        self.changed();
        Ok(report)
    }

    fn mount_info(&self) -> CtlResult<MountInfo> {
        Ok(MountInfo {
            mount_path: self.mount_path.display().to_string(),
            adapter: crate::mounts::adapter_name().to_owned(),
            mounted: self.live(),
        })
    }

    fn mount_snapshot(
        &self,
        params: &MountSnapshot,
        guard: &HolderGuard<'_>,
    ) -> CtlResult<MountInfo> {
        // Held across the check and the export, the same as a removal: whoever adds a holder
        // takes this lock, so a holder cannot appear between the check and the mount.
        let _serialised = guard.lock();
        guard.check_holders()?;
        self.exports.mount_snapshot(params)?;
        self.changed();
        Ok(MountInfo {
            mount_path: params.path.clone(),
            adapter: crate::mounts::adapter_name().to_owned(),
            mounted: true,
        })
    }

    fn unmount_snapshot(&self, params: &UnmountSnapshot) -> CtlResult<()> {
        self.exports.unmount_snapshot(params)?;
        self.changed();
        Ok(())
    }

    fn shutdown(&self) -> CtlResult<()> {
        // Every export and the default mount go before the answer, so the ordering is
        // unmount, then stop the control server, then close the backend.
        for e in self.shutdown_mount_tree() {
            eprintln!("cowfs-daemon: {e}");
        }
        Ok(())
    }
}

/// Lets a call that fires per entry report about twenty times a second, the protocol's rate.
struct Throttle(Option<std::time::Instant>);

impl Throttle {
    fn new() -> Self {
        Throttle(None)
    }

    /// True on the first call and then at most once per 50 ms.
    fn due(&mut self) -> bool {
        let now = std::time::Instant::now();
        if self
            .0
            .is_some_and(|t| now.duration_since(t) < std::time::Duration::from_millis(50))
        {
            return false;
        }
        self.0 = Some(now);
        true
    }
}

/// One store damage record as the protocol's problem, so `fsck` reports where it is rather than
/// that something is wrong.
fn damage(d: &cowfs_store::Damage) -> cowfs_ctl::FsckProblem {
    use cowfs_store::Damage;
    match d {
        Damage::Gap { pack, offset, len } => cowfs_ctl::FsckProblem {
            kind: "gap".into(),
            detail: format!("pack {pack} at offset {offset}, {len} bytes"),
        },
        Damage::HashMismatch { pack, offset, id } => cowfs_ctl::FsckProblem {
            kind: "hash_mismatch".into(),
            detail: format!("pack {pack} at offset {offset}, block {id}"),
        },
        Damage::BadPayload { pack, offset, id } => cowfs_ctl::FsckProblem {
            kind: "bad_payload".into(),
            detail: format!("pack {pack} at offset {offset}, block {id}"),
        },
        Damage::IndexEntry { id } => cowfs_ctl::FsckProblem {
            kind: "index_entry".into(),
            detail: format!("block {id}"),
        },
        Damage::MissingLiveBlock { id } => cowfs_ctl::FsckProblem {
            kind: "missing_live_block".into(),
            detail: format!("live reference to absent block {id}"),
        },
    }
}

/// Total bytes and file count under `root`, for `status`. Symlinks are not followed.
fn walk_sizes(root: &std::path::Path) -> (u64, u64) {
    fn walk(dir: &std::path::Path, bytes: &mut u64, files: &mut u64, depth: usize) {
        if depth > 64 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                walk(&path, bytes, files, depth + 1);
            } else if kind.is_file() {
                if let Ok(m) = entry.metadata() {
                    *bytes = bytes.saturating_add(m.len());
                    *files += 1;
                }
            }
        }
    }
    let (mut bytes, mut files) = (0, 0);
    walk(root, &mut bytes, &mut files, 0);
    (bytes, files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::PathBackend;

    fn handler() -> (tempfile::TempDir, Arc<Handler>) {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn Backend> =
            Arc::new(PathBackend::open(dir.path().join("store")).unwrap());
        backend.snapshots().create("base", None).unwrap();
        let mount = Arc::new(
            Mounted::no_mount(dir.path().join("mnt")).expect("the placeholder is always there"),
        );
        let exports = Exports::new(
            Arc::clone(&backend),
            vec![dir.path().join("pool")],
            vec![backend.store_path().to_owned()],
            dir.path().join("mnt"),
        );
        (dir, Handler::new(backend, mount, exports))
    }

    fn guard<'a>(
        name: &'a str,
        expect: bool,
        held: Arc<Lock<()>>,
        holders: &'a dyn Fn(&str) -> CtlResult<Vec<ProcessInfo>>,
    ) -> HolderGuard<'a> {
        HolderGuard::new(name, expect, held, holders)
    }

    #[test]
    fn snapshots_round_trip_through_the_handler() {
        let (_d, h) = handler();
        assert_eq!(h.snapshot_list().unwrap().len(), 1);
        h.snapshot_create(SnapshotCreate {
            name: "slot".into(),
            from: Some("base".into()),
        })
        .unwrap();
        let names: Vec<_> = h
            .snapshot_list()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["base", "slot"]);

        let held = Arc::new(Lock::new(()));
        let none = |_: &str| Ok(Vec::new());
        let g = guard("slot", false, Arc::clone(&held), &none);
        let swapped = h.swap("slot", "base", &g).unwrap();
        assert_eq!(swapped.parent.as_deref(), Some("base"));
        h.remove("slot", &guard("slot", false, Arc::clone(&held), &none))
            .unwrap();
        assert_eq!(h.snapshot_list().unwrap().len(), 1);
    }

    #[test]
    fn a_rename_reports_the_new_name_and_keeps_both_snapshots_out_of_the_way() {
        let (_d, h) = handler();
        h.snapshot_create(SnapshotCreate {
            name: "slot".into(),
            from: Some("base".into()),
        })
        .unwrap();
        let info = h.snapshot_rename("slot", "slot2").unwrap();
        assert_eq!(info.name, "slot2");
        let names: Vec<_> = h
            .snapshot_list()
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["base", "slot2"], "the old name is gone");
    }

    #[test]
    fn a_promote_is_idempotent_and_reports_a_base() {
        let (_d, h) = handler();
        let first = h.snapshot_promote("base").unwrap();
        assert!(first.base.is_some(), "{first:?}");
        let second = h.snapshot_promote("base").unwrap();
        assert!(second.base.is_some());
    }

    #[test]
    fn a_missing_snapshot_is_not_found_not_an_io_error() {
        let (_d, h) = handler();
        let held = Arc::new(Lock::new(()));
        let none = |_: &str| Ok(Vec::new());
        let e = h
            .remove("nosuch", &guard("nosuch", false, Arc::clone(&held), &none))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound, "{e}");
        let e = h.snapshot_rename("nosuch", "other").unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound, "{e}");
        let e = h.snapshot_promote("nosuch").unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound, "{e}");
        let e = h.holders("nosuch").unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound, "{e}");
    }

    #[test]
    fn a_holder_makes_a_removal_busy_and_the_snapshot_survives() {
        let (_d, h) = handler();
        let held = Arc::new(Lock::new(()));
        let source = |_: &str| {
            Ok(vec![ProcessInfo {
                pid: 1,
                command: "holder".into(),
                holds: Vec::new(),
            }])
        };
        let e = h
            .remove("base", &guard("base", true, Arc::clone(&held), &source))
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Busy, "{e}");
        assert_eq!(h.snapshot_list().unwrap().len(), 1, "nothing changed");

        // The same removal with expect_no_holders false goes through.
        h.remove("base", &guard("base", false, Arc::clone(&held), &source))
            .unwrap();
        assert!(h.snapshot_list().unwrap().is_empty());
    }

    #[test]
    fn status_reports_the_store_and_the_mount() {
        let (dir, h) = handler();
        let s = h.status().unwrap();
        assert_eq!(s.snapshot_count, 1);
        assert!(s.store_path.ends_with("store"), "{}", s.store_path);
        assert_eq!(s.mount_path, dir.path().join("mnt").display().to_string());
        let m = h.mount_info().unwrap();
        assert_eq!(m.adapter, crate::mounts::adapter_name());
    }

    #[test]
    fn gc_and_fsck_say_unsupported_rather_than_pretending() {
        let (_d, h) = handler();
        let e = h
            .gc(GcParams { dry_run: true }, &OpContext::detached())
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported, "{e}");
        let e = h.fsck(&OpContext::detached()).unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported, "{e}");
    }

    fn core_handler() -> (tempfile::TempDir, Arc<Handler>, Arc<dyn Backend>) {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn Backend> = Arc::new(
            crate::backend::CoreBackend::open_with_gc(
                dir.path().join("store"),
                cowfs_core::Options {
                    background: false,
                    store: cowfs_store::Options {
                        max_pack_size: 96 << 10,
                        ..cowfs_store::Options::default()
                    },
                    file_flush_bytes: 32 << 10,
                    ..cowfs_core::Options::default()
                },
                cowfs_gc::Options {
                    dead_ratio: 0.0,
                    min_dead_bytes: 1,
                    io_budget_bytes: 0,
                    batch_bytes: 4096,
                    ..cowfs_gc::Options::default()
                },
            )
            .unwrap(),
        );
        let mount = Arc::new(Mounted::no_mount(dir.path().join("mnt")).unwrap());
        let exports = Exports::new(
            Arc::clone(&backend),
            vec![dir.path().join("pool")],
            vec![backend.store_path().to_owned()],
            dir.path().join("mnt"),
        );
        (
            dir,
            Handler::new(Arc::clone(&backend), mount, exports),
            backend,
        )
    }

    fn body(n: usize, seed: u32) -> Vec<u8> {
        let mut h = seed.wrapping_mul(2654435761).wrapping_add(1);
        (0..n)
            .map(|_| {
                h = h.wrapping_mul(1664525).wrapping_add(1013904223);
                (h >> 16) as u8
            })
            .collect()
    }

    fn put(v: &dyn cowfs_vfs::Vfs, name: &str, data: &[u8]) {
        let a = v
            .create(cowfs_vfs::ROOT_INO, name.as_bytes(), 0o644)
            .unwrap();
        v.write(a.ino, 0, data).unwrap();
    }

    fn get(v: &dyn cowfs_vfs::Vfs, name: &str) -> Vec<u8> {
        let a = v.lookup(cowfs_vfs::ROOT_INO, name.as_bytes()).unwrap();
        v.read(a.ino, 0, a.size as u32).unwrap()
    }

    /// Two snapshots whose files share packs, then one removed. Returns the survivors.
    fn garbage(h: &Handler, backend: &Arc<dyn Backend>) -> Vec<(String, Vec<u8>)> {
        h.snapshot_create(SnapshotCreate {
            name: "keep".into(),
            from: None,
        })
        .unwrap();
        h.snapshot_create(SnapshotCreate {
            name: "drop".into(),
            from: None,
        })
        .unwrap();
        let (k, d) = (
            backend.snapshot("keep").unwrap(),
            backend.snapshot("drop").unwrap(),
        );
        let mut keep = Vec::new();
        for i in 0..14u32 {
            let f = (format!("k{i:02}"), body(40_000, i));
            put(k.as_ref(), &f.0, &f.1);
            put(d.as_ref(), &format!("d{i:02}"), &body(40_000, 900 + i));
            keep.push(f);
            k.fsync(cowfs_vfs::ROOT_INO, false).unwrap();
        }
        for i in 0..4u32 {
            let f = (format!("t{i:02}"), body(40_000, 700 + i));
            put(k.as_ref(), &f.0, &f.1);
            keep.push(f);
            k.fsync(cowfs_vfs::ROOT_INO, false).unwrap();
        }
        let held = Arc::new(Lock::new(()));
        let none = |_: &str| Ok(Vec::new());
        h.remove("drop", &guard("drop", false, held, &none))
            .unwrap();
        keep
    }

    fn recording() -> (Arc<Lock<Vec<ProgressEvent>>>, OpContext<'static>) {
        let events = Arc::new(Lock::new(Vec::new()));
        let sink = Arc::clone(&events);
        let ctx = OpContext::new(cowfs_ctl::CancelToken::new(), move |e| {
            sink.lock().unwrap().push(e);
            true
        });
        (events, ctx)
    }

    /// Issue 290: a client gives up on a daemon that stays silent for its timeout, and `fsck` of a
    /// large store used to say nothing until it was done.
    #[test]
    fn fsck_reports_progress_while_it_runs_not_only_at_the_end() {
        let (_d, h, backend) = core_handler();
        garbage(&h, &backend);
        let (events, ctx) = recording();
        h.fsck(&ctx).unwrap();
        let events = events.lock().unwrap();
        let first = events.first().expect("an event");
        assert!(
            events.len() >= 2 && first.done < first.total.unwrap(),
            "the first event comes before the work is done: {events:?}"
        );
    }

    /// The same for `import`: the hash of the source and of the snapshot run after the ingest and
    /// read every byte again.
    #[test]
    fn import_reports_progress_in_every_phase() {
        let (d, h, _backend) = core_handler();
        let src = d.path().join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("sub/a"), body(200_000, 3)).unwrap();
        std::fs::write(src.join("b"), body(10, 4)).unwrap();
        let (events, ctx) = recording();
        let report = h
            .import(
                ImportParams {
                    path: src.display().to_string(),
                    name: "imp".into(),
                },
                &ctx,
            )
            .unwrap();
        assert!(report.verified, "{report:?}");
        let events = events.lock().unwrap();
        for phase in ["ingest", "hash source", "hash snapshot"] {
            assert!(
                events.iter().any(|e| e.phase == phase),
                "no {phase:?} event: {events:?}"
            );
        }
    }

    #[test]
    fn gc_over_the_core_reclaims_dead_packs_and_survivors_still_read() {
        let (_d, h, backend) = core_handler();
        let keep = garbage(&h, &backend);
        let before = backend.usage().unwrap().unwrap();

        let events = Arc::new(Lock::new(Vec::new()));
        let sink = Arc::clone(&events);
        let ctx = OpContext::new(cowfs_ctl::CancelToken::new(), move |e| {
            sink.lock().unwrap().push(e);
            true
        });
        let dry = h.gc(GcParams { dry_run: true }, &ctx).unwrap();
        assert!(dry.dry_run);
        assert!(
            dry.candidate_blocks > 0 && dry.candidate_bytes > 0,
            "{dry:?}"
        );
        assert_eq!((dry.freed_blocks, dry.freed_bytes), (0, 0), "{dry:?}");
        assert_eq!(
            (
                dry.gross_removed_bytes,
                dry.rewrite_bytes,
                dry.net_reclaimed_bytes
            ),
            (0, Some(0), Some(0)),
            "a dry run reports no actual gross, rewrite or net: {dry:?}"
        );
        assert_eq!(
            backend.usage().unwrap().unwrap(),
            before,
            "a dry run changes nothing"
        );

        let live = h.gc(GcParams { dry_run: false }, &ctx).unwrap();
        assert!(!live.dry_run);
        assert!(live.freed_bytes > 0 && live.freed_blocks > 0, "{live:?}");
        assert_eq!(
            live.gross_removed_bytes, live.freed_bytes,
            "the explicit gross field agrees with the legacy one: {live:?}"
        );
        assert_eq!(
            live.net_reclaimed_bytes,
            Some(live.gross_removed_bytes as i64 - live.rewrite_bytes.unwrap() as i64),
            "net is gross minus rewrite, signed: {live:?}"
        );
        let after = backend.usage().unwrap().unwrap();
        assert!(after.blocks < before.blocks, "{before:?} -> {after:?}");
        assert_eq!(before.blocks - after.blocks, live.freed_blocks);
        let events = events.lock().unwrap();
        assert!(events.iter().any(|e| e.phase == "mark"), "{events:?}");
        assert!(events.iter().any(|e| e.phase == "sweep"), "{events:?}");

        let k = backend.snapshot("keep").unwrap();
        for (n, data) in &keep {
            assert!(get(k.as_ref(), n) == *data, "{n}");
        }
        assert!(backend
            .fsck(&mut |_, _| true)
            .unwrap()
            .unwrap()
            .damage
            .is_empty());
    }

    /// A backend that reports a GC cycle which failed but still wrote bytes into a new pack: an
    /// error, zero unlinked packs, and a positive rewrite. Used to check the daemon surfaces the
    /// real cost rather than dropping the report behind a bare "freed nothing".
    #[derive(Debug)]
    struct FailingGc {
        inner: PathBackend,
        report: cowfs_gc::GcReport,
    }

    impl Backend for FailingGc {
        fn ingests_directories(&self) -> bool {
            self.inner.ingests_directories()
        }

        fn root(&self) -> std::io::Result<Arc<dyn cowfs_vfs::Vfs>> {
            self.inner.root()
        }

        fn snapshot(&self, name: &str) -> std::io::Result<Arc<dyn cowfs_vfs::Vfs>> {
            self.inner.snapshot(name)
        }

        fn store_path(&self) -> &std::path::Path {
            self.inner.store_path()
        }

        fn snapshots(&self) -> &dyn crate::backend::Snapshots {
            self.inner.snapshots()
        }

        fn collect_garbage(
            &self,
            _dry_run: bool,
            _progress: crate::backend::GcProgress<'_>,
            _cancelled: &dyn Fn() -> bool,
        ) -> CtlResult<Option<crate::backend::GcOutcome>> {
            Ok(Some(crate::backend::GcOutcome {
                report: self.report.clone(),
                blocks_before: 10,
                blocks_after: 10,
            }))
        }
    }

    fn gc_handler_with_report(report: cowfs_gc::GcReport) -> (tempfile::TempDir, Arc<Handler>) {
        let dir = tempfile::tempdir().unwrap();
        let backend: Arc<dyn Backend> = Arc::new(FailingGc {
            inner: PathBackend::open(dir.path().join("store")).unwrap(),
            report,
        });
        backend.snapshots().create("base", None).unwrap();
        let mount = Arc::new(
            Mounted::no_mount(dir.path().join("mnt")).expect("the placeholder is always there"),
        );
        let exports = Exports::new(
            Arc::clone(&backend),
            vec![dir.path().join("pool")],
            vec![backend.store_path().to_owned()],
            dir.path().join("mnt"),
        );
        (dir, Handler::new(backend, mount, exports))
    }

    /// A pack the store unlinked but could not make durable is a terminal failure, not a quiet
    /// success: the bytes are gone, so the figures are real, but the cycle could not confirm the
    /// removal and the caller has to be told.
    #[test]
    fn an_unconfirmed_unlink_is_a_terminal_failure_carrying_the_real_figures() {
        let (_d, h) = gc_handler_with_report(cowfs_gc::GcReport {
            freed_bytes: 8192,
            gross_removed_bytes: 8192,
            rewrite_bytes: 2048,
            net_reclaimed_bytes: 6144,
            packs_unlinked: 2,
            unlink_durability_errors: 2,
            errors: vec!["i/o error: Is a directory (os error 21)".into()],
            ..Default::default()
        });
        let ctx = OpContext::new(cowfs_ctl::CancelToken::new(), |_| true);
        let e = h.gc(GcParams { dry_run: false }, &ctx).unwrap_err();
        assert_eq!(e.code, ErrorCode::IoError, "{e}");
        let msg = e.message.clone();
        assert!(msg.contains("durable"), "{msg}");
        // The real numbers, not zeros: the unlink happened.
        assert!(msg.contains("8192") && msg.contains("2048"), "{msg}");
        assert!(msg.contains("6144"), "the signed net is carried: {msg}");
    }

    #[test]
    fn a_failed_gc_that_wrote_bytes_reports_the_cost_not_a_bare_freed_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let report = cowfs_gc::GcReport {
            rewrite_bytes: 4096,
            net_reclaimed_bytes: -4096,
            errors: vec!["corrupt record in pack 5".into()],
            ..Default::default()
        };
        let backend: Arc<dyn Backend> = Arc::new(FailingGc {
            inner: PathBackend::open(dir.path().join("store")).unwrap(),
            report,
        });
        backend.snapshots().create("base", None).unwrap();
        let mount = Arc::new(
            Mounted::no_mount(dir.path().join("mnt")).expect("the placeholder is always there"),
        );
        let exports = Exports::new(
            Arc::clone(&backend),
            vec![dir.path().join("pool")],
            vec![backend.store_path().to_owned()],
            dir.path().join("mnt"),
        );
        let h = Handler::new(backend, mount, exports);
        let ctx = OpContext::new(cowfs_ctl::CancelToken::new(), |_| true);
        let e = h.gc(GcParams { dry_run: false }, &ctx).unwrap_err();
        assert_eq!(e.code, ErrorCode::IoError, "{e}");
        let msg = e.message.clone();
        assert!(
            msg.contains("4096") && msg.contains("rewritten"),
            "the failure must carry the real rewrite cost, not a bare 'freed nothing': {msg}"
        );
        assert!(
            msg.contains("-4096") || msg.contains("net -"),
            "the failure must show the signed net: {msg}"
        );
    }

    #[test]
    fn a_cancelled_gc_is_cancelled_and_leaves_the_store_readable() {
        let (_d, h, backend) = core_handler();
        let keep = garbage(&h, &backend);
        let token = cowfs_ctl::CancelToken::new();
        token.cancel();
        let ctx = OpContext::new(token, |_| true);
        let e = h.gc(GcParams { dry_run: false }, &ctx).unwrap_err();
        assert_eq!(e.code, ErrorCode::Cancelled, "{e}");
        let k = backend.snapshot("keep").unwrap();
        for (n, data) in &keep {
            assert!(get(k.as_ref(), n) == *data, "{n}");
        }
        assert!(backend
            .fsck(&mut |_, _| true)
            .unwrap()
            .unwrap()
            .damage
            .is_empty());
        let again = h
            .gc(GcParams { dry_run: false }, &OpContext::detached())
            .unwrap();
        assert!(
            again.freed_bytes > 0,
            "the next request finishes the job: {again:?}"
        );
    }

    /// A durable snapshot references a block the store acknowledged as lost: `fsck` over the
    /// control path must report it, not clean. This is the way an operator hits #84.
    #[test]
    fn fsck_reports_a_missing_live_block_over_the_control_path() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store");
        let core_opts = || cowfs_core::Options {
            background: false,
            store: cowfs_store::Options {
                max_pack_size: 96 << 10,
                checkpoint_on_drop: true,
                ..cowfs_store::Options::default()
            },
            file_flush_bytes: 32 << 10,
            ..cowfs_core::Options::default()
        };

        // Build a durable snapshot with one file, then close the backend cleanly.
        {
            let backend: Arc<dyn Backend> =
                Arc::new(crate::backend::CoreBackend::open(&store, core_opts()).unwrap());
            let mount = Arc::new(Mounted::no_mount(dir.path().join("mnt")).unwrap());
            let exports = Exports::new(
                Arc::clone(&backend),
                vec![dir.path().join("pool")],
                vec![backend.store_path().to_owned()],
                dir.path().join("mnt"),
            );
            let h = Handler::new(Arc::clone(&backend), mount, exports);
            h.snapshot_create(SnapshotCreate {
                name: "s".into(),
                from: None,
            })
            .unwrap();
            {
                let s = backend.snapshot("s").unwrap();
                put(s.as_ref(), "f", &body(300_000, 5));
                s.fsync(cowfs_vfs::ROOT_INO, false).unwrap();
            }
            backend.close().unwrap();
            drop(h);
        }

        // Fixture-owned corruption: remove the pack, acknowledge the loss as the old reader did.
        let block_store = store.join("store");
        let mut victims: Vec<std::path::PathBuf> = std::fs::read_dir(block_store.join("packs"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "cpk"))
            .collect();
        victims.sort();
        let victim = victims.pop().expect("a pack to remove");
        assert!(std::fs::metadata(&victim).unwrap().len() > 0, "empty pack");
        std::fs::remove_file(&victim).unwrap();
        {
            let s = cowfs_store::Store::open(&block_store, core_opts().store).unwrap();
            assert!(s.recovery().has_corruption(), "loss not seen");
            s.acknowledge_corruption().unwrap();
        }

        // Reopen over the control path and run fsck the way a client does.
        let backend: Arc<dyn Backend> =
            Arc::new(crate::backend::CoreBackend::open(&store, core_opts()).unwrap());
        let mount = Arc::new(Mounted::no_mount(dir.path().join("mnt")).unwrap());
        let exports = Exports::new(
            Arc::clone(&backend),
            vec![dir.path().join("pool")],
            vec![backend.store_path().to_owned()],
            dir.path().join("mnt"),
        );
        let h = Handler::new(Arc::clone(&backend), mount, exports);
        let report = h.fsck(&OpContext::detached()).unwrap();
        assert!(
            !report.ok,
            "fsck reported ok while a live file referenced a missing block"
        );
        assert!(
            report
                .problems
                .iter()
                .any(|p| p.kind == "missing_live_block"),
            "fsck did not name the missing live block: {:?}",
            report.problems
        );
    }

    #[test]
    fn closing_the_backend_after_a_gc_releases_the_store() {
        let (dir, h, backend) = core_handler();
        garbage(&h, &backend);
        h.gc(GcParams { dry_run: false }, &OpContext::detached())
            .unwrap();
        h.close_backend()
            .expect("no collector clone outlives the request");
        let reopened = crate::backend::CoreBackend::open(
            dir.path().join("store"),
            cowfs_core::Options {
                background: false,
                ..cowfs_core::Options::default()
            },
        )
        .expect("the store lock was released");
        reopened.close().unwrap();
    }

    /// The handler plus injected holders, so the framework suite can hold one without a real
    /// process holding a real mount. `holders` falls back to the injected set for a snapshot
    /// the real handler cannot see on the mount, which is exactly what the suite needs: it
    /// asks about a snapshot name before any directory exists at that path.
    struct WithHolders {
        inner: Arc<Handler>,
        injected: Lock<std::collections::BTreeMap<String, Vec<ProcessInfo>>>,
        locks: Lock<std::collections::BTreeMap<String, Arc<Lock<()>>>>,
    }

    impl WithHolders {
        fn add(&self, name: &str) {
            let mut holders = self
                .injected
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            holders
                .entry(name.to_owned())
                .or_default()
                .push(ProcessInfo {
                    pid: 4242,
                    command: "injected".into(),
                    holds: Vec::new(),
                });
        }

        fn lock_for(&self, name: &str) -> Arc<Lock<()>> {
            Arc::clone(
                self.locks
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .entry(name.to_owned())
                    .or_insert_with(|| Arc::new(Lock::new(()))),
            )
        }
    }

    impl ControlHandler for WithHolders {
        fn status(&self) -> CtlResult<Status> {
            self.inner.status()
        }
        fn snapshot_list(&self) -> CtlResult<Vec<SnapshotInfo>> {
            self.inner.snapshot_list()
        }
        fn snapshot_create(&self, p: SnapshotCreate) -> CtlResult<SnapshotInfo> {
            self.inner.snapshot_create(p)
        }
        fn remove(&self, name: &str, guard: &HolderGuard<'_>) -> CtlResult<()> {
            let held = self.lock_for(name);
            let _serialised = held.lock();
            self.inner.remove(name, guard)
        }
        fn swap(&self, name: &str, from: &str, guard: &HolderGuard<'_>) -> CtlResult<SnapshotInfo> {
            let held = self.lock_for(name);
            let _serialised = held.lock();
            self.inner.swap(name, from, guard)
        }
        fn holders(&self, snapshot: &str) -> CtlResult<Vec<ProcessInfo>> {
            let mut found = self.inner.holders(snapshot).unwrap_or_default();
            if let Some(injected) = self
                .injected
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(snapshot)
            {
                found.extend(injected.iter().cloned());
            }
            Ok(found)
        }
        fn snapshot_rename(&self, from: &str, to: &str) -> CtlResult<SnapshotInfo> {
            self.inner.snapshot_rename(from, to)
        }
        fn snapshot_promote(&self, name: &str) -> CtlResult<SnapshotInfo> {
            self.inner.snapshot_promote(name)
        }
        fn mount_info(&self) -> CtlResult<MountInfo> {
            self.inner.mount_info()
        }
        fn mount_snapshot(
            &self,
            params: &MountSnapshot,
            guard: &HolderGuard<'_>,
        ) -> CtlResult<MountInfo> {
            let held = self.lock_for(guard.name());
            let _serialised = held.lock();
            self.inner.mount_snapshot(params, guard)
        }
        fn unmount_snapshot(&self, params: &UnmountSnapshot) -> CtlResult<()> {
            self.inner.unmount_snapshot(params)
        }
    }

    #[test]
    fn the_handler_passes_the_framework_conformance_suite() {
        let (_d, h) = handler();
        let w = Arc::new(WithHolders {
            inner: h,
            injected: Lock::default(),
            locks: Lock::default(),
        });
        let dynh: Arc<dyn ControlHandler> = w.clone();
        cowfs_ctl::handler_conformance(
            dynh,
            Arc::new(move |name: &str| {
                // The holder must go in under the same per-snapshot lock the guard returns,
                // which is what a real adapter does with its own handle bookkeeping.
                let held = w.lock_for(name);
                let _serialised = held.lock();
                w.add(name);
            }),
        )
        .expect("the handler must satisfy handler_conformance");
    }
}
