use crate::BlockId;

/// Settings for [`crate::Store::open`].
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Start a new pack when appending would pass this size. Clamped to 2 GiB.
    pub max_pack_size: u64,
    /// Write the index checkpoint when the store is dropped after writes.
    pub checkpoint_on_drop: bool,
    /// Most read-only pack handles kept open at once. Default 64.
    pub max_open_packs: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_pack_size: 256 << 20,
            checkpoint_on_drop: true,
            max_open_packs: 64,
        }
    }
}

/// A run of bytes in a pack that belongs to no valid record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    /// Pack id.
    pub pack: u32,
    /// Offset of the first bad byte.
    pub offset: u64,
    /// Number of bad bytes.
    pub len: u64,
}

/// Damage found in bytes that an earlier `sync` had made durable. This is corruption, not a torn write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CorruptRegion {
    /// Pack id.
    pub pack: u32,
    /// Offset of the first bad byte.
    pub offset: u64,
    /// Number of bad bytes, or missing bytes when the pack is shorter than its durable length.
    pub len: u64,
    /// The id the damaged header claims, when that header still parses. Unverified.
    pub id: Option<BlockId>,
}

/// What [`crate::Store::open`] found and repaired.
///
/// Two kinds of damage are kept apart.
/// A torn tail is bytes after the durable watermark: the store never promised them, so open cuts
/// them and only reports how much (`torn_tail_discarded`). That is normal after a crash.
/// `corrupt_synced` and `missing_synced` are damage to data that a completed `sync` had made durable.
/// That is data loss and callers that must not serve a store with lost data should refuse to mount
/// when [`RecoveryReport::has_corruption`] is true.
#[derive(Clone, Debug, Default)]
pub struct RecoveryReport {
    /// The index checkpoint was valid and used.
    pub index_loaded: bool,
    /// The durable-watermark file was missing or unreadable, so damage could not be classified.
    pub watermark_missing: bool,
    /// Records found by scanning packs (beyond the checkpoint) and hash-verified.
    pub records_scanned: u64,
    /// Bytes cut from the end of the last pack as a torn tail (only beyond the durable watermark).
    /// The first megabyte of each cut is kept in `<pack>.torn-<n>` next to the pack.
    pub torn_tail_discarded: u64,
    /// Same value as `torn_tail_discarded`, kept for source compatibility.
    pub truncated_bytes: u64,
    /// Valid records found after a torn region and moved down over it, so nothing verifiable is lost.
    pub recovered_from_tail: u64,
    /// Bad regions that were skipped but not removed. Nothing in them is served or indexed.
    pub gaps: Vec<Gap>,
    /// The subset of bad regions inside durable bytes, and packs shorter than the watermark says.
    /// A non-empty list means data was lost to corruption.
    pub corrupt_synced: Vec<CorruptRegion>,
    /// Pack ids that the watermark says must exist and do not.
    pub missing_synced: Vec<u32>,
    /// Damaged regions whose block has a verified copy elsewhere, so nothing is lost. Informational.
    pub superseded: Vec<CorruptRegion>,
    /// Damaged regions that [`crate::Store::acknowledge_corruption`] accepted earlier. Informational.
    pub acknowledged: Vec<CorruptRegion>,
}

impl RecoveryReport {
    /// True when open found damage to bytes that had been made durable.
    ///
    /// Open does not re-read data covered by the index checkpoint, so bit rot there is not
    /// reported here. Use [`crate::Store::verify_all`] to find it.
    pub fn has_corruption(&self) -> bool {
        !self.corrupt_synced.is_empty()
            || !self.missing_synced.is_empty()
            || (self.watermark_missing && (self.torn_tail_discarded > 0 || !self.gaps.is_empty()))
    }
}

/// Result of [`crate::Store::salvage`].
#[derive(Clone, Debug, Default)]
pub struct SalvageReport {
    /// Records that passed structure, checksum and hash checks.
    pub records: u64,
    /// Verified records whose id was not indexed at all.
    pub newly_indexed: u64,
    /// Verified records that replaced an index entry that could not be read.
    pub repaired: u64,
    /// Regions that hold no verifiable record.
    pub damaged: Vec<Gap>,
}

/// Counters and sizes, see [`crate::Store::stats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Unique blocks in the index.
    pub blocks: u64,
    /// Sum of uncompressed sizes of unique blocks.
    pub uncompressed_bytes: u64,
    /// Sum of record sizes (header and payload) of indexed blocks.
    pub stored_bytes: u64,
    /// Number of pack files.
    pub packs: u64,
    /// Total size of all pack files.
    pub pack_bytes: u64,
    /// `put` calls since open.
    pub put_calls: u64,
    /// Bytes given to `put` since open.
    pub put_bytes: u64,
    /// `put` calls that found the block already stored.
    pub dedup_hits: u64,
    /// Bytes not stored because of those hits.
    pub dedup_bytes: u64,
}

/// A problem found by [`crate::Store::fsck`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Damage {
    /// Bytes that are not part of a valid record.
    Gap {
        /// Pack id.
        pack: u32,
        /// Offset of the first bad byte.
        offset: u64,
        /// Number of bad bytes.
        len: u64,
    },
    /// A record whose data does not hash to its id.
    HashMismatch {
        /// Pack id.
        pack: u32,
        /// Record offset.
        offset: u64,
        /// The id the record claims.
        id: BlockId,
    },
    /// A record with a valid checksum whose payload does not decode.
    BadPayload {
        /// Pack id.
        pack: u32,
        /// Record offset.
        offset: u64,
        /// The id the record claims.
        id: BlockId,
    },
    /// An index entry that does not point at a verified record for that id.
    IndexEntry {
        /// The indexed id.
        id: BlockId,
    },
}

/// Result of [`crate::Store::fsck`].
#[derive(Clone, Debug, Default)]
pub struct FsckReport {
    /// Pack files scanned.
    pub packs: u64,
    /// Bytes scanned.
    pub bytes_scanned: u64,
    /// Valid records found, duplicates included.
    pub records: u64,
    /// Distinct blocks whose data re-hashed correctly.
    pub blocks_verified: u64,
    /// Valid records for an id that already had one.
    pub duplicate_records: u64,
    /// Everything wrong.
    pub damage: Vec<Damage>,
}

impl FsckReport {
    /// True when nothing is wrong.
    pub fn is_clean(&self) -> bool {
        self.damage.is_empty()
    }
}
