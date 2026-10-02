//! File handles: `server generation || ino || inode generation || kind || MAC`, 41 bytes.
//!
//! - The generation (start time of the server) makes a restarted server refuse old handles.
//! - The MAC is a keyed BLAKE3 hash with a random per-server key, so a process that never
//!   received a handle from this server cannot forge one for any file.
//! - The inode generation detects inode reuse: the adapter counts the removals of each inode,
//!   the count is in the handle, and a handle minted before the last removal of its inode is
//!   stale even if the `Vfs` reuses the number. The handle of a live inode never changes.
//! - The kind says what the inode names: the file itself, or its AppleDouble sidecar. It is
//!   covered by the MAC like everything else, so it cannot be flipped on a valid handle. It is a
//!   field of its own and not a bit of the number, because the `Vfs` owns the whole `u64` inode
//!   space and spends the top bits itself (`cowfs-core` marks a virtual inode with `1 << 63`).
//! - A file and its AppleDouble sidecar share one inode generation, because they are one inode, so
//!   a sidecar handle goes stale exactly when the file's does.
use std::collections::{HashMap, VecDeque};
use std::io::{self, Read};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use cowfs_vfs::Ino;
use nfsserve::nfs::nfsstat3;

/// What a handle names. A translated AppleDouble sidecar is not an inode of the `Vfs`, so it needs
/// a mark of its own somewhere the `Vfs` cannot reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// An inode of the `Vfs`.
    Plain,
    /// The AppleDouble sidecar of that inode.
    Sidecar,
}

impl Kind {
    fn byte(self) -> u8 {
        match self {
            Kind::Plain => 0,
            Kind::Sidecar => 1,
        }
    }

    fn from_byte(b: u8) -> Result<Self, nfsstat3> {
        match b {
            0 => Ok(Kind::Plain),
            1 => Ok(Kind::Sidecar),
            _ => Err(nfsstat3::NFS3ERR_BADHANDLE),
        }
    }
}

/// Length of every handle.
pub const HANDLE_LEN: usize = 41;
/// Everything the MAC covers: the three words and the kind.
const BODY: usize = 25;
const MAC_LEN: usize = 16;
/// Removed inodes remembered for the epoch check. Beyond it the oldest are forgotten, which only
/// weakens the reuse check for a `Vfs` that breaks the "an Ino is never reused" contract.
const MAX_REMEMBERED: usize = 1 << 18;

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

    /// The generation of the inode `ino` names.
    fn inode_generation(&self, ino: Ino) -> u64 {
        let b = self.buried.lock().unwrap_or_else(PoisonError::into_inner);
        b.generation.get(&ino).copied().unwrap_or(0)
    }

    /// The handle for `ino` as of now.
    pub fn encode(&self, ino: Ino, kind: Kind) -> Vec<u8> {
        let mut h = Vec::with_capacity(HANDLE_LEN);
        h.extend_from_slice(&self.generation.to_le_bytes());
        h.extend_from_slice(&ino.to_le_bytes());
        h.extend_from_slice(&self.inode_generation(ino).to_le_bytes());
        h.push(kind.byte());
        h.extend_from_slice(&mac(&self.key, &h));
        h
    }

    /// The inode and kind a handle names, or why it is refused: `BADHANDLE` for anything not
    /// minted by this server, `STALE` for a previous server generation or a removed inode.
    pub fn decode(&self, data: &[u8]) -> Result<(Ino, Kind), nfsstat3> {
        if data.len() != HANDLE_LEN {
            return Err(nfsstat3::NFS3ERR_BADHANDLE);
        }
        let word = |i: usize| -> u64 {
            u64::from_le_bytes(<[u8; 8]>::try_from(&data[i..i + 8]).expect("checked above"))
        };
        let (generation, ino, inode_generation) = (word(0), word(8), word(16));
        if generation < self.generation {
            return Err(nfsstat3::NFS3ERR_STALE);
        }
        if generation > self.generation || !same(&mac(&self.key, &data[..BODY]), &data[BODY..]) {
            return Err(nfsstat3::NFS3ERR_BADHANDLE);
        }
        let kind = Kind::from_byte(data[BODY - 1])?;
        if inode_generation != self.inode_generation(ino) {
            return Err(nfsstat3::NFS3ERR_STALE);
        }
        Ok((ino, kind))
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
        let h = c.encode(77, Kind::Plain);
        assert_eq!(h.len(), HANDLE_LEN);
        assert_eq!(c.decode(&h), Ok((77, Kind::Plain)));
        c.bury(1);
        assert_eq!(
            c.encode(77, Kind::Plain),
            h,
            "removals elsewhere do not change it"
        );
    }

    /// The `Vfs` owns every `u64` but 0 as an inode number, so no bit of it may change what a
    /// handle means. `cowfs-core` puts `1 << 63` on every virtual inode.
    #[test]
    fn every_inode_number_round_trips_as_an_ordinary_inode() {
        let c = codec();
        for ino in [
            1,
            2,
            1 << 63,
            (1 << 63) | 1,
            0x8000_0100_0000_0001,
            u64::MAX - 1,
            u64::MAX,
        ] {
            assert_eq!(
                c.decode(&c.encode(ino, Kind::Plain)),
                Ok((ino, Kind::Plain)),
                "{ino:#x}"
            );
        }
    }

    #[test]
    fn forged_and_malformed_handles_are_refused() {
        let c = codec();
        let mut h = c.encode(77, Kind::Plain);
        h[8] ^= 1;
        assert_eq!(
            c.decode(&h),
            Err(nfsstat3::NFS3ERR_BADHANDLE),
            "another ino, old MAC"
        );
        let mut h = c.encode(77, Kind::Plain);
        h[39] ^= 1;
        assert_eq!(c.decode(&h), Err(nfsstat3::NFS3ERR_BADHANDLE));
        for len in [0, 1, 16, 24, 25, 40, 42, 64] {
            assert_eq!(
                c.decode(&vec![0; len]),
                Err(nfsstat3::NFS3ERR_BADHANDLE),
                "{len}"
            );
        }
        let other = HandleCodec::with_key([8; 32]);
        assert_eq!(
            c.decode(&other.encode(77, Kind::Plain)),
            Err(nfsstat3::NFS3ERR_BADHANDLE),
            "another key"
        );
    }

    /// The kind is covered by the MAC, so it cannot be changed on a handle that is otherwise
    /// valid: that would turn a file into its sidecar without ever having seen the sidecar.
    #[test]
    fn the_kind_cannot_be_changed_on_a_valid_handle() {
        let c = codec();
        let mut forged = c.encode(9, Kind::Plain);
        forged[BODY - 1] = Kind::Sidecar.byte();
        assert_eq!(c.decode(&forged), Err(nfsstat3::NFS3ERR_BADHANDLE));
        let mut back = c.encode(9, Kind::Sidecar);
        back[BODY - 1] = Kind::Plain.byte();
        assert_eq!(c.decode(&back), Err(nfsstat3::NFS3ERR_BADHANDLE));
        let mut unknown = c.encode(9, Kind::Plain);
        unknown[BODY - 1] = 2;
        assert_eq!(c.decode(&unknown), Err(nfsstat3::NFS3ERR_BADHANDLE));
        let mut stale = c.encode(9, Kind::Sidecar);
        c.bury(9);
        stale[16] ^= 1;
        assert_eq!(
            c.decode(&stale),
            Err(nfsstat3::NFS3ERR_BADHANDLE),
            "a stale inode generation with a kind of its own is still refused"
        );
    }

    #[test]
    fn other_generations_are_told_apart() {
        let c = codec();
        let mut old = c.encode(1, Kind::Plain);
        old[..8].copy_from_slice(&(c.generation() - 1).to_le_bytes());
        assert_eq!(
            c.decode(&old),
            Err(nfsstat3::NFS3ERR_STALE),
            "an earlier server, whatever its MAC"
        );
        let mut new = c.encode(1, Kind::Plain);
        new[..8].copy_from_slice(&(c.generation() + 1).to_le_bytes());
        assert_eq!(c.decode(&new), Err(nfsstat3::NFS3ERR_BADHANDLE));
    }

    #[test]
    fn removing_an_inode_stales_its_old_handles_only() {
        let c = codec();
        let before = c.encode(5, Kind::Plain);
        let other = c.encode(6, Kind::Plain);
        c.bury(5);
        assert_eq!(c.decode(&before), Err(nfsstat3::NFS3ERR_STALE));
        assert_eq!(c.decode(&other), Ok((6, Kind::Plain)));
        let reused = c.encode(5, Kind::Plain);
        assert_eq!(
            c.decode(&reused),
            Ok((5, Kind::Plain)),
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
    fn a_sidecar_handle_stales_with_its_file() {
        let c = codec();
        let file = c.encode(5, Kind::Plain);
        let side = c.encode(5, Kind::Sidecar);
        assert_eq!(
            c.decode(&side),
            Ok((5, Kind::Sidecar)),
            "a sidecar handle names the same inode as the file's"
        );
        c.bury(5);
        assert_eq!(c.decode(&side), Err(nfsstat3::NFS3ERR_STALE));
        assert_eq!(c.decode(&file), Err(nfsstat3::NFS3ERR_STALE));
    }

    #[test]
    fn keys_are_random() {
        assert_ne!(random_key().unwrap(), random_key().unwrap());
    }
}
