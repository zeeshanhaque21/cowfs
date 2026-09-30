//! Content-addressed block store. Contract: `docs/v1-architecture.md`.

use std::fmt;

pub const BLOCK_ID_LEN: usize = 32;

/// BLAKE3-256 of the uncompressed bytes of a block.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId([u8; BLOCK_ID_LEN]);

impl BlockId {
    pub fn of(data: &[u8]) -> Self {
        Self(*blake3::hash(data).as_bytes())
    }

    pub const fn from_bytes(bytes: [u8; BLOCK_ID_LEN]) -> Self {
        Self(bytes)
    }

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
    pub id: BlockId,
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
