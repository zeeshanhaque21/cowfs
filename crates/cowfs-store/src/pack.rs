//! Pack files and the resynchronizing scanner. See `docs/v1-store.md`.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::record::{Header, HEADER_LEN, RECORD_MAGIC};

pub(crate) const PACK_MAGIC: [u8; 8] = *b"COWPACK\0";
pub(crate) const PACK_VERSION: u32 = 1;
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

pub(crate) fn header_ok(file: &File) -> io::Result<bool> {
    let mut buf = [0u8; PACK_HEADER_LEN as usize];
    file.read_exact_at(&mut buf, 0)?;
    Ok(buf == header_bytes())
}

/// What the scanner found.
pub(crate) enum Event<'a> {
    /// A whole record whose CRC matched.
    Record {
        offset: u64,
        header: &'a Header,
        payload: &'a [u8],
    },
    /// Bytes that are not part of any valid record. `trailing` means none follows to the end.
    Gap {
        offset: u64,
        len: u64,
        trailing: bool,
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

/// Smallest offset in `from..end` where a valid record begins.
fn find_record(file: &File, from: u64, end: u64, payload: &mut Vec<u8>) -> io::Result<Option<u64>> {
    let mut window = vec![0u8; SEARCH_WINDOW];
    let mut base = from;
    while base + (HEADER_LEN as u64) <= end {
        let n = window.len().min((end - base) as usize);
        file.read_exact_at(&mut window[..n], base)?;
        let mut at = 0;
        while let Some(i) = window[at..n]
            .windows(RECORD_MAGIC.len())
            .position(|w| w == RECORD_MAGIC)
        {
            let cand = base + (at + i) as u64;
            if read_record(file, cand, end, payload)?.is_some() {
                return Ok(Some(cand));
            }
            at += i + 1;
        }
        if base + (n as u64) >= end {
            break;
        }
        base += (n - (RECORD_MAGIC.len() - 1)) as u64;
    }
    Ok(None)
}

/// Scan `file[from..end]`, calling `visit` for every valid record and every invalid gap in order.
pub(crate) fn scan(
    file: &File,
    from: u64,
    end: u64,
    mut visit: impl FnMut(Event<'_>) -> Result<()>,
) -> Result<()> {
    let mut payload = Vec::new();
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
        match find_record(file, pos + 1, end, &mut payload)? {
            Some(next) => {
                visit(Event::Gap {
                    offset: pos,
                    len: next - pos,
                    trailing: false,
                })?;
                pos = next;
            }
            None => {
                visit(Event::Gap {
                    offset: pos,
                    len: end - pos,
                    trailing: true,
                })?;
                break;
            }
        }
    }
    Ok(())
}
