//! The `ControlHandler` the control server dispatches to: one snapshot namespace, one mount,
//! one export registry, behind the protocol in `docs/v1-control-api.md`.
//!
//! `remove` and `swap` hold the framework's per-snapshot lock across the holder check and the
//! change, which is what `cowfs_ctl::handler_conformance` verifies. Everything a control
//! operation changes behind the mount is announced to the kernel afterwards, so the default
//! shared cache regime stays bounded instead of stale.

use crate::backend::{Backend, Snapshots};
use crate::exports::{Exports, MountSnapshot, UnmountSnapshot};
use crate::holders;
use crate::mounts::Mounted;
use cowfs_ctl::{
    BaseRefreshParams, BaseRefreshReport, ControlHandler, CtlError, CtlResult, ErrorCode,
    FsckReport, GcParams, GcReport, HolderGuard, ImportParams, ImportReport, MountInfo, OpContext,
    ProcessInfo, SnapshotCreate, SnapshotInfo, Status,
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
        let (logical, files) = walk_sizes(&self.store_path);
        Ok(Status {
            store_path: self.store_path.display().to_string(),
            mount_path: self.mount_path.display().to_string(),
            snapshot_count: names.len() as u64,
            // A passthrough backend has no block store, so these are counts of what it holds
            // rather than of blocks. The core reports the real numbers.
            block_count: files,
            logical_bytes: logical,
            stored_bytes: logical,
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
                "garbage collection needs the block store, which this backend does not have (dry_run: {})",
                params.dry_run
            ),
        ))
    }

    fn fsck(&self, _ctx: &OpContext<'_>) -> CtlResult<FsckReport> {
        Err(CtlError::new(
            ErrorCode::Unsupported,
            "fsck needs the block store, which this backend does not have",
        ))
    }

    fn import(&self, params: ImportParams, ctx: &OpContext<'_>) -> CtlResult<ImportReport> {
        crate::import::run(self.backend.as_ref(), self.snaps(), &params, ctx)
    }

    fn base_refresh(
        &self,
        params: BaseRefreshParams,
        ctx: &OpContext<'_>,
    ) -> CtlResult<BaseRefreshReport> {
        crate::import::base_refresh(self.backend.as_ref(), self.snaps(), &params, ctx)
    }

    fn mount_info(&self) -> CtlResult<MountInfo> {
        Ok(MountInfo {
            mount_path: self.mount_path.display().to_string(),
            adapter: crate::mounts::adapter_name().to_owned(),
            mounted: self.live(),
        })
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

impl Handler {
    /// `mount_snapshot {name, path, expect_no_holders?}`. The whole rule table lives in
    /// `Exports`, so this only translates the result and announces the change.
    pub fn mount_snapshot(&self, req: &MountSnapshot) -> CtlResult<MountInfo> {
        self.exports.mount_snapshot(req)?;
        self.changed();
        Ok(MountInfo {
            mount_path: req.path.clone(),
            adapter: crate::mounts::adapter_name().to_owned(),
            mounted: true,
        })
    }

    /// `unmount_snapshot {path}`.
    pub fn unmount_snapshot(&self, req: &UnmountSnapshot) -> CtlResult<()> {
        self.exports.unmount_snapshot(req)?;
        self.changed();
        Ok(())
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
