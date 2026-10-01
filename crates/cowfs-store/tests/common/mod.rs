#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use cowfs_store::{BlockId, Options};

/// Deterministic incompressible bytes (splitmix64).
pub fn random(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        out.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    out.truncate(len);
    out
}

/// Deterministic bytes that zstd shrinks a lot.
pub fn compressible(seed: u64, len: usize) -> Vec<u8> {
    let noise = random(seed, 64);
    (0..len)
        .map(|i| noise[(i / 13) % 64] ^ ((i / 1000) as u8))
        .collect()
}

pub fn opts() -> Options {
    Options {
        checkpoint_on_drop: false,
        ..Options::default()
    }
}

pub fn pack_path(dir: &Path, id: u32) -> PathBuf {
    dir.join("packs").join(format!("pack-{id:08}.cpk"))
}

pub fn index_path(dir: &Path) -> PathBuf {
    dir.join("index.cix")
}

pub fn pack_ids(dir: &Path) -> Vec<u32> {
    let mut ids: Vec<u32> = fs::read_dir(dir.join("packs"))
        .unwrap()
        .filter_map(|e| {
            let name = e.unwrap().file_name().into_string().unwrap();
            name.strip_prefix("pack-")?
                .strip_suffix(".cpk")?
                .parse()
                .ok()
        })
        .collect();
    ids.sort_unstable();
    ids
}

/// Bytes of a record header.
pub const REC_HDR: usize = 56;

/// A valid pack header.
/// `COWPACK\0`, version 2, nonce 0x44332211.
pub const PACK_HEADER: [u8; 16] = [
    b'C', b'O', b'W', b'P', b'A', b'C', b'K', 0, 2, 0, 0, 0, 0x11, 0x22, 0x33, 0x44,
];

/// Records of a well-formed pack as `(id, start, end)`, parsed independently of the library.
pub fn parse_pack(bytes: &[u8]) -> Vec<(BlockId, usize, usize)> {
    let mut out = Vec::new();
    let mut pos = PACK_HEADER.len();
    while pos + REC_HDR <= bytes.len() {
        assert_eq!(&bytes[pos..pos + 4], b"CWRB");
        let slen = u32::from_le_bytes(bytes[pos + 12..pos + 16].try_into().unwrap()) as usize;
        let id = BlockId::from_bytes(bytes[pos + 16..pos + 48].try_into().unwrap());
        out.push((id, pos, pos + REC_HDR + slen));
        pos += REC_HDR + slen;
    }
    assert_eq!(pos, bytes.len());
    out
}

/// Copy the store files, without the lock, into `to`.
pub fn copy_store(from: &Path, to: &Path) {
    fs::create_dir_all(to.join("packs")).unwrap();
    for id in pack_ids(from) {
        fs::copy(pack_path(from, id), pack_path(to, id)).unwrap();
    }
    if index_path(from).exists() {
        fs::copy(index_path(from), index_path(to)).unwrap();
    }
}

pub struct Fixture {
    pub blocks: Vec<(BlockId, Vec<u8>)>,
    pub packs: Vec<Vec<u8>>,
    pub index: Vec<u8>,
}

/// Build a store from `(len, compressible)` specs, synced and checkpointed, and return its files.
pub fn fixture(specs: &[(usize, bool)], max_pack_size: u64) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut blocks = Vec::new();
    {
        let s = cowfs_store::Store::open(
            dir.path(),
            Options {
                max_pack_size,
                checkpoint_on_drop: false,
                ..Default::default()
            },
        )
        .unwrap();
        for (i, &(len, comp)) in specs.iter().enumerate() {
            let seed = 1000 + i as u64;
            let d = if comp {
                compressible(seed, len)
            } else {
                random(seed, len)
            };
            blocks.push((s.put(&d).unwrap(), d));
        }
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    let packs = pack_ids(dir.path())
        .into_iter()
        .enumerate()
        .map(|(i, id)| {
            assert_eq!(i as u32, id);
            fs::read(pack_path(dir.path(), id)).unwrap()
        })
        .collect();
    let index = fs::read(index_path(dir.path())).unwrap();
    Fixture {
        blocks,
        packs,
        index,
    }
}

/// Replace the contents of the store's `packs/` directory and its index file.
pub fn install(dir: &Path, packs: &[(u32, &[u8])], index: Option<&[u8]>) {
    fs::create_dir_all(dir.join("packs")).unwrap();
    for e in fs::read_dir(dir.join("packs")).unwrap() {
        fs::remove_file(e.unwrap().path()).unwrap();
    }
    for (id, bytes) in packs {
        fs::write(pack_path(dir, *id), bytes).unwrap();
    }
    let _ = fs::remove_file(index_path(dir));
    let _ = fs::remove_file(dir.join("SYNCED"));
    let _ = fs::remove_file(dir.join("ACKED"));
    if let Some(bytes) = index {
        fs::write(index_path(dir), bytes).unwrap();
    }
}

/// A `SYNCED` watermark file saying pack `pack` is durable up to `len`, and that pack ids
/// below `next` are never reused.
pub fn wm_bytes(pack: u32, len: u64) -> Vec<u8> {
    wm_bytes_full(pack, len, pack.saturating_add(1))
}

pub fn wm_bytes_full(pack: u32, len: u64, next: u32) -> Vec<u8> {
    let mut b = vec![0u8; 64];
    b[..8].copy_from_slice(&2u64.to_le_bytes());
    b[8..12].copy_from_slice(&pack.to_le_bytes());
    b[16..24].copy_from_slice(&len.to_le_bytes());
    b[28..32].copy_from_slice(&next.to_le_bytes());
    let crc = crc32c::crc32c(&b[..24]);
    b[24..28].copy_from_slice(&crc.to_le_bytes());
    b
}

/// Like [`install`], and also write a watermark when `mark` is given.
pub fn install_wm(
    dir: &Path,
    packs: &[(u32, &[u8])],
    index: Option<&[u8]>,
    mark: Option<(u32, u64)>,
) {
    install(dir, packs, index);
    if let Some((pack, len)) = mark {
        fs::write(dir.join("SYNCED"), wm_bytes(pack, len)).unwrap();
    }
}

/// Encode a record with a valid CRC, whatever the payload and claimed id.
pub fn record(codec: u8, ulen: u32, id: [u8; 32], payload: &[u8]) -> Vec<u8> {
    let mut r = b"CWRB".to_vec();
    r.extend_from_slice(&[codec, 0, 0, 0]);
    r.extend_from_slice(&ulen.to_le_bytes());
    r.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    r.extend_from_slice(&id);
    let hcrc = crc32c::crc32c(&r);
    r.extend_from_slice(&hcrc.to_le_bytes());
    r.extend_from_slice(&crc32c::crc32c_append(hcrc, payload).to_le_bytes());
    r.extend_from_slice(payload);
    r
}

/// Encode an index checkpoint with a valid CRC. Each pack is (id, indexed length, nonce).
pub fn index_bytes(packs: &[(u32, u64)], entries: &[(BlockId, [u32; 4])]) -> Vec<u8> {
    let packs: Vec<(u32, u64, u32)> = packs
        .iter()
        .map(|&(id, len)| (id, len, nonce_of(&PACK_HEADER)))
        .collect();
    index_bytes_full(&packs, entries)
}

/// The creation nonce in a pack header.
pub fn nonce_of(header: &[u8]) -> u32 {
    u32::from_le_bytes(header[12..16].try_into().unwrap_or([0; 4]))
}

pub fn index_bytes_full(packs: &[(u32, u64, u32)], entries: &[(BlockId, [u32; 4])]) -> Vec<u8> {
    let mut b = b"COWIDX02".to_vec();
    b.extend_from_slice(&(packs.len() as u32).to_le_bytes());
    b.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for (id, len, nonce) in packs {
        b.extend_from_slice(&id.to_le_bytes());
        b.extend_from_slice(&len.to_le_bytes());
        b.extend_from_slice(&nonce.to_le_bytes());
    }
    for (id, loc) in entries {
        b.extend_from_slice(id.as_bytes());
        for v in loc {
            b.extend_from_slice(&v.to_le_bytes());
        }
    }
    let crc = crc32c::crc32c(&b);
    b.extend_from_slice(&crc.to_le_bytes());
    b
}
