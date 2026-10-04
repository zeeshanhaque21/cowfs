use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An object with no fields, used where a request or response carries no data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Empty {}

/// Parameters that must be exactly `{}`: unknown fields are rejected, for requests where a
/// misspelt option must not be silently dropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoParams {}

/// Parameters of `snapshot_create`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotCreate {
    pub name: String,
    /// Clone this snapshot. `None` creates a snapshot of the empty tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// Parameters of `snapshot_promote`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotName {
    pub name: String,
}

fn yes() -> bool {
    true
}

/// Parameters of `snapshot_rm`. Unknown fields are rejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRm {
    pub name: String,
    /// Fail with `busy` when a holder exists, checked under the same lock as the removal.
    #[serde(default = "yes")]
    pub expect_no_holders: bool,
}

/// Parameters of `snapshot_reset`. Unknown fields are rejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotReset {
    /// The snapshot to replace.
    pub name: String,
    /// The snapshot to clone into its place.
    pub from: String,
    /// Fail with `busy` when a holder exists, checked under the same lock as the swap.
    #[serde(default = "yes")]
    pub expect_no_holders: bool,
}

/// Parameters of `snapshot_rename`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRename {
    pub from: String,
    pub to: String,
}

/// Parameters of `gc`. `dry_run` is required: a real run is never the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GcParams {
    pub dry_run: bool,
}

/// Parameters of `import`. Unknown fields are rejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportParams {
    pub path: String,
    pub name: String,
}

/// Parameters of `base_refresh`. Unknown fields are rejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// Parameters of `mount_snapshot`. Unknown fields are rejected: a misspelled `path` must not
/// silently become the default export root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MountSnapshot {
    /// The snapshot to export.
    pub name: String,
    /// Where to export it. Absolute; the daemon refuses a path outside its export roots.
    pub path: String,
    /// Fail with `busy` when a holder exists, checked under the same lock as the export.
    #[serde(default = "yes")]
    pub expect_no_holders: bool,
}

/// Parameters of `unmount_snapshot`. Unknown fields are rejected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnmountSnapshot {
    /// The path the daemon exported. One the daemon did not export is `not_found`.
    pub path: String,
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
///
/// `freed_bytes` is the **gross** file length of every pack the cycle unlinked, kept under its
/// original name for compatibility. It is not the space saved: surviving live records are
/// rewritten into a new pack. `gross_removed_bytes` is the same number under an explicit gross
/// name.
///
/// `rewrite_bytes` and `net_reclaimed_bytes` are `Some` only when the report carries the post-#81
/// fields. A report from before those fields has no rewrite figure at all, so its net is
/// **unknown**, not zero: a legacy cycle may well have rewritten a pack, and deriving
/// `net = gross` or `net = gross - 0` would report a saving that was never measured. `None` is
/// that unknown, and the human output says so rather than printing a false zero.
///
/// A dry run reports the estimate in `candidate_bytes` and zero actual gross, rewrite and net.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "GcReportWire")]
pub struct GcReport {
    pub dry_run: bool,
    pub candidate_blocks: u64,
    pub candidate_bytes: u64,
    pub freed_blocks: u64,
    pub freed_bytes: u64,
    /// Bytes removed by unlinking packs, explicit gross name. Equal to `freed_bytes`.
    /// A payload from before #81 has no such field, so it falls back to `freed_bytes` rather
    /// than zero, which would read as "nothing removed".
    pub gross_removed_bytes: u64,
    /// Bytes written into the packs this cycle created, file headers included.
    /// `None` when the report predates the field, so the figure is unknown.
    pub rewrite_bytes: Option<u64>,
    /// Net space reclaimed: `gross_removed_bytes - rewrite_bytes`, signed.
    /// `None` when the report predates the field, so the figure is unknown.
    pub net_reclaimed_bytes: Option<i64>,
}

/// The wire shape of [`GcReport`], where the post-#81 fields are optional together.
///
/// The three fields are one unit: present means a new report, absent means a legacy one.
/// A payload that carries some but not all of them is inconsistent and is rejected, so a
/// half-written report can never default into a wrong number.
#[derive(Deserialize)]
struct GcReportWire {
    dry_run: bool,
    candidate_blocks: u64,
    candidate_bytes: u64,
    freed_blocks: u64,
    freed_bytes: u64,
    #[serde(default)]
    gross_removed_bytes: Option<u64>,
    #[serde(default)]
    rewrite_bytes: Option<u64>,
    #[serde(default)]
    net_reclaimed_bytes: Option<i64>,
}

impl TryFrom<GcReportWire> for GcReport {
    type Error = String;

    fn try_from(w: GcReportWire) -> Result<Self, Self::Error> {
        let GcReportWire {
            dry_run,
            candidate_blocks,
            candidate_bytes,
            freed_blocks,
            freed_bytes,
            gross_removed_bytes,
            rewrite_bytes,
            net_reclaimed_bytes,
        } = w;
        let (gross_removed_bytes, rewrite_bytes, net_reclaimed_bytes) =
            match (gross_removed_bytes, rewrite_bytes, net_reclaimed_bytes) {
                // A legacy report: gross is `freed_bytes`, net is unknown.
                (None, None, None) => (freed_bytes, None, None),
                // A new report: the explicit fields must be internally consistent.
                (Some(gross), Some(rewrite), Some(net)) => {
                    check_new_fields(freed_bytes, gross, rewrite, net)?;
                    (gross, Some(rewrite), Some(net))
                }
                // A partially present report cannot be repaired without guessing.
                _ => return Err(PARTIAL_FIELDS.to_owned()),
            };
        Ok(GcReport {
            dry_run,
            candidate_blocks,
            candidate_bytes,
            freed_blocks,
            freed_bytes,
            gross_removed_bytes,
            rewrite_bytes,
            net_reclaimed_bytes,
        })
    }
}

const PARTIAL_FIELDS: &str = "gc report carries only some of gross_removed_bytes, \
     rewrite_bytes, net_reclaimed_bytes; they are present together or absent together";

/// Check a report that claims the post-#81 fields: the explicit gross must agree with the legacy
/// `freed_bytes`, and the net must be gross minus rewrite exactly.
fn check_new_fields(freed_bytes: u64, gross: u64, rewrite: u64, net: i64) -> Result<(), String> {
    if gross != freed_bytes {
        return Err(format!(
            "gc report gross {gross} disagrees with freed_bytes {freed_bytes}"
        ));
    }
    let expected = i128::from(gross) - i128::from(rewrite);
    match i64::try_from(expected) {
        Ok(v) if v == net => Ok(()),
        Ok(_) => Err(format!(
            "gc report net {net} disagrees with gross {gross} minus rewrite {rewrite}"
        )),
        Err(_) => Err(format!(
            "gc report gross {gross} minus rewrite {rewrite} is out of i64 range"
        )),
    }
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

/// Most mismatches an `ImportReport` lists.
pub const MAX_MISMATCHES: usize = 100;

/// One difference between the source tree and the imported tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportMismatch {
    /// Path relative to the source root.
    pub path: String,
    /// What differs, for example `content`, `mode`, `missing` or `extra`.
    pub reason: String,
}

/// Result of `import`. The two root hashes let the caller check independently (see
/// `hash_tree`) before it swaps the directory for a mount.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportReport {
    pub name: String,
    pub files: u64,
    pub bytes: u64,
    /// True only when the source was re-read after ingest and the root hashes match.
    pub verified: bool,
    /// Always `blake3` in protocol version 1.
    pub hash_algorithm: String,
    /// Root hash of the source directory, hex.
    pub source_root_hash: String,
    /// Root hash of the imported snapshot, hex.
    pub imported_root_hash: String,
    /// Up to `MAX_MISMATCHES` differences. Empty when verified.
    pub mismatches: Vec<ImportMismatch>,
    /// True when there were more mismatches than listed.
    pub mismatches_truncated: bool,
    /// Bytes the store actually took for this import, after compression and deduplication, so a
    /// caller can see what the content cost against `bytes`. Absent when the backend has no block
    /// store, which is the passthrough backend and the stub.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stored_bytes: Option<u64>,
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
    SnapshotRm(SnapshotRm),
    SnapshotReset(SnapshotReset),
    SnapshotRename(SnapshotRename),
    SnapshotPromote(SnapshotName),
    Gc(GcParams),
    Fsck(Empty),
    Import(ImportParams),
    BaseRefresh(BaseRefreshParams),
    Ps(PsParams),
    MountInfo(Empty),
    MountSnapshot(MountSnapshot),
    UnmountSnapshot(UnmountSnapshot),
    Shutdown(NoParams),
}

/// Every method name the server understands, in `hello.methods` order.
pub const METHODS: &[&str] = &[
    "ping",
    "version",
    "status",
    "snapshot_list",
    "snapshot_create",
    "snapshot_rm",
    "snapshot_reset",
    "snapshot_rename",
    "snapshot_promote",
    "gc",
    "fsck",
    "import",
    "base_refresh",
    "ps",
    "mount_info",
    "mount_snapshot",
    "unmount_snapshot",
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
    /// A result kind from a newer server. Only ever produced by decoding, never sent.
    #[serde(skip)]
    Unknown {
        /// The `kind` string.
        kind: String,
        /// The raw `data` value.
        data: Value,
    },
}

/// Every response kind this version defines.
pub const RESPONSE_KINDS: &[&str] = &[
    "pong",
    "version",
    "status",
    "snapshot_list",
    "snapshot",
    "ok",
    "gc",
    "fsck",
    "import",
    "base_refresh",
    "processes",
    "mount_info",
];

impl Request {
    /// The wire method name.
    pub fn method(&self) -> &'static str {
        match self {
            Request::Ping(_) => "ping",
            Request::Version(_) => "version",
            Request::Status(_) => "status",
            Request::SnapshotList(_) => "snapshot_list",
            Request::SnapshotCreate(_) => "snapshot_create",
            Request::SnapshotRm(_) => "snapshot_rm",
            Request::SnapshotReset(_) => "snapshot_reset",
            Request::SnapshotRename(_) => "snapshot_rename",
            Request::SnapshotPromote(_) => "snapshot_promote",
            Request::Gc(_) => "gc",
            Request::Fsck(_) => "fsck",
            Request::Import(_) => "import",
            Request::BaseRefresh(_) => "base_refresh",
            Request::Ps(_) => "ps",
            Request::MountInfo(_) => "mount_info",
            Request::MountSnapshot(_) => "mount_snapshot",
            Request::UnmountSnapshot(_) => "unmount_snapshot",
            Request::Shutdown(_) => "shutdown",
        }
    }
}

impl Response {
    /// The wire `kind` string.
    pub fn kind(&self) -> &str {
        match self {
            Response::Pong(_) => "pong",
            Response::Version(_) => "version",
            Response::Status(_) => "status",
            Response::SnapshotList(_) => "snapshot_list",
            Response::Snapshot(_) => "snapshot",
            Response::Ok(_) => "ok",
            Response::Gc(_) => "gc",
            Response::Fsck(_) => "fsck",
            Response::Import(_) => "import",
            Response::BaseRefresh(_) => "base_refresh",
            Response::Processes(_) => "processes",
            Response::MountInfo(_) => "mount_info",
            Response::Unknown { kind, .. } => kind,
        }
    }

    /// The `data` object as JSON, including for kinds this version does not know.
    pub fn data_json(&self) -> Value {
        match self {
            Response::Unknown { data, .. } => data.clone(),
            known => serde_json::to_value(known)
                .ok()
                .and_then(|mut v| v.get_mut("data").map(Value::take))
                .unwrap_or(Value::Null),
        }
    }
}
