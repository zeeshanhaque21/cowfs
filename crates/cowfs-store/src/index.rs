//! In-memory block index and its on-disk checkpoint. See `docs/v1-store.md`.

use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{PoisonError, RwLock};

use crate::fsio::Io;
use crate::record::HEADER_LEN;
use crate::{BlockId, BLOCK_ID_LEN};

const SHARDS: usize = 64;
const MAGIC: [u8; 8] = *b"COWIDX01";
pub(crate) const FILE_NAME: &str = "index.cix";
const ENTRY_LEN: usize = BLOCK_ID_LEN + 16;

/// Where a record lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Loc {
    pub pack: u32,
    pub offset: u32,
    pub slen: u32,
    pub ulen: u32,
    /// The record was hash-verified in this session. Never persisted.
    pub verified: bool,
}

#[derive(Debug)]
pub(crate) struct Index {
    shards: Vec<RwLock<HashMap<BlockId, Loc>>>,
    blocks: AtomicU64,
    ulen: AtomicU64,
    stored: AtomicU64,
}

/// Index-wide totals, kept current so `stats` is O(1).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Totals {
    pub blocks: u64,
    pub ulen: u64,
    pub stored: u64,
}

fn stored_len(loc: &Loc) -> u64 {
    HEADER_LEN as u64 + u64::from(loc.slen)
}

impl Index {
    pub(crate) fn new() -> Self {
        Self {
            shards: (0..SHARDS).map(|_| RwLock::new(HashMap::new())).collect(),
            blocks: AtomicU64::new(0),
            ulen: AtomicU64::new(0),
            stored: AtomicU64::new(0),
        }
    }

    fn shard(&self, id: &BlockId) -> &RwLock<HashMap<BlockId, Loc>> {
        &self.shards[usize::from(id.as_bytes()[0]) % SHARDS]
    }

    fn add(&self, loc: &Loc) {
        self.blocks.fetch_add(1, Relaxed);
        self.ulen.fetch_add(u64::from(loc.ulen), Relaxed);
        self.stored.fetch_add(stored_len(loc), Relaxed);
    }

    fn sub(&self, loc: &Loc) {
        self.blocks.fetch_sub(1, Relaxed);
        self.ulen.fetch_sub(u64::from(loc.ulen), Relaxed);
        self.stored.fetch_sub(stored_len(loc), Relaxed);
    }

    pub(crate) fn totals(&self) -> Totals {
        Totals {
            blocks: self.blocks.load(Relaxed),
            ulen: self.ulen.load(Relaxed),
            stored: self.stored.load(Relaxed),
        }
    }

    pub(crate) fn get(&self, id: &BlockId) -> Option<Loc> {
        let map = self
            .shard(id)
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        map.get(id).copied()
    }

    /// Returns false and keeps the old entry when `id` is already present.
    pub(crate) fn insert_if_absent(&self, id: BlockId, loc: Loc) -> bool {
        let mut map = self
            .shard(&id)
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        match map.entry(id) {
            Entry::Occupied(_) => false,
            Entry::Vacant(v) => {
                v.insert(loc);
                self.add(&loc);
                true
            }
        }
    }

    /// Insert a hash-verified record. It wins over an existing entry that was never verified.
    pub(crate) fn insert_verified(&self, id: BlockId, loc: Loc) {
        let mut map = self
            .shard(&id)
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        match map.entry(id) {
            Entry::Vacant(v) => {
                v.insert(loc);
                self.add(&loc);
            }
            Entry::Occupied(mut o) if !o.get().verified => {
                self.sub(o.get());
                o.insert(loc);
                self.add(&loc);
            }
            Entry::Occupied(_) => {}
        }
    }

    /// Set the entry for `id` unconditionally.
    pub(crate) fn replace(&self, id: BlockId, loc: Loc) {
        let mut map = self
            .shard(&id)
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(old) = map.insert(id, loc) {
            self.sub(&old);
        }
        self.add(&loc);
    }

    /// Mark the entry verified if it still points at `loc`.
    pub(crate) fn mark_verified(&self, id: &BlockId, loc: Loc) {
        let mut map = self
            .shard(id)
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(cur) = map.get_mut(id) {
            if (cur.pack, cur.offset) == (loc.pack, loc.offset) {
                cur.verified = true;
            }
        }
    }

    /// Copy the entries `keep` accepts, one shard at a time so writers only wait on that shard.
    pub(crate) fn snapshot(&self, keep: impl Fn(&Loc) -> bool) -> Vec<(BlockId, Loc)> {
        let mut out = Vec::new();
        for shard in &self.shards {
            let map = shard.read().unwrap_or_else(PoisonError::into_inner);
            out.extend(map.iter().filter(|(_, v)| keep(v)).map(|(k, v)| (*k, *v)));
        }
        out
    }

    pub(crate) fn ids(&self) -> Vec<BlockId> {
        let mut out = Vec::new();
        for shard in &self.shards {
            let map = shard.read().unwrap_or_else(PoisonError::into_inner);
            out.extend(map.keys().copied());
        }
        out
    }

    pub(crate) fn entries(&self) -> Vec<(BlockId, Loc)> {
        self.snapshot(|_| true)
    }
}

/// A loaded checkpoint: per pack, the length up to which records are indexed, and the entries.
pub(crate) struct Checkpoint {
    pub packs: Vec<(u32, u64)>,
    pub entries: Vec<(BlockId, Loc)>,
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_at_checked(n)?;
        self.0 = tail;
        Some(head)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

/// Parse checkpoint bytes. `None` when malformed or when the CRC fails.
pub(crate) fn parse(bytes: &[u8]) -> Option<Checkpoint> {
    let (body, crc) = bytes.split_at_checked(bytes.len().checked_sub(4)?)?;
    if crc32c::crc32c(body).to_le_bytes() != crc {
        return None;
    }
    let mut c = Cursor(body);
    if c.take(8)? != MAGIC {
        return None;
    }
    let npacks = c.u32()? as usize;
    let nentries = usize::try_from(c.u64()?).ok()?;
    if c.0.len()
        != npacks
            .checked_mul(12)?
            .checked_add(nentries.checked_mul(ENTRY_LEN)?)?
    {
        return None;
    }
    let mut packs = Vec::with_capacity(npacks);
    for _ in 0..npacks {
        packs.push((c.u32()?, c.u64()?));
    }
    let mut entries = Vec::with_capacity(nentries);
    for _ in 0..nentries {
        let id = BlockId::from_bytes(c.take(BLOCK_ID_LEN)?.try_into().ok()?);
        let loc = Loc {
            pack: c.u32()?,
            offset: c.u32()?,
            slen: c.u32()?,
            ulen: c.u32()?,
            verified: false,
        };
        entries.push((id, loc));
    }
    Some(Checkpoint { packs, entries })
}

/// Load `index.cix` from the store directory. `None` when absent or invalid.
pub(crate) fn load(store: &Path) -> Option<Checkpoint> {
    parse(&fs::read(store.join(FILE_NAME)).ok()?)
}

/// Atomically replace `index.cix`.
pub(crate) fn save(
    io: &Io,
    store: &Path,
    packs: &[(u32, u64)],
    entries: &[(BlockId, Loc)],
) -> io::Result<()> {
    let mut buf = Vec::with_capacity(20 + packs.len() * 12 + entries.len() * ENTRY_LEN + 4);
    buf.extend_from_slice(&MAGIC);
    buf.extend_from_slice(&(packs.len() as u32).to_le_bytes());
    buf.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for (id, len) in packs {
        buf.extend_from_slice(&id.to_le_bytes());
        buf.extend_from_slice(&len.to_le_bytes());
    }
    for (id, loc) in entries {
        buf.extend_from_slice(id.as_bytes());
        for v in [loc.pack, loc.offset, loc.slen, loc.ulen] {
            buf.extend_from_slice(&v.to_le_bytes());
        }
    }
    let crc = crc32c::crc32c(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());

    io.log_whole(store, FILE_NAME, &buf);
    io.write_whole(store, FILE_NAME, &buf)
}
