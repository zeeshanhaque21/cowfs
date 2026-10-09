//! Content-addressed block store. Contract: `docs/v1-architecture.md`.

mod ack;
mod chunk;
mod compact;
mod error;
mod fdcache;
mod fsio;
mod index;
mod pack;
mod record;
mod store;
mod types;
mod wm;

use std::fmt;

pub use chunk::{chunks, Chunks, AVG_CHUNK_LEN, MAX_CHUNK_LEN, MIN_CHUNK_LEN};
pub use compact::{Compaction, Discarded, PackInfo, PackPlan, Rewrite};
pub use error::{Error, Result};
#[cfg(feature = "fault-injection")]
pub use fsio::{oplog_marker, oplog_start, oplog_take, LogOp};
#[doc(hidden)]
pub use fsio::{Op, Trace};
pub use store::Store;
pub use types::{
    CorruptRegion, Damage, FsckReport, Gap, Options, RecoveryReport, SalvageReport, Stats,
};

/// Length of a [`BlockId`] in bytes.
pub const BLOCK_ID_LEN: usize = 32;
/// Largest block `put` accepts, equal to the largest FastCDC chunk.
pub const MAX_BLOCK_LEN: usize = MAX_CHUNK_LEN;

/// BLAKE3-256 of the uncompressed bytes of a block.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId([u8; BLOCK_ID_LEN]);

impl BlockId {
    /// Hash `data` into its id.
    pub fn of(data: &[u8]) -> Self {
        Self(*blake3::hash(data).as_bytes())
    }

    /// Wrap raw id bytes.
    pub const fn from_bytes(bytes: [u8; BLOCK_ID_LEN]) -> Self {
        Self(bytes)
    }

    /// The raw id bytes.
    pub const fn as_bytes(&self) -> &[u8; BLOCK_ID_LEN] {
        &self.0
    }
}

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

impl fmt::Debug for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlockId({self})")
    }
}

/// One chunk of a file: which block holds it and how many uncomcompressed bytes it covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkRef {
    /// The block holding the chunk.
    pub id: BlockId,
    /// Uncompressed length of the chunk.
    pub len: u32,
    /// This ref is a hole: the bytes are zeros that were never stored.
    ///
    /// A hole's `id` is the all-zero sentinel and its length is at most [`HOLE_MAX`]. The flag is
    /// what lets a walker of a chunk list tell a hole from a stored block without comparing ids
    /// against the sentinel itself. It carries no information the sentinel does not: a ref decoded
    /// from a store written before the flag existed gets it from the sentinel, and a ref with it
    /// set re-encodes to the same 32 zero bytes and length.
    pub hole: bool,
}

/// The block id a hole ref carries. BLAKE3 never produces it, so it can never name a stored block.
pub const HOLE: BlockId = BlockId::from_bytes([0; BLOCK_ID_LEN]);

/// The longest run a hole ref may claim. A zero id with a longer length is a corrupt entry, not a
/// hole, and is refused by [`ChunkRef::validate`].
pub const HOLE_MAX: u32 = 1 << 30;

impl ChunkRef {
    /// A ref to a stored block.
    pub const fn block(id: BlockId, len: u32) -> Self {
        Self {
            id,
            len,
            hole: false,
        }
    }

    /// A hole ref: `len` bytes of zeros that are not in the store. `len` must be at most
    /// [`HOLE_MAX`]; use [`ChunkRef::hole_refs`] to cover a longer run.
    pub const fn hole(len: u32) -> Self {
        Self {
            id: HOLE,
            len,
            hole: true,
        }
    }

    /// True for a hole ref, whether it was built by [`ChunkRef::hole`] or decoded from the
    /// sentinel.
    pub const fn is_hole(&self) -> bool {
        self.hole
    }

    /// Hole refs covering `len` bytes, split at [`HOLE_MAX`] because one ref cannot claim more.
    pub fn hole_refs(mut len: u64) -> Vec<Self> {
        let mut out = Vec::new();
        while len > 0 {
            let n = len.min(u64::from(HOLE_MAX));
            out.push(Self::hole(n as u32));
            len -= n;
        }
        out
    }

    /// Whether the flag, the id and the length agree.
    ///
    /// A ref that is a hole and names a stored block would be skipped by a walker while the block
    /// it names is live, and a ref that is a stored block with the sentinel id would put the
    /// sentinel in front of a collector. Both are refused at the seam that writes chunk lists.
    pub fn validate(&self) -> std::result::Result<(), ChunkRefError> {
        if self.hole {
            if self.id != HOLE {
                return Err(ChunkRefError::HoleWithBlockId);
            }
            if self.len > HOLE_MAX {
                return Err(ChunkRefError::HoleTooLong);
            }
        } else if self.id == HOLE {
            return Err(ChunkRefError::BlockWithHoleId);
        }
        Ok(())
    }
}

/// Why a [`ChunkRef`] is not a legal chunk ref.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkRefError {
    /// Flagged a hole but named a stored block.
    HoleWithBlockId,
    /// A hole longer than [`HOLE_MAX`].
    HoleTooLong,
    /// Named the hole sentinel without the hole flag.
    BlockWithHoleId,
}

impl fmt::Display for ChunkRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::HoleWithBlockId => "a hole cannot name a stored block",
            Self::HoleTooLong => "a hole is longer than a hole may claim",
            Self::BlockWithHoleId => "a stored block cannot have the hole id",
        })
    }
}

impl std::error::Error for ChunkRefError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_blake3_of_bytes_and_displays_as_hex() {
        let id = BlockId::of(b"");
        assert_eq!(
            id.to_string(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }
}
