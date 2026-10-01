use fastcdc::v2020::{cut, select_masks, Normalization};

/// Smallest chunk FastCDC emits, except for the final chunk of an input.
pub const MIN_CHUNK_LEN: usize = 16 * 1024;
/// Target average chunk size.
pub const AVG_CHUNK_LEN: usize = 64 * 1024;
/// Largest chunk, and so the largest block.
pub const MAX_CHUNK_LEN: usize = 256 * 1024;

/// Cut-point finder with the masks computed once.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Chunker {
    masks: (u64, u64),
}

impl Chunker {
    pub(crate) fn new() -> Self {
        Self {
            masks: select_masks(AVG_CHUNK_LEN, Normalization::Level1),
        }
    }

    /// Length of the first chunk of `data`. Only the first `MAX_CHUNK_LEN` bytes are read.
    pub(crate) fn cut(&self, data: &[u8]) -> usize {
        if data.is_empty() {
            return 0;
        }
        let (mask_s, mask_l) = self.masks;
        let (_, n) = cut(
            data,
            MIN_CHUNK_LEN,
            AVG_CHUNK_LEN,
            MAX_CHUNK_LEN,
            mask_s,
            mask_l,
            mask_s << 1,
            mask_l << 1,
        );
        if n == 0 {
            data.len().min(MAX_CHUNK_LEN)
        } else {
            n.min(data.len())
        }
    }
}

/// Iterator over the FastCDC chunks (16/64/256 KiB) of a slice.
#[derive(Debug)]
pub struct Chunks<'a> {
    rest: &'a [u8],
    chunker: Chunker,
}

/// Split `data` into content-defined chunks. Deterministic; the chunks concatenate to `data`.
pub fn chunks(data: &[u8]) -> Chunks<'_> {
    Chunks {
        rest: data,
        chunker: Chunker::new(),
    }
}

impl<'a> Iterator for Chunks<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        if self.rest.is_empty() {
            return None;
        }
        let (head, tail) = self.rest.split_at(self.chunker.cut(self.rest));
        self.rest = tail;
        Some(head)
    }
}
