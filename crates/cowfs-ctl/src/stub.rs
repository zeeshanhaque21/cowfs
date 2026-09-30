use crate::error::{CtlError, CtlResult, ErrorCode};
use crate::handler::{ControlHandler, OpContext};
use crate::types::*;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A `ControlHandler` that keeps snapshots in memory and simulates the long operations.
///
/// It backs the tests and `cowfs serve --stub`. Long operations emit `steps` progress events,
/// sleeping `delay` between them and stopping early when cancelled.
#[derive(Debug)]
pub struct StubHandler {
    store_path: String,
    mount_path: String,
    steps: u64,
    delay: Duration,
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

    /// Makes `ps` report `process` as holding `snapshot`, which also makes `snapshot_rm` fail
    /// with `busy`. The snapshot need not exist yet.
    pub fn with_process(self, snapshot: &str, process: ProcessInfo) -> Self {
        self.lock()
            .processes
            .entry(snapshot.to_owned())
            .or_default()
            .push(process);
        self
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn work(&self, phase: &str, unit: Unit, ctx: &OpContext<'_>) -> CtlResult<()> {
        for done in 0..=self.steps {
            ctx.progress(ProgressEvent {
                phase: phase.to_owned(),
                done,
                total: Some(self.steps),
                unit,
                message: None,
            })?;
            thread::sleep(self.delay);
        }
        Ok(())
    }

    fn insert(&self, info: SnapshotInfo) -> SnapshotInfo {
        let mut st = self.lock();
        st.blocks += 1;
        st.bytes += STUB_BLOCK_BYTES;
        st.snapshots.insert(info.name.clone(), info.clone());
        info
    }
}

fn exists(name: &str) -> CtlError {
    CtlError::new(
        ErrorCode::AlreadyExists,
        format!("snapshot {name:?} already exists"),
    )
}

fn missing(name: &str) -> CtlError {
    CtlError::not_found(format!("snapshot {name:?} does not exist"))
}

fn tree_size(dir: &Path) -> std::io::Result<(u64, u64)> {
    let (mut files, mut bytes) = (0, 0);
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let md = entry.metadata()?;
        if md.is_dir() {
            let (f, b) = tree_size(&entry.path())?;
            files += f;
            bytes += b;
        } else {
            files += 1;
            bytes += md.len();
        }
    }
    Ok((files, bytes))
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
        {
            let st = self.lock();
            if st.snapshots.contains_key(&params.name) {
                return Err(exists(&params.name));
            }
            if let Some(from) = &params.from {
                if !st.snapshots.contains_key(from) {
                    return Err(missing(from));
                }
            }
        }
        Ok(self.insert(SnapshotInfo {
            name: params.name,
            parent: params.from,
            base: None,
            created_unix_ms: now_ms(),
        }))
    }

    fn snapshot_rm(&self, name: &str) -> CtlResult<()> {
        let mut st = self.lock();
        if !st.snapshots.contains_key(name) {
            return Err(missing(name));
        }
        if st.processes.get(name).is_some_and(|p| !p.is_empty()) {
            return Err(CtlError::new(
                ErrorCode::Busy,
                format!("snapshot {name:?} is in use by a process"),
            ));
        }
        st.snapshots.remove(name);
        Ok(())
    }

    fn snapshot_rename(&self, from: &str, to: &str) -> CtlResult<SnapshotInfo> {
        let mut st = self.lock();
        if !st.snapshots.contains_key(from) {
            return Err(missing(from));
        }
        if st.snapshots.contains_key(to) {
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
        if self.lock().snapshots.contains_key(&params.name) {
            return Err(exists(&params.name));
        }
        let dir = Path::new(&params.path);
        if !dir.is_dir() {
            return Err(CtlError::not_found(format!(
                "{} is not a directory",
                params.path
            )));
        }
        let (files, bytes) =
            tree_size(dir).map_err(|e| CtlError::new(ErrorCode::IoError, e.to_string()))?;
        self.work("ingest", Unit::Bytes, ctx)?;
        self.work("verify", Unit::Bytes, ctx)?;
        self.insert(SnapshotInfo {
            name: params.name.clone(),
            parent: None,
            base: None,
            created_unix_ms: now_ms(),
        });
        Ok(ImportReport {
            name: params.name,
            files,
            bytes,
            verified: true,
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
        self.work("build", Unit::Items, ctx)?;
        let previous_commit = self
            .lock()
            .snapshots
            .get(&name)
            .and_then(|s| s.base.as_ref())
            .and_then(|b| b.commit.clone());
        let snapshot = self.insert(SnapshotInfo {
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

    fn ps(&self, snapshot: &str) -> CtlResult<Vec<ProcessInfo>> {
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
