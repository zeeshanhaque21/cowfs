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
    MountSnapshot, OpContext, ProcessInfo, SnapshotCreate, SnapshotInfo, Status, UnmountSnapshot,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

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

    /// `import` and `base_refresh` copy a directory into the store, which only means anything for
    /// a backend whose snapshots are directories. On the core the source has to go in through the
    /// mount, which is a different operation, so this says so instead of writing a tree the core
    /// would not read back as a snapshot.
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

    fn dir_of(&self, name: &str) -> PathBuf {
        self.mount_path.join(name)
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
        let dir = self.dir_of(snapshot);
        if !dir.exists() {
            return Err(CtlError::not_found(format!(
                "snapshot {snapshot:?} does not exist"
            )));
        }
        Ok(holders::scan(&dir))
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

    fn gc(&self, params: GcParams, _ctx: &OpContext<'_>) -> CtlResult<GcReport> {
        Err(CtlError::new(
            ErrorCode::Unsupported,
            format!(
                "garbage collection is not wired to the core's mark-and-sweep yet, and this \
                 backend has no block store to sweep (dry_run: {})",
                params.dry_run
            ),
        ))
    }

    fn fsck(&self, ctx: &OpContext<'_>) -> CtlResult<FsckReport> {
        let report = self
            .backend
            .fsck()
            .map_err(|e| CtlError::new(ErrorCode::IoError, format!("fsck: {e}")))?
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

    fn import(&self, params: ImportParams, ctx: &OpContext<'_>) -> CtlResult<ImportReport> {
        self.can_ingest()?;
        crate::import::run(self.backend.as_ref(), self.snaps(), &params, ctx)
    }

    fn base_refresh(
        &self,
        params: BaseRefreshParams,
        ctx: &OpContext<'_>,
    ) -> CtlResult<BaseRefreshReport> {
        self.can_ingest()?;
        crate::import::base_refresh(self.backend.as_ref(), self.snaps(), &params, ctx)
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
