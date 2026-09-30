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
#[derive(Clone, Debug, Default)]
pub struct RecoveryReport {
    /// The index checkpoint was valid and used.
    pub index_loaded: bool,
    /// The durable-watermark file was missing or unreadable, so nothing was truncated.
    pub watermark_missing: bool,
    /// Records found by scanning packs (beyond the checkpoint) and hash-verified.
    pub records_scanned: u64,
    /// Bytes cut from the end of the last pack as a torn tail (only beyond the durable watermark).
    pub truncated_bytes: u64,
    /// Bad regions that were skipped but not removed. Nothing in them is served or indexed.
    pub gaps: Vec<Gap>,
    /// The subset of bad regions inside durable bytes. A non-empty list means data was lost to
    /// corruption. Callers should refuse to mount.
    pub corrupt_synced: Vec<CorruptRegion>,
}

impl RecoveryReport {
    /// True when open found damage to bytes that had been made durable.
    pub fn has_corruption(&self) -> bool {
        !self.corrupt_synced.is_empty() || (self.watermark_missing && !self.gaps.is_empty())
    }
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
