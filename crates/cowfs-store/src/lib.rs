//! Content-addressed block store. Contract: `docs/v1-architecture.md`.

mod ack;
mod chunk;
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
pub use error::{Error, Result};
#[doc(hidden)]
pub use fsio::{oplog_marker, oplog_start, oplog_take, LogOp, Op, Trace};
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

/// One chunk of a file: which block holds it and how many uncompressed bytes it covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkRef {
    /// The block holding the chunk.
    pub id: BlockId,
    /// Uncompressed length of the chunk.
    pub len: u32,
}

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
