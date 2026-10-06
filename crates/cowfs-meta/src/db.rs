//! Database handle, snapshots, and the commit pipeline.
//!
//! Every mutation is applied in memory first (`Session`), where it is visible to every reader at
//! once. The only way state reaches redb is a durable commit, and every durable commit is
//! preceded by the `before_sync` hook, which runs after all closures that contributed to the
//! commit have returned. See `docs/v1-meta.md`.

use crate::error::guard;
use crate::node::{Node, NodeId};
use crate::ptree::{Lazy, MemTree, NodeCache, NodeWriter};
use crate::read::{self, View};
use crate::tx::{InoAlloc, Tx};
use crate::types::*;
use crate::walk::{LiveBlocks, Marker};
use crate::{Error, Result};
use cowfs_store::ChunkRef;
use redb::{
    Builder, Database, Durability, ReadableDatabase, ReadableTable, ReadableTableMetadata,
    StorageBackend, TableDefinition,
};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub(crate) const NODES: TableDefinition<[u8; 32], &[u8]> = TableDefinition::new("nodes");
pub(crate) const REFS: TableDefinition<[u8; 32], u64> = TableDefinition::new("refs");
pub(crate) const SNAPSHOTS: TableDefinition<u64, &[u8]> = TableDefinition::new("snapshots");
pub(crate) const SNAP_NAMES: TableDefinition<&str, u64> = TableDefinition::new("snap_names");
pub(crate) const META: TableDefinition<&str, u64> = TableDefinition::new("meta");

/// The durable floor a pending reservation is about to move `ino_reserved` to.
///
/// Written by its own commit ahead of the one that moves the floor, so redb discarding the newest
/// commit still leaves the bound behind for `record_recovery` to skip to. Absent whenever no
/// reservation is in flight, and absent entirely in files written before it existed.
const INO_INTENT: &str = "ino_reserved_intent";
pub(crate) const REAP: TableDefinition<u64, [u8; 32]> = TableDefinition::new("reap");

/// Distinguishes one open store from another within this process, so a reservation ticket minted
/// against one store is refused by another even when the numeric inode is the same.
static NEXT_STORE_ID: AtomicU64 = AtomicU64::new(1);

fn next_store_id() -> u64 {
    NEXT_STORE_ID.fetch_add(1, SeqCst)
}

/// "COWFSMET": identifies a cowfs-meta database among redb files.
pub(crate) const MAGIC: u64 = 0x434f_5746_534d_4554;
pub(crate) const FORMAT_VERSION: u64 = 2;
const REAP_BUDGET: usize = 256;
const REAP_DURABLE_EVERY: u64 = 16;
const REAP_TIME: Duration = Duration::from_millis(4);
const GROUP_WAIT: Duration = Duration::from_millis(2);

/// Counts callers that are inside `mutate`, so a commit leader can wait for them.
struct Inflight<'a>(&'a AtomicUsize);

impl<'a> Inflight<'a> {
    fn enter(n: &'a AtomicUsize) -> Self {
        n.fetch_add(1, SeqCst);
        Self(n)
    }
}

impl Drop for Inflight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, SeqCst);
    }
}

/// Hook run before every durable commit; the mount layer sets it to the block store's `sync`.
pub type SyncHook = Arc<dyn Fn() -> io::Result<()> + Send + Sync>;

/// When a mutating call returns.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Ack {
    /// After the change is applied and visible to every reader. It becomes durable by the sync
    /// policy, `sync()` or `close()`.
    #[default]
    Applied,
    /// After the change is durable. Concurrent callers share one commit and one `before_sync`.
    Durable,
}

/// Tuning knobs for [`Meta::open`].
#[derive(Clone)]
pub struct Options {
    /// Target encoded size of a tree node in bytes. Fixed when the database is created.
    pub node_size: usize,
    /// A durable commit happens after this many applied mutating calls (a batch counts as one).
    pub sync_every_ops: u32,
    /// A durable commit happens at the latest this long after the oldest unsynced mutation.
    /// Enforced by a background thread unless `background` is false.
    pub sync_interval: Duration,
    /// Called before every durable commit, after every closure that contributed to it has
    /// returned, and by every `sync()` even when nothing is pending. If it fails, the commit does
    /// not happen. It must not call back into this crate (that returns `Error::Reentrant`).
    pub before_sync: Option<SyncHook>,
    /// redb page cache size in bytes.
    pub cache_size: usize,
    /// Verified tree nodes kept in memory (about `node_size` bytes each).
    pub node_cache: usize,
    /// When mutating calls return.
    pub ack: Ack,
    /// Run the timer/reaper thread. When false, the caller drives `sync()` and `reap_step()`.
    pub background: bool,
    /// A durable commit happens when this many bytes of changes are pending.
    pub max_pending_bytes: usize,
    /// Inode numbers are reserved durably in blocks of this size.
    ///
    /// Used only when the file is created. On reopen the stored block governs, exactly as the
    /// stored `node_size` does, because `open_recover` needs the block the reservations were
    /// actually written with. Changing it on an existing file has no effect; see
    /// [`Meta::open`].
    pub ino_block: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            node_size: 4096,
            sync_every_ops: 256,
            sync_interval: Duration::from_secs(1),
            before_sync: None,
            cache_size: 64 << 20,
            node_cache: 32768,
            ack: Ack::Applied,
            background: true,
            max_pending_bytes: 32 << 20,
            ino_block: 16384,
        }
    }
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("node_size", &self.node_size)
            .field("sync_every_ops", &self.sync_every_ops)
            .field("sync_interval", &self.sync_interval)
            .field("before_sync", &self.before_sync.is_some())
            .field("cache_size", &self.cache_size)
            .field("node_cache", &self.node_cache)
            .field("ack", &self.ack)
            .field("background", &self.background)
            .field("max_pending_bytes", &self.max_pending_bytes)
            .field("ino_block", &self.ino_block)
            .finish()
    }
}

thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

// Fault injection for the durable reservation path, for this crate's own unit tests only.
//
// `0` is off. `1` fails before the floor commit, `2` fails after it has persisted, and `3` fails
// before the bound commit. Thread-local because the reservation path runs under a process-wide
// write lock, so a process-wide fault would bleed into whichever other test happens to hold it.
#[cfg(test)]
thread_local! {
    static RESERVE_FAULT: Cell<u8> = const { Cell::new(0) };
}

// Durable reservation commits made on this thread, so a test can prove the count does not follow `n`.
#[cfg(test)]
thread_local! {
    static RESERVE_COMMITS: Cell<u32> = const { Cell::new(0) };
}

// Fault injection for the ordinary batch commit, this crate's own unit tests only.
//
// `0` is off. `1` fails just before `wtx.commit()`, so nothing is persisted; `2` fails just after
// it, so the edit is durable while the caller still sees an error. That second one is how a test
// proves the retry path does not treat a persisted create as a failed one. Thread-local for the
// same reason as `RESERVE_FAULT`.
#[cfg(test)]
thread_local! {
    static COMMIT_FAULT: Cell<u8> = const { Cell::new(0) };
}

#[cfg(test)]
fn commit_fault() -> u8 {
    COMMIT_FAULT.with(Cell::get)
}

#[cfg(test)]
fn reserve_fault() -> u8 {
    RESERVE_FAULT.with(Cell::get)
}

#[cfg(test)]
fn count_reserve_commit() {
    RESERVE_COMMITS.with(|c| c.set(c.get() + 1));
}

#[cfg(test)]
fn reserve_commits() -> u32 {
    RESERVE_COMMITS.with(Cell::get)
}

#[cfg(test)]
fn reset_reserve_probe() {
    RESERVE_FAULT.with(|c| c.set(0));
    RESERVE_COMMITS.with(|c| c.set(0));
    COMMIT_FAULT.with(|c| c.set(0));
}

#[cfg(test)]
fn set_commit_fault(v: u8) {
    COMMIT_FAULT.with(|c| c.set(v));
}

struct HookScope;

impl HookScope {
    fn enter() -> Self {
        IN_HOOK.with(|c| c.set(true));
        HookScope
    }
}

impl Drop for HookScope {
    fn drop(&mut self) {
        IN_HOOK.with(|c| c.set(false));
    }
}

fn reentered() -> bool {
    IN_HOOK.with(Cell::get)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Owns the redb handle so a panic in redb's close-time commit on a damaged file cannot escape `drop`.
pub(crate) struct Db(Option<Database>);

impl std::ops::Deref for Db {
    type Target = Database;

    fn deref(&self) -> &Database {
        match &self.0 {
            Some(db) => db,
            None => unreachable!("database used after drop"),
        }
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        if let Some(db) = self.0.take() {
            let _ = catch_unwind(AssertUnwindSafe(move || drop(db)));
        }
    }
}

pub(crate) struct SnapEntry {
    pub(crate) info: SnapshotInfo,
    pub(crate) tree: MemTree,
}

pub(crate) struct Session {
    pub(crate) snaps: BTreeMap<SnapshotId, SnapEntry>,
    pub(crate) names: HashMap<String, SnapshotId>,
    pub(crate) ino: InoAlloc,
    /// Numbers this session minted a [`ReservedIno`](crate::ReservedIno) for and that no create
    /// has spent yet. Minting a ticket adds its number here; a selected create removes it in the
    /// same write lock, so a number is spent at most once and a ticket from another store or a
    /// closed session is refused because it is not in this set.
    pub(crate) reserved: std::collections::HashSet<Ino>,
    pub(crate) next_snapshot: u64,
    applied: u64,
    durable: u64,
    pending_ops: u32,
    pending_bytes: usize,
    pending_since: Option<Instant>,
    flush_err: Option<String>,
    closed: bool,
}

struct BgState {
    stop: bool,
    deadline: Option<Instant>,
    reap: bool,
}

struct Bg {
    m: Mutex<BgState>,
    cv: Condvar,
}

pub(crate) struct Inner {
    pub(crate) db: Db,
    pub(crate) opts: Options,
    pub(crate) node_max: usize,
    pub(crate) cache: Arc<NodeCache>,
    pub(crate) session: RwLock<Session>,
    /// Identity of this open store, unique per process. A [`ReservedIno`](crate::ReservedIno)
    /// carries it, so a ticket minted against another store is refused even when its number is
    /// numerically the same as one this store reserved.
    pub(crate) store_id: u64,
    durable_seq: AtomicU64,
    gc: Mutex<bool>,
    gc_cv: Condvar,
    bg: Bg,
    poisoned: AtomicBool,
    inflight: AtomicUsize,
    reap_len: AtomicU64,
    reap_steps: AtomicU64,
    /// Why the last durable commit failed. Sticky, so a later success does not hide it.
    last_error: Mutex<Option<String>>,
    flush_failures: AtomicU64,
    consecutive_flush_failures: AtomicU64,
    bg_panics: AtomicU64,
    /// Read from the file, so a rolled-back store keeps reporting how often it was rolled back.
    recoveries: AtomicU64,
    /// The reservation block this file was created with, or `None` for a file written before the
    /// block was persisted. `record_recovery` refuses in that case, because the size of the lost
    /// reservation cannot be proven.
    ino_block: Option<u64>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner").finish_non_exhaustive()
    }
}

/// What [`Meta::health`] reports: background failures and recovery accounting.
///
/// A store that never failed reports `None` and zeros. Every counter only ever grows, so a
/// caller can compare two samples and tell progress from a stall.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Health {
    /// Why the last durable commit failed, of any kind. Sticky: a later success does not clear
    /// it, so a recovered stall stays visible.
    pub last_flush_error: Option<String>,
    /// Durable commits that failed since this handle opened, from any path: the background flush
    /// timer, a background reap step, an inline commit under `Ack::Applied`, `sync`, `close` and
    /// the durable wait.
    pub flush_failures: u64,
    /// Those in a row since the last one that worked. Reset to 0 by any successful commit.
    ///
    /// This counts; it does not by itself refuse anything. The refusal in `mutate` is gated on the
    /// session's own `flush_err`, which this does not set, so read it as a signal to look at
    /// `last_flush_error` rather than as a gate that has already tripped.
    pub consecutive_flush_failures: u64,
    /// Times a background job panicked, the flush timer or a reap step. The store keeps running
    /// and retries instead of losing the thread silently.
    pub background_panics: u64,
    /// True once the store has seen corruption; it refuses writes until it is reopened.
    pub poisoned: bool,
    /// Times this file has been rolled back by [`Meta::open_recover`]. Durable, so a plain
    /// reopen still reports it. 0 on a store that never lost a commit.
    pub recoveries: u64,
    /// Durable floor for inode numbers. Nothing at or above this has been handed out.
    pub ino_floor: u64,
    /// Durable floor for snapshot ids.
    pub snapshot_floor: u64,
}

type Committed = (Vec<(SnapshotId, NodeId)>, Option<(SnapshotInfo, NodeId)>);

enum Extra<'a> {
    None,
    Add {
        name: &'a str,
        from: Option<SnapshotId>,
    },
    Remove(SnapshotId),
    Rename {
        id: SnapshotId,
        name: &'a str,
    },
}

fn corrupt(what: &str) -> Error {
    Error::Corrupt(what.to_string())
}

pub(crate) fn encode_snap(s: &SnapshotInfo) -> Vec<u8> {
    let mut b = Vec::with_capacity(2 + s.name.len() + 32 + 12 + 8);
    b.extend((s.name.len() as u16).to_le_bytes());
    b.extend_from_slice(s.name.as_bytes());
    b.extend_from_slice(s.root.as_bytes());
    b.extend(s.created.secs.to_le_bytes());
    b.extend(s.created.nanos.to_le_bytes());
    b.extend(s.parent.map_or(0, |p| p.0).to_le_bytes());
    b
}

pub(crate) fn decode_snap(id: u64, b: &[u8]) -> Result<SnapshotInfo> {
    let bad = || corrupt("bad snapshot row");
    let nl = usize::from(u16::from_le_bytes(
        b.get(..2).and_then(|s| s.try_into().ok()).ok_or_else(bad)?,
    ));
    let rest = b.get(2 + nl..).ok_or_else(bad)?;
    if rest.len() != 32 + 12 + 8 {
        return Err(bad());
    }
    let name = std::str::from_utf8(&b[2..2 + nl])
        .map_err(|_| bad())?
        .to_string();
    let arr = |r: std::ops::Range<usize>| rest.get(r).ok_or_else(bad);
    let root = NodeId::from_bytes(arr(0..32)?.try_into().map_err(|_| bad())?);
    let secs = i64::from_le_bytes(arr(32..40)?.try_into().map_err(|_| bad())?);
    let nanos = u32::from_le_bytes(arr(40..44)?.try_into().map_err(|_| bad())?);
    let parent = u64::from_le_bytes(arr(44..52)?.try_into().map_err(|_| bad())?);
    Ok(SnapshotInfo {
        id: SnapshotId(id),
        name,
        root,
        created: Timestamp { secs, nanos },
        parent: (parent != 0).then_some(SnapshotId(parent)),
    })
}

pub(crate) fn meta_get<T: ReadableTable<&'static str, u64>>(t: &T, k: &str) -> Result<u64> {
    t.get(k)?
        .map(|g| g.value())
        .ok_or_else(|| Error::Corrupt(format!("missing meta key {k}")))
}

impl Inner {
    fn rlock(&self) -> Result<RwLockReadGuard<'_, Session>> {
        if reentered() {
            return Err(Error::Reentrant);
        }
        Ok(self.session.read().unwrap_or_else(|e| e.into_inner()))
    }

    fn wlock(&self) -> Result<RwLockWriteGuard<'_, Session>> {
        if reentered() {
            return Err(Error::Reentrant);
        }
        Ok(self.session.write().unwrap_or_else(|e| e.into_inner()))
    }

    fn note(&self, e: &Error) {
        if matches!(e, Error::Corrupt(_)) {
            self.poisoned.store(true, SeqCst);
        }
    }

    /// A durable commit did not happen. Counted and remembered so the caller can be told, from
    /// anywhere, why its changes are not durable yet.
    fn note_flush_failure(&self, why: String) {
        self.flush_failures.fetch_add(1, SeqCst);
        self.consecutive_flush_failures.fetch_add(1, SeqCst);
        *lock(&self.last_error) = Some(why);
    }

    fn note_flush_ok(&self) {
        self.consecutive_flush_failures.store(0, SeqCst);
    }

    /// A background job panicked. Recorded rather than swallowed: without it the job stops and
    /// nothing says so.
    fn note_bg_panic(&self, what: &str) {
        self.bg_panics.fetch_add(1, SeqCst);
        self.note_flush_failure(format!("the {what} panicked and was retried"));
    }

    /// Re-arms the flush timer after a background failure so the pending window is retried.
    fn rearm_flush(&self) {
        if let Some(d) = Instant::now().checked_add(self.opts.sync_interval) {
            self.arm_timer(d);
        }
    }

    fn health(&self) -> Health {
        let s = self.session.read().unwrap_or_else(|e| e.into_inner());
        Health {
            last_flush_error: lock(&self.last_error).clone(),
            flush_failures: self.flush_failures.load(SeqCst),
            consecutive_flush_failures: self.consecutive_flush_failures.load(SeqCst),
            background_panics: self.bg_panics.load(SeqCst),
            poisoned: self.poisoned.load(SeqCst),
            recoveries: self.recoveries.load(SeqCst),
            ino_floor: s.ino.reserved,
            snapshot_floor: s.next_snapshot,
        }
    }

    fn check_writable(&self, s: &Session) -> Result<()> {
        if s.closed {
            return Err(Error::Closed);
        }
        if self.poisoned.load(SeqCst) {
            return Err(corrupt(
                "handle refuses writes after detecting corruption; reopen and run check()",
            ));
        }
        Ok(())
    }

    fn run_hook(&self) -> Result<()> {
        match &self.opts.before_sync {
            Some(h) => {
                let _scope = HookScope::enter();
                h().map_err(Error::Hook)
            }
            None => Ok(()),
        }
    }

    fn arm_timer(&self, deadline: Instant) {
        if !self.opts.background {
            return;
        }
        let mut st = lock(&self.bg.m);
        if st.deadline.is_none() {
            st.deadline = Some(deadline);
            self.bg.cv.notify_all();
        }
    }

    fn disarm_timer(&self) {
        lock(&self.bg.m).deadline = None;
    }

    /// One durable commit of every pending change, plus `extra`, preceded by the hook.
    ///
    /// Nothing changes in memory unless the commit succeeds.
    fn commit(
        &self,
        s: &mut Session,
        extra: Extra<'_>,
        closing: bool,
        force_hook: bool,
    ) -> Result<Option<SnapshotId>> {
        self.check_writable(s)?;
        let removed = match &extra {
            Extra::Remove(id) => Some(*id),
            _ => None,
        };
        let dirty = s
            .snaps
            .iter()
            .any(|(id, e)| Some(*id) != removed && e.tree.is_dirty());
        let close_work = closing && s.ino.next < s.ino.reserved;
        let has_work = dirty || close_work || !matches!(extra, Extra::None);
        if !has_work && !force_hook {
            return Ok(None);
        }
        match &extra {
            Extra::Add { name, from } => {
                if s.names.contains_key(*name) {
                    return Err(Error::SnapshotExists);
                }
                if let Some(f) = from {
                    if !s.snaps.contains_key(f) {
                        return Err(Error::NoSuchSnapshot);
                    }
                }
                if s.next_snapshot >= SNAPSHOT_LIMIT {
                    return Err(Error::LimitExceeded("snapshot ids exhausted"));
                }
            }
            Extra::Remove(id) => {
                if !s.snaps.contains_key(id) {
                    return Err(Error::NoSuchSnapshot);
                }
            }
            Extra::Rename { id, name } => {
                let Some(e) = s.snaps.get(id) else {
                    return Err(Error::NoSuchSnapshot);
                };
                // A name held by a different snapshot is refused, never replaced: the other
                // snapshot's id and tree have to keep answering to it.
                if let Some(taken) = s.names.get(*name) {
                    if *taken != *id {
                        return Err(Error::SnapshotExists);
                    }
                }
                // Renaming to the name the snapshot already has changes nothing, so there is
                // nothing to write. It is not an error either: the caller asked for the name it
                // can already see.
                if e.info.name == *name {
                    return Ok(None);
                }
            }
            Extra::None => {}
        }
        self.run_hook()?;
        if !has_work {
            return Ok(None);
        }
        let node_max = self.node_max;
        let r = guard(|| -> Result<Committed> {
            let mut wtx = self.db.begin_write()?;
            wtx.set_two_phase_commit(true);
            let mut new_roots = Vec::new();
            let mut added = None;
            {
                let mut snaps = wtx.open_table(SNAPSHOTS)?;
                let mut names = wtx.open_table(SNAP_NAMES)?;
                let mut meta = wtx.open_table(META)?;
                let mut reap = wtx.open_table(REAP)?;
                let mut w = NodeWriter::new(
                    wtx.open_table(NODES)?,
                    wtx.open_table(REFS)?,
                    self.cache.clone(),
                );
                for (id, e) in &s.snaps {
                    if Some(*id) == removed || !e.tree.is_dirty() {
                        continue;
                    }
                    let root = e.tree.write(&mut w)?;
                    if root != e.info.root {
                        w.add_ref(root);
                        w.drop_ref(e.info.root);
                        let info = SnapshotInfo {
                            root,
                            ..e.info.clone()
                        };
                        snaps.insert(id.0, encode_snap(&info).as_slice())?;
                    }
                    new_roots.push((*id, root));
                }
                match &extra {
                    Extra::None => {}
                    Extra::Add { name, from } => {
                        let root = match from {
                            Some(src) => new_roots
                                .iter()
                                .find(|(i, _)| i == src)
                                .map(|(_, r)| *r)
                                .or_else(|| s.snaps.get(src).map(|e| e.info.root))
                                .ok_or(Error::NoSuchSnapshot)?,
                            None => {
                                let mut tree = MemTree::empty(node_max);
                                let now = Timestamp::now();
                                let rec = InodeRec {
                                    kind: FileType::Dir,
                                    mode: 0o755,
                                    nlink: 2,
                                    size: 0,
                                    atime: now,
                                    mtime: now,
                                    ctime: now,
                                    parent: ROOT_INO.0,
                                    next_cookie: 1,
                                    covered: 0,
                                    cversion: 0,
                                };
                                let empty = Lazy::new(&self.db, &self.cache);
                                tree.insert(&empty, &key(ROOT_INO, K_INODE, &[]), rec.encode())?;
                                tree.write(&mut w)?
                            }
                        };
                        w.add_ref(root);
                        let id = s.next_snapshot;
                        let info = SnapshotInfo {
                            id: SnapshotId(id),
                            name: (*name).to_string(),
                            root,
                            created: Timestamp::now(),
                            parent: *from,
                        };
                        snaps.insert(id, encode_snap(&info).as_slice())?;
                        names.insert(*name, id)?;
                        meta.insert("next_snapshot", id + 1)?;
                        added = Some((info, root));
                    }
                    Extra::Remove(id) => {
                        let e = s.snaps.get(id).ok_or(Error::NoSuchSnapshot)?;
                        snaps.remove(id.0)?;
                        names.remove(e.info.name.as_str())?;
                        let next = meta_get(&meta, "next_reap")?;
                        reap.insert(next, *e.info.root.as_bytes())?;
                        meta.insert("next_reap", next + 1)?;
                    }
                    Extra::Rename { id, name } => {
                        // One transaction moves the name in both tables: the row keeps its id, so
                        // the snapshot's tree, its inode numbers and any handle already open on it
                        // are untouched, and a name held by another snapshot is refused above.
                        let e = s.snaps.get(id).ok_or(Error::NoSuchSnapshot)?;
                        // The dirty flush above may have moved the root before session publication.
                        let root = new_roots
                            .iter()
                            .find(|(i, _)| i == id)
                            .map(|(_, r)| *r)
                            .unwrap_or(e.info.root);
                        let mut info = e.info.clone();
                        info.root = root;
                        info.name = (*name).to_string();
                        snaps.insert(id.0, encode_snap(&info).as_slice())?;
                        names.remove(e.info.name.as_str())?;
                        names.insert(*name, id.0)?;
                    }
                }
                w.settle()?;
                let reserved = if closing {
                    s.ino.next.min(s.ino.reserved)
                } else {
                    s.ino.reserved
                };
                meta.insert("ino_reserved", reserved)?;
            }
            #[cfg(test)]
            if commit_fault() == 1 {
                return Err(Error::Storage("injected before the batch commit".into()));
            }
            wtx.commit()?;
            #[cfg(test)]
            if commit_fault() == 2 {
                return Err(Error::Storage("injected after the batch commit".into()));
            }
            Ok((new_roots, added))
        });
        let (new_roots, added) = match r {
            Ok(v) => v,
            Err(e) => {
                self.note(&e);
                return Err(e);
            }
        };
        for (id, root) in new_roots {
            if let Some(e) = s.snaps.get_mut(&id) {
                e.info.root = root;
                e.tree.reset(root);
            }
        }
        let mut out = None;
        match extra {
            Extra::Add { name, .. } => {
                if let Some((info, root)) = added {
                    let id = info.id;
                    s.snaps.insert(
                        id,
                        SnapEntry {
                            info,
                            tree: MemTree::new(root, node_max),
                        },
                    );
                    s.names.insert(name.to_string(), id);
                    s.next_snapshot += 1;
                    out = Some(id);
                }
            }
            Extra::Remove(id) => {
                if let Some(e) = s.snaps.remove(&id) {
                    s.names.remove(&e.info.name);
                }
                self.reap_len.fetch_add(1, SeqCst);
                self.wake_reaper();
            }
            Extra::Rename { id, name } => {
                // The session mirrors the two tables, so it moves here too, and only after the
                // transaction committed: a handle already open on this snapshot keeps working and
                // reports the new name, because its id did not change.
                if let Some(old) = s.snaps.get(&id).map(|e| e.info.name.clone()) {
                    s.names.remove(&old);
                    s.names.insert(name.to_string(), id);
                }
                if let Some(e) = s.snaps.get_mut(&id) {
                    e.info.name = name.to_string();
                }
            }
            Extra::None => {}
        }
        if closing {
            s.ino.reserved = s.ino.next.min(s.ino.reserved);
        }
        s.durable = s.applied;
        s.pending_ops = 0;
        s.pending_bytes = 0;
        s.pending_since = None;
        s.flush_err = None;
        self.note_flush_ok();
        self.disarm_timer();
        self.durable_seq.store(s.durable, SeqCst);
        let _g = lock(&self.gc);
        self.gc_cv.notify_all();
        Ok(out)
    }

    fn wake_reaper(&self) {
        if self.opts.background {
            lock(&self.bg.m).reap = true;
            self.bg.cv.notify_all();
        }
    }

    /// Durably reserves inode numbers below `new`. Carries no chunk references, so no hook.
    ///
    /// Spends any pending bound, because the floor has now reached the target that bound named.
    fn reserve_durable(&self, new: u64) -> Result<()> {
        #[cfg(test)]
        {
            count_reserve_commit();
            if reserve_fault() == 1 {
                return Err(Error::Storage("injected before the durable commit".into()));
            }
        }
        guard(|| {
            let mut wtx = self.db.begin_write()?;
            wtx.set_two_phase_commit(true);
            {
                let mut meta = wtx.open_table(META)?;
                meta.insert("ino_reserved", new)?;
                meta.remove(INO_INTENT)?;
            }
            wtx.commit()?;
            Ok(())
        })?;
        #[cfg(test)]
        if reserve_fault() == 2 {
            return Err(Error::Storage("injected after the durable commit".into()));
        }
        Ok(())
    }

    /// Durably records that a reservation is about to move the floor to `target`.
    ///
    /// Written in a commit of its own, ahead of the one that moves the floor, because that is the
    /// only arrangement that survives redb discarding its latest commit: the bound outlives the
    /// floor move it describes, so `record_recovery` can still see how far the floor had been asked
    /// to jump. Storing the amount in the same commit as the floor move would lose both together
    /// and leave only the one-block bound. Carries no chunk references, so no hook.
    fn reserve_intent(&self, target: u64) -> Result<()> {
        #[cfg(test)]
        count_reserve_commit();
        #[cfg(test)]
        if reserve_fault() == 3 {
            return Err(Error::Storage("injected before the bound commit".into()));
        }
        guard(|| {
            let mut wtx = self.db.begin_write()?;
            wtx.set_two_phase_commit(true);
            wtx.open_table(META)?.insert(INO_INTENT, target)?;
            wtx.commit()?;
            Ok(())
        })
    }

    /// Reserves `n` contiguous inode numbers, durably, before any of them names an inode.
    ///
    /// Takes the same lock and draws on the same allocator as `mutate`, so a reservation and an
    /// ordinary create can never overlap and no number is handed out twice.
    ///
    /// The floor is moved in one commit rather than a block at a time, and the bound that commit
    /// moved it by is durable in a commit of its own ahead of it. `record_recovery` then knows how
    /// far the floor had been asked to jump even when redb discards the floor move itself, so a
    /// lost commit can never reissue a number. A range the cached floor already covers commits
    /// nothing at all. `next` is advanced only after the floor is durable, so a failure here
    /// exposes no number and a reopen starts at or above the whole range.
    pub(crate) fn reserve_inodes(&self, n: u64) -> Result<InoRange> {
        let _flight = Inflight::enter(&self.inflight);
        let mut s = self.wlock()?;
        self.check_writable(&s)?;
        if n == 0 {
            return Err(Error::Invalid(
                "a reservation must ask for at least one inode",
            ));
        }
        if s.ino.next >= INO_LIMIT || n > INO_LIMIT - s.ino.next {
            return Err(Error::LimitExceeded("inode numbers exhausted"));
        }
        let target = s.ino.next + n;
        if target > s.ino.reserved {
            self.reserve_intent(target)?;
            self.reserve_durable(target)?;
            s.ino.reserved = target;
        }
        let start = s.ino.next;
        s.ino.next = target;
        Ok(InoRange::new(Ino(start), Ino(target)))
    }

    /// Reserves `n` inode numbers and returns one [`ReservedIno`] ticket per number.
    ///
    /// Same durable reservation as [`Inner::reserve_inodes`], with the numbers wrapped in
    /// session-owned tickets and recorded in the session's outstanding set. A ticket is spent by the
    /// selected-number create that consumes it; until then it is the only way to name one of these
    /// numbers on a create, and it is refused by a create on a different store or a session that has
    /// closed.
    pub(crate) fn reserve_tickets(&self, n: u64) -> Result<Vec<ReservedIno>> {
        let range = self.reserve_inodes(n)?;
        let store = self.store_id;
        let mut s = self.wlock()?;
        let mut out = Vec::with_capacity(range.len() as usize);
        for ino in range.iter() {
            s.reserved.insert(ino);
            out.push(ReservedIno { store, ino });
        }
        Ok(out)
    }

    /// Records a rollback that lost the newest commit, and moves the two counter floors past
    /// everything the lost commit could have handed out.
    ///
    /// A rollback returns the store to the previous commit, so the inode and snapshot counters go
    /// back with it. The floor is moved by one commit that may jump by any amount, and the bound
    /// naming that jump is written by an earlier commit, so redb discarding the newest commit
    /// leaves that bound behind and the skip below can still cover it. Where there is no bound, the
    /// floor only ever moved one block, which is what a reservation's own commits can have lost.
    /// `next_snapshot` moves one per commit.
    /// Adding those bounds keeps the never-reused rule: a number can be wasted, never handed out
    /// twice.
    ///
    /// A bound, where one was left behind, names the floor move exactly and so no block is needed.
    /// Only a file with no bound, which is one written before bounds existed, needs the block, and
    /// that must be the one the file was CREATED with because it is the size the lost reservation
    /// was written with. A file written before the block was persisted has no provable bound, so
    /// this refuses instead of guessing; refusing to recover is recoverable, re-issuing a number is
    /// not. Carries no chunk references, so no hook.
    fn record_recovery(&self) -> Result<(u64, u64, u64)> {
        let (recoveries, ino_floor, snapshot_floor) = guard(|| {
            let mut wtx = self.db.begin_write()?;
            wtx.set_two_phase_commit(true);
            let mut meta = wtx.open_table(META)?;
            let bound = meta.get(INO_INTENT)?.map_or(0, |g| g.value());
            let reserved = meta_get(&meta, "ino_reserved")?;
            let floor = if bound > 0 {
                bound
            } else {
                let Some(block) = self.ino_block else {
                    return Err(Error::Format(
                        "cannot recover: this file predates the persisted inode reservation block, \
                         so the size of the lost reservation cannot be proven"
                            .into(),
                    ));
                };
                reserved.saturating_add(block)
            };
            let ino_floor = floor.max(reserved).min(INO_LIMIT);
            let snapshot_floor = meta_get(&meta, "next_snapshot")?
                .saturating_add(1)
                .min(SNAPSHOT_LIMIT);
            let recoveries = meta
                .get("recoveries")?
                .map_or(0, |g| g.value())
                .saturating_add(1);
            meta.insert("ino_reserved", ino_floor)?;
            meta.insert("next_snapshot", snapshot_floor)?;
            meta.insert("recoveries", recoveries)?;
            meta.remove(INO_INTENT)?;
            drop(meta);
            wtx.commit()?;
            Ok((recoveries, ino_floor, snapshot_floor))
        })?;
        let mut s = self.wlock()?;
        s.ino.reserved = s.ino.reserved.max(ino_floor);
        s.ino.next = s.ino.next.max(ino_floor);
        s.next_snapshot = s.next_snapshot.max(snapshot_floor);
        self.recoveries.store(recoveries, SeqCst);
        Ok((recoveries, ino_floor, snapshot_floor))
    }

    pub(crate) fn mutate<T>(
        &self,
        id: SnapshotId,
        f: impl FnOnce(&mut Tx<'_>) -> Result<T>,
    ) -> Result<T> {
        // Kept past the block so a failed durable wait can put the session's outstanding
        // reservations back the way a retry expects to find them.
        let mut spent: HashSet<Ino> = HashSet::new();
        let (out, wait_for) = {
            let _flight = Inflight::enter(&self.inflight);
            let mut guard_ = self.wlock()?;
            let s = &mut *guard_;
            self.check_writable(s)?;
            if s.pending_ops > self.opts.sync_every_ops.saturating_mul(8) && s.flush_err.is_some() {
                let msg = s.flush_err.clone().unwrap_or_default();
                return Err(Error::Storage(format!(
                    "durable commits keep failing, refusing more changes: {msg}"
                )));
            }
            let e = s.snaps.get_mut(&id).ok_or(Error::NoSuchSnapshot)?;
            let saved = e.tree.clone();
            let lazy = Lazy::new(&self.db, &self.cache);
            let reserve = |n: u64| self.reserve_durable(n);
            // numbers spent inside this one transaction; a second create at the same number in the
            // same batch is refused rather than silently overwriting
            let res = {
                let store = self.store_id;
                let mut tx = Tx {
                    tree: &mut e.tree,
                    src: &lazy,
                    ino: &mut s.ino,
                    reserve: &reserve,
                    store,
                    reserved: &s.reserved,
                    spent: &mut spent,
                    now: Timestamp::now(),
                };
                catch_unwind(AssertUnwindSafe(|| f(&mut tx)))
            };
            let out = match res {
                Err(p) => {
                    e.tree = saved;
                    resume_unwind(p);
                }
                Ok(Err(er)) => {
                    e.tree = saved;
                    self.note(&er);
                    return Err(er);
                }
                Ok(Ok(v)) => v,
            };
            // the closure succeeded, so every reserved number a selected create spent is now named
            // and must not be minted again from this session. Removal is deferred to here, not done
            // inside the closure, so a closure that returns `Err` leaves the ticket usable for a
            // retry.
            for ino in &spent {
                s.reserved.remove(ino);
            }
            let changed = e.tree.edits() != saved.edits();
            if changed {
                s.applied += 1;
                s.pending_ops += 1;
                s.pending_bytes += e.tree.dirty_bytes().saturating_sub(saved.dirty_bytes());
                if s.pending_since.is_none() {
                    let now = Instant::now();
                    s.pending_since = Some(now);
                    if let Some(d) = now.checked_add(self.opts.sync_interval) {
                        self.arm_timer(d);
                    }
                }
                if self.opts.ack == Ack::Applied {
                    let due = s.pending_ops >= self.opts.sync_every_ops
                        || s.pending_bytes >= self.opts.max_pending_bytes
                        || s.pending_since
                            .is_some_and(|t| t.elapsed() >= self.opts.sync_interval);
                    if due {
                        // A panicking hook on this path is still the caller's panic to see, so it
                        // is re-raised, but it is recorded first: otherwise `Health` would report
                        // zeros for an operation the store knows it could not make durable.
                        // Safe to catch here for the same reason the background job may catch it:
                        // `run_hook` runs before `begin_write`, so no transaction is left open.
                        let r = catch_unwind(AssertUnwindSafe(|| {
                            self.commit(s, Extra::None, false, true)
                        }));
                        match r {
                            Ok(Err(er)) => {
                                let why = er.to_string();
                                s.flush_err = Some(why.clone());
                                self.note_flush_failure(why);
                            }
                            Err(p) => {
                                let why = "the before_sync hook panicked on the inline commit"
                                    .to_string();
                                s.flush_err = Some(why.clone());
                                self.note_flush_failure(why);
                                resume_unwind(p);
                            }
                            Ok(Ok(_)) => {}
                        }
                    }
                }
            }
            (
                out,
                (changed && self.opts.ack == Ack::Durable).then_some(s.applied),
            )
        };
        if let Some(seq) = wait_for {
            if let Err(er) = self.wait_durable(seq) {
                // The caller sees a failure and will retry. Nothing durable was written, so the
                // numbers this batch reserved must be spendable again; the create itself stays
                // pending in the tree like any other applied edit, so a retry at a live number is
                // stopped by the inode-exists check rather than silently duplicated.
                let mut s = self.wlock()?;
                for ino in &spent {
                    s.reserved.insert(*ino);
                }
                return Err(er);
            }
        }
        Ok(out)
    }

    /// Returns once every change applied up to `seq` is durable. Callers that arrive while a
    /// commit is running share the next one (leader and followers).
    fn wait_durable(&self, seq: u64) -> Result<()> {
        loop {
            let mut led = lock(&self.gc);
            if self.durable_seq.load(SeqCst) >= seq {
                return Ok(());
            }
            if *led {
                let _ = self.gc_cv.wait_timeout(led, Duration::from_millis(50));
                continue;
            }
            *led = true;
            drop(led);
            // Let callers that are queued to apply their change join this commit.
            let start = Instant::now();
            while self.inflight.load(SeqCst) > 0 && start.elapsed() < GROUP_WAIT {
                std::thread::yield_now();
            }
            let r = match self.wlock() {
                Ok(mut s) => self.commit(&mut s, Extra::None, false, true).map(|_| ()),
                Err(e) => Err(e),
            };
            if let Err(e) = &r {
                self.note_flush_failure(e.to_string());
            }
            *lock(&self.gc) = false;
            self.gc_cv.notify_all();
            r?;
        }
    }

    pub(crate) fn sync(&self) -> Result<()> {
        let mut s = self.wlock()?;
        self.commit(&mut s, Extra::None, false, true).map(|_| ())
    }

    pub(crate) fn close(&self) -> Result<()> {
        let mut s = self.wlock()?;
        if s.closed {
            return Ok(());
        }
        let r = self.commit(&mut s, Extra::None, true, true).map(|_| ());
        if let Err(e) = &r {
            self.note_flush_failure(e.to_string());
        }
        s.closed = true;
        r
    }

    /// Drop path: make everything durable if the hook allows it, otherwise discard it.
    fn finish_on_drop(&self) {
        let Ok(mut s) = self.wlock() else { return };
        if s.closed {
            return;
        }
        let _ = self.commit(&mut s, Extra::None, true, true);
        s.closed = true;
        s.snaps.clear();
    }

    fn add_snapshot(&self, name: &str, from: Option<SnapshotId>) -> Result<SnapshotId> {
        if name.is_empty() || name.len() > usize::from(u16::MAX) {
            return Err(Error::Invalid("bad snapshot name"));
        }
        let mut s = self.wlock()?;
        self.commit(&mut s, Extra::Add { name, from }, false, true)?
            .ok_or_else(|| corrupt("snapshot was not created"))
    }

    fn remove_snapshot(&self, id: SnapshotId) -> Result<()> {
        let mut s = self.wlock()?;
        self.commit(&mut s, Extra::Remove(id), false, true)
            .map(|_| ())
    }

    /// Moves a snapshot to a new name in one transaction, keeping its id.
    ///
    /// The name is the only thing that changes: the id, the tree, every inode number in it and any
    /// handle already open on the snapshot all stay as they were, so a consumer needs one write
    /// transaction instead of the two forks a rename otherwise costs.
    ///
    /// A name held by a different snapshot is refused with [`Error::SnapshotExists`] and changes
    /// nothing. A name the snapshot already has is a no-op that writes nothing.
    fn rename_snapshot(&self, id: SnapshotId, name: &str) -> Result<()> {
        if name.is_empty() || name.len() > usize::from(u16::MAX) {
            return Err(Error::Invalid("bad snapshot name"));
        }
        let mut s = self.wlock()?;
        self.commit(&mut s, Extra::Rename { id, name }, false, true)
            .map(|_| ())
    }

    /// Frees a bounded number of nodes of removed snapshots in one small transaction. Returns
    /// whether more work remains.
    ///
    /// Runs without the session lock: redb serializes it with commits, and in-memory changes never
    /// wait for it.
    pub(crate) fn reap_step(&self) -> Result<bool> {
        if self.reap_len.load(SeqCst) == 0 {
            return Ok(false);
        }
        self.check_writable(&*self.rlock()?)?;
        let r = guard(|| -> Result<(u64, u64)> {
            let mut wtx = self.db.begin_write()?;
            wtx.set_durability(Durability::None)?;
            let started = Instant::now();
            let (taken, added);
            {
                let mut reap = wtx.open_table(REAP)?;
                let mut nodes = wtx.open_table(NODES)?;
                let mut refs = wtx.open_table(REFS)?;
                let mut meta = wtx.open_table(META)?;
                let mut rows: Vec<(u64, [u8; 32])> = Vec::new();
                for r in reap.iter()?.take(REAP_BUDGET) {
                    let (k, v) = r?;
                    rows.push((k.value(), v.value()));
                }
                taken = rows.len() as u64;
                for (k, _) in &rows {
                    reap.remove(*k)?;
                }
                let mut work: Vec<[u8; 32]> = rows.into_iter().map(|(_, v)| v).collect();
                let (mut freed, mut left) = (0usize, Vec::new());
                while let Some(id) = work.pop() {
                    if freed >= REAP_BUDGET || (freed > 0 && started.elapsed() >= REAP_TIME) {
                        left.push(id);
                        continue;
                    }
                    let cur = refs
                        .get(id)?
                        .map(|g| g.value())
                        .ok_or_else(|| corrupt("reap entry names a node without a count"))?;
                    if cur > 1 {
                        refs.insert(id, cur - 1)?;
                        continue;
                    }
                    let bytes = nodes
                        .remove(id)?
                        .map(|g| g.value().to_vec())
                        .ok_or_else(|| corrupt("reap entry names a missing node"))?;
                    refs.remove(id)?;
                    freed += 1;
                    let n = Node::parse(bytes)?;
                    if !n.is_leaf() {
                        for i in 0..n.len() {
                            work.push(*n.child(i).as_bytes());
                        }
                    }
                }
                let mut next = meta_get(&meta, "next_reap")?;
                added = left.len() as u64;
                for id in left {
                    reap.insert(next, id)?;
                    next += 1;
                }
                meta.insert("next_reap", next)?;
            }
            // A durable commit flushes every earlier non-durable one, so bound the backlog: it
            // carries no chunk references, hence no hook.
            if self.reap_steps.fetch_add(1, SeqCst) % REAP_DURABLE_EVERY == REAP_DURABLE_EVERY - 1 {
                wtx.set_durability(Durability::Immediate)?;
                wtx.set_two_phase_commit(true);
            }
            wtx.commit()?;
            Ok((taken, added))
        });
        match r {
            Ok((taken, added)) => {
                let _ = self
                    .reap_len
                    .try_update(SeqCst, SeqCst, |n| Some(n.saturating_sub(taken) + added));
                Ok(self.reap_len.load(SeqCst) > 0)
            }
            Err(e) => {
                self.note(&e);
                Err(e)
            }
        }
    }

    fn timer_flush(&self) {
        let Ok(mut s) = self.wlock() else { return };
        if s.closed || s.pending_ops == 0 {
            return;
        }
        let waited = s
            .pending_since
            .map_or(self.opts.sync_interval, |t| t.elapsed());
        let retry = |wait: Duration| {
            if let Some(d) = Instant::now().checked_add(wait) {
                self.arm_timer(d);
            }
        };
        if waited < self.opts.sync_interval {
            retry(self.opts.sync_interval - waited);
            return;
        }
        if let Err(e) = self.commit(&mut s, Extra::None, false, true) {
            let why = e.to_string();
            s.flush_err = Some(why.clone());
            self.note_flush_failure(why);
            retry(self.opts.sync_interval);
        }
    }

    fn stop_bg(&self) {
        lock(&self.bg.m).stop = true;
        self.bg.cv.notify_all();
    }
}

fn bg_main(inner: Arc<Inner>) {
    enum Job {
        Flush,
        Reap,
    }
    loop {
        let job = {
            let mut st = lock(&inner.bg.m);
            loop {
                if st.stop {
                    return;
                }
                if st.reap {
                    st.reap = false;
                    break Job::Reap;
                }
                match st.deadline {
                    Some(d) => {
                        let now = Instant::now();
                        if d <= now {
                            st.deadline = None;
                            break Job::Flush;
                        }
                        st = inner
                            .bg
                            .cv
                            .wait_timeout(st, d - now)
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                    }
                    None => st = inner.bg.cv.wait(st).unwrap_or_else(|e| e.into_inner()),
                }
            }
        };
        match job {
            // A panic in the sync hook must not unwind out of this loop: a dead timer thread
            // silently stops flushing idle changes, so the reason is recorded and the flush
            // re-armed instead.
            Job::Flush => {
                if catch_unwind(AssertUnwindSafe(|| inner.timer_flush())).is_err() {
                    inner.note_bg_panic("background flush");
                    inner.rearm_flush();
                }
            }
            // Same for the reaper, and the reason is the same. `matches!(.., Ok(Ok(true)))` was
            // false for `Ok(Err(e))`, so a failed reap never re-armed the flag: reaping stopped,
            // a removed snapshot's space never came back, and nothing said why. Both a panic and
            // an error now record the reason and re-arm, the same as a failed flush. The 300us
            // pause before re-arming is what keeps a persistently failing reap from spinning.
            Job::Reap => match catch_unwind(AssertUnwindSafe(|| inner.reap_step())) {
                Ok(Ok(true)) => {
                    std::thread::sleep(Duration::from_micros(300));
                    lock(&inner.bg.m).reap = true;
                }
                Ok(Ok(false)) => {}
                Ok(Err(e)) => {
                    inner.note_flush_failure(format!("reap step failed: {e}"));
                    std::thread::sleep(Duration::from_micros(300));
                    lock(&inner.bg.m).reap = true;
                }
                Err(_) => {
                    inner.note_bg_panic("background reap");
                    std::thread::sleep(Duration::from_micros(300));
                    lock(&inner.bg.m).reap = true;
                }
            },
        }
    }
}

/// Shared by every `Meta` and `Snapshot` clone. Dropping the last one stops the background
/// thread, makes pending changes durable if the hook allows it, and closes the file.
pub(crate) struct Handle {
    pub(crate) inner: Arc<Inner>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Handle").finish_non_exhaustive()
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.inner.stop_bg();
        if let Some(t) = lock(&self.thread).take() {
            let _ = t.join();
        }
        let _ = catch_unwind(AssertUnwindSafe(|| self.inner.finish_on_drop()));
    }
}

/// What `Meta::open_recover` did.
///
/// Every field is additive. `rolled_back` false means nothing was lost, so the floors are `None`
/// and only the durable count is reported.
#[derive(Clone, Debug)]
pub struct Recovery {
    /// True when the newest commit could not be read and the previous one was used.
    pub rolled_back: bool,
    /// Copy of the file as it was before recovery (only when `rolled_back`).
    pub backup: Option<PathBuf>,
    /// The snapshots of the recovered state.
    pub snapshots: Vec<SnapshotInfo>,
    /// Times this file has been rolled back, including this one. Durable, so a later plain
    /// reopen reports the same number. 0 when nothing was lost.
    pub recoveries: u64,
    /// New durable floor for inode numbers, when a rollback lost the commit that recorded the old
    /// one. `None` when nothing was lost. No number at or above this has ever been handed out.
    pub ino_floor: Option<u64>,
    /// New durable floor for snapshot ids, likewise.
    pub snapshot_floor: Option<u64>,
}

/// [`Error::Storage`] from a failed [`Meta::open_recover`] starts with this, so a caller can tell
/// a recovery that failed closed from an ordinary I/O error.
pub const RECOVERY_FAILED: &str = "recovery failed";

/// The metadata store: one redb file holding every snapshot's tree.
///
/// Cloning is cheap and shares the database. All methods take `&self`. Writers are serialized;
/// readers see every applied change immediately and never a partial one.
#[derive(Clone, Debug)]
pub struct Meta {
    pub(crate) h: Arc<Handle>,
}

fn builder(opts: &Options) -> Builder {
    let mut b = Builder::new();
    b.set_cache_size(opts.cache_size);
    b
}

impl Meta {
    /// Opens the database at `path`, creating it if the file is missing or empty.
    ///
    /// Fails closed: a file that is not a cowfs-meta database is refused with [`Error::Format`],
    /// and a file whose newest commit cannot be verified is refused with a storage error. See
    /// [`Meta::open_recover`].
    pub fn open(path: impl AsRef<Path>, opts: Options) -> Result<Meta> {
        guard(|| Self::init(builder(&opts).create(path)?, opts.clone()))
    }

    /// Like [`Meta::open`] on a caller-supplied redb backend (used by crash-injection tests).
    pub fn open_with_backend(backend: impl StorageBackend, opts: Options) -> Result<Meta> {
        guard(|| Self::init(builder(&opts).create_with_backend(backend)?, opts.clone()))
    }

    /// Opens a file that [`Meta::open`] refuses because its newest commit is unreadable (for
    /// example after a disk lied about an fsync), using the previous commit instead.
    ///
    /// Never called automatically. The file is copied to `<path>.pre-recover` first, and restored
    /// from that copy if recovery fails. Everything committed after the previous commit is lost;
    /// the returned report lists the recovered snapshots so the caller can tell.
    ///
    /// A rollback also moves the inode and snapshot counters forward, past everything the lost
    /// commit could have handed out, so no number is ever reused. That write failing is fatal
    /// here: handing out a reused number is worse than refusing to open.
    pub fn open_recover(path: impl AsRef<Path>, opts: Options) -> Result<(Meta, Recovery)> {
        let path = path.as_ref();
        match catch_unwind(AssertUnwindSafe(|| builder(&opts).create(path))) {
            Ok(Ok(db)) => {
                let m = guard(|| Self::init(db, opts.clone()))?;
                let snapshots = m.snapshots()?;
                let recoveries = m.health().recoveries;
                return Ok((
                    m,
                    Recovery {
                        rolled_back: false,
                        backup: None,
                        snapshots,
                        recoveries,
                        ino_floor: None,
                        snapshot_floor: None,
                    },
                ));
            }
            Ok(Err(redb::DatabaseError::Storage(redb::StorageError::Corrupted(_)))) | Err(_) => {}
            Ok(Err(e)) => return Err(e.into()),
        }
        let mut head = [0u8; 10];
        {
            use std::io::Read;
            let mut f = std::fs::File::open(path).map_err(|e| Error::Storage(e.to_string()))?;
            f.read_exact(&mut head)
                .map_err(|e| Error::Storage(e.to_string()))?;
        }
        if head[..9] != REDB_MAGIC {
            return Err(Error::Format("not a redb file".into()));
        }
        let mut backup = path.as_os_str().to_owned();
        backup.push(".pre-recover");
        let backup = PathBuf::from(backup);
        std::fs::copy(path, &backup)
            .map_err(|e| Error::Storage(format!("cannot back up before recovery: {e}")))?;
        let restore = |why: String| -> Error {
            let _ = std::fs::copy(&backup, path);
            Error::Storage(format!(
                "{RECOVERY_FAILED} ({why}); file restored from {}",
                backup.display()
            ))
        };
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(|e| Error::Storage(e.to_string()))?;
            f.seek(SeekFrom::Start(9))
                .map_err(|e| Error::Storage(e.to_string()))?;
            // Clear the two-phase-commit flag and ask for repair: redb then verifies the primary
            // commit slot and falls back to the other one when it does not verify.
            f.write_all(&[(head[9] | GOD_RECOVERY) & !GOD_TWO_PHASE])
                .map_err(|e| Error::Storage(e.to_string()))?;
            f.sync_all().map_err(|e| Error::Storage(e.to_string()))?;
        }
        match Self::open(path, opts.clone()) {
            Ok(m) => {
                let snapshots = m.snapshots()?;
                let (recoveries, ino_floor, snapshot_floor) =
                    m.h.inner.record_recovery().map_err(|e| {
                        Error::Storage(format!(
                            "{RECOVERY_FAILED} ({e}); the recovered file is at {}, and the \
                             pre-recovery copy is at {}",
                            path.display(),
                            backup.display()
                        ))
                    })?;
                Ok((
                    m,
                    Recovery {
                        rolled_back: true,
                        backup: Some(backup),
                        snapshots,
                        recoveries,
                        ino_floor: Some(ino_floor),
                        snapshot_floor: Some(snapshot_floor),
                    },
                ))
            }
            Err(e) => Err(restore(e.to_string())),
        }
    }

    fn init(db: Database, opts: Options) -> Result<Meta> {
        if opts.node_size < 256 {
            return Err(Error::Invalid("node_size below 256"));
        }
        // Validated here so a nonsense block is refused at the door rather than at the first
        // reservation. A block of zero would make every reservation a no-op.
        let ino_block = opts.ino_block.clamp(1, INO_LIMIT);
        let tables: Vec<String> = {
            let rtx = db.begin_read()?;
            let list: Vec<String> = rtx
                .list_tables()?
                .map(|t| redb::TableHandle::name(&t).to_string())
                .collect();
            list
        };
        if tables.is_empty() {
            let mut wtx = db.begin_write()?;
            wtx.set_two_phase_commit(true);
            {
                wtx.open_table(NODES)?;
                wtx.open_table(REFS)?;
                wtx.open_table(SNAPSHOTS)?;
                wtx.open_table(SNAP_NAMES)?;
                wtx.open_table(REAP)?;
                let mut m = wtx.open_table(META)?;
                m.insert("magic", MAGIC)?;
                m.insert("version", FORMAT_VERSION)?;
                m.insert("node_size", opts.node_size as u64)?;
                m.insert("ino_block", ino_block)?;
                m.insert("ino_reserved", 2)?;
                m.insert("next_snapshot", 1)?;
                m.insert("next_reap", 1)?;
            }
            wtx.commit()?;
        } else if !tables.iter().any(|t| t == "meta") {
            return Err(Error::Format(
                "no cowfs-meta header in this redb file".into(),
            ));
        }
        let rtx = db.begin_read()?;
        let meta = rtx.open_table(META)?;
        if meta.get("magic")?.map(|g| g.value()) != Some(MAGIC) {
            return Err(Error::Format(
                "no cowfs-meta header in this redb file".into(),
            ));
        }
        let version = meta_get(&meta, "version")?;
        if version != FORMAT_VERSION {
            return Err(Error::Format(format!(
                "format version {version}, this build reads {FORMAT_VERSION}"
            )));
        }
        let node_max = usize::try_from(meta_get(&meta, "node_size")?)
            .ok()
            .filter(|&n| n >= 256)
            .ok_or_else(|| corrupt("bad node size"))?;
        // The stored block governs, not the caller's: `open_recover` needs the size the lost
        // reservation was actually written with, and a caller may pass anything. Absent means a
        // file written before the key existed, so the historical bound is unknown; only recovery
        // cares, and it refuses rather than guessing.
        let stored_block = match meta.get("ino_block")? {
            Some(g) => {
                let b = g.value();
                if b == 0 {
                    return Err(corrupt("ino_block of zero"));
                }
                Some(b)
            }
            None => None,
        };
        let reserved = meta_get(&meta, "ino_reserved")?;
        let recoveries = meta.get("recoveries")?.map_or(0, |g| g.value());
        let mut snaps = BTreeMap::new();
        let mut names = HashMap::new();
        for r in rtx.open_table(SNAPSHOTS)?.iter()? {
            let (k, v) = r?;
            let info = decode_snap(k.value(), v.value())?;
            names.insert(info.name.clone(), info.id);
            let tree = MemTree::new(info.root, node_max);
            snaps.insert(info.id, SnapEntry { info, tree });
        }
        let reap_len = rtx.open_table(REAP)?.len()?;
        let session = Session {
            snaps,
            names,
            ino: InoAlloc {
                next: reserved,
                reserved,
                // A legacy file has no stored block, so the caller's is used for forward
                // allocation only; `record_recovery` refuses in that case.
                block: stored_block.unwrap_or(ino_block),
            },
            next_snapshot: meta_get(&meta, "next_snapshot")?,
            reserved: HashSet::new(),
            applied: 0,
            durable: 0,
            pending_ops: 0,
            pending_bytes: 0,
            pending_since: None,
            flush_err: None,
            closed: false,
        };
        drop(meta);
        drop(rtx);
        let background = opts.background;
        let inner = Arc::new(Inner {
            db: Db(Some(db)),
            node_max,
            cache: Arc::new(NodeCache::new(opts.node_cache)),
            session: RwLock::new(session),
            store_id: next_store_id(),
            ino_block: stored_block,
            durable_seq: AtomicU64::new(0),
            gc: Mutex::new(false),
            gc_cv: Condvar::new(),
            bg: Bg {
                m: Mutex::new(BgState {
                    stop: false,
                    deadline: None,
                    reap: false,
                }),
                cv: Condvar::new(),
            },
            poisoned: AtomicBool::new(false),
            inflight: AtomicUsize::new(0),
            reap_len: AtomicU64::new(reap_len),
            reap_steps: AtomicU64::new(0),
            last_error: Mutex::new(None),
            flush_failures: AtomicU64::new(0),
            consecutive_flush_failures: AtomicU64::new(0),
            bg_panics: AtomicU64::new(0),
            recoveries: AtomicU64::new(recoveries),
            opts,
        });
        let thread = if background {
            let i2 = inner.clone();
            let t = std::thread::Builder::new()
                .name("cowfs-meta-bg".into())
                .spawn(move || bg_main(i2))
                .map_err(|e| Error::Storage(format!("cannot start the background thread: {e}")))?;
            if inner.reap_len.load(SeqCst) > 0 {
                inner.wake_reaper();
            }
            Some(t)
        } else {
            None
        };
        Ok(Meta {
            h: Arc::new(Handle {
                inner,
                thread: Mutex::new(thread),
            }),
        })
    }

    fn snap(&self, id: SnapshotId) -> Snapshot {
        Snapshot {
            h: self.h.clone(),
            id,
        }
    }

    /// Creates a snapshot holding an empty tree (just the root directory). Durable on return.
    pub fn new_snapshot(&self, name: &str) -> Result<Snapshot> {
        Ok(self.snap(self.h.inner.add_snapshot(name, None)?))
    }

    /// Opens a snapshot by name.
    pub fn snapshot(&self, name: &str) -> Result<Snapshot> {
        let s = self.h.inner.rlock()?;
        s.names
            .get(name)
            .map(|id| self.snap(*id))
            .ok_or(Error::NoSuchSnapshot)
    }

    /// Opens a snapshot by id.
    pub fn snapshot_by_id(&self, id: SnapshotId) -> Result<Snapshot> {
        let s = self.h.inner.rlock()?;
        if s.snaps.contains_key(&id) {
            Ok(self.snap(id))
        } else {
            Err(Error::NoSuchSnapshot)
        }
    }

    /// Lists all snapshots in id order. Roots include applied changes that are not yet durable.
    pub fn snapshots(&self) -> Result<Vec<SnapshotInfo>> {
        let s = self.h.inner.rlock()?;
        Ok(s.snaps.values().map(entry_info).collect())
    }

    /// Snapshots as of the last durable commit, read from the file. This is what a crash right now
    /// would leave, so tests and tooling can compare it with a reopened database.
    pub fn durable_snapshots(&self) -> Result<Vec<SnapshotInfo>> {
        guard(|| {
            let rtx = self.h.inner.db.begin_read()?;
            let t = rtx.open_table(SNAPSHOTS)?;
            let mut out = Vec::new();
            for r in t.iter()? {
                let (k, v) = r?;
                out.push(decode_snap(k.value(), v.value())?);
            }
            Ok(out)
        })
    }

    /// Removes a snapshot. Durable on return. Its nodes are freed in small steps afterwards by
    /// the background thread (or by `reap_step` when `background` is off), so no long lock is
    /// held; the space returns as the steps run.
    pub fn remove_snapshot(&self, id: SnapshotId) -> Result<()> {
        self.h.inner.remove_snapshot(id)
    }

    /// Renames a snapshot in one transaction, keeping its id.
    ///
    /// `new_name` follows the same rule as [`Meta::new_snapshot`]: a name that is empty or longer
    /// than `u16::MAX` bytes is [`Error::Invalid`]. This is meta's own rule and it is deliberately
    /// the same one `new_snapshot` already applies, so a rename cannot produce a name a create
    /// would have refused.
    ///
    /// What is preserved: the [`SnapshotId`], the tree and its Merkle root, every inode number in
    /// it, the creation time and parent, the inode reservation high-water mark, the reap queue and
    /// the `next_snapshot` counter. A [`Snapshot`] handle taken before the rename stays usable and
    /// reports the new name.
    ///
    /// A name held by a different snapshot is refused with [`Error::SnapshotExists`]; the other
    /// snapshot is not replaced, removed or renamed. A [`SnapshotId`] that is not present is
    /// [`Error::NoSuchSnapshot`]. Renaming a snapshot to the name it already has succeeds and
    /// writes nothing.
    ///
    /// This is the metadata API only. No consumer is wired to it yet, and `cowfs-core` still stages
    /// its own rename through `src/swap.rs`, so nothing outside `cowfs-meta` changes behaviour
    /// until a consumer adopts this.
    pub fn rename_snapshot(&self, id: SnapshotId, new_name: &str) -> Result<()> {
        self.h.inner.rename_snapshot(id, new_name)
    }

    /// Frees a bounded number of nodes of removed snapshots. Returns true when more remain.
    pub fn reap_step(&self) -> Result<bool> {
        self.h.inner.reap_step()
    }

    /// Runs `reap_step` until nothing is left.
    pub fn reap_all(&self) -> Result<()> {
        while self.reap_step()? {}
        Ok(())
    }

    /// Number of removed-snapshot roots still waiting to be freed.
    pub fn pending_reap(&self) -> Result<u64> {
        Ok(self.h.inner.reap_len.load(SeqCst))
    }

    /// What the store knows about its own background work and its last recovery.
    ///
    /// Pollable and infallible. A store that never failed reports `None` and zeros; the counters
    /// only grow, so two samples tell progress from a stall. Read it instead of inferring health
    /// from `check()`, which says nothing about a flush that never happened.
    pub fn health(&self) -> Health {
        self.h.inner.health()
    }

    /// Reserves `n` inode numbers before any inode exists, and hands them back.
    ///
    /// The numbers come from the same allocator [`Snapshot::batch`] creation draws on, so an
    /// ordinary create never receives one of them and this never receives one from a create.
    /// Numbers are contiguous and `end` is exclusive.
    ///
    /// The durable floor is committed before this returns, so a number is never reissued after a
    /// reopen, including one that was reserved and then never used. Because it commits, it is a
    /// durable operation rather than an applied one: it runs under the same lock as a batch and
    /// runs no `before_sync` hook, since it carries no chunk references.
    ///
    /// Asking for zero is [`Error::Invalid`], and asking for more than the remaining numbers below
    /// [`INO_LIMIT`] is [`Error::LimitExceeded`]. Neither writes anything.
    ///
    /// This hands out numbers; it does not create inodes. Creating an inode at a reserved number is
    /// a separate concern and is not provided here.
    pub fn reserve_inodes(&self, n: u64) -> Result<InoRange> {
        self.h.inner.reserve_inodes(n)
    }

    /// Reserves `n` inode numbers and returns one [`ReservedIno`] ticket per number.
    ///
    /// Same durable reservation as [`Meta::reserve_inodes`], with each number wrapped in a
    /// session-owned ticket. A ticket is the only way to create an inode at one of these numbers:
    /// [`Tx::create_at`](crate::Tx::create_at) and its siblings take a `&ReservedIno`, refuse a
    /// ticket from another store or a session that has closed, and spend it at most once. Ask for
    /// zero or more than the remaining numbers below [`INO_LIMIT`] and the whole call is refused
    /// with nothing written and no ticket minted.
    pub fn reserve_tickets(&self, n: u64) -> Result<Vec<ReservedIno>> {
        self.h.inner.reserve_tickets(n)
    }

    /// Runs `before_sync`, then makes every applied change durable. The hook runs on every call,
    /// also when nothing is pending, so a caller can use this as "sync the store, then the
    /// metadata". Returns the hook's or the commit's error.
    pub fn sync(&self) -> Result<()> {
        self.h.inner.sync()
    }

    /// Final sync: runs the hook, makes everything durable, records the exact inode counter, and
    /// stops accepting changes. Reports errors, which drop cannot. Dropping the last handle does
    /// the same, but discards unsynced changes if the hook fails.
    pub fn close(&self) -> Result<()> {
        self.h.inner.close()
    }

    /// Verifies every structural and semantic invariant (after a `sync()`). See `docs/v1-meta.md`.
    pub fn check(&self) -> Result<()> {
        self.sync()?;
        guard(|| crate::check::check(&self.h.inner))
    }

    /// Packs a snapshot id and an inode number into one restart-stable `u64`: the snapshot id in
    /// the top 24 bits, the inode in the low 40. `None` if either is out of range; the store never
    /// hands out such values (it returns [`Error::LimitExceeded`] instead), so `None` means the
    /// arguments did not come from this store.
    pub fn pack_ino(snapshot: SnapshotId, ino: Ino) -> Option<u64> {
        (snapshot.0 != 0 && snapshot.0 < SNAPSHOT_LIMIT && ino.0 < INO_LIMIT)
            .then_some(snapshot.0 << 40 | ino.0)
    }

    /// Inverse of [`Meta::pack_ino`].
    pub fn unpack_ino(packed: u64) -> (SnapshotId, Ino) {
        (SnapshotId(packed >> 40), Ino(packed & (INO_LIMIT - 1)))
    }
}

const REDB_MAGIC: [u8; 9] = [b'r', b'e', b'd', b'b', 0x1A, 0x0A, 0xA9, 0x0D, 0x0A];
const GOD_RECOVERY: u8 = 2;
const GOD_TWO_PHASE: u8 = 4;

fn entry_info(e: &SnapEntry) -> SnapshotInfo {
    let mut info = e.info.clone();
    if e.tree.is_dirty() {
        info.root = e.tree.root_id();
    }
    info
}

/// A cheap `Send + Sync` handle to one snapshot. All methods take `&self`.
///
/// A snapshot is a full writable tree. Changes made through one handle never appear in another
/// snapshot, including the one it was forked from.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub(crate) h: Arc<Handle>,
    id: SnapshotId,
}

macro_rules! forward_writes {
    ($($(#[$doc:meta])* $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
        impl Snapshot {$(
            $(#[$doc])*
            pub fn $name(&self, $($arg: $ty),*) -> Result<$ret> {
                self.batch(|tx| tx.$name($($arg),*))
            }
        )*}
    };
}

forward_writes! {
    /// Creates an empty regular file. See [`Tx::create`].
    create(dir: Ino, name: &[u8], mode: u32) -> Attr;
    /// Creates an empty directory. See [`Tx::mkdir`].
    mkdir(dir: Ino, name: &[u8], mode: u32) -> Attr;
    /// Creates a symbolic link. See [`Tx::symlink`].
    symlink(dir: Ino, name: &[u8], target: &[u8]) -> Attr;
    /// Adds a hardlink. See [`Tx::link`].
    link(ino: Ino, dir: Ino, name: &[u8]) -> Attr;
    /// Removes a name of a file or symlink. See [`Tx::unlink`].
    unlink(dir: Ino, name: &[u8]) -> Removed;
    /// Removes an empty directory. See [`Tx::rmdir`].
    rmdir(dir: Ino, name: &[u8]) -> Removed;
    /// Renames atomically. See [`Tx::rename`].
    rename(from_dir: Ino, from_name: &[u8], to_dir: Ino, to_name: &[u8]) -> Option<Removed>;
    /// Changes mode, times or size. See [`Tx::setattr`].
    setattr(ino: Ino, set: SetAttr) -> Attr;
    /// Replaces a file's chunk list and size. See [`Tx::set_content`].
    set_content(ino: Ino, chunks: &[ChunkRef], size: u64) -> Attr;
    /// Compare-and-swap replacement of a chunk range. See [`Tx::splice_content`].
    splice_content(ino: Ino, expected_version: u64, start: u64, end: u64, new_chunks: &[ChunkRef], new_size: u64) -> u64;
    /// Sets an extended attribute. See [`Tx::setxattr`].
    setxattr(ino: Ino, name: &[u8], value: &[u8]) -> ();
    /// Removes an extended attribute. See [`Tx::removexattr`].
    removexattr(ino: Ino, name: &[u8]) -> ();
}

impl Snapshot {
    /// The snapshot id.
    pub fn id(&self) -> SnapshotId {
        self.id
    }

    /// Name, root, creation time and parent of this snapshot. The root includes applied changes
    /// that are not yet durable.
    pub fn info(&self) -> Result<SnapshotInfo> {
        let s = self.h.inner.rlock()?;
        s.snaps
            .get(&self.id)
            .map(entry_info)
            .ok_or(Error::NoSuchSnapshot)
    }

    /// The Merkle root of this snapshot's tree.
    pub fn root(&self) -> Result<NodeId> {
        Ok(self.info()?.root)
    }

    /// Creates a writable clone named `name`. Durable on return. Costs one row and one counter
    /// plus the commit, independent of tree size.
    pub fn fork(&self, name: &str) -> Result<Snapshot> {
        let id = self.h.inner.add_snapshot(name, Some(self.id))?;
        Ok(Snapshot {
            h: self.h.clone(),
            id,
        })
    }

    fn read<T>(&self, f: impl FnOnce(&View<'_>) -> Result<T>) -> Result<T> {
        let inner = &self.h.inner;
        let r = guard(|| {
            let s = inner.rlock()?;
            let e = s.snaps.get(&self.id).ok_or(Error::NoSuchSnapshot)?;
            let lazy = Lazy::new(&inner.db, &inner.cache);
            f(&View {
                tree: &e.tree,
                src: &lazy,
            })
        });
        if let Err(e) = &r {
            inner.note(e);
        }
        r
    }

    /// Looks up a name in a directory. `.` and `..` resolve.
    pub fn lookup(&self, dir: Ino, name: &[u8]) -> Result<Attr> {
        self.read(|r| read::lookup(r, dir, name))
    }

    /// Attributes of an inode.
    pub fn getattr(&self, ino: Ino) -> Result<Attr> {
        self.read(|r| read::getattr(r, ino))
    }

    /// Lists up to `max` entries after `cookie` (0 starts a listing).
    ///
    /// Cookies belong to entries and never change, so a listing resumed after removals neither
    /// repeats nor skips surviving entries.
    pub fn readdir(&self, dir: Ino, cookie: u64, max: usize) -> Result<ReadDir> {
        self.read(|r| read::readdir(r, dir, cookie, max))
    }

    /// Target of a symlink.
    pub fn readlink(&self, ino: Ino) -> Result<Vec<u8>> {
        self.read(|r| read::readlink(r, ino))
    }

    /// Whole chunk list of a regular file.
    pub fn chunks(&self, ino: Ino) -> Result<Vec<ChunkRef>> {
        self.read(|r| read::chunks(r, ino))
    }

    /// The chunks starting in byte range `start..end` with the content version they were read at.
    pub fn chunk_range(&self, ino: Ino, start: u64, end: u64) -> Result<ChunkRange> {
        self.read(|r| read::chunk_range(r, ino, start, end))
    }

    /// The content version of a file: the token `splice_content` compares.
    pub fn content_version(&self, ino: Ino) -> Result<u64> {
        self.read(|r| read::content_version(r, ino))
    }

    /// Value of an extended attribute.
    pub fn getxattr(&self, ino: Ino, name: &[u8]) -> Result<Vec<u8>> {
        self.read(|r| read::getxattr(r, ino, name))
    }

    /// Names of all extended attributes of an inode.
    pub fn listxattr(&self, ino: Ino) -> Result<Vec<Vec<u8>>> {
        self.read(|r| read::listxattr(r, ino))
    }

    /// Walks the tree yielding every block id referenced by a chunk list.
    ///
    /// Makes everything durable first (`sync`, including the hook), so the walk sees exactly the
    /// state a crash would leave and a collector never frees a block the durable tree needs.
    /// Subtrees whose root is already in `marker` are skipped, and each subtree is added to
    /// `marker` once fully walked. A block may be yielded more than once.
    pub fn live_blocks<'m>(&self, marker: &'m mut Marker) -> Result<LiveBlocks<'m>> {
        Ok(self.live_blocks_with_root(marker)?.1)
    }

    /// Like [`Snapshot::live_blocks`], but also reports the Merkle root the walk actually read.
    ///
    /// The root and the walked nodes come from one epoch: the session read lock is held while the
    /// root is read and the node table is opened, so a commit that changes this snapshot's root
    /// either finishes before both or after both. A caller that must record which root a walk
    /// covered (the collector's incremental marks) has to use the root returned here, not a root
    /// it read earlier: reading the root separately could name a root the walk never descended.
    pub fn live_blocks_with_root<'m>(
        &self,
        marker: &'m mut Marker,
    ) -> Result<(NodeId, LiveBlocks<'m>)> {
        let inner = &self.h.inner;
        inner.sync()?;
        guard(|| {
            // Hold the session read lock across both the root read and the read transaction, so
            // the root and the node table are the same epoch. A writer needs the write lock and
            // cannot slip a new root in between.
            let s = inner.rlock()?;
            let root = s
                .snaps
                .get(&self.id)
                .ok_or(Error::NoSuchSnapshot)?
                .info
                .root;
            let rtx = inner.db.begin_read()?;
            let walk = LiveBlocks::new(rtx.open_table(NODES)?, root, marker)?;
            drop(s);
            Ok((root, walk))
        })
    }

    /// Runs several operations as one atomic change: all apply or none do. The closure runs on the
    /// calling thread while it holds the writer lock, so it may `put` blocks and then reference
    /// them; the `before_sync` hook of any commit that carries this change runs after the closure.
    /// Do not call this store from inside the closure other than through the `Tx`.
    pub fn batch<T>(&self, f: impl FnOnce(&mut Tx<'_>) -> Result<T>) -> Result<T> {
        self.h.inner.mutate(self.id, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small blocks, so a range that crosses several of them still costs a fixed commit count.
    fn opts() -> Options {
        Options {
            node_size: 512,
            sync_every_ops: 1,
            background: false,
            ino_block: 8,
            ..Options::default()
        }
    }

    fn open(dir: &std::path::Path, name: &str) -> Meta {
        reset_reserve_probe();
        Meta::open(dir.join(name), opts()).unwrap()
    }

    /// The same small store, but a batch returns only once its change is durable.
    fn opts_durable() -> Options {
        Options {
            ack: Ack::Durable,
            ..opts()
        }
    }

    fn open_durable(dir: &std::path::Path, name: &str) -> Meta {
        reset_reserve_probe();
        Meta::open(dir.join(name), opts_durable()).unwrap()
    }

    /// The durable floor as the store holds it, read through a fresh read transaction.
    fn durable_reserved(m: &Meta) -> u64 {
        let rtx = m.h.inner.db.begin_read().unwrap();
        let meta = rtx.open_table(META).unwrap();
        meta_get(&meta, "ino_reserved").unwrap()
    }

    /// The pending bound as the store holds it, if one is there.
    fn durable_bound(m: &Meta) -> Option<u64> {
        let rtx = m.h.inner.db.begin_read().unwrap();
        let meta = rtx.open_table(META).unwrap();
        meta.get(INO_INTENT).unwrap().map(|g| g.value())
    }

    // T1: the regression this change exists for. The commit count must not follow `n`.
    #[test]
    fn a_large_reservation_costs_two_durable_commits_however_big() {
        let dir = tempfile::tempdir().unwrap();
        let m = open(dir.path(), "m.redb");
        let before = m.new_snapshot("s").unwrap().info().unwrap();

        reset_reserve_probe();
        let r = m.reserve_inodes(100_001).unwrap();
        let commits = reserve_commits();

        assert_eq!(r.len(), 100_001, "the whole range is handed back");
        assert!(
            commits <= 2,
            "a reservation must cost a fixed number of durable commits, got {commits}"
        );
        assert_eq!(
            commits, 2,
            "one commit for the bound and one for the floor move"
        );
        assert_eq!(
            durable_reserved(&m),
            r.end().0,
            "the floor lands exactly on the end of the range"
        );
        assert_eq!(durable_bound(&m), None, "the bound is spent by the move");

        // The same work under the old loop needed 12501 commits at this block size.
        let after = m.snapshot_by_id(before.id).unwrap().info().unwrap();
        assert_eq!(after.id, before.id, "the snapshot is untouched");
        m.check().expect("check after a large reservation");
    }

    // T2: a range the cached floor already covers must commit nothing at all. The cached floor
    // leads `next` after an ordinary create, which is the only way `next < reserved`.
    #[test]
    fn a_range_the_cached_floor_covers_commits_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let m = open(dir.path(), "m.redb");

        let s = m.new_snapshot("s").unwrap();
        s.create(ROOT_INO, b"f", 0o644).unwrap();
        m.sync().unwrap();
        let covered_to = durable_reserved(&m);
        let next = s.create(ROOT_INO, b"g", 0o644).unwrap();
        assert!(
            next.ino.0 + 4 <= covered_to,
            "the cached floor must lead next for this case to exist"
        );

        reset_reserve_probe();
        let r = m.reserve_inodes(4).unwrap();

        assert_eq!(
            reserve_commits(),
            0,
            "a covered range must not touch the store at all"
        );
        assert_eq!(r.start().0, next.ino.0 + 1, "hands out from next");
        assert!(r.end().0 <= covered_to, "and stays inside the cached floor");
    }

    // T3: the reason the bound is written first. A bound left behind by a lost floor move is what
    // recovery skips to, and it is the only thing that can cover a jump larger than one block.
    #[test]
    fn a_bound_left_behind_is_exactly_what_recovery_skips_to() {
        let dir = tempfile::tempdir().unwrap();
        let m = open(dir.path(), "m.redb");

        // The state redb leaves behind when it discards the newest commit: the bound is durable,
        // the floor move that was to follow it is not.
        let target = 500_000;
        m.h.inner.reserve_intent(target).unwrap();
        assert_eq!(durable_bound(&m), Some(target), "the bound survived");
        assert!(
            durable_reserved(&m) < target,
            "the floor move did not, which is the case under test"
        );

        let (_, ino_floor, _) = m.h.inner.record_recovery().unwrap();

        assert_eq!(
            ino_floor, target,
            "recovery must skip to the bound, not one block past the floor"
        );
        assert!(ino_floor > target - 8, "and one block alone would not have");
        assert_eq!(durable_bound(&m), None, "the bound is spent by recovery");
        assert_eq!(durable_reserved(&m), target, "and the floor is durable");

        // No number below the floor can come back after a reopen.
        drop(m);
        let again = Meta::open(dir.path().join("m.redb"), opts()).unwrap();
        let r = again.reserve_inodes(4).unwrap();
        assert!(
            r.start().0 >= target,
            "the allocator resumes at or above the recovered floor, got {}",
            r.start().0
        );
    }

    // T4: a file written before bounds existed still recovers, by the one-block rule it always used.
    #[test]
    fn a_file_without_a_bound_still_recovers_by_one_block() {
        let dir = tempfile::tempdir().unwrap();
        let m = open(dir.path(), "m.redb");
        let reserved = durable_reserved(&m);
        assert_eq!(durable_bound(&m), None, "no bound is present");

        let block = opts().ino_block;
        let (_, ino_floor, _) = m.h.inner.record_recovery().unwrap();

        assert_eq!(
            ino_floor,
            reserved + block,
            "a legacy file keeps the one-block skip it has always used"
        );
    }

    // T5: failing before anything is persisted must expose no number and consume nothing.
    #[test]
    fn a_failure_before_persisting_exposes_no_number_and_consumes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let m = open(dir.path(), "m.redb");
        let before = durable_reserved(&m);

        RESERVE_FAULT.with(|c| c.set(1));
        let e = m.reserve_inodes(4096).unwrap_err();
        assert!(matches!(e, Error::Storage(_)), "{e:?}");

        assert_eq!(
            durable_reserved(&m),
            before,
            "a commit that never ran cannot have moved the floor"
        );

        // The allocator did not move, so the same numbers are still available and nothing was
        // handed out in between.
        reset_reserve_probe();
        let r = m.reserve_inodes(4096).unwrap();
        assert_eq!(r.len(), 4096);
        assert!(
            r.start().0 < before + 4096,
            "the refused call consumed nothing"
        );
    }

    // T6: the uncertain outcome. The floor move persisted and the caller still saw an error, so the
    // floor is ahead of anything this process was told. That is safe precisely because it only ever
    // moves forward, and a retry cannot reissue what was already skipped.
    #[test]
    fn a_failure_after_the_floor_persisted_leaves_the_floor_ahead_and_never_reissues() {
        let dir = tempfile::tempdir().unwrap();
        let m = open(dir.path(), "m.redb");
        let before = durable_reserved(&m);

        RESERVE_FAULT.with(|c| c.set(2));
        let e = m.reserve_inodes(4096).unwrap_err();
        assert!(matches!(e, Error::Storage(_)), "{e:?}");

        let after = durable_reserved(&m);
        assert_eq!(
            after,
            before + 4096,
            "the commit really did persist before the error was returned"
        );

        // Recovery has nothing to skip, because the move is durable and the bound is spent.
        let (_, ino_floor, _) = m.h.inner.record_recovery().unwrap();
        assert_eq!(ino_floor, after + opts().ino_block);

        // Reopening must not hand the skipped range back, even though the caller saw an error and
        // believes it holds nothing.
        reset_reserve_probe();
        drop(m);
        let again = Meta::open(dir.path().join("m.redb"), opts()).unwrap();
        let r = again.reserve_inodes(8).unwrap();
        assert!(
            r.start().0 >= after,
            "a number the failed call covered must not come back, got {}",
            r.start().0
        );
        again.check().unwrap();
    }

    // T7: failing before the bound commit must also expose nothing.
    #[test]
    fn a_failure_before_the_bound_commits_exposes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let m = open(dir.path(), "m.redb");
        let before = durable_reserved(&m);

        RESERVE_FAULT.with(|c| c.set(3));
        let e = m.reserve_inodes(4096).unwrap_err();
        assert!(matches!(e, Error::Storage(_)), "{e:?}");

        assert_eq!(durable_reserved(&m), before, "the floor never moved");
        assert_eq!(durable_bound(&m), None, "and no bound was left behind");

        reset_reserve_probe();
        let r = m.reserve_inodes(4096).unwrap();
        assert_eq!(r.len(), 4096, "the same range is still available");
        m.check().unwrap();
    }

    // T8: an ordinary create and a reservation still draw from one allocator after all of this.
    #[test]
    fn ordinary_creation_still_starts_above_a_large_reserved_range() {
        let dir = tempfile::tempdir().unwrap();
        let m = open(dir.path(), "m.redb");
        let r = m.reserve_inodes(50_000).unwrap();
        m.sync().unwrap();

        let s = m.new_snapshot("s").unwrap();
        let f = s.create(ROOT_INO, b"f", 0o644).unwrap();
        assert!(
            f.ino.0 >= r.end().0,
            "creation must not land inside a reserved range: {} < {}",
            f.ino.0,
            r.end().0
        );
        m.check().unwrap();
    }

    // T9: a ticket creates at exactly the number it names, and is spent by the create.
    #[test]
    fn a_ticket_creates_at_its_number_and_is_spent_once() {
        let dir = tempfile::tempdir().unwrap();
        let m = open(dir.path(), "m.redb");
        let s = m.new_snapshot("s").unwrap();
        let tickets = m.reserve_tickets(3).unwrap();
        let want = tickets[0].ino();

        let a = s
            .batch(|tx| tx.create_at(ROOT_INO, b"a", 0o644, &tickets[0]))
            .unwrap();
        assert_eq!(a.ino, want, "the create landed at the ticket's number");
        assert_eq!(
            s.getattr(want).unwrap().ino,
            want,
            "the inode is really at that number"
        );

        // the same ticket cannot be spent again
        let again = s.batch(|tx| tx.create_at(ROOT_INO, b"b", 0o644, &tickets[0]));
        assert!(
            again.is_err(),
            "a spent ticket created a second inode: {again:?}"
        );

        // a different ticket still works
        let b = s
            .batch(|tx| tx.create_at(ROOT_INO, b"c", 0o644, &tickets[1]))
            .unwrap();
        assert_eq!(b.ino, tickets[1].ino());
        m.check().unwrap();
    }

    // T10: a ticket is refused by a different store, even when the number is the same.
    #[test]
    fn a_ticket_from_another_store_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let a = open(dir.path(), "a.redb");
        let b = Meta::open(dir.path().join("b.redb"), opts()).unwrap();
        let sa = a.new_snapshot("s").unwrap();
        let tickets = a.reserve_tickets(1).unwrap();
        let foreign = &tickets[0];

        // store b's own reservation can hand out the same numeric inode; the foreign ticket is
        // still refused because it carries store a's identity
        let err = sa.batch(|tx| tx.create_at(ROOT_INO, b"x", 0o644, foreign));
        assert!(
            err.is_err(),
            "a foreign store's ticket was accepted: {err:?}"
        );
        let _ = b;
    }

    // T11: a ticket minted by a session that has closed is refused by the reopened store.
    #[test]
    fn a_ticket_from_a_closed_session_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let m = open(dir.path(), "m.redb");
        let s = m.new_snapshot("s").unwrap();
        let tickets = m.reserve_tickets(1).unwrap();
        drop(s);
        drop(m);

        let again = Meta::open(dir.path().join("m.redb"), opts()).unwrap();
        let s2 = again.snapshot_by_id(SnapshotId(1)).unwrap();
        let err = s2.batch(|tx| tx.create_at(ROOT_INO, b"x", 0o644, &tickets[0]));
        assert!(err.is_err(), "a stale ticket was accepted: {err:?}");
    }

    // T12: a closure error does not spend the ticket, so a retry uses the same number.
    #[test]
    fn a_closure_error_leaves_the_ticket_usable_for_retry() {
        let dir = tempfile::tempdir().unwrap();
        let m = open(dir.path(), "m.redb");
        let s = m.new_snapshot("s").unwrap();
        let tickets = m.reserve_tickets(1).unwrap();
        let want = tickets[0].ino();

        let failed = s.batch(|tx| {
            tx.create_at(ROOT_INO, b"a", 0o644, &tickets[0])?;
            Err::<(), _>(Error::Invalid("deliberate failure after the create"))
        });
        assert!(
            failed.is_err(),
            "the batch was expected to fail: {failed:?}"
        );
        assert!(
            matches!(s.getattr(want), Err(Error::NotFound)),
            "the failed batch must have left no inode"
        );

        let ok = s
            .batch(|tx| tx.create_at(ROOT_INO, b"a", 0o644, &tickets[0]))
            .unwrap();
        assert_eq!(ok.ino, want, "the retry reused the ticket's number");
        m.check().unwrap();
    }

    // T13: a durable commit that fails before it persists must not strand the reserved number.
    //
    // The batch returns an error, so the caller retries. With the removal of the number from the
    // session's outstanding set placed before the commit, the retry was refused as if the ticket
    // had never been minted. Here the number stays owned by the session, so the retry is stopped
    // only by the create that is still pending in the tree, never by a lost reservation.
    #[test]
    fn a_failed_durable_commit_does_not_strand_the_reserved_number() {
        let dir = tempfile::tempdir().unwrap();
        let m = open_durable(dir.path(), "m.redb");
        let s = m.new_snapshot("s").unwrap();
        let tickets = m.reserve_tickets(1).unwrap();
        let want = tickets[0].ino();

        set_commit_fault(1);
        let failed = s.batch(|tx| tx.create_at(ROOT_INO, b"a", 0o644, &tickets[0]));
        set_commit_fault(0);
        assert!(
            failed.is_err(),
            "the durable commit was expected to fail: {failed:?}"
        );

        let retry = s.batch(|tx| tx.create_at(ROOT_INO, b"a", 0o644, &tickets[0]));
        if let Err(e) = &retry {
            assert!(
                !e.to_string().contains("was not issued"),
                "the failed durable commit stranded the reserved number: {e:?}"
            );
        }
        m.check().unwrap();
    }

    // T14: a durable commit that fails after it persisted must not let the number be created twice.
    //
    // The caller sees an error and retries, but the first create is already on disk, so the retry
    // at the same number is refused rather than silently duplicating it.
    #[test]
    fn a_durable_commit_that_persisted_then_failed_does_not_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let m = open_durable(dir.path(), "m.redb");
        let s = m.new_snapshot("s").unwrap();
        let tickets = m.reserve_tickets(1).unwrap();
        let want = tickets[0].ino();

        set_commit_fault(2);
        let failed = s.batch(|tx| tx.create_at(ROOT_INO, b"a", 0o644, &tickets[0]));
        set_commit_fault(0);
        assert!(
            failed.is_err(),
            "the durable commit was expected to fail: {failed:?}"
        );

        // the create did persist, so the inode is there at the same number and the retry cannot
        // make a second one
        let got = s
            .getattr(want)
            .expect("the persisted create must be visible");
        assert_eq!(
            got.ino, want,
            "the persisted create is not at the reserved number"
        );
        let retry = s.batch(|tx| tx.create_at(ROOT_INO, b"a", 0o644, &tickets[0]));
        assert!(
            retry.is_err(),
            "a retry after a persisted-but-failed commit duplicated the inode: {retry:?}"
        );
    }
}
