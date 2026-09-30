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

/// Records of a well-formed pack as `(id, start, end)`, parsed independently of the library.
pub fn parse_pack(bytes: &[u8]) -> Vec<(BlockId, usize, usize)> {
    let mut out = Vec::new();
    let mut pos = 16;
    while pos + 52 <= bytes.len() {
        assert_eq!(&bytes[pos..pos + 4], b"CWRB");
        let slen = u32::from_le_bytes(bytes[pos + 12..pos + 16].try_into().unwrap()) as usize;
        let id = BlockId::from_bytes(bytes[pos + 16..pos + 48].try_into().unwrap());
        out.push((id, pos, pos + 52 + slen));
        pos += 52 + slen;
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
