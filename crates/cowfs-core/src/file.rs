//! File content: chunk lists, holes, dirty extents, partial-chunk writes and reads.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use cowfs_store::{chunks, BlockId, ChunkRef, MAX_CHUNK_LEN};
use cowfs_vfs::{Error, Result};

use crate::blocks::Blocks;

/// Chunk refs with this id are holes: zeros that are never stored.
pub(crate) const HOLE: BlockId = BlockId::from_bytes([0; 32]);
const HOLE_MAX: u64 = 1 << 30;
/// Largest file size, as in the reference implementation.
pub(crate) const MAX_FILE: u64 = 1 << 42;

/// Hole refs covering `len` bytes.
pub(crate) fn hole_refs(mut len: u64) -> Vec<ChunkRef> {
    let mut out = Vec::new();
    while len > 0 {
        let n = len.min(HOLE_MAX);
        out.push(ChunkRef {
            id: HOLE,
            len: n as u32,
        });
        len -= n;
    }
    out
}

/// A chunk list with the end offset of every chunk, for offset lookups.
#[derive(Clone, Debug, Default)]
pub(crate) struct Chunks {
    pub(crate) refs: Vec<ChunkRef>,
    ends: Vec<u64>,
}

impl Chunks {
    pub(crate) fn from_refs(refs: Vec<ChunkRef>) -> Self {
        let mut c = Self {
            refs,
            ends: Vec::new(),
        };
        c.reindex(0);
        c
    }

    fn reindex(&mut self, from: usize) {
        self.ends.truncate(from);
        let mut acc = self.ends.last().copied().unwrap_or(0);
        for r in &self.refs[from..] {
            acc += u64::from(r.len);
            self.ends.push(acc);
        }
    }

    /// Bytes covered by chunks. Bytes from here to the file size are an implicit hole.
    pub(crate) fn total(&self) -> u64 {
        self.ends.last().copied().unwrap_or(0)
    }

    fn start(&self, i: usize) -> u64 {
        if i == 0 {
            0
        } else {
            self.ends[i - 1]
        }
    }

    /// Index of the chunk containing `off`, or the chunk count if `off` is past the list.
    fn find(&self, off: u64) -> usize {
        self.ends.partition_point(|&e| e <= off)
    }

    fn replace(&mut self, range: Range<usize>, new: Vec<ChunkRef>) {
        let from = range.start;
        self.refs.splice(range, new);
        self.reindex(from);
    }

    pub(crate) fn stored_bytes(&self) -> u64 {
        self.refs
            .iter()
            .filter(|r| r.id != HOLE)
            .map(|r| u64::from(r.len))
            .sum()
    }
}

/// The loaded content of one regular file: its committed chunk list plus unflushed writes.
#[derive(Debug, Default)]
pub(crate) struct FileData {
    pub(crate) chunks: Arc<Chunks>,
    /// Disjoint, non-adjacent runs of written bytes that are not in `chunks` yet.
    dirty: BTreeMap<u64, Vec<u8>>,
    dirty_bytes: usize,
}

impl FileData {
    pub(crate) fn new(refs: Vec<ChunkRef>) -> Self {
        Self {
            chunks: Arc::new(Chunks::from_refs(refs)),
            ..Self::default()
        }
    }

    pub(crate) fn dirty_bytes(&self) -> usize {
        self.dirty_bytes
    }

    /// Forgets every unflushed byte and returns how many there were.
    pub(crate) fn discard(&mut self) -> usize {
        self.dirty.clear();
        std::mem::take(&mut self.dirty_bytes)
    }

    pub(crate) fn is_clean(&self) -> bool {
        self.dirty.is_empty()
    }

    /// Bytes reported by `st_blocks`: stored chunk bytes plus unflushed bytes, capped at `size`.
    pub(crate) fn used_bytes(&self, size: u64) -> u64 {
        (self.chunks.stored_bytes() + self.dirty_bytes as u64).min(size)
    }

    /// Records `data` at `off`. The caller has already raised the file size to cover it.
    pub(crate) fn write(&mut self, off: u64, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let end = off + data.len() as u64;
        let mut keys = Vec::new();
        if let Some((&s, v)) = self.dirty.range(..=off).next_back() {
            if s + v.len() as u64 >= off {
                keys.push(s);
            }
        }
        keys.extend(self.dirty.range(off + 1..=end).map(|(&s, _)| s));
        if let [s] = keys[..] {
            if s <= off {
                if let Some(v) = self.dirty.get_mut(&s) {
                    let old = v.len();
                    let need = (end - s) as usize;
                    if need > old {
                        v.resize(need, 0);
                    }
                    let at = (off - s) as usize;
                    v[at..at + data.len()].copy_from_slice(data);
                    self.dirty_bytes += v.len() - old;
                    return;
                }
            }
        }
        let mut lo = off;
        let mut hi = end;
        for k in &keys {
            if let Some(v) = self.dirty.get(k) {
                lo = lo.min(*k);
                hi = hi.max(*k + v.len() as u64);
            }
        }
        let mut buf = vec![0u8; (hi - lo) as usize];
        for k in keys {
            if let Some(v) = self.dirty.remove(&k) {
                self.dirty_bytes -= v.len();
                let at = (k - lo) as usize;
                buf[at..at + v.len()].copy_from_slice(&v);
            }
        }
        let at = (off - lo) as usize;
        buf[at..at + data.len()].copy_from_slice(data);
        self.dirty_bytes += buf.len();
        self.dirty.insert(lo, buf);
    }

    /// Copies of the dirty bytes that overlap `[start, end)`, clipped to it.
    pub(crate) fn overlay(&self, start: u64, end: u64) -> Vec<(u64, Vec<u8>)> {
        let mut out = Vec::new();
        let first = self
            .dirty
            .range(..=start)
            .next_back()
            .map(|(&s, _)| s)
            .unwrap_or(start);
        for (&s, v) in self.dirty.range(first..end) {
            let e = s + v.len() as u64;
            if e <= start {
                continue;
            }
            let lo = s.max(start);
            let hi = e.min(end);
            out.push((lo, v[(lo - s) as usize..(hi - s) as usize].to_vec()));
        }
        out
    }

    /// Moves every dirty extent into the chunk list: reads the (at most two) boundary chunks
    /// per extent, re-chunks the modified region with the store's FastCDC and puts the pieces.
    /// On error nothing changes.
    pub(crate) fn flush(&mut self, blocks: &Blocks) -> Result<()> {
        if self.dirty.is_empty() {
            return Ok(());
        }
        let mut list = (*self.chunks).clone();
        for (&a, data) in &self.dirty {
            flush_extent(&mut list, blocks, a, data)?;
        }
        self.chunks = Arc::new(list);
        self.dirty.clear();
        self.dirty_bytes = 0;
        Ok(())
    }

    /// Cuts the chunk list at `size`. The caller has flushed, so there are no dirty extents.
    pub(crate) fn truncate(&mut self, blocks: &Blocks, size: u64) -> Result<()> {
        if !self.dirty.is_empty() {
            return Err(Error::Io("truncate with unflushed data".into()));
        }
        let total = self.chunks.total();
        if size >= total {
            return Ok(());
        }
        let mut list = (*self.chunks).clone();
        let i = list.find(size);
        let start = list.start(i);
        let c = list.refs[i];
        let mut tail = Vec::new();
        if size > start {
            if c.id == HOLE {
                tail = hole_refs(size - start);
            } else {
                let bytes = blocks.get(c.id)?;
                check_len(&bytes, c)?;
                for piece in chunks(&bytes[..(size - start) as usize]) {
                    tail.push(put_piece(blocks, piece)?);
                }
            }
        }
        let n = list.refs.len();
        list.replace(i..n, tail);
        self.chunks = Arc::new(list);
        Ok(())
    }
}

fn check_len(bytes: &[u8], c: ChunkRef) -> Result<()> {
    if bytes.len() == c.len as usize {
        Ok(())
    } else {
        Err(Error::Corrupt(format!(
            "block {} holds {} bytes, its chunk ref says {}",
            c.id,
            bytes.len(),
            c.len
        )))
    }
}

fn put_piece(blocks: &Blocks, piece: &[u8]) -> Result<ChunkRef> {
    Ok(ChunkRef {
        id: blocks.put(piece)?,
        len: piece.len() as u32,
    })
}

fn flush_extent(list: &mut Chunks, blocks: &Blocks, a: u64, data: &[u8]) -> Result<()> {
    let b = a + data.len() as u64;
    let total = list.total();
    let n = list.refs.len();
    let mut prefix = Vec::new();
    let mut suffix = Vec::new();
    let mut head: Vec<u8> = Vec::new();
    let mut tail: Vec<u8> = Vec::new();
    let first;
    let last_excl;
    if a >= total {
        last_excl = n;
        let mut f = n;
        if a > total {
            prefix = hole_refs(a - total);
        } else if let Some(&last) = list.refs.last() {
            // appending: restart chunking at the last chunk so boundaries match a one-shot write
            if last.id != HOLE && (last.len as usize) < MAX_CHUNK_LEN {
                let bytes = blocks.get(last.id)?;
                check_len(&bytes, last)?;
                head = bytes.to_vec();
                f = n - 1;
            }
        }
        first = f;
    } else {
        let i = list.find(a);
        let start = list.start(i);
        let c = list.refs[i];
        first = i;
        if c.id == HOLE {
            if a > start {
                prefix = hole_refs(a - start);
            }
        } else {
            let bytes = blocks.get(c.id)?;
            check_len(&bytes, c)?;
            head = bytes[..(a - start) as usize].to_vec();
        }
        if b > total {
            last_excl = n;
        } else {
            let j = list.find(b - 1);
            let end = list.ends[j];
            let start = list.start(j);
            let c = list.refs[j];
            last_excl = j + 1;
            if b < end {
                if c.id == HOLE {
                    suffix = hole_refs(end - b);
                } else {
                    let bytes = blocks.get(c.id)?;
                    check_len(&bytes, c)?;
                    tail = bytes[(b - start) as usize..].to_vec();
                }
            }
        }
    }
    let mut region = Vec::with_capacity(head.len() + data.len() + tail.len());
    region.extend_from_slice(&head);
    region.extend_from_slice(data);
    region.extend_from_slice(&tail);
    let mut new = prefix;
    for piece in chunks(&region) {
        new.push(put_piece(blocks, piece)?);
    }
    new.extend(suffix);
    list.replace(first..last_excl, new);
    Ok(())
}

/// Reads `[off, end)` of a file: chunk data, zeros for holes, then the dirty `overlay` on top.
pub(crate) fn read_range(
    blocks: &Blocks,
    list: &Chunks,
    overlay: &[(u64, Vec<u8>)],
    off: u64,
    end: u64,
) -> Result<Vec<u8>> {
    let mut out = vec![0u8; (end - off) as usize];
    if off < list.total() {
        let mut i = list.find(off);
        while i < list.refs.len() {
            let s = list.start(i);
            if s >= end {
                break;
            }
            let c = list.refs[i];
            if c.id != HOLE {
                let blk = blocks.get(c.id)?;
                check_len(&blk, c)?;
                let lo = off.max(s);
                let hi = end.min(s + u64::from(c.len));
                out[(lo - off) as usize..(hi - off) as usize]
                    .copy_from_slice(&blk[(lo - s) as usize..(hi - s) as usize]);
            }
            i += 1;
        }
    }
    for (s, bytes) in overlay {
        let at = (*s - off) as usize;
        out[at..at + bytes.len()].copy_from_slice(bytes);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cowfs_store::{Options, Store};

    fn blocks() -> (tempfile::TempDir, Blocks) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Options::default()).unwrap();
        (dir, Blocks::new(Arc::new(store), 8 << 20))
    }

    fn pattern(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8 | 1
            })
            .collect()
    }

    /// Applies a write to a model vector.
    fn model_write(m: &mut Vec<u8>, off: usize, data: &[u8]) {
        if m.len() < off + data.len() {
            m.resize(off + data.len(), 0);
        }
        m[off..off + data.len()].copy_from_slice(data);
    }

    fn full(blocks: &Blocks, f: &FileData, size: u64) -> Vec<u8> {
        let ov = f.overlay(0, size);
        read_range(blocks, &f.chunks, &ov, 0, size).unwrap()
    }

    #[test]
    fn extents_merge_and_overlay() {
        let mut f = FileData::default();
        f.write(10, b"abc");
        f.write(13, b"def");
        f.write(100, b"zz");
        assert_eq!(f.dirty.len(), 2);
        assert_eq!(f.dirty_bytes(), 8);
        f.write(5, &[1u8; 200]);
        assert_eq!(f.dirty.len(), 1);
        assert_eq!(f.dirty_bytes(), 200);
        let ov = f.overlay(0, 10);
        assert_eq!(ov, vec![(5, vec![1u8; 5])]);
    }

    #[test]
    fn flush_and_read_match_model_across_chunk_sizes() {
        let (_d, b) = blocks();
        let mut f = FileData::default();
        let mut m: Vec<u8> = Vec::new();
        let mut x = 0x2545_F491_4F6C_DD1Du64;
        let mut next = |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n
        };
        for i in 0..60u64 {
            let off = next(700_000) as usize;
            let len = 1 + next(if i % 7 == 0 { 400_000 } else { 9_000 }) as usize;
            let data = pattern(len, i + 1);
            f.write(off as u64, &data);
            model_write(&mut m, off, &data);
            if i % 3 == 0 {
                f.flush(&b).unwrap();
                assert!(f.is_clean());
                assert!(f.chunks.total() <= m.len() as u64);
            }
            assert_eq!(full(&b, &f, m.len() as u64), m, "after write {i}");
        }
        f.flush(&b).unwrap();
        assert_eq!(full(&b, &f, m.len() as u64), m);
        assert!(f.chunks.refs.iter().all(|r| r.id != HOLE || r.len > 0));
    }

    #[test]
    fn holes_are_not_materialised() {
        let (_d, b) = blocks();
        let mut f = FileData::default();
        let far = 1u64 << 40;
        f.write(far, b"end");
        f.flush(&b).unwrap();
        assert!(f.chunks.refs.len() <= 1100, "{}", f.chunks.refs.len());
        assert_eq!(f.chunks.stored_bytes(), 3);
        let r = read_range(&b, &f.chunks, &[], far - 2, far + 3).unwrap();
        assert_eq!(r, b"\0\0end");
        f.write(far / 2, b"mid");
        f.flush(&b).unwrap();
        let r = read_range(&b, &f.chunks, &[], far / 2 - 1, far / 2 + 4).unwrap();
        assert_eq!(r, b"\0mid\0");
        assert_eq!(f.chunks.total(), far + 3);
        assert_eq!(f.chunks.stored_bytes(), 6);
    }

    #[test]
    fn truncate_cuts_inside_a_chunk_and_inside_a_hole() {
        let (_d, b) = blocks();
        let mut f = FileData::default();
        let data = pattern(300_000, 4);
        f.write(0, &data);
        f.flush(&b).unwrap();
        f.truncate(&b, 123_457).unwrap();
        assert_eq!(f.chunks.total(), 123_457);
        assert_eq!(
            read_range(&b, &f.chunks, &[], 0, 123_457).unwrap(),
            data[..123_457]
        );
        f.write(5_000_000, b"x");
        f.flush(&b).unwrap();
        f.truncate(&b, 2_000_000).unwrap();
        assert_eq!(f.chunks.total(), 2_000_000);
        assert_eq!(
            read_range(&b, &f.chunks, &[], 123_457, 123_460).unwrap(),
            vec![0u8; 3]
        );
        f.truncate(&b, 0).unwrap();
        assert_eq!(f.chunks.total(), 0);
        assert!(f.chunks.refs.is_empty());
    }

    #[test]
    fn sequential_appends_chunk_like_a_single_write() {
        let (_d, b) = blocks();
        let data = pattern(3 << 20, 9);
        let mut one = FileData::default();
        one.write(0, &data);
        one.flush(&b).unwrap();
        let mut many = FileData::default();
        for (i, piece) in data.chunks(700_001).enumerate() {
            many.write((i * 700_001) as u64, piece);
            many.flush(&b).unwrap();
        }
        let ids = |f: &FileData| f.chunks.refs.iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids(&one), ids(&many));
    }
}
