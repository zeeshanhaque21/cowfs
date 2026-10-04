//! What a cycle did, and what it deliberately did not do.

use std::fmt;

/// One pack the cycle looked at and left alone, with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    /// Pack id, or a synthetic id of `u32::MAX` for a cycle-level skip.
    pub pack: u32,
    /// Why it was left alone.
    pub reason: SkipReason,
}

/// Why a pack was not rewritten and unlinked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// The active pack, the one `put` appends to.
    Active,
    /// Not enough dead bytes to be worth the copy.
    BelowThreshold,
    /// A block condemned from this pack became reachable again while the copy ran.
    BecameLive,
    /// The pack holds damage to bytes an earlier `sync` made durable.
    Corrupt,
    /// The cycle was cancelled, or its I/O budget ran out, before this pack.
    NotReached,
    /// The store directory could not be read.
    Io,
    /// The reference side would not answer, so the cycle was stopped before it copied anything.
    RootsUnavailable,
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            SkipReason::Active => "active pack",
            SkipReason::BelowThreshold => "dead bytes below the threshold",
            SkipReason::BecameLive => "a condemned block became live again",
            SkipReason::Corrupt => "pack holds durable corruption",
            SkipReason::NotReached => "cancelled or out of I/O budget",
            SkipReason::Io => "i/o error",
            SkipReason::RootsUnavailable => "the reference side would not answer",
        };
        f.write_str(s)
    }
}

/// How far a cycle got, and what it freed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Blocks a snapshot walk yielded, duplicates included.
    pub marked: u64,
    /// Snapshot roots skipped because an earlier walk of the same root already covered them.
    pub marked_skipped_roots: u64,
    /// Blocks only memory names, from [`crate::ExtraRoots`].
    pub pinned: u64,
    /// Live blocks after the hole and duplicate pass.
    pub live_blocks: usize,
    /// Blocks the store holds.
    pub store_blocks: u64,
    /// Packs that reached the dead-bytes threshold.
    pub candidates: u64,
    /// Bytes of records in those packs.
    pub candidate_bytes: u64,
    /// Bytes of records in those packs that no root reaches: the most a cycle can free from them.
    pub candidate_dead_bytes: u64,
    /// Bytes on disk freed by unlinking packs. **Gross**: the file length of each unlinked pack.
    /// This is not the space saved, because surviving live records are rewritten into a new pack.
    /// Kept for compatibility; [`GcReport::gross_removed_bytes`] is the same number under its
    /// explicit name.
    pub freed_bytes: u64,
    /// Bytes on disk removed by unlinking packs, under an explicit gross name. Equal to
    /// [`GcReport::freed_bytes`].
    pub gross_removed_bytes: u64,
    /// Bytes written into the packs this cycle created, file headers included. Counts committed
    /// rewrites and any abandoned partial copy, so it is real new bytes this cycle put on disk.
    pub rewrite_bytes: u64,
    /// Net space this cycle reclaimed: [`GcReport::gross_removed_bytes`] minus
    /// [`GcReport::rewrite_bytes`], signed. Negative when the rewrite cost exceeds the removed
    /// bytes, which is a truthful no-savings outcome rather than a saturated zero.
    pub net_reclaimed_bytes: i64,
    /// Packs whose live records were copied into a new pack.
    pub packs_rewritten: u64,
    /// Packs unlinked. Equal to `packs_rewritten` unless something became live again.
    pub packs_unlinked: u64,
    /// Records copied into new packs.
    pub records_copied: u64,
    /// Bytes written into new packs, headers excluded.
    pub bytes_copied: u64,
    /// Packs looked at and left alone, and why.
    pub skipped: Vec<Skipped>,
    /// Bytes kept in `<pack>.torn-*` sidecars. Reported, never removed.
    pub quarantined_bytes: u64,
    /// Access-time hints currently tracked in memory.
    pub hints_tracked: u64,
    /// Hints not recorded because the in-memory cap was reached. A lost hint, never a lost block.
    pub hints_dropped: u64,
    /// Hint records appended to disk this cycle.
    pub hints_flushed: u64,
    /// Everything that went wrong that did not stop the cycle.
    pub errors: Vec<String>,
    /// True when nothing was changed.
    pub dry_run: bool,
    /// True when the caller supplied a reference barrier, so packs were actually unlinked.
    pub barrier: bool,
    /// Why the cycle freed nothing because the reference side would not answer, if it would not.
    ///
    /// Any error from [`crate::ExtraRoots`] lands here and stops the cycle. The mark still ran and
    /// its numbers are here, so the report explains itself, but nothing was freed and nothing was
    /// compacted. An empty answer is indistinguishable from "nothing pinned" and this is how that
    /// case is spelled.
    pub roots_error: Option<crate::RootsError>,
}

impl GcReport {
    /// Record that a pack was left alone.
    pub fn skip(&mut self, pack: u32, reason: SkipReason) {
        self.skipped.push(Skipped { pack, reason });
    }
    /// Record a failure that did not stop the cycle.
    pub fn error(&mut self, e: impl fmt::Display) {
        if self.errors.len() < MAX_ERRORS {
            self.errors.push(e.to_string());
        }
    }

    /// Bytes the cycle would free, from a dry run or a real one. This is the gross removal; for
    /// the net space reclaimed use [`GcReport::net`].
    pub fn reclaimed(&self) -> u64 {
        self.freed_bytes
    }

    /// Net space the cycle reclaimed: gross removed minus the bytes written into new packs,
    /// signed.
    pub fn net(&self) -> i64 {
        self.net_reclaimed_bytes
    }

    /// True when the cycle freed nothing and reported no failure.
    pub fn is_noop(&self) -> bool {
        self.freed_bytes == 0 && self.errors.is_empty()
    }
}

/// Most failures a cycle reports. A cycle that hits this many has a systemic problem and the rest
/// would only bury it.
const MAX_ERRORS: usize = 32;

/// Net reclaimed, `gross - rewrite`, exactly.
///
/// Computed in `i128` so neither operand can wrap: a store whose gross or rewrite exceeded
/// `i64::MAX` would produce a wrapped, bogus net from a plain cast. A net that does not fit `i64`
/// is clamped to the representable bounds, which only a store past 8 EiB could reach.
pub(crate) fn net_reclaimed(gross: u64, rewrite: u64) -> i64 {
    let n = i128::from(gross) - i128::from(rewrite);
    i64::try_from(n).unwrap_or(if n < 0 { i64::MIN } else { i64::MAX })
}

/// Progress of one cycle, handed to the caller's callback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// Packs the cycle has finished looking at, rewritten or skipped.
    pub packs_done: u64,
    /// Packs the cycle will look at.
    pub packs_total: u64,
    /// Bytes copied into new packs so far.
    pub bytes_copied: u64,
    /// Bytes freed so far.
    pub freed_bytes: u64,
    /// Live blocks after the mark.
    pub live_blocks: u64,
    /// Blocks the store holds.
    pub store_blocks: u64,
    /// True once the mark is finished and the copy phase has begun.
    pub sweeping: bool,
}

impl Progress {
    /// Packs done over packs total, 0 when the total is 0.
    pub fn fraction(&self) -> f64 {
        if self.packs_total == 0 {
            0.0
        } else {
            self.packs_done as f64 / self.packs_total as f64
        }
    }
}
