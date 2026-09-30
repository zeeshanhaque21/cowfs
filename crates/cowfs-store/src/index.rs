//! In-memory block index and its on-disk checkpoint. See `docs/v1-store.md`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{PoisonError, RwLock};

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
}

#[derive(Debug)]
pub(crate) struct Index {
    shards: Vec<RwLock<HashMap<BlockId, Loc>>>,
}

impl Index {
    pub(crate) fn new() -> Self {
        Self {
            shards: (0..SHARDS).map(|_| RwLock::new(HashMap::new())).collect(),
        }
    }

    fn shard(&self, id: &BlockId) -> &RwLock<HashMap<BlockId, Loc>> {
        &self.shards[usize::from(id.as_bytes()[0]) % SHARDS]
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
            std::collections::hash_map::Entry::Occupied(_) => false,
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(loc);
                true
            }
        }
    }

    pub(crate) fn entries(&self) -> Vec<(BlockId, Loc)> {
        let mut out = Vec::new();
        for shard in &self.shards {
            let map = shard.read().unwrap_or_else(PoisonError::into_inner);
            out.extend(map.iter().map(|(k, v)| (*k, *v)));
        }
        out
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

    let tmp = store.join(format!("{FILE_NAME}.tmp"));
    let mut f = File::create(&tmp)?;
    f.write_all(&buf)?;
    f.sync_all()?;
    fs::rename(&tmp, store.join(FILE_NAME))?;
    File::open(store)?.sync_all()
}
