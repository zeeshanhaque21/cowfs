//! File handles: `server generation || ino || inode generation || MAC`, 40 bytes.
//!
//! - The generation (start time of the server) makes a restarted server refuse old handles.
//! - The MAC is a keyed BLAKE3 hash with a random per-server key, so a process that never
//!   received a handle from this server cannot forge one for any file.
//! - The inode generation detects inode reuse: the adapter counts the removals of each inode,
//!   the count is in the handle, and a handle minted before the last removal of its inode is
//!   stale even if the `Vfs` reuses the number. The handle of a live inode never changes.
use std::collections::{HashMap, VecDeque};
use std::io::{self, Read};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use cowfs_vfs::Ino;
use nfsserve::nfs::nfsstat3;

/// Length of every handle.
pub const HANDLE_LEN: usize = 40;
const MAC_LEN: usize = 16;
/// Removed inodes remembered for the epoch check. Beyond it the oldest are forgotten, which only
/// weakens the reuse check for a `Vfs` that breaks the "an Ino is never reused" contract.
const MAX_REMEMBERED: usize = 1 << 20;

#[derive(Debug, Default)]
struct Buried {
    generation: HashMap<Ino, u64>,
    order: VecDeque<Ino>,
}

/// Encodes and checks file handles.
#[derive(Debug)]
pub struct HandleCodec {
    key: [u8; 32],
    generation: u64,
    buried: Mutex<Buried>,
}

/// 32 random bytes from the operating system.
pub fn random_key() -> io::Result<[u8; 32]> {
    let mut key = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut key)?;
    Ok(key)
}

fn mac(key: &[u8; 32], body: &[u8]) -> [u8; MAC_LEN] {
    let full = blake3::keyed_hash(key, body);
    let mut out = [0u8; MAC_LEN];
    out.copy_from_slice(&full.as_bytes()[..MAC_LEN]);
    out
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl HandleCodec {
    /// A codec with a fresh random key and the current time as generation.
    pub fn new() -> io::Result<Self> {
        Ok(Self::with_key(random_key()?))
    }

    pub fn with_key(key: [u8; 32]) -> Self {
        let generation = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| {
            u64::try_from(d.as_millis()).unwrap_or(u64::MAX).max(1)
        });
        Self {
            key,
            generation,
            buried: Mutex::new(Buried::default()),
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Notes that the last name of `ino` is gone: handles minted so far for it go stale.
    pub fn bury(&self, ino: Ino) {
        let mut b = self.buried.lock().unwrap_or_else(PoisonError::into_inner);
        let g = b.generation.entry(ino).or_insert(0);
        *g += 1;
        if *g == 1 {
            b.order.push_back(ino);
        }
        while b.generation.len() > MAX_REMEMBERED {
            match b.order.pop_front() {
                Some(old) => {
                    b.generation.remove(&old);
                }
                None => break,
            }
        }
    }

    fn inode_generation(&self, ino: Ino) -> u64 {
        let b = self.buried.lock().unwrap_or_else(PoisonError::into_inner);
        b.generation.get(&ino).copied().unwrap_or(0)
    }

    /// The handle for `ino` as of now.
    pub fn encode(&self, ino: Ino) -> Vec<u8> {
        let mut h = Vec::with_capacity(HANDLE_LEN);
        h.extend_from_slice(&self.generation.to_le_bytes());
        h.extend_from_slice(&ino.to_le_bytes());
        h.extend_from_slice(&self.inode_generation(ino).to_le_bytes());
        let m = mac(&self.key, &h);
        h.extend_from_slice(&m);
        h
    }

    /// The inode a handle names, or why it is refused: `BADHANDLE` for anything not minted by
    /// this server, `STALE` for a previous server generation or a removed inode.
    pub fn decode(&self, data: &[u8]) -> Result<Ino, nfsstat3> {
        let word = |i: usize| -> Option<u64> {
            data.get(i..i + 8)
                .and_then(|b| <[u8; 8]>::try_from(b).ok())
                .map(u64::from_le_bytes)
        };
        let (Some(generation), Some(ino), Some(inode_generation)) = (word(0), word(8), word(16))
        else {
            return Err(nfsstat3::NFS3ERR_BADHANDLE);
        };
        if data.len() != HANDLE_LEN {
            return Err(nfsstat3::NFS3ERR_BADHANDLE);
        }
        if generation < self.generation {
            return Err(nfsstat3::NFS3ERR_STALE);
        }
        if generation > self.generation || !same(&mac(&self.key, &data[..24]), &data[24..]) {
            return Err(nfsstat3::NFS3ERR_BADHANDLE);
        }
        if inode_generation != self.inode_generation(ino) {
            return Err(nfsstat3::NFS3ERR_STALE);
        }
        Ok(ino)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codec() -> HandleCodec {
        HandleCodec::with_key([7; 32])
    }

    #[test]
    fn a_handle_round_trips_and_is_stable() {
        let c = codec();
        let h = c.encode(77);
        assert_eq!(h.len(), HANDLE_LEN);
        assert_eq!(c.decode(&h), Ok(77));
        c.bury(1);
        assert_eq!(c.encode(77), h, "removals elsewhere do not change it");
    }

    #[test]
    fn forged_and_malformed_handles_are_refused() {
        let c = codec();
        let mut h = c.encode(77);
        h[8] ^= 1;
        assert_eq!(
            c.decode(&h),
            Err(nfsstat3::NFS3ERR_BADHANDLE),
            "another ino, old MAC"
        );
        let mut h = c.encode(77);
        h[39] ^= 1;
        assert_eq!(c.decode(&h), Err(nfsstat3::NFS3ERR_BADHANDLE));
        for len in [0, 1, 16, 24, 39, 41, 64] {
            assert_eq!(
                c.decode(&vec![0; len]),
                Err(nfsstat3::NFS3ERR_BADHANDLE),
                "{len}"
            );
        }
        let other = HandleCodec::with_key([8; 32]);
        assert_eq!(
            c.decode(&other.encode(77)),
            Err(nfsstat3::NFS3ERR_BADHANDLE),
            "another key"
        );
    }

    #[test]
    fn other_generations_are_told_apart() {
        let c = codec();
        let mut old = c.encode(1);
        old[..8].copy_from_slice(&(c.generation() - 1).to_le_bytes());
        assert_eq!(
            c.decode(&old),
            Err(nfsstat3::NFS3ERR_STALE),
            "an earlier server, whatever its MAC"
        );
        let mut new = c.encode(1);
        new[..8].copy_from_slice(&(c.generation() + 1).to_le_bytes());
        assert_eq!(c.decode(&new), Err(nfsstat3::NFS3ERR_BADHANDLE));
    }

    #[test]
    fn removing_an_inode_stales_its_old_handles_only() {
        let c = codec();
        let before = c.encode(5);
        let other = c.encode(6);
        c.bury(5);
        assert_eq!(c.decode(&before), Err(nfsstat3::NFS3ERR_STALE));
        assert_eq!(c.decode(&other), Ok(6));
        let reused = c.encode(5);
        assert_eq!(
            c.decode(&reused),
            Ok(5),
            "a handle minted after the removal is good"
        );
        assert_eq!(c.decode(&before), Err(nfsstat3::NFS3ERR_STALE));
        c.bury(5);
        assert_eq!(c.decode(&reused), Err(nfsstat3::NFS3ERR_STALE));
    }

    #[test]
    fn memory_of_removals_is_bounded() {
        let c = codec();
        for i in 0..(MAX_REMEMBERED as u64 + 100) {
            c.bury(i);
        }
        let b = c.buried.lock().unwrap();
        assert_eq!(b.generation.len(), MAX_REMEMBERED);
        assert_eq!(b.order.len(), MAX_REMEMBERED);
    }

    #[test]
    fn keys_are_random() {
        assert_ne!(random_key().unwrap(), random_key().unwrap());
    }
}
