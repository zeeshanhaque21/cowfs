//! Shared state, node loading, and the flush and commit machinery.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use cowfs_meta::Meta;
use cowfs_vfs::{Attr, Error, FileKind, Ino, Result, Timestamp};

use crate::blocks::Blocks;
use crate::dcache::{DCache, Target};
use crate::error::{from_meta, stale};
use crate::file::FileData;
use crate::gate::{Entry, Gate};
use crate::ino::{classify, pack, snap_of, Aliases, Id};
use crate::node::{Node, NodeState};
use crate::queue::{Batch, Create, Op, SnapCtx};
use crate::util::{MutexExt, RwExt, ShardMap, SHARDS};

/// How many reserved inode tickets one meta reservation covers. A create pops one, so a refill
/// happens once per this many creates and each refill is one durable meta reservation.
pub(crate) const RESERVED_BLOCK: u64 = 1 << 16;

/// Tuning knobs for [`Core::open`](crate::Core::open). See `docs/v1-core.md` for the exact
/// durability and loss bounds they control.
#[derive(Clone, Debug)]
pub struct Options {
    /// Options of the block store.
    pub store: cowfs_store::Options,
    /// Options of the metadata database. `before_sync` is overwritten with the store's sync.
    pub meta: cowfs_meta::Options,
    /// Queued operations older than this are committed by the background flusher.
    pub flush_interval: Duration,
    /// Commits that are not durable yet are made durable after this long.
    pub sync_interval: Duration,
    /// A snapshot's queue is committed when it holds this many operations.
    pub max_pending_ops: usize,
    /// Writers flush file data to the store when unflushed bytes exceed this.
    pub max_dirty_bytes: usize,
    /// A file's unflushed bytes are chunked and stored when they reach this.
    pub file_flush_bytes: usize,
    /// Dentry cache bound, in entries.
    pub dentry_cache: usize,
    /// Node table bound, in nodes.
    pub node_cache: usize,
    /// Block cache bound, in bytes.
    pub block_cache_bytes: usize,
    /// Run the background flusher thread. Without it only explicit flushes and thresholds commit.
    pub background: bool,
    /// How many session aliases the mount keeps at once. Every inode a session creates needs one,
    /// and it is released when the inode is unlinked and unreferenced, so this is the ceiling on how
    /// many files one session may create. A create past it is `Error::NoSpace` rather than a number
    /// that later goes stale.
    pub alias_limit: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            store: cowfs_store::Options::default(),
            meta: cowfs_meta::Options::default(),
            flush_interval: Duration::from_millis(500),
            sync_interval: Duration::from_secs(1),
            max_pending_ops: 4096,
            max_dirty_bytes: 128 << 20,
            file_flush_bytes: 4 << 20,
            dentry_cache: 262_144,
            node_cache: 131_072,
            block_cache_bytes: 128 << 20,
            background: true,
            alias_limit: 1 << 20,
        }
    }
}

/// Counters for tests, benchmarks and diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Meta batches committed.
    pub batches: u64,
    /// Operations committed in those batches.
    pub ops_committed: u64,
    /// Creates cancelled by an unlink before they were committed.
    pub elided: u64,
    /// Flushes forced by an operation that needed meta to be current.
    pub barriers: u64,
    /// `forget` calls that named more references than existed.
    pub forget_underflows: u64,
    /// Flushes that failed.
    pub flush_errors: u64,
    /// Files whose flush failed and are poisoned.
    pub poisoned: u64,
    /// Flush failures that kept their data pending (transient, retried).
    pub transient: u64,
    /// Virtual aliases released because nothing held their number.
    pub aliases_dropped: u64,
    /// Dentry cache hits and misses.
    pub dentry_hits: u64,
    /// See `dentry_hits`.
    pub dentry_misses: u64,
    /// Nodes in the node table.
    pub nodes: usize,
    /// Entries in the dentry cache.
    pub dentries: usize,
    /// Virtual inode aliases.
    pub aliases: usize,
    /// Unflushed file bytes.
    pub dirty_bytes: usize,
    /// Operations queued and not committed, over all snapshots.
    pub pending_ops: usize,
}

#[derive(Debug, Default)]
pub(crate) struct Counters {
    pub(crate) batches: AtomicU64,
    pub(crate) ops: AtomicU64,
    pub(crate) elided: AtomicU64,
    pub(crate) barriers: AtomicU64,
    pub(crate) underflows: AtomicU64,
    pub(crate) flush_errors: AtomicU64,
    /// Files whose flush failed and are therefore poisoned.
    pub(crate) poisoned: AtomicU64,
    /// Flush failures that kept their data pending (transient, retried).
    pub(crate) transient: AtomicU64,
    pub(crate) aliases_dropped: AtomicU64,
    pub(crate) dhit: AtomicU64,
    pub(crate) dmiss: AtomicU64,
    pub(crate) inodes_net: AtomicI64,
}

/// One file that cannot currently be written, from `Core::health`.
#[derive(Debug, Clone)]
pub struct FileHealth {
    /// The file's inode number.
    pub ino: Ino,
    /// The snapshot holding it, if any.
    pub snapshot: Option<u64>,
    /// True when the failure was a corruption: the file is dead until `Core::unpoison`.
    pub poisoned: bool,
    /// Why it last failed.
    pub reason: Option<String>,
}

/// One snapshot's commit lane, from `Core::health`.
#[derive(Debug, Clone)]
pub struct LaneHealth {
    /// The snapshot's name.
    pub snapshot: String,
    /// The snapshot's id.
    pub id: u64,
    /// Files whose data is still pending, so this lane is not making progress on them.
    pub files_stuck: usize,
}

/// What `Core::health` reports.
#[derive(Debug, Clone, Default)]
pub struct Health {
    /// Poisoned files first, then files whose last flush failed transiently.
    pub files: Vec<FileHealth>,
    /// Snapshots with files still pending.
    pub lanes: Vec<LaneHealth>,
    /// The last error a flush reported, whatever its kind.
    pub last_error: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Snaps {
    pub(crate) by_id: HashMap<u64, Arc<SnapCtx>>,
    pub(crate) by_name: BTreeMap<String, u64>,
}

pub(crate) struct Inner {
    pub(crate) meta: Meta,
    pub(crate) blocks: Blocks,
    /// The reference gate; see `docs/gc-core-integration.md`. The outermost lock.
    pub(crate) gate: Arc<Gate>,
    pub(crate) opts: Options,
    pub(crate) snaps: RwLock<Snaps>,
    pub(crate) nodes: ShardMap<Ino, Arc<Node>>,
    pub(crate) dents: DCache,
    pub(crate) aliases: RwLock<Aliases>,
    pub(crate) handles: Mutex<HashMap<u64, Ino>>,
    pub(crate) next_handle: AtomicU64,
    pub(crate) next_virt: AtomicU64,
    /// Virtual numbers up to here are recorded in `<root>/virt.ino`, so a restart never hands out
    /// a number an earlier session used.
    pub(crate) virt_reserved: AtomicU64,
    pub(crate) virt_lock: Mutex<()>,
    /// Tickets minted from a [`cowfs_meta::Meta::reserve_tickets`] batch and not yet spent by a
    /// create. A create pops one here; the pool refills a block ahead so a create does not hold a
    /// Core lock while meta's writer lock is taken. Numbers in a popped-but-unused ticket are
    /// wasted, never reused.
    pub(crate) reserved: Mutex<Vec<cowfs_meta::ReservedIno>>,
    pub(crate) dirty_bytes: AtomicUsize,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
    pub(crate) root_time: Mutex<Timestamp>,
    pub(crate) ctr: Counters,
    pub(crate) pressure: Mutex<()>,
    pub(crate) bg: (Mutex<bool>, Condvar),
    pub(crate) unsynced: Mutex<Option<Instant>>,
    pub(crate) last_error: Mutex<Option<String>>,
    pub(crate) capacity_blocks: u64,
    pub(crate) base_pack_bytes: u64,
    /// The mount root directory, which holds the intent files of an interrupted snapshot swap.
    pub(crate) root: std::path::PathBuf,
    /// Test seam for the swap, see `Core::set_swap_fault`.
    pub(crate) swap_fault: std::sync::atomic::AtomicU8,
    /// Test seam: the inode whose node-table insertions lose this many races before they win.
    pub(crate) load_node_contention: Mutex<Option<(Ino, usize)>>,
    /// Test seam: per-inode flush faults `(kind, times remaining)`, see `Core::set_flush_fault`.
    pub(crate) flush_fault: Mutex<HashMap<Ino, (u8, u32)>>,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Inner").finish_non_exhaustive()
    }
}

pub(crate) fn kind_of(t: cowfs_meta::FileType) -> FileKind {
    match t {
        cowfs_meta::FileType::File => FileKind::Regular,
        cowfs_meta::FileType::Dir => FileKind::Directory,
        cowfs_meta::FileType::Symlink => FileKind::Symlink,
    }
}

pub(crate) fn to_meta_ts(t: Timestamp) -> cowfs_meta::Timestamp {
    cowfs_meta::Timestamp {
        secs: t.secs,
        nanos: t.nanos,
    }
}

fn from_meta_ts(t: cowfs_meta::Timestamp) -> Timestamp {
    Timestamp {
        secs: t.secs,
        nanos: t.nanos,
    }
}

pub(crate) fn mino(m: u64) -> cowfs_meta::Ino {
    cowfs_meta::Ino(m)
}

impl Inner {
    pub(crate) fn snapctx_id(&self, id: u64) -> Result<Arc<SnapCtx>> {
        self.snaps.rd().by_id.get(&id).cloned().ok_or(Error::Stale)
    }

    pub(crate) fn snapctx(&self, ino: Ino) -> Result<Arc<SnapCtx>> {
        self.snapctx_id(snap_of(ino).ok_or(Error::Stale)?)
    }

    pub(crate) fn all_snaps(&self) -> Vec<Arc<SnapCtx>> {
        self.snaps.rd().by_id.values().cloned().collect()
    }

    /// The meta inode number behind `ino`, if meta has one yet.
    /// The next virtual number, extending the durable reservation when it runs out.
    ///
    /// Refuses at the alias ceiling: the number would be handed out now and released later, and a
    /// client still holding it would see `Stale`, so the create fails here instead.
    pub(crate) fn alloc_virt(&self, snap: u64) -> Result<Ino> {
        let live = self.aliases.rd().len();
        if live >= self.opts.alias_limit {
            *self.last_error.lk() = Some(format!(
                "session inode limit reached: {live} inodes are live, the ceiling is {}",
                self.opts.alias_limit
            ));
            return Err(Error::NoSpace);
        }
        let n = self.next_virt.fetch_add(1, Ordering::AcqRel) + 1;
        if n >= self.virt_reserved.load(Ordering::Acquire) {
            self.reserve_virt()?;
        }
        crate::ino::virt(snap, n)
    }

    /// Records a block of virtual numbers durably before any of them is handed out, so a crash
    /// can only waste numbers, never reuse them.
    fn reserve_virt(&self) -> Result<()> {
        let _g = self.virt_lock.lk();
        let next = self.next_virt.load(Ordering::Acquire);
        if next < self.virt_reserved.load(Ordering::Acquire) {
            return Ok(());
        }
        let new = next.saturating_add(crate::ino::VIRT_BLOCK);
        crate::ino::write_virt_mark(&self.root, new).map_err(|e| crate::error::from_io(&e))?;
        self.virt_reserved.store(new, Ordering::Release);
        Ok(())
    }

    /// One reserved ticket for a create, refilling the pool in blocks.
    ///
    /// Takes no snapshot lock: a caller reserves before it takes `sc.ns`. A refill opens a meta
    /// reservation, which no Core lock may be held across.
    pub(crate) fn take_reserved(&self) -> Result<cowfs_meta::ReservedIno> {
        {
            let mut pool = self.reserved.lk();
            if let Some(t) = pool.pop() {
                return Ok(t);
            }
        }
        let block = self
            .meta
            .reserve_tickets(RESERVED_BLOCK)
            .map_err(from_meta)?;
        let mut pool = self.reserved.lk();
        let mut it = block.into_iter();
        let first = it.next().ok_or(Error::NoSpace)?;
        pool.extend(it);
        Ok(first)
    }

    /// Test seam: report that the next node-table insertion for `ino` lost its race, up to the
    /// armed count. 0 disables it.
    fn lose_the_next_insert(&self, ino: Ino) -> bool {
        let mut f = self.load_node_contention.lk();
        match *f {
            Some((target, left)) if (target == ino || target == 0) && left > 0 => {
                *f = Some((target, left - 1));
                true
            }
            _ => false,
        }
    }

    pub(crate) fn meta_of(&self, ino: Ino) -> Option<u64> {
        match classify(ino) {
            Id::Meta { m, .. } => Some(m),
            Id::Virt { .. } => self.aliases.rd().meta_of(ino),
            Id::Root => None,
        }
    }

    pub(crate) fn canon(&self, snap: u64, m: u64) -> Result<Ino> {
        if let Some(v) = self.aliases.rd().canon(snap, m) {
            return Ok(v);
        }
        pack(snap, m)
    }

    pub(crate) fn attr_from_meta(&self, ino: Ino, a: &cowfs_meta::Attr) -> Attr {
        Attr {
            ino,
            kind: kind_of(a.kind),
            mode: a.mode,
            nlink: a.nlink,
            uid: self.uid,
            gid: self.gid,
            size: a.size,
            blocks: 0,
            atime: from_meta_ts(a.atime),
            mtime: from_meta_ts(a.mtime),
            ctime: from_meta_ts(a.ctime),
        }
    }

    pub(crate) fn flushed_of(&self) -> HashMap<u64, u64> {
        self.snaps
            .rd()
            .by_id
            .iter()
            .map(|(id, sc)| (*id, sc.flushed()))
            .collect()
    }

    /// The node for `ino`, loaded from meta when it is not cached. `Stale` if there is none.
    pub(crate) fn node(&self, ino: Ino) -> Result<Arc<Node>> {
        if let Some(n) = self.nodes.get(&ino) {
            return Ok(n);
        }
        self.load_node(ino)
    }

    fn load_node(&self, ino: Ino) -> Result<Arc<Node>> {
        let (snap, m) = match classify(ino) {
            Id::Root => return Err(Error::Stale),
            Id::Meta { snap, m } => {
                let v = self.aliases.rd().canon(snap, m);
                if let Some(v) = v {
                    return self.nodes.get(&v).map_or_else(|| self.node(v), Ok);
                }
                (snap, m)
            }
            Id::Virt { snap } => (snap, self.aliases.rd().meta_of(ino).ok_or(Error::Stale)?),
        };
        let sc = self.snapctx_id(snap)?;
        for tries in 0.. {
            let epoch = self.nodes.epoch(&ino);
            let a = sc.snap.getattr(mino(m)).map_err(from_meta).map_err(stale)?;
            let st = NodeState {
                attr: self.attr_from_meta(ino, &a),
                target: None,
                file: None,
                xattrs: None,
                kids: None,
            };
            let node = Arc::new(Node::new(ino, st));
            // a competing write to the shard makes the epoch read above stale, so the insert loses
            if self.lose_the_next_insert(ino) {
                self.nodes.bump_shard_of(&ino);
            }
            match self.nodes.insert_if(ino, node.clone(), epoch) {
                Ok(n) => {
                    self.shrink_nodes(ino);
                    return Ok(n);
                }
                // a live node for this inode keeps changing; never overwrite it with a state read
                // from meta, or a dirty file's unflushed extents are lost
                Err(()) if tries < 64 => {}
                Err(()) => return Err(Error::Stale),
            }
        }
        Err(Error::Stale)
    }

    /// The node for `ino`, unless it is an unlinked file nobody holds any more.
    pub(crate) fn live(&self, ino: Ino) -> Result<Arc<Node>> {
        let n = self.node(ino)?;
        if !n.pinned() && n.st.rd().attr.nlink == 0 {
            return Err(Error::Stale);
        }
        Ok(n)
    }

    /// A live directory node together with its snapshot.
    ///
    /// An unlinked directory nobody holds is `Stale`, not a silent "no such name".
    pub(crate) fn dir(&self, ino: Ino) -> Result<(Arc<SnapCtx>, Arc<Node>)> {
        let sc = self.snapctx(ino)?;
        let n = self.live(ino)?;
        if n.st.rd().attr.kind != FileKind::Directory {
            return Err(Error::NotDir);
        }
        Ok((sc, n))
    }

    pub(crate) fn shrink_nodes(&self, near: Ino) {
        let cap = self.opts.node_cache / SHARDS + 1;
        if self.nodes.shard_len(&near) <= cap {
            return;
        }
        let flushed = self.flushed_of();
        self.nodes
            .shrink_shard_of(&near, cap, cap * 3 / 4, |ino, n| {
                let fl = snap_of(*ino).and_then(|s| flushed.get(&s)).copied();
                let Some(fl) = fl else { return true };
                Arc::strong_count(n) == 1
                    && !n.pinned()
                    && n.seq.load(Ordering::Acquire) <= fl
                    && n.ns_seq.load(Ordering::Acquire) <= fl
                    && n.st.try_read().is_ok_and(|s| s.dirty_bytes() == 0)
            });
    }

    pub(crate) fn shrink_dents(&self, near: Ino) {
        if !self.dents.over_cap(near) {
            return;
        }
        let flushed = self.flushed_of();
        self.dents.shrink(near, &|d| {
            snap_of(d)
                .and_then(|s| flushed.get(&s))
                .copied()
                .unwrap_or(u64::MAX)
        });
    }

    /// Caches a node from attributes meta just returned, so the caller's next `node()` needs no read.
    pub(crate) fn seed_node(&self, ino: Ino, a: &cowfs_meta::Attr) {
        if self.nodes.get(&ino).is_some() {
            return;
        }
        let epoch = self.nodes.epoch(&ino);
        let st = NodeState {
            attr: self.attr_from_meta(ino, a),
            target: None,
            file: None,
            xattrs: None,
            kids: None,
        };
        if self
            .nodes
            .insert_if(ino, Arc::new(Node::new(ino, st)), epoch)
            .is_ok()
        {
            self.shrink_nodes(ino);
        }
    }

    /// Resolves `name` in `parent` through the dentry cache, then meta.
    /// Resolves `name` in `parent` through the dentry cache, then meta.
    ///
    /// The cache stores META-derived inode numbers only, and every hit is canonicalised through
    /// [`Inner::canon`], so releasing a virtual alias (nothing holds its number any more) cannot
    /// leave a cached entry pointing at a number the mount no longer knows.
    pub(crate) fn dent_lookup(&self, sc: &SnapCtx, parent: &Node, name: &[u8]) -> Result<Target> {
        let canon = |t: Target| -> Result<Target> {
            t.map(|(i, k)| match classify(i) {
                // a pending create still has its virtual number; it is re-pointed at the meta
                // number when the batch commits, and an unlink replaces it
                Id::Meta { snap, m } => Ok((self.canon(snap, m)?, k)),
                _ => Ok((i, k)),
            })
            .transpose()
        };
        if let Some(d) = self.dents.get(parent.ino, name) {
            self.ctr.dhit.fetch_add(1, Ordering::Relaxed);
            return canon(d.target);
        }
        self.ctr.dmiss.fetch_add(1, Ordering::Relaxed);
        let Some(pm) = self.meta_of(parent.ino) else {
            return Ok(None);
        };
        let epoch = self.dents.epoch(parent.ino);
        match sc.snap.lookup(mino(pm), name) {
            Ok(a) => {
                let meta_ino = pack(sc.id, a.ino.0)?;
                let t = Some((meta_ino, kind_of(a.kind)));
                self.dents.fill(parent.ino, name, t, epoch);
                self.shrink_dents(parent.ino);
                let out = canon(t);
                if let Ok(Some((c, _))) = out {
                    self.seed_node(c, &a);
                }
                out
            }
            Err(cowfs_meta::Error::NotFound) => {
                self.dents.fill(parent.ino, name, None, epoch);
                Ok(None)
            }
            Err(e) => Err(from_meta(e)),
        }
    }

    /// Loads a file's chunk list from meta if it is not loaded yet.
    ///
    /// The meta read happens with no node lock held (lock order: the meta lock is a leaf), and the
    /// result is published under the node lock. If another thread loaded one meanwhile, the first
    /// result wins, which is the same bytes.
    pub(crate) fn ensure_file(&self, sc: &SnapCtx, node: &Node) -> Result<()> {
        if node.st.rd().file.is_some() {
            return Ok(());
        }
        let refs = match self.meta_of(node.ino) {
            Some(m) => sc.snap.chunks(mino(m)).map_err(from_meta).map_err(stale)?,
            None => Vec::new(),
        };
        let mut st = node.st.wr();
        if st.file.is_none() {
            st.file = Some(FileData::new(refs));
        }
        Ok(())
    }

    pub(crate) fn ensure_target(&self, sc: &SnapCtx, node: &Node) -> Result<Arc<[u8]>> {
        if let Some(t) = &node.st.rd().target {
            return Ok(t.clone());
        }
        let m = self.meta_of(node.ino).ok_or(Error::Stale)?;
        let t: Arc<[u8]> = sc
            .snap
            .readlink(mino(m))
            .map_err(from_meta)
            .map_err(stale)?
            .into();
        let mut st = node.st.wr();
        Ok(st.target.get_or_insert_with(|| t.clone()).clone())
    }

    /// Gives a node that is about to become an orphan everything meta is about to drop.
    pub(crate) fn preserve_orphan(&self, sc: &SnapCtx, node: &Node) -> Result<()> {
        let kind = node.st.rd().attr.kind;
        match kind {
            FileKind::Regular => self.ensure_file(sc, node)?,
            FileKind::Symlink => {
                self.ensure_target(sc, node)?;
            }
            FileKind::Directory => {}
            _ => {}
        }
        if node.st.rd().xattrs.is_none() {
            let mut map = BTreeMap::new();
            if let Some(m) = self.meta_of(node.ino) {
                for name in sc
                    .snap
                    .listxattr(mino(m))
                    .map_err(from_meta)
                    .map_err(stale)?
                {
                    let v = sc
                        .snap
                        .getxattr(mino(m), &name)
                        .map_err(from_meta)
                        .map_err(stale)?;
                    map.insert(name, v);
                }
            }
            node.st.wr().xattrs = Some(map);
        }
        Ok(())
    }

    /// Drops a committed, clean, unreferenced node from the node table, so a file a session created
    /// does not keep a node for the rest of the session. Its alias stays: the number a client was
    /// handed has to keep naming this inode, and reloading it from meta resolves the alias.
    fn maybe_evict_node(&self, node: &Arc<Node>) {
        if node.pinned()
            || node
                .st
                .try_read()
                .map_or(true, |st| st.dirty_bytes() > 0 || st.attr.nlink == 0)
        {
            return;
        }
        let Ok(sc) = self.snapctx(node.ino) else {
            return;
        };
        if node.seq.load(Ordering::Acquire) > sc.flushed()
            || node.ns_seq.load(Ordering::Acquire) > sc.flushed()
        {
            return;
        }
        let ino = node.ino;
        self.nodes.remove_if(&ino, |n| {
            Arc::ptr_eq(n, node) && !n.pinned() && n.seq.load(Ordering::Acquire) <= sc.flushed()
        });
    }

    /// Drops an orphan nobody holds once its removal is committed.
    pub(crate) fn try_reclaim(&self, node: &Arc<Node>) {
        if node.pinned() {
            return;
        }
        let Ok(sc) = self.snapctx(node.ino) else {
            return;
        };
        let fl = sc.flushed();
        let never_committed = node.elided.load(Ordering::Acquire);
        if !never_committed
            && (node.seq.load(Ordering::Acquire) > fl || node.ns_seq.load(Ordering::Acquire) > fl)
        {
            return;
        }
        if node.st.rd().attr.nlink != 0 {
            return;
        }
        let removed = self
            .nodes
            .remove_if(&node.ino, |n| Arc::ptr_eq(n, node) && !n.pinned());
        if removed {
            self.aliases.wr().remove(node.ino);
            let n = node.st.wr().file.as_mut().map_or(0, FileData::discard);
            self.dirty_bytes.fetch_sub(n, Ordering::AcqRel);
        }
    }

    /// Queues the file's current chunk list and size.
    pub(crate) fn queue_content(&self, sc: &SnapCtx, node: &Node, st: &NodeState) {
        if let Some(f) = &st.file {
            if st.attr.nlink > 0 {
                sc.q.lk().set_content(node, f.chunks.clone(), st.attr.size);
            }
        }
    }

    /// Flushes one file's dirty bytes to the store and queues its chunk list.
    pub(crate) fn flush_locked(
        &self,
        sc: &SnapCtx,
        node: &Node,
        st: &mut NodeState,
        entry: &Entry<'_>,
    ) -> Result<()> {
        let Some(f) = st.file.as_mut() else {
            return Ok(());
        };
        if f.is_clean() {
            return Ok(());
        }
        if let Some(e) = self.take_flush_fault(node.ino) {
            return Err(e);
        }
        let n = f.dirty_bytes();
        f.flush(&self.blocks, entry)?;
        self.dirty_bytes.fetch_sub(n, Ordering::AcqRel);
        self.queue_content(sc, node, st);
        Ok(())
    }

    fn flush_node(&self, sc: &SnapCtx, node: &Node, entry: &Entry<'_>) -> Result<()> {
        let mut st = node.st.wr();
        self.flush_locked(sc, node, &mut st, entry)
    }

    /// How often a transient flush failure is retried inside one flush, with a short backoff.
    const FLUSH_RETRIES: u32 = 3;

    /// Chunks and stores every file with unflushed bytes in `sc`.
    ///
    /// A corruption poisons the file and drops it from the dirty set, so the rest of the snapshot
    /// still commits and the queue cannot be wedged by one damaged block. The file's bytes stay in
    /// memory and stay counted, and `fsync` of that file reports the error until `Core::unpoison`.
    ///
    /// A transient failure (out of space, too many descriptors, a retryable I/O error) is retried a
    /// few times, then leaves the file dirty and in the dirty set: nothing is lost, the next flush
    /// tries again, and only `fsync` of that file reports `EIO`.
    pub(crate) fn flush_data(&self, sc: &SnapCtx, entry: &Entry<'_>) -> Result<()> {
        let files = sc.q.lk().take_dirty_files();
        let mut poison: Option<Error> = None;
        let mut transient: Option<String> = None;
        let mut backoff = 50u64;
        for ino in files {
            let Some(node) = self.nodes.get(&ino) else {
                continue;
            };
            let mut err = None;
            for _ in 0..=Self::FLUSH_RETRIES {
                match self.flush_node(sc, &node, entry) {
                    Ok(()) => {
                        err = None;
                        node.clear_degraded();
                        break;
                    }
                    Err(e) => {
                        let fatal = Node::classify(&e);
                        err = Some(e);
                        if fatal {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_micros(backoff));
                        backoff *= 8;
                    }
                }
            }
            let Some(e) = err else {
                continue;
            };
            if Node::classify(&e) {
                let err = node.poison(format!("{e} (file {ino:#x})"));
                poison.get_or_insert(err);
                self.ctr.poisoned.fetch_add(1, Ordering::Relaxed);
            } else {
                let why = format!("{e} (file {ino:#x})");
                node.degrade(why.clone());
                transient.get_or_insert(why);
                // keep the bytes and keep the file in the dirty set for the next flush
                sc.q.lk().add_dirty_file(ino);
                self.ctr.transient.fetch_add(1, Ordering::Relaxed);
            }
        }
        let msg = match (poison, transient) {
            (Some(p), Some(t)) => Some(format!("poisoned file: {p}; transient: {t}")),
            (Some(p), None) => Some(format!("poisoned file: {p}")),
            (None, Some(t)) => Some(format!("transient flush failure: {t}")),
            (None, None) => None,
        };
        *self.last_error.lk() = msg;
        Ok(())
    }

    /// Test seam: the next `times` flushes of `ino` fail, `kind` 1 transient (out of space),
    /// 2 corruption. Only `tests/` sets it, and the real store is never asked.
    pub(crate) fn take_flush_fault(&self, ino: Ino) -> Option<Error> {
        let mut f = self.flush_fault.lk();
        let &(kind, times) = f.get(&ino)?;
        if times == 0 {
            f.remove(&ino);
            return None;
        }
        if times == 1 {
            f.remove(&ino);
        } else {
            f.insert(ino, (kind, times - 1));
        }
        Some(match kind {
            1 => Error::NoSpace,
            2 => Error::Corrupt("injected flush fault".to_string()),
            _ => Error::Retry,
        })
    }

    /// Commits everything queued for `sc`: data to the store, then one meta batch.
    ///
    /// The gate is entered first, before the queue is drained: a drained batch holds chunk lists
    /// that no node and no snapshot root names, so a barrier must not be able to close between
    /// the drain and the commit.
    pub(crate) fn flush_snapshot(&self, sc: &SnapCtx) -> Result<()> {
        let entry = self.gate.enter();
        let _g = sc.flush.lk();
        if sc.removed.load(Ordering::Acquire) {
            return Ok(());
        }
        let r = self.flush_locked_snapshot(sc, &entry);
        if let Err(e) = &r {
            self.ctr.flush_errors.fetch_add(1, Ordering::Relaxed);
            *self.last_error.lk() = Some(e.to_string());
        }
        r
    }

    fn flush_locked_snapshot(&self, sc: &SnapCtx, entry: &Entry<'_>) -> Result<()> {
        self.flush_data(sc, entry)?;
        let batch = {
            let mut q = sc.q.lk();
            if !q.pending() {
                return Ok(());
            }
            q.drain(sc)
        };
        if batch.ops.is_empty() && batch.touched.is_empty() {
            return Ok(());
        }
        self.commit_batch(sc, batch)
    }

    fn commit_batch(&self, sc: &SnapCtx, batch: Batch) -> Result<()> {
        let states = self.restore_states(&batch);
        let times = self.op_times(&batch);
        match self.commit(sc, &batch, &states, &times) {
            Ok(created) => {
                {
                    let mut al = self.aliases.wr();
                    // Only a virtual child needs the bridge to its meta number. A create at a
                    // reserved number already holds the packed meta number, so aliasing it would be
                    // a self-entry that inflates the table and counts against the alias ceiling.
                    for (v, m) in &created {
                        if matches!(classify(*v), Id::Virt { .. }) {
                            al.insert(*v, sc.id, *m);
                        }
                    }
                }
                sc.flushed.store(batch.seq, Ordering::Release);
                self.dents.bump_all();
                self.nodes.bump_all();
                self.ctr.batches.fetch_add(1, Ordering::Relaxed);
                self.ctr
                    .ops
                    .fetch_add(batch.applied() as u64, Ordering::Relaxed);
                self.unsynced.lk().get_or_insert_with(Instant::now);
                for ino in &batch.touched {
                    if let Some(n) = self.nodes.get(ino) {
                        self.try_reclaim(&n);
                    }
                }
                // the dentry entry of a committed create now names a meta inode
                for op in &batch.ops {
                    if let Op::Create {
                        parent,
                        name,
                        child,
                        what,
                        ..
                    } = op
                    {
                        if let Some((_, m)) = created.iter().find(|(v, _)| v == child) {
                            if let Ok(meta_ino) = pack(sc.id, *m) {
                                let kind = match what {
                                    Create::File => FileKind::Regular,
                                    Create::Dir => FileKind::Directory,
                                    Create::Symlink(_) => FileKind::Symlink,
                                };
                                self.dents.retarget(*parent, name, *child, (meta_ino, kind));
                            }
                        }
                    }
                }
                // the node of a file that is committed, clean and unreferenced can go; its alias
                // cannot, since the number was handed out already
                for (v, _) in created.iter() {
                    if let Some(n) = self.nodes.get(v) {
                        self.maybe_evict_node(&n);
                    }
                }
                Ok(())
            }
            Err(e) => {
                sc.q.lk().restore(batch);
                Err(e)
            }
        }
    }

    /// The cached mode and times of a node that a batch has to write.
    ///
    /// Read with no meta lock held: the commit closure runs inside meta's writer lock, so a node
    /// lock taken there would invert the order (see `docs/v1-core.md`, "Lock order").
    fn restore_state(&self, ino: Ino) -> Option<(u32, Timestamp, Timestamp)> {
        let n = self.nodes.get(&ino)?;
        let st = n.st.rd();
        (st.attr.nlink > 0).then_some((st.attr.mode, st.attr.atime, st.attr.mtime))
    }

    /// States of every touched node, read before the commit opens meta's writer lock.
    fn restore_states(&self, b: &Batch) -> HashMap<Ino, (u32, Timestamp, Timestamp)> {
        b.touched
            .iter()
            .filter_map(|i| self.restore_state(*i).map(|s| (*i, s)))
            .collect()
    }

    /// The time each queued operation happened, keyed by the inode whose cached `ctime` carries it.
    ///
    /// Every operation that reaches meta stamps two or three inodes from one `Timestamp::now()`
    /// reading, and the layer above writes that same value into each cached node as it queues, so
    /// one of those nodes is the operation's time. Read before the commit opens meta's writer lock,
    /// like [`Inner::restore_states`], for the same lock-order reason.
    ///
    /// An inode whose node has gone is absent, and its operations keep the batch time, which is what
    /// they had before.
    fn op_times(&self, b: &Batch) -> HashMap<Ino, cowfs_meta::Timestamp> {
        // `NodeState::attr` is behind an `RwLock`, so this reads it the way `restore_state` does and
        // is listed in the audit table beside it for the same lock-order reason.
        let stamp = |ino: &Ino| {
            self.nodes
                .get(ino)
                .map(|n| to_meta_ts(n.st.rd().attr.ctime))
        };
        let mut out = HashMap::new();
        for op in &b.ops {
            // the inode whose cached ctime is this operation's single clock reading
            let subject = match op {
                Op::Create { child, .. } => child,
                Op::Link { ino, .. } | Op::Content { ino, .. } => ino,
                // the parent, which the namespace operations stamp from the same reading as the
                // child they name, and which the operation carries by inode
                Op::Unlink { parent, .. } | Op::Rmdir { parent, .. } => parent,
                Op::Rename { from, .. } => from,
            };
            if let Some(t) = stamp(subject) {
                out.insert(*subject, t);
            }
        }
        for ino in &b.touched {
            if let Some(t) = stamp(ino) {
                out.insert(*ino, t);
            }
        }
        out
    }

    fn commit(
        &self,
        sc: &SnapCtx,
        b: &Batch,
        states: &HashMap<Ino, (u32, Timestamp, Timestamp)>,
        times: &HashMap<Ino, cowfs_meta::Timestamp>,
    ) -> Result<Vec<(Ino, u64)>> {
        // `ctime` is the time of the change, not the time of the batch: stamp each operation with
        // the time its cached node already holds. Only `ctime` follows it, and an inode the batch
        // does not name keeps the wall clock the transaction opened with.
        let stamp = |tx: &mut cowfs_meta::Tx<'_>, ino: Ino| {
            if let Some(t) = times.get(&ino) {
                tx.set_now(*t);
            }
        };
        use cowfs_meta::Error as M;
        let alias = self.aliases.rd().clone();
        let res = sc.snap.batch(|tx| {
            let mut newly: HashMap<Ino, u64> = HashMap::new();
            let resolve =
                |ino: Ino, newly: &HashMap<Ino, u64>| -> std::result::Result<cowfs_meta::Ino, M> {
                    match classify(ino) {
                        Id::Meta { m, .. } => Ok(mino(m)),
                        Id::Virt { .. } => newly
                            .get(&ino)
                            .copied()
                            .or_else(|| alias.meta_of(ino))
                            .map(mino)
                            .ok_or(M::Invalid("queued operation names an unknown inode")),
                        Id::Root => Err(M::Invalid("queued operation names the mount root")),
                    }
                };
            for op in &b.ops {
                match op {
                    Op::Create {
                        parent,
                        name,
                        mode,
                        child,
                        reserved,
                        what,
                    } => {
                        if b.elided.contains(child) {
                            continue;
                        }
                        stamp(tx, *child);
                        let p = resolve(*parent, &newly)?;
                        let a = match (reserved, what) {
                            (Some(ticket), Create::File) => tx.create_at(p, name, *mode, ticket)?,
                            (Some(ticket), Create::Dir) => tx.mkdir_at(p, name, *mode, ticket)?,
                            (Some(ticket), Create::Symlink(t)) => {
                                tx.symlink_at(p, name, t, ticket)?
                            }
                            (None, Create::File) => tx.create(p, name, *mode)?,
                            (None, Create::Dir) => tx.mkdir(p, name, *mode)?,
                            (None, Create::Symlink(t)) => tx.symlink(p, name, t)?,
                        };
                        newly.insert(*child, a.ino.0);
                    }
                    Op::Link { ino, parent, name } => {
                        stamp(tx, *ino);
                        tx.link(resolve(*ino, &newly)?, resolve(*parent, &newly)?, name)?;
                    }
                    Op::Unlink { parent, name } => {
                        stamp(tx, *parent);
                        tx.unlink(resolve(*parent, &newly)?, name)?;
                    }
                    Op::Rmdir { parent, name } => {
                        stamp(tx, *parent);
                        tx.rmdir(resolve(*parent, &newly)?, name)?;
                    }
                    Op::Rename {
                        from,
                        from_name,
                        to,
                        to_name,
                    } => {
                        tx.rename(
                            resolve(*from, &newly)?,
                            from_name,
                            resolve(*to, &newly)?,
                            to_name,
                        )?;
                    }
                    Op::Content { ino, chunks, size } => {
                        if b.elided.contains(ino) {
                            continue;
                        }
                        stamp(tx, *ino);
                        tx.set_content(resolve(*ino, &newly)?, &chunks.refs, *size)?;
                    }
                }
            }
            for ino in &b.touched {
                if b.elided.contains(ino) {
                    continue;
                }
                let Some((mode, atime, mtime)) = states.get(ino) else {
                    continue;
                };
                let Ok(m) = resolve(*ino, &newly) else {
                    continue;
                };
                let set = cowfs_meta::SetAttr {
                    mode: Some(*mode),
                    atime: Some(to_meta_ts(*atime)),
                    mtime: Some(to_meta_ts(*mtime)),
                    size: None,
                };
                // the same reason: this loop runs once per touched inode, and without a stamp per
                // inode every one of them would take the transaction's opening time
                stamp(tx, *ino);
                match tx.setattr(m, set) {
                    Ok(_) | Err(M::NotFound) => {}
                    Err(e) => return Err(e),
                }
            }
            Ok(newly)
        });
        res.map(|m| m.into_iter().collect()).map_err(from_meta)
    }

    /// Commits all queued work in every snapshot and makes it durable.
    pub(crate) fn sync_all(&self) -> Result<()> {
        for sc in self.all_snaps() {
            self.flush_snapshot(&sc)?;
        }
        self.finish_sync()
    }

    /// Flushes and syncs until this snapshot's work is durable.
    pub(crate) fn fsync_snapshot(&self, sc: &SnapCtx) -> Result<()> {
        self.flush_snapshot(sc)?;
        self.finish_sync()
    }

    /// Makes this snapshot's names and attributes durable, leaving file data that is still dirty
    /// alone.
    ///
    /// A namespace commit names only blocks the store already has: [`Inner::queue_content`] runs
    /// after the flush that wrote them, and a file with unflushed bytes has no content operation
    /// queued at all. `finish_sync` then syncs the store before the metadata, so nothing that
    /// becomes durable here can point at a block that is not.
    pub(crate) fn sync_ns_snapshot(&self, sc: &SnapCtx) -> Result<()> {
        self.barrier(sc)?;
        self.finish_sync()
    }

    /// Makes everything applied to the metadata durable and clears the unsynced mark. A failure is
    /// recorded where [`Core::health`] reports it, so a caller that is told the sync worked is
    /// never the only thing that knows it did not.
    fn finish_sync(&self) -> Result<()> {
        let r = self.meta.sync().map_err(from_meta);
        if let Err(e) = &r {
            self.ctr.flush_errors.fetch_add(1, Ordering::Relaxed);
            *self.last_error.lk() = Some(e.to_string());
        } else {
            *self.unsynced.lk() = None;
        }
        r
    }

    /// Commits the snapshot's namespace so an operation that needs meta to be current can go on.
    ///
    /// Unrelated files' dirty data is left alone: their chunk lists are simply not queued yet, so
    /// a `readdir` of one directory does not have to chunk 48 MiB written to another file.
    /// `fsync` and `sync` are the operations that must flush data, and they call
    /// [`Inner::flush_snapshot`].
    pub(crate) fn barrier(&self, sc: &SnapCtx) -> Result<()> {
        self.ctr.barriers.fetch_add(1, Ordering::Relaxed);
        let _entry = self.gate.enter();
        let _g = sc.flush.lk();
        if sc.removed.load(Ordering::Acquire) {
            return Ok(());
        }
        let r = self.flush_namespace_locked(sc);
        if let Err(e) = &r {
            self.ctr.flush_errors.fetch_add(1, Ordering::Relaxed);
            *self.last_error.lk() = Some(e.to_string());
        }
        r
    }

    fn flush_namespace_locked(&self, sc: &SnapCtx) -> Result<()> {
        let batch = {
            let mut q = sc.q.lk();
            if !q.pending() {
                return Ok(());
            }
            q.drain(sc)
        };
        self.commit_batch(sc, batch)
    }

    /// Wakes the background flusher.
    pub(crate) fn wake(&self) {
        self.bg.1.notify_all();
    }

    /// One pass of the background flusher.
    pub(crate) fn tick(&self) {
        for sc in self.all_snaps() {
            let due = {
                let q = sc.q.lk();
                q.pending()
                    && (q.op_count() >= self.opts.max_pending_ops
                        || q.age().is_none_or(|a| a >= self.opts.flush_interval))
            };
            if due {
                let _ = self.flush_snapshot(&sc);
            }
        }
        let need = self
            .unsynced
            .lk()
            .is_some_and(|t| t.elapsed() >= self.opts.sync_interval);
        if need {
            let ok = self.meta.sync().is_ok();
            if ok {
                *self.unsynced.lk() = None;
            }
        }
    }

    /// Flushes file data when unflushed bytes exceed the bound.
    pub(crate) fn relieve(&self) {
        if self.dirty_bytes.load(Ordering::Acquire) <= self.opts.max_dirty_bytes {
            return;
        }
        // best effort: while a collector holds the barrier the bytes stay dirty
        let Some(entry) = self.gate.try_enter() else {
            return;
        };
        let Ok(_g) = self.pressure.try_lock() else {
            return;
        };
        for sc in self.all_snaps() {
            let _ = self.flush_data(&sc, &entry);
        }
    }
}
