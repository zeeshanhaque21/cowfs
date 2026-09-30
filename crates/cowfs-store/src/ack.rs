//! Accepted losses and pending ones. A file of 56 byte records, rewritten whole, newest state last.
//!
//! An entry says "this region of this pack is damaged". `pending` means open found it and nobody
//! has accepted the loss yet, so `has_corruption()` keeps reporting it. `acked` means a caller ran
//! `acknowledge_corruption`. An entry is also bound to the pack's creation nonce and to the block
//! id its header claimed, so damage that is not the same damage is never swallowed.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use crate::fsio::Io;
use crate::BlockId;

const FILE_NAME: &str = "ACKED";
const ENTRY: usize = 68;

/// Offset 0 of a whole pack, with a length that covers any region of it.
pub(crate) const WHOLE_PACK: (u64, u64) = (0, u64::MAX);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Pending,
    Acked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub pack: u32,
    pub nonce: u32,
    pub state: State,
    pub offset: u64,
    pub len: u64,
    pub id: Option<BlockId>,
}

fn encode(e: &Entry) -> [u8; ENTRY] {
    let mut b = [0u8; ENTRY];
    b[..4].copy_from_slice(&e.pack.to_le_bytes());
    b[4..8].copy_from_slice(&e.nonce.to_le_bytes());
    b[8..9].copy_from_slice(&[u8::from(e.state == State::Acked)]);
    b[16..24].copy_from_slice(&e.offset.to_le_bytes());
    b[24..32].copy_from_slice(&e.len.to_le_bytes());
    if let Some(id) = e.id {
        b[32..64].copy_from_slice(id.as_bytes());
    }
    let crc = crc32c::crc32c(&b[..ENTRY - 4]);
    b[ENTRY - 4..].copy_from_slice(&crc.to_le_bytes());
    b
}

fn decode(b: &[u8]) -> Option<Entry> {
    let b: &[u8; ENTRY] = b.try_into().ok()?;
    if crc32c::crc32c(&b[..ENTRY - 4]) != u32::from_le_bytes(b[ENTRY - 4..].try_into().ok()?) {
        return None;
    }
    let state = if b[8] == 0 {
        State::Pending
    } else {
        State::Acked
    };
    let raw: [u8; 32] = b[32..64].try_into().ok()?;
    let id = raw
        .iter()
        .any(|&x| x != 0)
        .then_some(BlockId::from_bytes(raw));
    Some(Entry {
        pack: u32::from_le_bytes(b[..4].try_into().ok()?),
        nonce: u32::from_le_bytes(b[4..8].try_into().ok()?),
        state,
        offset: u64::from_le_bytes(b[16..24].try_into().ok()?),
        len: u64::from_le_bytes(b[24..32].try_into().ok()?),
        id,
    })
}

/// Every entry, newest last. A damaged file yields nothing up to the first bad entry.
pub(crate) fn load(dir: &Path) -> Vec<Entry> {
    let bytes = fs::read(dir.join(FILE_NAME)).unwrap_or_default();
    bytes
        .as_chunks::<ENTRY>()
        .0
        .iter()
        .map_while(|c| decode(c))
        .collect()
}

/// Entries indexed by the region they start at, so a lookup does not walk the whole file.
pub(crate) struct Table {
    by_start: HashMap<(u32, u64), Vec<Entry>>,
}

impl Table {
    pub(crate) fn new(entries: &[Entry]) -> Self {
        let mut by_start: HashMap<(u32, u64), Vec<Entry>> = HashMap::new();
        for e in entries {
            by_start.entry((e.pack, e.offset)).or_default().push(*e);
        }
        Self { by_start }
    }
}

/// Replace the file with `entries` merged over what it holds, keeping only the newest entry per
/// region so that repeated damage at one place cannot grow the file without bound.
pub(crate) fn save(io: &Io, dir: &Path, entries: Vec<Entry>) -> io::Result<()> {
    let mut all = load(dir);
    all.extend(entries);
    let mut newest: HashMap<(u32, u64, u64), usize> = HashMap::new();
    for (i, e) in all.iter().enumerate() {
        newest.insert((e.pack, e.offset, e.len), i);
    }
    let all: Vec<Entry> = newest.into_values().map(|i| all[i]).collect();
    let mut buf = Vec::with_capacity(all.len() * ENTRY);
    for e in &all {
        buf.extend_from_slice(&encode(e));
    }
    io.log_whole(dir, FILE_NAME, &buf);
    io.write_whole(dir, FILE_NAME, &buf)
}

/// The entry that governs this region, newest first. A nonce or id mismatch does not match.
pub(crate) fn find(
    table: &Table,
    pack: u32,
    nonce: u32,
    offset: u64,
    len: u64,
    id: Option<BlockId>,
) -> Option<&Entry> {
    let mut best: Option<&Entry> = None;
    for e in table.by_start.get(&(pack, offset))? {
        if !(e.nonce == nonce || e.nonce == 0)
            || offset.saturating_add(len) > e.offset.saturating_add(e.len)
            || e.id.is_some_and(|x| Some(x) != id)
        {
            continue;
        }
        let newer =
            best.is_none_or(|b| b.offset.saturating_add(b.len) <= e.offset.saturating_add(e.len));
        if newer {
            best = Some(e);
        }
    }
    best
}

/// True when an accepted entry covers the whole of a pack. A pending one does not count: the loss
/// is still to be reported.
pub(crate) fn covers_pack(entries: &[Entry], pack: u32) -> bool {
    entries.iter().rev().any(|e| {
        e.pack == pack
            && e.state == State::Acked
            && e.offset == WHOLE_PACK.0
            && e.len == WHOLE_PACK.1
    })
}
