//! Pack files and the resynchronizing scanner. See `docs/v1-store.md`.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::record::{Header, HEADER_LEN, RECORD_MAGIC};

pub(crate) const PACK_MAGIC: [u8; 8] = *b"COWPACK\0";
pub(crate) const PACK_VERSION: u32 = 2;
pub(crate) const PACK_HEADER_LEN: u64 = 16;
const SEARCH_WINDOW: usize = 1 << 20;

pub(crate) fn pack_dir(store: &Path) -> PathBuf {
    store.join("packs")
}

pub(crate) fn pack_path(store: &Path, id: u32) -> PathBuf {
    pack_dir(store).join(format!("pack-{id:08}.cpk"))
}

pub(crate) fn parse_pack_name(name: &str) -> Option<u32> {
    name.strip_prefix("pack-")?
        .strip_suffix(".cpk")?
        .parse()
        .ok()
}

pub(crate) fn header_bytes() -> [u8; PACK_HEADER_LEN as usize] {
    let mut h = [0u8; PACK_HEADER_LEN as usize];
    h[..8].copy_from_slice(&PACK_MAGIC);
    h[8..12].copy_from_slice(&PACK_VERSION.to_le_bytes());
    h
}

/// Check the pack header. `Err` names why this is not a pack this version can read.
pub(crate) fn header_check(file: &File) -> io::Result<std::result::Result<(), &'static str>> {
    let mut buf = [0u8; PACK_HEADER_LEN as usize];
    file.read_exact_at(&mut buf, 0)?;
    Ok(if buf == header_bytes() {
        Ok(())
    } else if buf[..8] == PACK_MAGIC && buf[8..12] != PACK_VERSION.to_le_bytes() {
        Err("unsupported pack format version (format 1 packs are not readable)")
    } else {
        Err("bad pack header")
    })
}

/// What the scanner found.
pub(crate) enum Event<'a> {
    /// A whole record whose CRC matched.
    Record {
        offset: u64,
        header: &'a Header,
        payload: &'a [u8],
    },
    /// Bytes that are not part of any valid record. `exhausted` means the search gave up because
    /// it hit its work bound, so the rest of the pack was not searched.
    Gap {
        offset: u64,
        len: u64,
        exhausted: bool,
    },
}

/// Read and check the record at `pos`. `Ok(None)` means not a valid record.
fn read_record(
    file: &File,
    pos: u64,
    end: u64,
    payload: &mut Vec<u8>,
) -> io::Result<Option<Header>> {
    if pos + HEADER_LEN as u64 > end {
        return Ok(None);
    }
    let mut raw = [0u8; HEADER_LEN];
    file.read_exact_at(&mut raw, pos)?;
    let Ok(header) = Header::parse(&raw) else {
        return Ok(None);
    };
    if pos + header.total_len() > end {
        return Ok(None);
    }
    payload.resize(header.slen as usize, 0);
    file.read_exact_at(payload, pos + HEADER_LEN as u64)?;
    if Header::expected_crc(&raw, payload) != header.crc {
        return Ok(None);
    }
    Ok(Some(header))
}

fn magic_at(window: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    while let Some(i) = window[at..].iter().position(|&b| b == RECORD_MAGIC[0]) {
        let p = at + i;
        if window.len() - p >= RECORD_MAGIC.len() && window[p..p + 4] == RECORD_MAGIC {
            return Some(p);
        }
        at = p + 1;
    }
    None
}

enum Found {
    At(u64),
    Nothing,
    Exhausted,
}

/// Smallest offset in `from..end` where a valid record begins.
///
/// A candidate is screened by its header checksum alone, so only a header that really was
/// written by us costs a payload read. Work is linear in the bytes searched plus one payload
/// read per surviving header. Damage that comes from crashes or bit rot cannot make headers with
/// valid checksums, so the payload bytes read for candidates stay below twice the bytes scanned.
/// `budget` enforces that bound against crafted input and is never reached otherwise.
fn find_record(
    file: &File,
    from: u64,
    end: u64,
    payload: &mut Vec<u8>,
    budget: &mut u64,
) -> io::Result<Found> {
    let mut window = vec![0u8; SEARCH_WINDOW];
    let mut base = from;
    while base + (HEADER_LEN as u64) <= end {
        let n = window.len().min((end - base) as usize);
        file.read_exact_at(&mut window[..n], base)?;
        let mut at = 0;
        while let Some(p) = magic_at(&window[..n], at) {
            let cand = base + p as u64;
            let head = match window[p..n].first_chunk::<HEADER_LEN>() {
                Some(raw) => Header::parse(raw).ok(),
                None => peek_header(file, cand, end)?,
            };
            if let Some(h) = head.filter(|h| cand + h.total_len() <= end) {
                if *budget < u64::from(h.slen) {
                    return Ok(Found::Exhausted);
                }
                *budget -= u64::from(h.slen);
                if read_record(file, cand, end, payload)?.is_some() {
                    return Ok(Found::At(cand));
                }
            }
            at = p + 1;
        }
        if base + (n as u64) >= end {
            break;
        }
        base += (n - (RECORD_MAGIC.len() - 1)) as u64;
    }
    Ok(Found::Nothing)
}

/// The header at `pos` if it is structurally valid and its record fits before `end`.
fn peek_header(file: &File, pos: u64, end: u64) -> io::Result<Option<Header>> {
    if pos + HEADER_LEN as u64 > end {
        return Ok(None);
    }
    let mut raw = [0u8; HEADER_LEN];
    file.read_exact_at(&mut raw, pos)?;
    Ok(Header::parse(&raw)
        .ok()
        .filter(|h| pos + h.total_len() <= end))
}

/// The id a header at `pos` claims, if its structure is valid. Unverified.
pub(crate) fn peek_id(file: &File, pos: u64, end: u64) -> Option<crate::BlockId> {
    peek_header(file, pos, end).ok().flatten().map(|h| h.id)
}

/// Scan `file[from..end]`, calling `visit` for every valid record and every invalid gap in order.
pub(crate) fn scan(
    file: &File,
    from: u64,
    end: u64,
    mut visit: impl FnMut(Event<'_>) -> Result<()>,
) -> Result<()> {
    let mut payload = Vec::new();
    let mut budget = 2 * end.saturating_sub(from) + (1 << 20);
    let mut pos = from;
    while pos < end {
        if let Some(header) = read_record(file, pos, end, &mut payload)? {
            visit(Event::Record {
                offset: pos,
                header: &header,
                payload: &payload,
            })?;
            pos += header.total_len();
            continue;
        }
        match find_record(file, pos + 1, end, &mut payload, &mut budget)? {
            Found::At(next) => {
                visit(Event::Gap {
                    offset: pos,
                    len: next - pos,
                    exhausted: false,
                })?;
                pos = next;
            }
            found => {
                visit(Event::Gap {
                    offset: pos,
                    len: end - pos,
                    exhausted: matches!(found, Found::Exhausted),
                })?;
                break;
            }
        }
    }
    Ok(())
}
