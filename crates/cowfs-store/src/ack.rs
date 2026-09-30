//! Accepted losses. An entry says "this region of this pack is known damaged and accepted".
//!
//! Entries are 24 bytes (pack, offset, len, CRC32C), appended and fsynced. A torn last entry is
//! dropped on load and overwritten by the next append.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::Path;

use crate::fsio::Io;

const FILE_NAME: &str = "ACKED";
const ENTRY: usize = 24;

/// `(offset, len)` that stands for "the whole pack, including its existence".
pub(crate) const WHOLE_PACK: (u64, u64) = (0, u64::MAX);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub pack: u32,
    pub offset: u64,
    pub len: u64,
}

fn encode(e: &Entry) -> [u8; ENTRY] {
    let mut b = [0u8; ENTRY];
    b[..4].copy_from_slice(&e.pack.to_le_bytes());
    b[4..12].copy_from_slice(&e.offset.to_le_bytes());
    b[12..20].copy_from_slice(&e.len.to_le_bytes());
    let crc = crc32c::crc32c(&b[..20]);
    b[20..].copy_from_slice(&crc.to_le_bytes());
    b
}

fn decode(b: &[u8]) -> Option<Entry> {
    let b: &[u8; ENTRY] = b.try_into().ok()?;
    if crc32c::crc32c(&b[..20]).to_le_bytes() != b[20..] {
        return None;
    }
    Some(Entry {
        pack: u32::from_le_bytes(b[..4].try_into().ok()?),
        offset: u64::from_le_bytes(b[4..12].try_into().ok()?),
        len: u64::from_le_bytes(b[12..20].try_into().ok()?),
    })
}

/// Every intact entry before the first damaged one. A missing file is empty.
pub(crate) fn load(dir: &Path) -> Vec<Entry> {
    let bytes = fs::read(dir.join(FILE_NAME)).unwrap_or_default();
    bytes
        .as_chunks::<ENTRY>()
        .0
        .iter()
        .map_while(|c| decode(c))
        .collect()
}

pub(crate) fn append(io: &Io, dir: &Path, entries: &[Entry]) -> io::Result<()> {
    let path = dir.join(FILE_NAME);
    let existed = path.exists();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    let intact = load(dir).len() as u64 * ENTRY as u64;
    let buf: Vec<u8> = entries.iter().flat_map(encode).collect();
    io.write_at(&file, &path, intact, &buf)?;
    io.truncate(&file, &path, intact + buf.len() as u64)?;
    io.sync_file(&file, &path)?;
    if !existed {
        io.created(&path);
        io.sync_dir(dir)?;
    }
    Ok(())
}

/// True when some entry covers the whole of `pack[offset..offset+len]`.
pub(crate) fn covers(entries: &[Entry], pack: u32, offset: u64, len: u64) -> bool {
    entries.iter().any(|e| {
        e.pack == pack
            && e.offset <= offset
            && offset.saturating_add(len) <= e.offset.saturating_add(e.len)
    })
}
