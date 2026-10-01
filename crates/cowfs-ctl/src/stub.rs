use crate::error::{CtlError, CtlResult, ErrorCode};
use crate::handler::{ControlHandler, HolderGuard, OpContext};
use crate::treehash::{hash_tree, HASH_ALGORITHM};
use crate::types::*;
use crate::validate::{name_key, validate_snapshot_name};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A `ControlHandler` that keeps snapshots in memory and simulates the long operations.
///
/// It backs the tests and `cowfs serve --stub`. Long operations emit `steps` progress events,
/// sleeping `delay` between them and stopping early when cancelled. Every operation runs under
/// one lock, which is what a real backend must match for `snapshot_rm` and `snapshot_reset`.
#[derive(Debug)]
pub struct StubHandler {
    store_path: String,
    mount_path: String,
    steps: u64,
    delay: Duration,
    ignore_cancel: bool,
    started: Instant,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    snapshots: BTreeMap<String, SnapshotInfo>,
    processes: BTreeMap<String, Vec<ProcessInfo>>,
    blocks: u64,
    bytes: u64,
}

const STUB_BLOCK_BYTES: u64 = 64 * 1024;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

impl StubHandler {
    /// A stub reporting the given store and mount paths, with four instant progress steps.
    pub fn new(store_path: impl Into<String>, mount_path: impl Into<String>) -> Self {
        StubHandler {
            store_path: store_path.into(),
            mount_path: mount_path.into(),
            steps: 4,
            delay: Duration::ZERO,
            ignore_cancel: false,
            started: Instant::now(),
            state: Mutex::new(State::default()),
        }
    }

    /// Sets how many progress events long operations emit and the pause between them.
    pub fn with_work(mut self, steps: u64, delay: Duration) -> Self {
        self.steps = steps;
        self.delay = delay;
        self
    }

    /// Makes long operations run to the end even when cancelled, like an uncooperative backend.
    pub fn ignoring_cancel(mut self) -> Self {
        self.ignore_cancel = true;
        self
    }

    /// Makes `ps` report `process` as holding `snapshot`, which also makes `snapshot_rm` and
    /// `snapshot_reset` fail with `busy`. The snapshot need not exist yet.
    pub fn with_process(self, snapshot: &str, process: ProcessInfo) -> Self {
        self.add_process(snapshot, process);
        self
    }

    /// Like `with_process`, on a live handler.
    pub fn add_process(&self, snapshot: &str, process: ProcessInfo) {
        self.lock()
            .processes
            .entry(snapshot.to_owned())
            .or_default()
            .push(process);
    }

    /// Adds a holder while the snapshot's framework lock is held, which is what a real adapter
    /// must do too, so a check inside a swap cannot miss it.
    pub fn add_holder(&self, snapshot: &str, process: ProcessInfo, lock: &Arc<Mutex<()>>) {
        let _serialised = lock.lock().unwrap_or_else(PoisonError::into_inner);
        self.add_process(snapshot, process);
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn work(&self, phase: &str, unit: Unit, ctx: &OpContext<'_>) -> CtlResult<()> {
        for done in 0..=self.steps {
            let event = ProgressEvent {
                phase: phase.to_owned(),
                done,
                total: Some(self.steps),
                unit,
                message: None,
            };
            if self.ignore_cancel {
                let _ = ctx.progress(event);
            } else {
                ctx.progress(event)?;
            }
            thread::sleep(self.delay);
        }
        Ok(())
    }
}

fn exists(name: &str) -> CtlError {
    CtlError::new(
        ErrorCode::AlreadyExists,
        format!("snapshot {name:?} already exists or collides with an existing name"),
    )
}

fn missing(name: &str) -> CtlError {
    CtlError::not_found(format!("snapshot {name:?} does not exist"))
}

impl State {
    fn collides(&self, name: &str, except: Option<&str>) -> bool {
        let key = name_key(name);
        self.snapshots
            .keys()
            .any(|k| Some(k.as_str()) != except && name_key(k) == key)
    }

    fn insert(&mut self, info: SnapshotInfo) -> SnapshotInfo {
        self.blocks += 1;
        self.bytes += STUB_BLOCK_BYTES;
        self.snapshots.insert(info.name.clone(), info.clone());
        info
    }
}

impl ControlHandler for StubHandler {
    fn status(&self) -> CtlResult<Status> {
        let st = self.lock();
        Ok(Status {
            store_path: self.store_path.clone(),
            mount_path: self.mount_path.clone(),
            snapshot_count: st.snapshots.len() as u64,
            block_count: st.blocks,
            logical_bytes: st.bytes,
            stored_bytes: st.bytes / 2,
            uptime_secs: self.started.elapsed().as_secs(),
        })
    }

    fn snapshot_list(&self) -> CtlResult<Vec<SnapshotInfo>> {
        Ok(self.lock().snapshots.values().cloned().collect())
    }

    fn snapshot_create(&self, params: SnapshotCreate) -> CtlResult<SnapshotInfo> {
        validate_snapshot_name(&params.name)?;
        let mut st = self.lock();
        if st.collides(&params.name, None) {
            return Err(exists(&params.name));
        }
        if let Some(from) = &params.from {
            if !st.snapshots.contains_key(from) {
                return Err(missing(from));
            }
        }
        Ok(st.insert(SnapshotInfo {
            name: params.name,
            parent: params.from,
            base: None,
            created_unix_ms: now_ms(),
        }))
    }

    fn remove(&self, name: &str, guard: &HolderGuard<'_>) -> CtlResult<()> {
        let _serialised = guard.lock();
        guard.check_holders()?;
        let mut st = self.lock();
        if !st.snapshots.contains_key(name) {
            return Err(missing(name));
        }
        st.snapshots.remove(name);
        st.processes.remove(name);
        Ok(())
    }

    fn swap(&self, name: &str, from: &str, guard: &HolderGuard<'_>) -> CtlResult<SnapshotInfo> {
        let _serialised = guard.lock();
        guard.check_holders()?;
        let mut st = self.lock();
        if !st.snapshots.contains_key(name) {
            return Err(missing(name));
        }
        if !st.snapshots.contains_key(from) {
            return Err(missing(from));
        }
        Ok(st.insert(SnapshotInfo {
            name: name.to_owned(),
            parent: Some(from.to_owned()),
            base: None,
            created_unix_ms: now_ms(),
        }))
    }

    fn snapshot_rename(&self, from: &str, to: &str) -> CtlResult<SnapshotInfo> {
        validate_snapshot_name(to)?;
        let mut st = self.lock();
        if !st.snapshots.contains_key(from) {
            return Err(missing(from));
        }
        if st.collides(to, Some(from)) || (from != to && st.snapshots.contains_key(to)) {
            return Err(exists(to));
        }
        let Some(mut info) = st.snapshots.remove(from) else {
            return Err(missing(from));
        };
        info.name = to.to_owned();
        for child in st.snapshots.values_mut() {
            if child.parent.as_deref() == Some(from) {
                child.parent = Some(to.to_owned());
            }
        }
        if let Some(p) = st.processes.remove(from) {
            st.processes.insert(to.to_owned(), p);
        }
        st.snapshots.insert(to.to_owned(), info.clone());
        Ok(info)
    }

    fn snapshot_promote(&self, name: &str) -> CtlResult<SnapshotInfo> {
        let mut st = self.lock();
        let info = st.snapshots.get_mut(name).ok_or_else(|| missing(name))?;
        info.base.get_or_insert_with(BaseMeta::default);
        Ok(info.clone())
    }

    fn gc(&self, params: GcParams, ctx: &OpContext<'_>) -> CtlResult<GcReport> {
        self.work("mark", Unit::Items, ctx)?;
        self.work("sweep", Unit::Bytes, ctx)?;
        let (blocks, bytes) = (3, 3 * STUB_BLOCK_BYTES);
        Ok(GcReport {
            dry_run: params.dry_run,
            candidate_blocks: blocks,
            candidate_bytes: bytes,
            freed_blocks: if params.dry_run { 0 } else { blocks },
            freed_bytes: if params.dry_run { 0 } else { bytes },
        })
    }

    fn fsck(&self, ctx: &OpContext<'_>) -> CtlResult<FsckReport> {
        self.work("verify", Unit::Bytes, ctx)?;
        let st = self.lock();
        Ok(FsckReport {
            ok: true,
            blocks_checked: st.blocks,
            bytes_checked: st.bytes,
            snapshots_checked: st.snapshots.len() as u64,
            problems: Vec::new(),
        })
    }

    fn import(&self, params: ImportParams, ctx: &OpContext<'_>) -> CtlResult<ImportReport> {
        if self.lock().collides(&params.name, None) {
            return Err(exists(&params.name));
        }
        let dir = Path::new(&params.path);
        if !dir.is_dir() {
            return Err(CtlError::not_found(format!(
                "{} is not a directory",
                params.path
            )));
        }
        let io_err = |e: std::io::Error| CtlError::new(ErrorCode::IoError, e.to_string());
        let ingested = hash_tree(dir).map_err(io_err)?;
        self.work("ingest", Unit::Bytes, ctx)?;
        self.work("verify", Unit::Bytes, ctx)?;
        let reread = hash_tree(dir).map_err(io_err)?;
        let verified = reread.root == ingested.root;
        let mismatches = if verified {
            Vec::new()
        } else {
            vec![ImportMismatch {
                path: ".".into(),
                reason: "source changed during import".into(),
            }]
        };
        let mut st = self.lock();
        if st.collides(&params.name, None) {
            return Err(exists(&params.name));
        }
        st.insert(SnapshotInfo {
            name: params.name.clone(),
            parent: None,
            base: None,
            created_unix_ms: now_ms(),
        });
        Ok(ImportReport {
            name: params.name,
            files: ingested.files,
            bytes: ingested.bytes,
            verified,
            hash_algorithm: HASH_ALGORITHM.into(),
            source_root_hash: reread.root,
            imported_root_hash: ingested.root,
            mismatches,
            mismatches_truncated: false,
        })
    }

    fn base_refresh(
        &self,
        params: BaseRefreshParams,
        ctx: &OpContext<'_>,
    ) -> CtlResult<BaseRefreshReport> {
        let name = params.name.unwrap_or_else(|| {
            let leaf = Path::new(&params.repo)
                .file_name()
                .map_or_else(|| "repo".into(), |n| n.to_string_lossy().into_owned());
            format!("{leaf}-base")
        });
        validate_snapshot_name(&name)?;
        self.work("build", Unit::Items, ctx)?;
        let mut st = self.lock();
        let previous_commit = st
            .snapshots
            .get(&name)
            .and_then(|s| s.base.as_ref())
            .and_then(|b| b.commit.clone());
        let snapshot = st.insert(SnapshotInfo {
            name,
            parent: None,
            base: Some(BaseMeta {
                commit: Some(format!("stub-{}", params.git_ref)),
                repo: Some(params.repo),
                git_ref: Some(params.git_ref),
            }),
            created_unix_ms: now_ms(),
        });
        Ok(BaseRefreshReport {
            snapshot,
            previous_commit,
        })
    }

    fn holders(&self, snapshot: &str) -> CtlResult<Vec<ProcessInfo>> {
        let st = self.lock();
        if !st.snapshots.contains_key(snapshot) && !st.processes.contains_key(snapshot) {
            return Err(missing(snapshot));
        }
        Ok(st.processes.get(snapshot).cloned().unwrap_or_default())
    }

    fn mount_info(&self) -> CtlResult<MountInfo> {
        Ok(MountInfo {
            mount_path: self.mount_path.clone(),
            adapter: "stub".into(),
            mounted: false,
        })
    }
}

#[cfg(test)]
mod conformance_tests {
    use super::*;
    use crate::conformance::handler_conformance;
    use crate::CancelToken;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    fn holder_pid() -> u32 {
        0xC0FFEE
    }

    #[test]
    fn the_stub_passes_the_handler_conformance() {
        let h = Arc::new(StubHandler::new("/s", "/m"));
        let held: Arc<Mutex<()>> = Arc::new(Mutex::new(()));
        let h2 = Arc::clone(&h);
        let held2 = Arc::clone(&held);
        let inject: Arc<crate::conformance::AddHolder> = Arc::new(move |name: &str| {
            h2.add_holder(
                name,
                ProcessInfo {
                    pid: holder_pid(),
                    command: "conformance".into(),
                    holds: Vec::new(),
                },
                &held2,
            )
        });
        handler_conformance(h as Arc<dyn ControlHandler>, inject).unwrap();
    }

    #[test]
    fn import_reports_unverified_when_the_source_changes_while_it_is_ingested() {
        let h = StubHandler::new("/s", "/m").with_work(2, Duration::ZERO);
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("a");
        std::fs::write(&source_path, b"before").unwrap();
        let touched = Arc::new(source_path.clone());
        let changed = Arc::new(AtomicBool::new(false));
        let once = Arc::clone(&changed);
        let mut ctx = OpContext::new(CancelToken::new(), move |_| {
            if !once.swap(true, Ordering::SeqCst) {
                std::fs::write(touched.as_path(), b"after!").unwrap();
            }
            true
        });
        let report = h
            .import(
                ImportParams {
                    path: dir.path().to_string_lossy().into_owned(),
                    name: "imp".into(),
                },
                &mut ctx,
            )
            .unwrap();
        assert!(!report.verified, "{report:?}");
        assert_ne!(report.source_root_hash, report.imported_root_hash);
        assert!(!report.mismatches.is_empty());
    }
}
