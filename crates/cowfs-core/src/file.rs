//! File content: chunk lists, holes, dirty extents, partial-chunk writes and reads.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;

use cowfs_store::{chunks, ChunkRef, MAX_CHUNK_LEN};
use cowfs_vfs::{Error, Result};

use crate::blocks::Blocks;
use crate::gate::Entry;

/// The hole marker and its length bound live in `cowfs-store`, beside the flag that carries them.
pub(crate) use cowfs_store::HOLE;

/// The hole marker: the flag a decoded ref carries.
pub(crate) fn is_hole(c: &ChunkRef) -> bool {
    c.is_hole()
}
/// Largest file size, as in the reference implementation.
pub(crate) const MAX_FILE: u64 = 1 << 42;

/// Hole refs covering `len` bytes.
pub(crate) fn hole_refs(len: u64) -> Vec<ChunkRef> {
    ChunkRef::hole_refs(len)
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
            .filter(|r| !is_hole(r))
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
    pub(crate) fn flush(&mut self, blocks: &Blocks, entry: &Entry<'_>) -> Result<()> {
        if self.dirty.is_empty() {
            return Ok(());
        }
        let mut list = (*self.chunks).clone();
        for (&a, data) in &self.dirty {
            flush_extent(&mut list, blocks, entry, a, data)?;
        }
        self.chunks = Arc::new(list);
        self.dirty.clear();
        self.dirty_bytes = 0;
        Ok(())
    }

    /// Cuts the chunk list at `size`. The caller has flushed, so there are no dirty extents.
    pub(crate) fn truncate(&mut self, blocks: &Blocks, entry: &Entry<'_>, size: u64) -> Result<()> {
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
            if is_hole(&c) {
                tail = hole_refs(size - start);
            } else {
                let bytes = blocks.get(c.id)?;
                check_len(&bytes, c)?;
                for piece in chunks(&bytes[..(size - start) as usize]) {
                    tail.push(put_piece(blocks, entry, piece)?);
                }
            }
        }
        let n = list.refs.len();
        list.replace(i..n, tail);
        self.chunks = Arc::new(list);
        Ok(())
    }

    /// Makes `[a, b)` read as zeros with hole refs. The caller has flushed. Bytes at or past the
    /// chunk-covered total are an implicit hole already, so `b` is clamped to it. A stored chunk
    /// cut at an edge is read, verified and its surviving bytes re-chunked, as truncate does; a
    /// chunk inside the range is dropped unread. The new list is built before it is published, so
    /// on error nothing changes.
    pub(crate) fn punch(
        &mut self,
        blocks: &Blocks,
        entry: &Entry<'_>,
        a: u64,
        b: u64,
    ) -> Result<()> {
        if !self.dirty.is_empty() {
            return Err(Error::Io("punch with unflushed data".into()));
        }
        let b = b.min(self.chunks.total());
        if a >= b {
            return Ok(());
        }
        let mut list = (*self.chunks).clone();
        let (i, j) = (list.find(a), list.find(b - 1));
        let (si, sj, ej) = (list.start(i), list.start(j), list.ends[j]);
        let (first, last) = (list.refs[i], list.refs[j]);
        let mut new = Vec::new();
        if a > si {
            if is_hole(&first) {
                new = hole_refs(a - si);
            } else {
                let bytes = blocks.get(first.id)?;
                check_len(&bytes, first)?;
                for piece in chunks(&bytes[..(a - si) as usize]) {
                    new.push(put_piece(blocks, entry, piece)?);
                }
            }
        }
        new.extend(hole_refs(b - a));
        if b < ej {
            if is_hole(&last) {
                new.extend(hole_refs(ej - b));
            } else {
                let bytes = blocks.get(last.id)?;
                check_len(&bytes, last)?;
                for piece in chunks(&bytes[(b - sj) as usize..]) {
                    new.push(put_piece(blocks, entry, piece)?);
                }
            }
        }
        list.replace(i..j + 1, new);
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

fn put_piece(blocks: &Blocks, entry: &Entry<'_>, piece: &[u8]) -> Result<ChunkRef> {
    Ok(ChunkRef::block(
        blocks.put(entry, piece)?,
        piece.len() as u32,
    ))
}

fn flush_extent(
    list: &mut Chunks,
    blocks: &Blocks,
    entry: &Entry<'_>,
    a: u64,
    data: &[u8],
) -> Result<()> {
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
            if !is_hole(&last) && (last.len as usize) < MAX_CHUNK_LEN {
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
        if is_hole(&c) {
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
                if is_hole(&c) {
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
        new.push(put_piece(blocks, entry, piece)?);
    }
    new.extend(suffix);
    list.replace(first..last_excl, new);
    Ok(())
}

/// Verifies every stored chunk that a write of `[a, b)` only partially covers, so a
/// read-modify-write of a damaged chunk fails at the write instead of later at the flush.
/// A fully covered chunk is never read, because its old bytes are not needed.
pub(crate) fn verify_partial(blocks: &Blocks, list: &Chunks, a: u64, b: u64) -> Result<()> {
    let mut i = list.find(a);
    while i < list.refs.len() {
        let s = list.start(i);
        if s >= b {
            break;
        }
        let c = list.refs[i];
        if (s < a || list.ends[i] > b) && !is_hole(&c) {
            check_len(&blocks.get(c.id)?, c)?;
        }
        i += 1;
    }
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
            if !is_hole(&c) {
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
        let gate = crate::gate::Gate::new();
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
                f.flush(&b, &gate.enter()).unwrap();
                assert!(f.is_clean());
                assert!(f.chunks.total() <= m.len() as u64);
            }
            assert_eq!(full(&b, &f, m.len() as u64), m, "after write {i}");
        }
        f.flush(&b, &gate.enter()).unwrap();
        assert_eq!(full(&b, &f, m.len() as u64), m);
        assert!(f.chunks.refs.iter().all(|r| !is_hole(r) || r.len > 0));
    }

    #[test]
    fn holes_are_not_materialised() {
        let (_d, b) = blocks();
        let gate = crate::gate::Gate::new();
        let mut f = FileData::default();
        let far = 1u64 << 40;
        f.write(far, b"end");
        f.flush(&b, &gate.enter()).unwrap();
        assert!(f.chunks.refs.len() <= 1100, "{}", f.chunks.refs.len());
        assert_eq!(f.chunks.stored_bytes(), 3);
        let r = read_range(&b, &f.chunks, &[], far - 2, far + 3).unwrap();
        assert_eq!(r, b"\0\0end");
        f.write(far / 2, b"mid");
        f.flush(&b, &gate.enter()).unwrap();
        let r = read_range(&b, &f.chunks, &[], far / 2 - 1, far / 2 + 4).unwrap();
        assert_eq!(r, b"\0mid\0");
        assert_eq!(f.chunks.total(), far + 3);
        assert_eq!(f.chunks.stored_bytes(), 6);
    }

    #[test]
    fn truncate_cuts_inside_a_chunk_and_inside_a_hole() {
        let (_d, b) = blocks();
        let gate = crate::gate::Gate::new();
        let mut f = FileData::default();
        let data = pattern(300_000, 4);
        f.write(0, &data);
        f.flush(&b, &gate.enter()).unwrap();
        f.truncate(&b, &gate.enter(), 123_457).unwrap();
        assert_eq!(f.chunks.total(), 123_457);
        assert_eq!(
            read_range(&b, &f.chunks, &[], 0, 123_457).unwrap(),
            data[..123_457]
        );
        f.write(5_000_000, b"x");
        f.flush(&b, &gate.enter()).unwrap();
        f.truncate(&b, &gate.enter(), 2_000_000).unwrap();
        assert_eq!(f.chunks.total(), 2_000_000);
        assert_eq!(
            read_range(&b, &f.chunks, &[], 123_457, 123_460).unwrap(),
            vec![0u8; 3]
        );
        f.truncate(&b, &gate.enter(), 0).unwrap();
        assert_eq!(f.chunks.total(), 0);
        assert!(f.chunks.refs.is_empty());
    }

    fn assert_refs_valid(f: &FileData) {
        for r in &f.chunks.refs {
            assert!(r.len > 0, "a zero-length ref");
            assert_eq!(r.id == HOLE, is_hole(r), "hole flag and sentinel disagree");
            assert!(
                !is_hole(r) || r.len <= cowfs_store::HOLE_MAX,
                "a hole longer than HOLE_MAX: {}",
                r.len
            );
        }
    }

    #[test]
    fn punch_keeps_refs_valid_and_matches_the_model() {
        let (_d, b) = blocks();
        let gate = crate::gate::Gate::new();
        let data = pattern(3 << 20, 8);
        let mut f = FileData::default();
        f.write(0, &data);
        f.flush(&b, &gate.enter()).unwrap();
        let total = f.chunks.total();
        // unaligned edges inside stored chunks, a punch that spans many chunks, a punch inside
        // an existing hole, and one that runs past the chunk-covered total
        let mut m = data.clone();
        for (a, e) in [
            (1000u64, 2000u64),
            (500_000, 1_900_000),
            (700_000, 800_000),
            (total - 5, total + 1_000_000),
        ] {
            f.punch(&b, &gate.enter(), a, e).unwrap();
            m[a as usize..(e.min(total)) as usize].fill(0);
            assert_eq!(f.chunks.total(), total, "punch changed the covered total");
            assert_refs_valid(&f);
            assert_eq!(read_range(&b, &f.chunks, &[], 0, total).unwrap(), m);
        }
    }

    #[test]
    fn a_punch_on_chunk_boundaries_touches_no_surviving_chunk() {
        let (_d, b) = blocks();
        let gate = crate::gate::Gate::new();
        let data = pattern(3 << 20, 9);
        let mut f = FileData::default();
        f.write(0, &data);
        f.flush(&b, &gate.enter()).unwrap();
        let before = f.chunks.refs.clone();
        let ends = f.chunks.ends.clone();
        assert!(ends.len() > 6, "the file must span many chunks");
        let (k, m) = (1usize, 4usize);
        f.punch(&b, &gate.enter(), ends[k], ends[m]).unwrap();
        // refs 0..=k and m+1.. are the same stored chunks, byte for byte, and nothing was re-put
        let after = &f.chunks.refs;
        assert_eq!(&after[..=k], &before[..=k]);
        assert_eq!(
            &after[after.len() - (before.len() - m - 1)..],
            &before[m + 1..]
        );
        assert!(after[k + 1..after.len() - (before.len() - m - 1)]
            .iter()
            .all(is_hole));
        assert_eq!(f.chunks.total(), *ends.last().unwrap());
    }

    #[test]
    fn a_punch_across_the_longest_hole_ref_splits_it_validly() {
        let (_d, b) = blocks();
        let gate = crate::gate::Gate::new();
        let n = 3 * u64::from(cowfs_store::HOLE_MAX);
        let mut f = FileData::new(hole_refs(n));
        f.punch(&b, &gate.enter(), 1, n - 1).unwrap();
        assert_eq!(f.chunks.total(), n);
        assert_refs_valid(&f);
        assert_eq!(f.chunks.stored_bytes(), 0);
    }

    #[test]
    fn reads_match_the_model_at_every_chunk_boundary_alignment() {
        let (_d, b) = blocks();
        let gate = crate::gate::Gate::new();
        let data = pattern(3 << 20, 21);
        let mut f = FileData::default();
        f.write(0, &data);
        f.flush(&b, &gate.enter()).unwrap();
        let ends: Vec<u64> = f.chunks.ends.clone();
        assert!(ends.len() > 8, "the file must span many chunks");
        for &e in ends.iter().step_by(ends.len() / 8 + 1) {
            for (off, len) in [
                (e, 1usize),
                (e, 64 << 10),
                (e.saturating_sub(1), 2),
                (e.saturating_sub(7), 15),
                (0, e as usize),
            ] {
                if (off + len as u64) > data.len() as u64 {
                    continue;
                }
                let got = read_range(&b, &f.chunks, &[], off, off + len as u64).unwrap();
                assert_eq!(
                    got,
                    &data[off as usize..off as usize + len],
                    "off {off} len {len}"
                );
            }
        }
    }

    #[test]
    fn reads_over_a_hole_match_the_model_and_keep_the_ref_valid() {
        let (_d, b) = blocks();
        let gate = crate::gate::Gate::new();
        let mid = pattern(200_000, 22);
        let mut f = FileData::default();
        f.write(0, b"head");
        f.write(3 << 20, &mid);
        f.write(9 << 20, b"tail");
        f.flush(&b, &gate.enter()).unwrap();
        let holes: Vec<&ChunkRef> = f.chunks.refs.iter().filter(|r| is_hole(r)).collect();
        assert!(
            !holes.is_empty(),
            "the gaps must stay holes, not stored zeros"
        );
        assert!(
            holes
                .iter()
                .all(|r| r.hole && r.len <= cowfs_store::HOLE_MAX),
            "a hole ref must stay the all-zero id with a length a hole can hold"
        );
        let mut model = vec![0u8; (9 << 20) + 4];
        model[..4].copy_from_slice(b"head");
        model[(3 << 20)..(3 << 20) + mid.len()].copy_from_slice(&mid);
        model[(9 << 20)..(9 << 20) + 4].copy_from_slice(b"tail");
        let size = model.len() as u64;
        for (off, len) in [
            (0u64, 4usize),
            ((3 << 20) - 3, 6),
            ((3 << 20) + mid.len() as u64 - 2, 5),
            (1, (3 << 20) as usize),
            (0, model.len()),
            (size - 1, 1),
        ] {
            if (off + len as u64) > size {
                continue;
            }
            let got = read_range(&b, &f.chunks, &[], off, off + len as u64).unwrap();
            assert_eq!(
                got,
                &model[off as usize..off as usize + len],
                "off {off} len {len}"
            );
        }
        assert_eq!(f.chunks.total(), size);
    }

    #[test]
    fn reads_over_a_flushed_chunk_and_its_dirty_overlay_match_the_model() {
        let (_d, b) = blocks();
        let gate = crate::gate::Gate::new();
        let base = pattern(1 << 20, 23);
        let mut f = FileData::default();
        f.write(0, &base);
        f.flush(&b, &gate.enter()).unwrap();
        let mut model = base.clone();
        // one dirty byte inside a flushed chunk, one just past its end, one out in a fresh hole
        for (off, patch) in [
            (7usize, b"X".as_slice()),
            (1 << 20, b"Y".as_slice()),
            (5 << 20, b"Z".as_slice()),
        ] {
            f.write(off as u64, patch);
            model.resize(model.len().max(off + patch.len()), 0);
            model[off..off + patch.len()].copy_from_slice(patch);
        }
        for (off, len) in [
            (0u64, 16usize),
            ((1 << 20) - 2, 5),
            ((1 << 20) - 1, 3),
            ((5 << 20) - 1, 2),
            (0, model.len()),
        ] {
            let end = off + len as u64;
            let ov = f.overlay(off, end);
            let got = read_range(&b, &f.chunks, &ov, off, end).unwrap();
            assert_eq!(
                got,
                &model[off as usize..off as usize + len],
                "off {off} len {len}"
            );
        }
    }

    #[test]
    fn sequential_appends_chunk_like_a_single_write() {
        let (_d, b) = blocks();
        let gate = crate::gate::Gate::new();
        let data = pattern(3 << 20, 9);
        let mut one = FileData::default();
        one.write(0, &data);
        one.flush(&b, &gate.enter()).unwrap();
        let mut many = FileData::default();
        for (i, piece) in data.chunks(700_001).enumerate() {
            many.write((i * 700_001) as u64, piece);
            many.flush(&b, &gate.enter()).unwrap();
        }
        let ids = |f: &FileData| f.chunks.refs.iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(ids(&one), ids(&many));
    }
}
