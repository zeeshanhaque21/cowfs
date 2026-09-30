use crate::error::{CtlError, CtlResult, ErrorCode};
use crate::types::*;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A shared cancellation flag. Cloning shares the flag.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A token that is not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels every clone of this token.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// True once `cancel` was called on any clone.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

type Sink<'a> = dyn Fn(ProgressEvent) -> bool + Send + Sync + 'a;

/// What a long-running handler method uses to report progress and to notice cancellation.
pub struct OpContext<'a> {
    token: CancelToken,
    sink: Box<Sink<'a>>,
}

impl fmt::Debug for OpContext<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpContext")
            .field("cancelled", &self.token.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl<'a> OpContext<'a> {
    /// Builds a context. `sink` delivers one event and returns false when the peer is gone.
    pub fn new(
        token: CancelToken,
        sink: impl Fn(ProgressEvent) -> bool + Send + Sync + 'a,
    ) -> Self {
        OpContext {
            token,
            sink: Box::new(sink),
        }
    }

    /// A context that is never cancelled and discards progress, for calling handlers directly.
    pub fn detached() -> OpContext<'static> {
        OpContext::new(CancelToken::new(), |_| true)
    }

    /// True when the client cancelled or the connection died.
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    /// Returns the `cancelled` error when the request was cancelled, so `?` stops the work.
    pub fn check(&self) -> CtlResult<()> {
        if self.is_cancelled() {
            Err(CtlError::cancelled())
        } else {
            Ok(())
        }
    }

    /// Reports progress. Fails with `cancelled` when the request was cancelled or the client is gone.
    pub fn progress(&self, event: ProgressEvent) -> CtlResult<()> {
        self.check()?;
        if (self.sink)(event) {
            Ok(())
        } else {
            self.token.cancel();
            Err(CtlError::cancelled())
        }
    }
}

fn unsupported<T>(method: &str) -> CtlResult<T> {
    Err(CtlError::new(
        ErrorCode::Unsupported,
        format!("this backend does not implement {method}"),
    ))
}

/// The operations a cowfs daemon implements. `ping` and `version` are answered by the framework.
///
/// Called concurrently, one thread per in-flight request. Snapshot names are validated by the
/// framework before the call. Long operations must check `ctx` and stop early on cancellation,
/// leaving the store consistent. Every method defaults to `unsupported`, so a backend implements
/// what it has.
pub trait ControlHandler: Send + Sync {
    /// Store and mount paths, counts and uptime.
    fn status(&self) -> CtlResult<Status> {
        unsupported("status")
    }

    /// Every snapshot.
    fn snapshot_list(&self) -> CtlResult<Vec<SnapshotInfo>> {
        unsupported("snapshot_list")
    }

    /// Creates a snapshot, an O(1) clone of `from` or the empty tree.
    fn snapshot_create(&self, params: SnapshotCreate) -> CtlResult<SnapshotInfo> {
        let _ = params;
        unsupported("snapshot_create")
    }

    /// Removes a snapshot. `busy` if a process is using it.
    fn snapshot_rm(&self, name: &str) -> CtlResult<()> {
        let _ = name;
        unsupported("snapshot_rm")
    }

    /// Renames a snapshot.
    fn snapshot_rename(&self, from: &str, to: &str) -> CtlResult<SnapshotInfo> {
        let _ = (from, to);
        unsupported("snapshot_rename")
    }

    /// Turns a clone into a base. Idempotent.
    fn snapshot_promote(&self, name: &str) -> CtlResult<SnapshotInfo> {
        let _ = name;
        unsupported("snapshot_promote")
    }

    /// Mark and sweep. Streams progress.
    fn gc(&self, params: GcParams, ctx: &OpContext<'_>) -> CtlResult<GcReport> {
        let _ = (params, ctx);
        unsupported("gc")
    }

    /// Verifies every block and snapshot. Streams progress.
    fn fsck(&self, ctx: &OpContext<'_>) -> CtlResult<FsckReport> {
        let _ = ctx;
        unsupported("fsck")
    }

    /// Ingests a directory into a new snapshot and verifies it by hash. Streams progress.
    fn import(&self, params: ImportParams, ctx: &OpContext<'_>) -> CtlResult<ImportReport> {
        let _ = (params, ctx);
        unsupported("import")
    }

    /// Builds or refreshes the warm base for a repository at a ref. Streams progress.
    fn base_refresh(
        &self,
        params: BaseRefreshParams,
        ctx: &OpContext<'_>,
    ) -> CtlResult<BaseRefreshReport> {
        let _ = (params, ctx);
        unsupported("base_refresh")
    }

    /// Processes holding the snapshot directory as cwd, open file or lock.
    fn ps(&self, snapshot: &str) -> CtlResult<Vec<ProcessInfo>> {
        let _ = snapshot;
        unsupported("ps")
    }

    /// Where and how the store is mounted.
    fn mount_info(&self) -> CtlResult<MountInfo> {
        unsupported("mount_info")
    }

    /// Called after a `shutdown` request, before the response is sent. The framework stops the
    /// server afterwards.
    fn shutdown(&self) -> CtlResult<()> {
        Ok(())
    }
}
