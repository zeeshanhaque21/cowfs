use std::collections::BTreeMap;

pub(crate) const PAGE: u64 = 4096;
const PAGE_USIZE: usize = PAGE as usize;
/// Largest file `MemVfs` accepts; keeps offset arithmetic far from overflow.
pub(crate) const MAX_FILE: u64 = 1 << 42;

/// Sparse file body: only pages that were written exist, holes read as zeros.
#[derive(Default)]
pub(crate) struct Pages {
    pub size: u64,
    map: BTreeMap<u64, Box<[u8; PAGE_USIZE]>>,
}

impl Pages {
    /// 512-byte units of stored pages.
    pub fn blocks(&self) -> u64 {
        self.map.len() as u64 * (PAGE / 512)
    }

    pub fn read(&self, offset: u64, len: u64, pad: bool) -> Vec<u8> {
        let end = if pad {
            offset.saturating_add(len)
        } else {
            self.size.min(offset.saturating_add(len))
        };
        if end <= offset {
            return Vec::new();
        }
        let mut out = vec![0u8; usize::try_from(end - offset).unwrap_or(0)];
        for (idx, page) in self.map.range(offset / PAGE..=(end - 1) / PAGE) {
            let start = idx * PAGE;
            let lo = start.max(offset);
            let hi = (start + PAGE).min(end);
            let (src, dst) = (lo - start, lo - offset);
            let n = usize::try_from(hi - lo).unwrap_or(0);
            let (src, dst) = (src as usize, dst as usize);
            out[dst..dst + n].copy_from_slice(&page[src..src + n]);
        }
        out
    }

    pub fn write(&mut self, offset: u64, data: &[u8], garbage: bool) {
        if garbage && offset > self.size && offset.is_multiple_of(PAGE) && offset >= PAGE {
            self.map
                .insert(offset / PAGE - 1, Box::new([0xAAu8; PAGE_USIZE]));
        }
        let mut pos = offset;
        let mut rest = data;
        while !rest.is_empty() {
            let idx = pos / PAGE;
            let within = (pos % PAGE) as usize;
            let n = rest.len().min(PAGE_USIZE - within);
            let page = self
                .map
                .entry(idx)
                .or_insert_with(|| Box::new([0u8; PAGE_USIZE]));
            page[within..within + n].copy_from_slice(&rest[..n]);
            pos += n as u64;
            rest = &rest[n..];
        }
        self.size = self.size.max(offset + data.len() as u64);
    }

    /// Makes `[a, b)` read as zeros: whole pages are dropped, partial ones zeroed. The caller
    /// has clamped `b` to the size, so the size does not change.
    pub fn punch(&mut self, a: u64, b: u64) {
        if a >= b {
            return;
        }
        for idx in a / PAGE..=(b - 1) / PAGE {
            let start = idx * PAGE;
            let (lo, hi) = (a.max(start), b.min(start + PAGE));
            if hi - lo == PAGE {
                self.map.remove(&idx);
            } else if let Some(page) = self.map.get_mut(&idx) {
                page[(lo - start) as usize..(hi - start) as usize].fill(0);
            }
        }
    }

    pub fn truncate(&mut self, new_size: u64, zero_tail: bool, keep: bool) {
        if new_size < self.size {
            if !keep {
                self.map.split_off(&new_size.div_ceil(PAGE));
            }
            if zero_tail && !new_size.is_multiple_of(PAGE) {
                if let Some(page) = self.map.get_mut(&(new_size / PAGE)) {
                    page[(new_size % PAGE) as usize..].fill(0);
                }
            }
        }
        self.size = new_size;
    }
}
