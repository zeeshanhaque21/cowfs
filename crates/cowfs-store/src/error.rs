use std::io;
use std::path::PathBuf;

use crate::{BlockId, MAX_BLOCK_LEN};

/// Errors from the block store.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An underlying I/O call failed.
    #[error("i/o error: {0}")]
    Io(#[from] io::Error),
    /// The block is not in the index.
    #[error("block {0} not found")]
    NotFound(BlockId),
    /// `put` was given more than [`MAX_BLOCK_LEN`] bytes.
    #[error("block of {0} bytes exceeds the {MAX_BLOCK_LEN} byte maximum")]
    BlockTooLarge(usize),
    /// A stored record failed a structural or checksum check.
    #[error("corrupt record in pack {pack} at offset {offset}: {reason}")]
    Corrupt {
        /// Pack id.
        pack: u32,
        /// Byte offset of the record in the pack.
        offset: u64,
        /// What failed.
        reason: &'static str,
    },
    /// The decoded bytes do not hash to the requested id.
    #[error("block {0} failed hash verification")]
    HashMismatch(BlockId),
    /// A file in `packs/` is not a usable pack.
    #[error("unusable pack {path}: {reason}")]
    BadPack {
        /// Path of the pack file.
        path: PathBuf,
        /// What failed.
        reason: &'static str,
    },
    /// Another handle holds the store lock.
    #[error("store {0} is open elsewhere")]
    Locked(PathBuf),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;
