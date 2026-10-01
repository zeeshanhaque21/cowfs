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

/// Name of the file that records how much of a retired pack was durable.
pub(crate) fn cut_name(id: u32) -> String {
    format!("pack-{id:08}.cpk.cut")
}

/// Read a pack's cut file, which is the durable length of a pack whose torn tail was cut.
pub(crate) fn cut_len(dir: &Path, id: u32) -> Option<u64> {
    let b = std::fs::read(pack_dir(dir).join(cut_name(id))).ok()?;
    if b.len() != 12 || crc32c::crc32c(&b[..8]) != u32::from_le_bytes(b[8..12].try_into().ok()?) {
        return None;
    }
    Some(u64::from_le_bytes(b[..8].try_into().ok()?))
}

pub(crate) fn parse_pack_name(name: &str) -> Option<u32> {
    name.strip_prefix("pack-")?
        .strip_suffix(".cpk")?
        .parse()
        .ok()
}

/// Header layout: magic 8, version 4, creation nonce 4. The nonce is what binds a stale checkpoint
/// to a specific pack, so a checkpoint can never validate against a different pack that has the id.
/// It is written once, into a file that is empty at the time, so no durable byte is ever rewritten.
pub(crate) const NONCE_AT: usize = 12;

pub(crate) fn header_bytes(nonce: u32) -> [u8; PACK_HEADER_LEN as usize] {
    let mut h = [0u8; PACK_HEADER_LEN as usize];
    h[..8].copy_from_slice(&PACK_MAGIC);
    h[8..12].copy_from_slice(&PACK_VERSION.to_le_bytes());
    h[NONCE_AT..NONCE_AT + 4].copy_from_slice(&nonce.to_le_bytes());
    h
}

/// The creation nonce in a header.
pub(crate) fn header_nonce(buf: &[u8; PACK_HEADER_LEN as usize]) -> u32 {
    u32::from_le_bytes(buf[NONCE_AT..NONCE_AT + 4].try_into().unwrap_or([0; 4]))
}

/// A nonce that is never 0, so a pack does not look like a version-1 pack.
pub(crate) fn new_nonce(id: u32) -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let d = SystemTime::now().duration_since(UNIX_EPOCH);
    let nanos = d.map_or(0u64, |d| d.as_secs() ^ u64::from(d.subsec_nanos()));
    let mut input = Vec::with_capacity(20);
    input.extend_from_slice(&nanos.to_le_bytes());
    input.extend_from_slice(&id.to_le_bytes());
    input.extend_from_slice(&std::process::id().to_le_bytes());
    let hash = blake3::hash(&input);
    let n = u32::from_le_bytes(hash.as_bytes()[..4].try_into().unwrap_or([1, 0, 0, 0]));
    if n == 0 {
        1
    } else {
        n
    }
}

/// `Ok(nonce)` for a readable pack. `Err` says why not, including the all-zero header that a crash
/// between extending a pack file and writing its data leaves behind.
pub(crate) fn header_check(file: &File) -> io::Result<std::result::Result<u32, &'static str>> {
    let mut buf = [0u8; PACK_HEADER_LEN as usize];
    file.read_exact_at(&mut buf, 0)?;
    Ok(
        if buf[..8] == PACK_MAGIC && buf[8..12] == PACK_VERSION.to_le_bytes() {
            Ok(header_nonce(&buf))
        } else if buf.iter().all(|&b| b == 0) {
            Err("empty pack (no data was written)")
        } else if buf[..8] == PACK_MAGIC {
            Err("unsupported pack format version (format 1 packs are not readable)")
        } else {
            Err("bad pack header")
        },
    )
}

/// What the scanner found.
pub(crate) enum Event<'a> {
    /// A whole record whose CRC matched.
    Record {
        offset: u64,
        header: &'a Header,
        /// The stored bytes, for a caller that verifies the hash as well.
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

/// How much payload reading a scan may do. [`Work::Bounded`] is what an open uses, so that
/// crafted input cannot make it read without limit; [`Work::Unlimited`] is for salvage, which is
/// run by an operator on a store that already needs repair.
#[derive(Clone, Copy)]
pub(crate) enum Work {
    /// Stop when the bound runs out.
    Bounded(u64),
    /// Read whatever the bytes claim.
    Unlimited,
}

impl Work {
    fn charge(&mut self, slen: u32) -> bool {
        match self {
            Self::Bounded(b) => charge(b, slen),
            Self::Unlimited => true,
        }
    }
}

/// Take `slen` bytes from the work bound. A candidate that exactly fits is still read.
pub(crate) fn charge(budget: &mut u64, slen: u32) -> bool {
    if *budget < u64::from(slen) {
        return false;
    }
    *budget -= u64::from(slen);
    true
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
    budget: &mut Work,
) -> io::Result<Found> {
    let mut window = vec![0u8; SEARCH_WINDOW];
    let mut base = from;
    while base + (HEADER_LEN as u64) <= end {
        let n = window.len().min((end - base) as usize);
        file.read_exact_at(&mut window[..n], base)?;
        let mut at = 0;
        while let Some(p) = magic_at(&window[..n], at) {
            let cand = base + p as u64;
            if std::env::var_os("COWFS_PACK_DEBUG").is_some() {
                eprintln!("MAGIC {cand} magic={:?}", &window[p..p + 4]);
            }
            let head = match window[p..n].first_chunk::<HEADER_LEN>() {
                Some(raw) => {
                    let h = Header::parse(raw);
                    if h.is_err() && std::env::var_os("COWFS_PACK_DEBUG").is_some() {
                        eprintln!("REJECT {cand}: {:?}", h.err());
                    }
                    h.ok()
                }
                None => peek_header(file, cand, end)?,
            };
            if std::env::var_os("COWFS_PACK_DEBUG").is_some() {
                eprintln!(
                    "CHECK {cand} head={:?}",
                    head.map(|h| (h.slen, h.total_len(), cand + h.total_len() <= end))
                );
            }
            if let Some(h) = head.filter(|h| cand + h.total_len() <= end) {
                if std::env::var_os("COWFS_PACK_DEBUG").is_some() {
                    eprintln!("CAND {cand} slen={}", h.slen);
                }
                if !budget.charge(h.slen) {
                    return Ok(Found::Exhausted);
                }
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

/// What a deep scan found. A deep scan trusts nothing in the bytes: a record counts only when its
/// header, its checksums and the BLAKE3 hash of its payload all agree.
pub(crate) enum Deep {
    /// A record whose bytes verify. The payload is not handed out: it was already checked.
    Record { offset: u64, header: Header },
    /// Bytes that hold no verifiable record.
    Gap { offset: u64, len: u64 },
}

/// Slide over every byte of `file[..end]` and yield what verifies.
///
/// It is linear in the pack size plus one payload read per position that carries the record magic,
/// so a pack full of forged headers costs one read per header. It is a repair tool, so it does not
/// stop early: see [`Work::Unlimited`].
pub(crate) fn scan_deep(
    file: &File,
    end: u64,
    mut visit: impl FnMut(Deep) -> io::Result<()>,
) -> io::Result<()> {
    let mut work = Work::Unlimited;
    let mut payload = Vec::new();
    let mut pos = PACK_HEADER_LEN;
    while pos < end {
        match find_record(file, pos, end, &mut payload, &mut work)? {
            Found::At(off) => {
                let Some(header) = read_record(file, off, end, &mut payload)? else {
                    break;
                };
                if off > pos {
                    visit(Deep::Gap {
                        offset: pos,
                        len: off - pos,
                    })?;
                }
                let total = header.total_len();
                // The checksums in the header are only as good as the bytes that carry them, so the
                // payload hash decides. This is what a forged header with valid checksums fails.
                let found = crate::record::verify(&header, &payload);
                visit(if found {
                    Deep::Record {
                        offset: off,
                        header,
                    }
                } else {
                    Deep::Gap {
                        offset: off,
                        len: total,
                    }
                })?;
                pos = off + total;
            }
            Found::Nothing => {
                visit(Deep::Gap {
                    offset: pos,
                    len: end - pos,
                })?;
                pos = end;
            }
            Found::Exhausted => break,
        }
    }
    Ok(())
}

/// Scan `file[from..end]`, calling `visit` for every valid record and every invalid gap in order.
pub(crate) fn scan(
    file: &File,
    from: u64,
    end: u64,
    mut visit: impl FnMut(Event<'_>) -> Result<()>,
) -> Result<()> {
    let mut payload = Vec::new();
    let mut budget = Work::Bounded(2 * end.saturating_sub(from) + (1 << 20));
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

#[cfg(test)]
mod tests {
    use super::charge;

    #[test]
    fn the_work_bound_allows_a_candidate_that_exactly_fits() {
        let mut b = 10u64;
        assert!(charge(&mut b, 10));
        assert_eq!(b, 0);
        let mut b = 9u64;
        assert!(!charge(&mut b, 10));
        assert_eq!(b, 9, "a refused candidate costs nothing");
        let mut b = 11u64;
        assert!(charge(&mut b, 0));
        assert_eq!(b, 11);
    }
}
