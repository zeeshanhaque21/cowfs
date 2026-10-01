//! Garbage collection for cowfs: mark-and-sweep from snapshot roots, freeing bytes by rewriting
//! packs. Design: `docs/v1-gc.md`.
//!
//! Two decisions carry the whole crate.
//!
//! Marking reuses `cowfs-meta`'s shared `Marker`, so a subtree an earlier walk covered is skipped
//! and snapshots that share subtrees cost only their differing paths.
//! That is the incremental marking of `docs/design.md`, and this crate adds the persistent half:
//! a snapshot whose root an earlier cycle already walked is skipped whole, and the blocks that walk
//! yielded are remembered, so a block is never condemned because the walk did not run again.
//!
//! Freeing needs a barrier.
//! A write can deduplicate onto a block that is already garbage, so a collector that decided such
//! a block was dead a moment earlier would, on unlinking its pack, take away a block a commit has
//! just made live.
//! No waiting closes that window, because a metadata commit can land at any instant.
//! So the long parts of a cycle run with no barrier at all, and one short window at the end holds
//! the reference side still, re-walks what the marker has not seen, and only then unlinks.
//! A caller that offers no [`ExtraRoots::reference_barrier`] gets a report and no free.
//!
//! Last-access times are a hint, never a reason.
//! They order the packs a cycle rewrites, coldest first, so a run under a write load reclaims cold
//! packs before warm ones.
//! Reachability decides what is freed, and the barrier re-checks reachability at the free.

mod error;
mod report;
mod state;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex, PoisonError};

use cowfs_meta::{Marker, Meta};
use cowfs_store::{BlockId, PackPlan, Rewrite, Store};

pub use error::{Error, Result, RootsError};
pub use report::{GcReport, Progress, SkipReason, Skipped};
use state::{Hints, Marks};

/// The block id a sparse hole chunk carries. It is not a block and is never stored.
pub const HOLE: BlockId = BlockId::from_bytes([0; 32]);

/// A held-still reference side. Implemented by `cowfs-core` over its flusher lock.
pub trait Barrier: Send + Sync {
    /// Nothing to do: holding the value is the barrier.
    fn hold(&self) {}
}

impl<T: Barrier + ?Sized> Barrier for Box<T> {
    fn hold(&self) {
        (**self).hold();
    }
}

/// Roots and pinned blocks the collector cannot find by walking snapshots alone.
///
/// Both methods are fallible, and the contract is narrow: an answer is either exact or an error.
/// There is no "partial" answer and no error that means "probably nothing is pinned".
///
/// This exists because a reference side under load cannot always answer. A core that returns an
/// empty vector when a writer holds a node lock tells the collector that nothing is pinned, and
/// the collector frees whatever those writers have in flight. So the caller says
/// [`RootsError::Busy`] and the collector frees nothing.
///
/// The collector polls more than once per cycle and unions every answer it got, so a block that
/// appears pinned on any poll is protected. That makes an implementation safe against its own
/// laziness: it may forget to report a block on one poll and report it on another, and the block
/// still survives. What it must not do is report a block as unpinned forever.
pub trait ExtraRoots {
    /// Blocks only memory names: open orphans, uncommitted chunk lists, pending write-back ops.
    /// Garbage collection must treat every one of them as live.
    ///
    /// Must be exact: every block the reference side holds is in the vector, and nothing else is.
    /// A hole is dropped by the collector, so returning one is harmless but pointless.
    fn pinned_blocks(&self) -> std::result::Result<Vec<BlockId>, RootsError>;

    /// A guard that keeps the reference side still while it is alive: no new metadata commit can
    /// make a new block reference visible.
    ///
    /// `Ok(None)` means the caller offers no ordering, and the collector then reports candidates
    /// and frees nothing. That is deliberate. Proceeding without a barrier risks losing a block a
    /// commit made live, and criterion 3 is zero data loss.
    ///
    /// `Err` means the answer is unknown and the collector treats it as no barrier at all, so
    /// nothing is freed.
    fn reference_barrier(&self) -> std::result::Result<Option<Box<dyn Barrier>>, RootsError> {
        Ok(None)
    }
}

/// Settings for one collector.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Rewrite a pack when its dead record bytes reach this fraction of its record bytes.
    pub dead_ratio: f64,
    /// Never rewrite a pack with fewer dead bytes than this. Keeps a small store cheap.
    pub min_dead_bytes: u64,
    /// Bytes copied per cycle, 0 for no bound. Bounds a cycle's duration and its I/O rate.
    pub io_budget_bytes: u64,
    /// Bytes copied between cancellation checks.
    pub batch_bytes: u64,
    /// Most access-time hints held in memory. Past this, new ids are not recorded.
    pub max_hints: usize,
    /// Most persisted marked roots and blocks. Past this the persistent set is dropped.
    pub max_persisted_blocks: usize,
    /// Report and change nothing.
    pub dry_run: bool,
    /// Unlink packs when the barrier confirms them. Off means a cycle reports only.
    pub reclaim: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            dead_ratio: 0.5,
            min_dead_bytes: 8 << 20,
            io_budget_bytes: 2 << 30,
            batch_bytes: 8 << 20,
            max_hints: 1 << 20,
            max_persisted_blocks: 4 << 20,
            dry_run: false,
            reclaim: true,
        }
    }
}

type ProgressFn = Box<dyn FnMut(&Progress) + Send + Sync>;

/// A collector over one store and one metadata database.
pub struct Gc {
    store: Arc<Store>,
    meta: Arc<Meta>,
    state: PathBuf,
    opts: Options,
    hints: Mutex<Hints>,
    marks: Mutex<Marks>,
    progress: Mutex<Option<ProgressFn>>,
    cancelled: AtomicBool,
    /// One cycle at a time per collector. Two overlapping cycles on one store each hold their own
    /// picture of what is live, and the second one to unlink a pack can free a block the first
    /// one has just decided to keep. Serialising the cycle removes that whole class of race, and
    /// a real deployment runs one collect at a time anyway.
    cycle: Mutex<()>,
}

impl Gc {
    /// Open a collector. `state_dir` holds the batched hints and the marked root set; nothing is
    /// written into the store directory except through the store.
    pub fn open(
        state_dir: impl AsRef<Path>,
        store: Arc<Store>,
        meta: Arc<Meta>,
        opts: Options,
    ) -> Result<Self> {
        let state = state_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&state).map_err(|e| Error::Io {
            path: state.clone(),
            source: e,
        })?;
        let hints = Hints::load(&state, opts.max_hints).map_err(|e| Error::Io {
            path: state.join("atime.bin"),
            source: e,
        })?;
        let marks = Marks::load(&state, opts.max_persisted_blocks);
        Ok(Self {
            store,
            meta,
            state,
            opts,
            hints: Mutex::new(hints),
            marks: Mutex::new(marks),
            progress: Mutex::new(None),
            cancelled: AtomicBool::new(false),
            cycle: Mutex::new(()),
        })
    }

    /// The store this collector works on.
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// The metadata database this collector walks.
    pub fn meta(&self) -> &Arc<Meta> {
        &self.meta
    }

    /// The directory holding the hints and the marked root set.
    pub fn state_dir(&self) -> &Path {
        &self.state
    }

    /// Record that a block was read. Touches memory only: a read never becomes a write.
    pub fn note_access(&self, id: BlockId) {
        if id == HOLE {
            return;
        }
        self.hints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .note(id);
    }

    /// Append the pending hints and fsync them. Returns the number of records written.
    pub fn flush_hints(&self) -> Result<usize> {
        self.hints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .flush()
            .map_err(|e| Error::Io {
                path: self.state.join("atime.bin"),
                source: e,
            })
    }

    /// The last access second recorded for a block, 0 when none.
    pub fn last_access(&self, id: &BlockId) -> u32 {
        self.hints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
    }

    /// Ask a running cycle to stop at its next check, and make the next cycle stop too.
    ///
    /// The flag stays set until [`Gc::resume`], so a client that cancels once and then polls
    /// `collect` does not get a second full cycle by accident.
    pub fn cancel(&self) {
        self.cancelled.store(true, Relaxed);
    }

    /// Clear a previous [`Gc::cancel`].
    pub fn resume(&self) {
        self.cancelled.store(false, Relaxed);
    }

    /// True once a cancel has been asked for.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Relaxed)
    }

    /// Set the progress callback. A second call replaces the first.
    pub fn set_progress(&self, f: impl FnMut(&Progress) + Send + Sync + 'static) {
        *self.progress.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(f));
    }

    /// One cycle: freeze, mark, choose candidates, copy, then verify and unlink under a barrier.
    ///
    /// One cycle runs at a time per collector: a second call waits. Two overlapping cycles would
    /// each hold their own picture of what is live, and the second to unlink a pack could free a
    /// block the first has just decided to keep.
    pub fn collect(&self, roots: Option<&dyn ExtraRoots>) -> Result<GcReport> {
        let _cycle = self.cycle.lock().unwrap_or_else(PoisonError::into_inner);
        let mut r = GcReport {
            dry_run: self.opts.dry_run,
            ..GcReport::default()
        };
        let recovery = self.store.recovery();
        if recovery.has_corruption() {
            return Err(Error::CorruptStore(
                recovery.corrupt_synced.len() + recovery.missing_synced.len(),
            ));
        }
        let want_free = self.opts.reclaim && !self.opts.dry_run;
        // An error from the reference side is not an empty answer, so the cycle stops with the mark
        // reported and nothing freed. The barrier is polled here for the same reason: a barrier the
        // caller cannot promise is no barrier.
        let barrier = if want_free {
            match roots.map_or(Ok(None), ExtraRoots::reference_barrier) {
                Ok(Some(b)) => {
                    r.barrier = true;
                    Some(b)
                }
                Ok(None) => None,
                Err(e) => {
                    r.roots_error = Some(e);
                    None
                }
            }
        } else {
            None
        };

        // 1. Freeze. The roots come from the durable state, and each root is immutable, so the
        // walk in step 2 describes exactly the blocks that root referenced at the freeze.
        let epoch = self.store.epoch();
        let mut pinned: Vec<BlockId> = match roots.map_or(Ok(Vec::new()), |x| {
            x.pinned_blocks().map(|v| {
                let mut v: Vec<BlockId> = v.into_iter().filter(|b| *b != HOLE).collect();
                v.sort_unstable();
                v.dedup();
                v
            })
        }) {
            Ok(v) => v,
            Err(e) => {
                r.roots_error = Some(e);
                Vec::new()
            }
        };
        r.pinned = pinned.len() as u64;
        self.meta.sync()?;

        // 2. Mark. No barrier: a root captured above is immutable, so a write during the walk
        // cannot change what it yields.
        let mut marker = Marker::new();
        let mut live: HashSet<BlockId> = HashSet::new();
        {
            // Blocks an earlier cycle yielded. They are live because a root that is still there
            // still references them, and a root that is gone lets them go, which the prune in
            // `finish` handles.
            let marks = self.marks();
            live.extend(marks.blocks().iter().copied());
        }
        let mut walked_roots: HashSet<[u8; 32]> = HashSet::new();
        let walked = self.marked(&mut marker, &mut r, true, &mut walked_roots)?;
        live.extend(walked);
        // The second poll closes the window between the freeze and the end of the mark. A block the
        // first poll missed is caught here, and a block that was pinned only for the duration of
        // the mark is still protected when the sweep runs.
        self.repin(roots, &mut pinned, &mut r);
        live.extend(pinned.iter().copied());
        r.live_blocks = live.len();
        r.store_blocks = self.store.stats().blocks;

        // 3. Candidates.
        let mut candidates: Vec<(PackPlan, u64)> = Vec::new();
        {
            let hints = self.hints.lock().unwrap_or_else(PoisonError::into_inner);
            for info in self.store.packs()? {
                let mut ids = Vec::new();
                let plan = match self
                    .store
                    .plan_pack(info.id, &|b| live.contains(&b), &mut ids)
                {
                    Ok(p) => p,
                    Err(e) => {
                        r.error(e);
                        continue;
                    }
                };
                r.quarantined_bytes += plan.quarantined_bytes;
                if plan.corrupt {
                    r.skip(info.id, SkipReason::Corrupt);
                    continue;
                }
                // The active pack is being appended to, and a pack at or above the epoch was
                // written after the freeze, so the mark cannot have seen any reference to it.
                if info.active || Some(info.id) >= epoch.map(|(p, _)| p) {
                    r.skip(info.id, SkipReason::Active);
                    continue;
                }
                if plan.dead_bytes < self.opts.min_dead_bytes
                    || plan.dead_ratio() < self.opts.dead_ratio
                {
                    r.skip(info.id, SkipReason::BelowThreshold);
                    continue;
                }
                r.candidates += 1;
                r.candidate_bytes += plan.record_bytes();
                // A hint only orders the work. Coldest first, so a run that gets cut short
                // reclaims cold bytes.
                let cold = hints.coldness(&ids, plan.live_bytes);
                candidates.push((plan, cold));
            }
        }
        candidates.sort_by_key(|(p, cold)| (*cold, std::cmp::Reverse(p.dead_bytes)));
        if let Some(e) = r.roots_error {
            // The reference side would not answer, so the cycle is marked and reported and stops
            // here. Not copying either: a copy nobody may unlink only costs space, and the next
            // cycle has to redo it.
            for (plan, _) in &candidates {
                r.skip(plan.id, SkipReason::RootsUnavailable);
            }
            r.error(Error::RootsUnavailable(e));
            self.finish(&mut r, &live, &pinned);
            return Ok(r);
        }
        if barrier.is_none() {
            // Without a barrier the copy could not be followed by a safe free, and a copy nobody
            // unlinks only costs space. So a cycle that cannot free does not copy either.
            for (plan, _) in &candidates {
                r.skip(plan.id, SkipReason::NotReached);
            }
            self.finish(&mut r, &live, &pinned);
            return Ok(r);
        }

        // The last poll before anything is copied. A failure here aborts with nothing copied and
        // nothing freed, which is the whole point of aborting early: the copies are the expensive
        // part of a cycle and there is no reason to make them if the answer is going to fail.
        self.repin(roots, &mut pinned, &mut r);
        if let Some(e) = r.roots_error {
            for (plan, _) in &candidates {
                r.skip(plan.id, SkipReason::RootsUnavailable);
            }
            live.extend(pinned.iter().copied());
            r.error(Error::RootsUnavailable(e));
            self.finish(&mut r, &live, &pinned);
            return Ok(r);
        }
        live.extend(pinned.iter().copied());

        // 4. Copy. No barrier. A put during the copy either writes above the epoch or deduplicates
        // onto a record in a pack step 5 will refuse to unlink.
        let mut budget = self.opts.io_budget_bytes;
        let mut copied: Vec<Rewrite> = Vec::new();
        let mut progress = Progress {
            packs_total: candidates.len() as u64,
            store_blocks: r.store_blocks,
            live_blocks: r.live_blocks as u64,
            sweeping: true,
            ..Progress::default()
        };
        for (plan, _) in &candidates {
            if self.is_cancelled() || (self.opts.io_budget_bytes > 0 && budget == 0) {
                r.skip(plan.id, SkipReason::NotReached);
                progress.packs_done += 1;
                self.emit(&progress);
                continue;
            }
            match self.copy_one(plan, &live, &mut budget, &mut progress) {
                // `None` means the copy was abandoned part way: the pack is left whole and the
                // next cycle starts it again.
                Ok(Some(rw)) => {
                    r.packs_rewritten += 1;
                    r.records_copied += rw.records;
                    r.bytes_copied += rw.bytes;
                    copied.push(rw);
                }
                Ok(None) => r.skip(plan.id, SkipReason::NotReached),
                Err(e) => r.error(e),
            }
            progress.packs_done += 1;
            self.emit(&progress);
        }

        // 5. Verify and unlink with the reference side held still. The guard is alive from before
        // the copy, so no commit landed during the copy; its `hold` waits for the writers already
        // inside one.
        if let Some(guard) = barrier {
            guard.hold();
            match self.marked(&mut marker, &mut r, false, &mut walked_roots) {
                Ok(new) => live.extend(new),
                Err(e) => r.error(e),
            }
            // The poll just before each unlink, so a reference side that fails late still stops the
            // unlinks and a block that only now reports itself pinned is unioned in and survives.
            for rw in &copied {
                self.repin(roots, &mut pinned, &mut r);
                live.extend(pinned.iter().copied());
                if let Some(e) = r.roots_error {
                    // Everything from here on stays where it is. The copies above are already
                    // written and indexed, so a later cycle reuses them instead of redoing them.
                    r.skip(rw.from, SkipReason::RootsUnavailable);
                    r.error(Error::RootsUnavailable(e));
                    continue;
                }
                if self.is_cancelled() {
                    r.skip(rw.from, SkipReason::NotReached);
                    continue;
                }
                if rw.condemned.iter().any(|b| live.contains(b)) {
                    r.skip(rw.from, SkipReason::BecameLive);
                    continue;
                }
                match self.store.discard_pack(rw.from, &rw.condemned) {
                    Ok(freed) => {
                        r.packs_unlinked += 1;
                        r.freed_bytes += freed;
                        progress.freed_bytes = r.freed_bytes;
                        self.emit(&progress);
                    }
                    Err(e) => r.error(e),
                }
            }
        } else {
            for rw in &copied {
                r.skip(rw.from, SkipReason::NotReached);
            }
        }
        self.finish(&mut r, &live, &pinned);
        Ok(r)
    }

    /// Walk every durable snapshot root, sharing one marker, and union what they reference.
    ///
    /// `record` is the step 2 pass. It persists each root it walked and each block it saw, so a
    /// later cycle can skip a root whole while still knowing the blocks that root protects.
    /// The `!record` pass is step 5, inside the barrier: the marker already holds everything
    /// step 2 walked, so it only descends into subtrees that changed during the copy, and its
    /// cost is the write stall a caller feels.
    fn marked(
        &self,
        marker: &mut Marker,
        r: &mut GcReport,
        record: bool,
        walked: &mut HashSet<[u8; 32]>,
    ) -> Result<HashSet<BlockId>> {
        let mut live = HashSet::new();
        for info in self.meta.durable_snapshots()? {
            let key = *info.root.as_bytes();
            if walked.contains(&key) {
                // Already covered this cycle, by the step 2 pass or by an earlier cycle. Its
                // blocks are in the set and the marker holds its subtrees.
                continue;
            }
            if record && self.marks().has_root(&key) {
                // An earlier cycle walked this exact root, so its blocks are already in the set.
                r.marked_skipped_roots += 1;
                walked.insert(key);
                continue;
            }
            // A snapshot removed between the listing and the lookup is gone, so its blocks are
            // not live. That is not an error: a collect runs while snapshots come and go.
            let Ok(snap) = self.meta.snapshot_by_id(info.id) else {
                continue;
            };
            for b in snap.live_blocks(marker)? {
                let b = b?;
                if b == HOLE {
                    continue;
                }
                r.marked += 1;
                live.insert(b);
                if record {
                    self.marks().add_block(b);
                }
            }
            walked.insert(key);
            if record {
                self.marks().add_root(&key);
            }
        }
        Ok(live)
    }

    /// Poll the reference side again and union the answer into everything pinned so far.
    ///
    /// Polling twice is deliberate. A reference side that is asked while a writer holds a lock may
    /// answer with fewer blocks the second time than the first, and a block that was reported on
    /// one poll and dropped on the next is still pinned. Unioning means a block survives unless
    /// every poll agreed it was unpinned.
    fn repin(&self, roots: Option<&dyn ExtraRoots>, pinned: &mut Vec<BlockId>, r: &mut GcReport) {
        let Some(x) = roots else {
            return;
        };
        let fresh = match x.pinned_blocks() {
            Ok(v) => v,
            Err(e) => {
                r.roots_error.get_or_insert(e);
                return;
            }
        };
        pinned.extend(fresh.into_iter().filter(|b| *b != HOLE));
        pinned.sort_unstable();
        pinned.dedup();
        r.pinned = pinned.len() as u64;
    }

    /// Copy one candidate. `Ok(None)` means the copy was abandoned: the budget ran out or the
    /// cycle was cancelled, and the pack is left whole for the next cycle.
    fn copy_one(
        &self,
        plan: &PackPlan,
        live: &HashSet<BlockId>,
        budget: &mut u64,
        progress: &mut Progress,
    ) -> Result<Option<Rewrite>> {
        let is_live = |b: BlockId| live.contains(&b);
        let mut c = self.store.begin_compaction(plan, &is_live)?;
        if c.outstanding_bytes() > 0 && *budget > 0 && c.outstanding_bytes() > *budget {
            // The whole copy does not fit in what is left of the budget, so do not start it.
            return Ok(None);
        }
        while !self.store.copy_batch(&mut c, self.opts.batch_bytes)? {
            let owed = c.outstanding_bytes();
            progress.bytes_copied += c.written();
            if self.is_cancelled() || (*budget > 0 && owed > *budget) {
                return Ok(None);
            }
            *budget = budget.saturating_sub(owed);
        }
        progress.bytes_copied += c.written();
        if *budget > 0 {
            *budget = budget.saturating_sub(c.written());
        }
        Ok(Some(self.store.finish_compaction(&c)?))
    }

    /// Flush the hints, then drop every persisted block nothing references any more.
    ///
    /// A block stays in the persisted set while any root that was walked still names it. Anything
    /// the store holds that this cycle found unreachable is dropped, so the set shrinks as
    /// snapshots go away instead of pinning garbage forever.
    fn finish(&self, r: &mut GcReport, live: &HashSet<BlockId>, pinned: &[BlockId]) {
        {
            let h = self.hints.lock().unwrap_or_else(PoisonError::into_inner);
            r.hints_tracked = h.tracked();
            r.hints_dropped = h.dropped();
        }
        if self.opts.dry_run {
            return;
        }
        match self.flush_hints() {
            Ok(n) => r.hints_flushed = n as u64,
            Err(e) => r.error(e),
        }
        let mut marks = self.marks.lock().unwrap_or_else(PoisonError::into_inner);
        let dead: HashSet<BlockId> = self
            .store
            .iter_ids()
            .filter(|b| !live.contains(b) && !pinned.contains(b))
            .collect();
        if let Err(e) = marks.save(&dead) {
            r.error(Error::Io {
                path: self.state.join("mark.bin"),
                source: e,
            });
        }
    }

    fn marks(&self) -> std::sync::MutexGuard<'_, Marks> {
        self.marks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn emit(&self, p: &Progress) {
        let mut g = self.progress.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(f) = g.as_mut() {
            f(p);
        }
    }
}

impl std::fmt::Debug for Gc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gc")
            .field("state", &self.state)
            .field(
                "hints",
                &self.hints.lock().map(|h| h.tracked()).unwrap_or(0),
            )
            .field(
                "marked_blocks",
                &self.marks.lock().map(|m| m.blocks().len()).unwrap_or(0),
            )
            .field("cancelled", &self.is_cancelled())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Noop;
    impl Barrier for Noop {}

    struct Roots {
        pinned: Vec<BlockId>,
        barrier: bool,
    }
    impl ExtraRoots for Roots {
        fn pinned_blocks(&self) -> std::result::Result<Vec<BlockId>, RootsError> {
            Ok(self.pinned.clone())
        }
        fn reference_barrier(&self) -> std::result::Result<Option<Box<dyn Barrier>>, RootsError> {
            Ok(self.barrier.then(|| Box::new(Noop) as Box<dyn Barrier>))
        }
    }

    #[test]
    fn a_hole_is_the_zero_id_not_the_hash_of_nothing() {
        // cowfs-core names a sparse run with the all-zero id, not BLAKE3 of the empty string.
        assert_eq!(HOLE, BlockId::from_bytes([0; 32]));
        assert_ne!(HOLE, BlockId::of(&[]), "a real empty block is a real block");
        assert_ne!(HOLE, BlockId::of(b"x"));
    }

    #[test]
    fn a_roots_without_a_barrier_offers_none() {
        let r = Roots {
            pinned: vec![BlockId::of(b"a")],
            barrier: false,
        };
        assert!(r.reference_barrier().unwrap().is_none());
        assert_eq!(r.pinned_blocks().unwrap().len(), 1);
        let r = Roots {
            pinned: Vec::new(),
            barrier: true,
        };
        r.reference_barrier().unwrap().unwrap().hold();
    }
}
