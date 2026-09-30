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
    if let Some(bytes) = index {
        fs::write(index_path(dir), bytes).unwrap();
    }
}
