//! Record format and per-block compression. See `docs/v1-store.md`.

use std::cell::RefCell;
use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

use zstd::bulk::{Compressor, Decompressor};

use crate::{BlockId, BLOCK_ID_LEN, MAX_BLOCK_LEN};

pub(crate) const RECORD_MAGIC: [u8; 4] = *b"CWRB";
pub(crate) const HEADER_LEN: usize = 56;
const HCRC_AT: usize = 48;
const RCRC_AT: usize = 52;
const ZSTD_LEVEL: i32 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Codec {
    Raw = 0,
    Zstd = 1,
}

/// A parsed record header. The CRC covers the payload too, so it is checked separately.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Header {
    pub codec: Codec,
    pub ulen: u32,
    pub slen: u32,
    pub id: BlockId,
    pub crc: u32,
}

/// The offset where the record whose header starts at `pos` ends, when that header checksums.
///
/// A header that passes its own checksum states its length truthfully, even when its payload does
/// not verify, so the bytes up to that offset are that record's and a record found inside them is
/// nested, not real. A header whose checksum fails says nothing about its length, since the flipped
/// byte may be the length itself, so trusting it could hide the real records that follow.
pub(crate) fn trusted_end_at(file: &File, pos: u64, end: u64) -> io::Result<Option<u64>> {
    if pos + HEADER_LEN as u64 > end {
        return Ok(None);
    }
    let mut raw = [0u8; HEADER_LEN];
    file.read_exact_at(&mut raw, pos)?;
    Ok(Header::parse(&raw)
        .ok()
        .map(|h| pos + h.total_len())
        .filter(|&e| e <= end && e > pos))
}

impl Header {
    pub(crate) fn total_len(&self) -> u64 {
        HEADER_LEN as u64 + u64::from(self.slen)
    }

    /// Header-only checks: magic, header checksum, padding, codec and length rules.
    pub(crate) fn parse(buf: &[u8; HEADER_LEN]) -> Result<Header, &'static str> {
        if buf[..4] != RECORD_MAGIC {
            return Err("bad record magic");
        }
        if crc32c::crc32c(&buf[..HCRC_AT]) != u32_at(buf, HCRC_AT) {
            return Err("bad header checksum");
        }
        if buf[5..8] != [0, 0, 0] {
            return Err("nonzero padding");
        }
        let ulen = u32_at(buf, 8);
        let slen = u32_at(buf, 12);
        if ulen as usize > MAX_BLOCK_LEN {
            return Err("block length over maximum");
        }
        let codec = match buf[4] {
            0 if slen == ulen => Codec::Raw,
            1 if slen < ulen => Codec::Zstd,
            0 | 1 => return Err("stored length inconsistent with codec"),
            _ => return Err("unknown codec"),
        };
        let mut id = [0u8; BLOCK_ID_LEN];
        id.copy_from_slice(&buf[16..48]);
        Ok(Header {
            codec,
            ulen,
            slen,
            id: BlockId::from_bytes(id),
            crc: u32_at(buf, RCRC_AT),
        })
    }

    /// CRC that `crc` must equal, given the raw header bytes and the payload.
    pub(crate) fn expected_crc(raw: &[u8; HEADER_LEN], payload: &[u8]) -> u32 {
        crc32c::crc32c_append(crc32c::crc32c(&raw[..HCRC_AT]), payload)
    }
}

fn u32_at(buf: &[u8], at: usize) -> u32 {
    let mut b = [0u8; 4];
    b.copy_from_slice(&buf[at..at + 4]);
    u32::from_le_bytes(b)
}

thread_local! {
    static COMPRESSOR: RefCell<Option<Compressor<'static>>> = const { RefCell::new(None) };
    static DECOMPRESSOR: RefCell<Option<Decompressor<'static>>> = const { RefCell::new(None) };
}

/// Encode a whole record (header and payload) for `data`, whose id is `id`.
pub(crate) fn encode(id: BlockId, data: &[u8]) -> io::Result<Vec<u8>> {
    let compressed = COMPRESSOR.with(|c| -> io::Result<Vec<u8>> {
        let mut slot = c.borrow_mut();
        if slot.is_none() {
            *slot = Some(Compressor::new(ZSTD_LEVEL)?);
        }
        match slot.as_mut() {
            Some(comp) => comp.compress(data),
            None => Err(io::Error::other("zstd compressor unavailable")),
        }
    })?;
    let (codec, payload): (Codec, &[u8]) = if compressed.len() * 100 <= data.len() * 95 {
        (Codec::Zstd, &compressed)
    } else {
        (Codec::Raw, data)
    };
    let mut rec = Vec::with_capacity(HEADER_LEN + payload.len());
    rec.extend_from_slice(&RECORD_MAGIC);
    rec.extend_from_slice(&[codec as u8, 0, 0, 0]);
    rec.extend_from_slice(&(data.len() as u32).to_le_bytes());
    rec.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    rec.extend_from_slice(id.as_bytes());
    let hcrc = crc32c::crc32c(&rec);
    let rcrc = crc32c::crc32c_append(hcrc, payload);
    rec.extend_from_slice(&hcrc.to_le_bytes());
    rec.extend_from_slice(&rcrc.to_le_bytes());
    rec.extend_from_slice(payload);
    Ok(rec)
}

/// Decode a payload whose CRC has already been checked. Does not check the block hash.
pub(crate) fn decode(header: &Header, payload: &[u8]) -> Result<Vec<u8>, &'static str> {
    match header.codec {
        Codec::Raw => Ok(payload.to_vec()),
        Codec::Zstd => {
            let out = DECOMPRESSOR.with(|d| -> io::Result<Vec<u8>> {
                let mut slot = d.borrow_mut();
                if slot.is_none() {
                    *slot = Some(Decompressor::new()?);
                }
                match slot.as_mut() {
                    Some(dec) => dec.decompress(payload, header.ulen as usize),
                    None => Err(io::Error::other("zstd decompressor unavailable")),
                }
            });
            match out {
                Ok(v) if v.len() == header.ulen as usize => Ok(v),
                Ok(_) => Err("decoded length differs from header"),
                Err(_) => Err("zstd payload does not decode"),
            }
        }
    }
}

/// True when the payload decodes and its bytes hash to the record's id.
pub(crate) fn verify(header: &Header, payload: &[u8]) -> bool {
    match decode(header, payload) {
        Ok(data) => BlockId::of(&data) == header.id,
        Err(_) => false,
    }
}
