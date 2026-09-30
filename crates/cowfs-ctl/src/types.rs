use serde::{Deserialize, Serialize};

/// An object with no fields, used where a request or response carries no data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Empty {}

/// Parameters of `snapshot_create`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotCreate {
    pub name: String,
    /// Clone this snapshot. `None` creates a snapshot of the empty tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// Parameters of `snapshot_rm` and `snapshot_promote`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotName {
    pub name: String,
}

/// Parameters of `snapshot_rename`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRename {
    pub from: String,
    pub to: String,
}

/// Parameters of `gc`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcParams {
    #[serde(default)]
    pub dry_run: bool,
}

/// Parameters of `import`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportParams {
    pub path: String,
    pub name: String,
}

/// Parameters of `base_refresh`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseRefreshParams {
    pub repo: String,
    pub git_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Parameters of `ps`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PsParams {
    pub snapshot: String,
}

/// Where a base snapshot came from. Every field is null for a promoted clone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseMeta {
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
}

/// One snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotInfo {
    pub name: String,
    #[serde(default)]
    pub parent: Option<String>,
    /// Set when the snapshot is a base.
    #[serde(default)]
    pub base: Option<BaseMeta>,
    pub created_unix_ms: u64,
}

/// Result of `version`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionInfo {
    pub protocol: u32,
    pub server: String,
    pub ctl: String,
}

/// Result of `status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub store_path: String,
    pub mount_path: String,
    pub snapshot_count: u64,
    pub block_count: u64,
    pub logical_bytes: u64,
    pub stored_bytes: u64,
    pub uptime_secs: u64,
}

/// Result of `snapshot_list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotList {
    pub snapshots: Vec<SnapshotInfo>,
}

/// Result of `gc`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcReport {
    pub dry_run: bool,
    pub candidate_blocks: u64,
    pub candidate_bytes: u64,
    pub freed_blocks: u64,
    pub freed_bytes: u64,
}

/// One problem found by `fsck`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsckProblem {
    pub kind: String,
    pub detail: String,
}

/// Result of `fsck`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsckReport {
    pub ok: bool,
    pub blocks_checked: u64,
    pub bytes_checked: u64,
    pub snapshots_checked: u64,
    pub problems: Vec<FsckProblem>,
}

/// Result of `import`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    pub name: String,
    pub files: u64,
    pub bytes: u64,
    /// True only when every ingested file was re-read and matched its hash.
    pub verified: bool,
}

/// Result of `base_refresh`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseRefreshReport {
    pub snapshot: SnapshotInfo,
    #[serde(default)]
    pub previous_commit: Option<String>,
}

/// Why a process appears in `ps`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldKind {
    Cwd,
    Fd,
    Lock,
    #[serde(other)]
    Other,
}

/// One hold a process has on the snapshot directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    pub kind: HoldKind,
    pub path: String,
}

/// One process holding a snapshot directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub command: String,
    pub holds: Vec<Hold>,
}

/// Result of `ps`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessList {
    pub processes: Vec<ProcessInfo>,
}

/// Result of `mount_info`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountInfo {
    pub mount_path: String,
    pub adapter: String,
    pub mounted: bool,
}

/// What a progress event counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Bytes,
    Items,
    #[serde(other)]
    Other,
}

/// A progress event of a long operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub phase: String,
    pub done: u64,
    #[serde(default)]
    pub total: Option<u64>,
    pub unit: Unit,
    #[serde(default)]
    pub message: Option<String>,
}

/// A request: `{"method": ..., "params": {...}}` on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    Ping(Empty),
    Version(Empty),
    Status(Empty),
    SnapshotList(Empty),
    SnapshotCreate(SnapshotCreate),
    SnapshotRm(SnapshotName),
    SnapshotRename(SnapshotRename),
    SnapshotPromote(SnapshotName),
    Gc(GcParams),
    Fsck(Empty),
    Import(ImportParams),
    BaseRefresh(BaseRefreshParams),
    Ps(PsParams),
    MountInfo(Empty),
    Shutdown(Empty),
}

/// Every method name the server understands, in `hello.methods` order.
pub const METHODS: &[&str] = &[
    "ping",
    "version",
    "status",
    "snapshot_list",
    "snapshot_create",
    "snapshot_rm",
    "snapshot_rename",
    "snapshot_promote",
    "gc",
    "fsck",
    "import",
    "base_refresh",
    "ps",
    "mount_info",
    "shutdown",
];

/// A successful result: `{"kind": ..., "data": {...}}` on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Response {
    Pong(Empty),
    Version(VersionInfo),
    Status(Status),
    SnapshotList(SnapshotList),
    Snapshot(SnapshotInfo),
    Ok(Empty),
    Gc(GcReport),
    Fsck(FsckReport),
    Import(ImportReport),
    BaseRefresh(BaseRefreshReport),
    Processes(ProcessList),
    MountInfo(MountInfo),
}
