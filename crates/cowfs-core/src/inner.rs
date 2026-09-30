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
use crate::ino::{classify, pack, snap_of, Aliases, Id};
use crate::node::{Node, NodeState};
use crate::queue::{Batch, Create, Op, SnapCtx};
use crate::util::{MutexExt, RwExt, ShardMap, SHARDS};

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
    pub(crate) dhit: AtomicU64,
    pub(crate) dmiss: AtomicU64,
    pub(crate) inodes_net: AtomicI64,
}

#[derive(Debug, Default)]
pub(crate) struct Snaps {
    pub(crate) by_id: HashMap<u64, Arc<SnapCtx>>,
    pub(crate) by_name: BTreeMap<String, u64>,
}

pub(crate) struct Inner {
    pub(crate) meta: Meta,
    pub(crate) blocks: Blocks,
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
    pub(crate) fn alloc_virt(&self, snap: u64) -> Result<Ino> {
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
                if let Some(v) = self.aliases.rd().canon(snap, m) {
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
            match self.nodes.insert_if(ino, node.clone(), epoch) {
                Ok(n) => {
                    self.shrink_nodes(ino);
                    return Ok(n);
                }
                Err(()) if tries < 64 => {}
                Err(()) => {
                    self.nodes.upsert(ino, node.clone());
                    return Ok(node);
                }
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
    fn seed_node(&self, ino: Ino, a: &cowfs_meta::Attr) {
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
    pub(crate) fn dent_lookup(&self, sc: &SnapCtx, parent: &Node, name: &[u8]) -> Result<Target> {
        if let Some(d) = self.dents.get(parent.ino, name) {
            self.ctr.dhit.fetch_add(1, Ordering::Relaxed);
            return Ok(d.target);
        }
        self.ctr.dmiss.fetch_add(1, Ordering::Relaxed);
        let Some(pm) = self.meta_of(parent.ino) else {
            return Ok(None);
        };
        let epoch = self.dents.epoch(parent.ino);
        match sc.snap.lookup(mino(pm), name) {
            Ok(a) => {
                let child = self.canon(sc.id, a.ino.0)?;
                self.seed_node(child, &a);
                let t = Some((child, kind_of(a.kind)));
                self.dents.fill(parent.ino, name, t, epoch);
                self.shrink_dents(parent.ino);
                Ok(t)
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
    pub(crate) fn flush_locked(&self, sc: &SnapCtx, node: &Node, st: &mut NodeState) -> Result<()> {
        let Some(f) = st.file.as_mut() else {
            return Ok(());
        };
        if f.is_clean() {
            return Ok(());
        }
        let n = f.dirty_bytes();
        f.flush(&self.blocks)?;
        self.dirty_bytes.fetch_sub(n, Ordering::AcqRel);
        self.queue_content(sc, node, st);
        Ok(())
    }

    fn flush_node(&self, sc: &SnapCtx, node: &Node) -> Result<()> {
        let mut st = node.st.wr();
        self.flush_locked(sc, node, &mut st)
    }

    /// Chunks and stores every file with unflushed bytes in `sc`.
    ///
    /// A file whose flush fails is poisoned and dropped from the dirty set, so the rest of the
    /// snapshot still commits and the queue cannot be wedged by one damaged block.
    /// The file's bytes stay in memory and stay counted; `fsync` of that file reports the error.
    pub(crate) fn flush_data(&self, sc: &SnapCtx) -> Result<()> {
        let files = sc.q.lk().take_dirty_files();
        let mut first: Option<Error> = None;
        for ino in files {
            let Some(node) = self.nodes.get(&ino) else {
                continue;
            };
            if let Err(e) = self.flush_node(sc, &node) {
                let err = node.poison(format!("{e} (file {ino:#x})"));
                first.get_or_insert(err);
                self.ctr.poisoned.fetch_add(1, Ordering::Relaxed);
            }
        }
        match first {
            Some(e) => {
                *self.last_error.lk() = Some(format!("poisoned file: {e}"));
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Commits everything queued for `sc`: data to the store, then one meta batch.
    pub(crate) fn flush_snapshot(&self, sc: &SnapCtx) -> Result<()> {
        let _g = sc.flush.lk();
        if sc.removed.load(Ordering::Acquire) {
            return Ok(());
        }
        let r = self.flush_locked_snapshot(sc);
        if let Err(e) = &r {
            self.ctr.flush_errors.fetch_add(1, Ordering::Relaxed);
            *self.last_error.lk() = Some(e.to_string());
        }
        r
    }

    fn flush_locked_snapshot(&self, sc: &SnapCtx) -> Result<()> {
        self.flush_data(sc)?;
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
        let states = self.restore_states(&batch);
        match self.commit(sc, &batch, &states) {
            Ok(created) => {
                {
                    let mut al = self.aliases.wr();
                    for (v, m) in created {
                        al.insert(v, sc.id, m);
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

    fn commit(
        &self,
        sc: &SnapCtx,
        b: &Batch,
        states: &HashMap<Ino, (u32, Timestamp, Timestamp)>,
    ) -> Result<Vec<(Ino, u64)>> {
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
                        what,
                    } => {
                        if b.elided.contains(child) {
                            continue;
                        }
                        let p = resolve(*parent, &newly)?;
                        let a = match what {
                            Create::File => tx.create(p, name, *mode)?,
                            Create::Dir => tx.mkdir(p, name, *mode)?,
                            Create::Symlink(t) => tx.symlink(p, name, t)?,
                        };
                        newly.insert(*child, a.ino.0);
                    }
                    Op::Link { ino, parent, name } => {
                        tx.link(resolve(*ino, &newly)?, resolve(*parent, &newly)?, name)?;
                    }
                    Op::Unlink { parent, name } => {
                        tx.unlink(resolve(*parent, &newly)?, name)?;
                    }
                    Op::Rmdir { parent, name } => {
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
        self.meta.sync().map_err(from_meta)?;
        *self.unsynced.lk() = None;
        Ok(())
    }

    /// Flushes and syncs until this snapshot's work is durable.
    pub(crate) fn fsync_snapshot(&self, sc: &SnapCtx) -> Result<()> {
        self.flush_snapshot(sc)?;
        self.meta.sync().map_err(from_meta)?;
        *self.unsynced.lk() = None;
        Ok(())
    }

    pub(crate) fn barrier(&self, sc: &SnapCtx) -> Result<()> {
        self.ctr.barriers.fetch_add(1, Ordering::Relaxed);
        self.flush_snapshot(sc)
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
        let Ok(_g) = self.pressure.try_lock() else {
            return;
        };
        for sc in self.all_snaps() {
            let _ = self.flush_data(&sc);
        }
    }
}
