//! Inode number shapes and the alias table. See `docs/v1-core.md`, "Inode numbers".

use std::collections::{HashMap, HashSet};
use std::io::Write as _;

use cowfs_meta::SnapshotId;
use cowfs_vfs::{Error, Ino, Result, ROOT_INO};

/// Set on inode numbers handed out before meta assigned a real one.
pub(crate) const VIRT: u64 = 1 << 63;
/// How many virtual numbers one durable reservation covers.
pub(crate) const VIRT_BLOCK: u64 = 1 << 20;
const VIRT_FILE: &str = "virt.ino";
const SHIFT: u32 = 40;
const LOW: u64 = (1 << SHIFT) - 1;
/// Snapshot ids must stay below this to fit in an `Ino`.
pub(crate) const MAX_SNAP: u64 = 1 << 23;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Id {
    Root,
    Meta { snap: u64, m: u64 },
    Virt { snap: u64 },
}

pub(crate) fn classify(ino: Ino) -> Id {
    if ino == ROOT_INO {
        Id::Root
    } else if ino & VIRT != 0 {
        Id::Virt {
            snap: (ino >> SHIFT) & (MAX_SNAP - 1),
        }
    } else {
        Id::Meta {
            snap: ino >> SHIFT,
            m: ino & LOW,
        }
    }
}

pub(crate) fn snap_of(ino: Ino) -> Option<u64> {
    match classify(ino) {
        Id::Root => None,
        Id::Meta { snap, .. } | Id::Virt { snap } => Some(snap),
    }
}

/// The meta-derived inode number of meta inode `m` in snapshot `snap`.
pub(crate) fn pack(snap: u64, m: u64) -> Result<Ino> {
    if snap >= MAX_SNAP {
        return Err(Error::NoSpace);
    }
    cowfs_meta::Meta::pack_ino(SnapshotId(snap), cowfs_meta::Ino(m)).ok_or(Error::NoSpace)
}

pub(crate) fn virt(snap: u64, n: u64) -> Result<Ino> {
    if snap == 0 || snap >= MAX_SNAP || n > LOW {
        return Err(Error::NoSpace);
    }
    Ok(VIRT | snap << SHIFT | n)
}

/// Virtual inode number to meta inode number and back, for files created by this mount session.
///
/// `Clone` so a commit can take a copy of the map before opening meta's writer lock.
#[derive(Debug, Default, Clone)]
pub(crate) struct Aliases {
    fwd: HashMap<Ino, u64>,
    rev: HashMap<(u64, u64), Ino>,
}

impl Aliases {
    pub(crate) fn meta_of(&self, virt: Ino) -> Option<u64> {
        self.fwd.get(&virt).copied()
    }

    /// The number a meta inode is known by: its virtual alias if it has one.
    pub(crate) fn canon(&self, snap: u64, m: u64) -> Option<Ino> {
        if self.rev.is_empty() {
            return None;
        }
        self.rev.get(&(snap, m)).copied()
    }

    pub(crate) fn insert(&mut self, virt: Ino, snap: u64, m: u64) {
        self.fwd.insert(virt, m);
        self.rev.insert((snap, m), virt);
    }

    pub(crate) fn remove(&mut self, virt: Ino) {
        if let (Some(m), Some(snap)) = (self.fwd.remove(&virt), snap_of(virt)) {
            self.rev.remove(&(snap, m));
        }
    }

    pub(crate) fn purge_snapshot(&mut self, snap: u64) {
        self.fwd.retain(|v, _| snap_of(*v) != Some(snap));
        self.rev.retain(|(s, _), _| *s != snap);
    }

    pub(crate) fn len(&self) -> usize {
        self.fwd.len()
    }

    /// Every virtual number that still has a meta number.
    pub(crate) fn live(&self) -> impl Iterator<Item = Ino> + '_ {
        self.fwd.keys().copied()
    }

    /// Drops every alias except those in `keep`, so `canon` can never return a released number.
    pub(crate) fn retain_only(&mut self, keep: &HashSet<Ino>) {
        self.fwd.retain(|v, _| keep.contains(v));
        self.rev = self
            .fwd
            .iter()
            .filter_map(|(v, m)| snap_of(*v).map(|s| ((s, *m), *v)))
            .collect();
    }
}

/// Reads the durable virtual-number high-water mark, 0 when there is none.
pub(crate) fn read_virt_mark(root: &std::path::Path) -> std::result::Result<u64, String> {
    let p = root.join(VIRT_FILE);
    let b = match std::fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(format!("{}: {e}", p.display())),
    };
    let v: [u8; 8] = b
        .get(..8)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| format!("{} is short", p.display()))?;
    let w: [u8; 8] = b
        .get(8..16)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| format!("{} is short", p.display()))?;
    let n = u64::from_le_bytes(v);
    if u64::from_le_bytes(w) != n {
        return Err(format!("{} is torn", p.display()));
    }
    Ok(n)
}

/// Records `n` durably (value twice, so a torn write is detected rather than believed).
pub(crate) fn write_virt_mark(root: &std::path::Path, n: u64) -> std::io::Result<()> {
    let p = root.join(VIRT_FILE);
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&n.to_le_bytes());
    b[8..].copy_from_slice(&n.to_le_bytes());
    let mut f = std::fs::File::create(&p)?;
    f.write_all(&b)?;
    f.sync_all()?;
    std::fs::File::open(root)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_round_trip() {
        assert_eq!(classify(ROOT_INO), Id::Root);
        let i = pack(5, 77).unwrap();
        assert_eq!(classify(i), Id::Meta { snap: 5, m: 77 });
        assert_eq!(pack(5, 1).unwrap(), 5 << 40 | 1);
        let v = virt(5, 9).unwrap();
        assert_eq!(classify(v), Id::Virt { snap: 5 });
        assert_eq!(snap_of(v), Some(5));
        assert_ne!(pack(5, 9).unwrap(), pack(6, 9).unwrap());
    }

    #[test]
    fn out_of_range_is_rejected() {
        assert!(pack(0, 1).is_err());
        assert!(pack(MAX_SNAP, 1).is_err());
        assert!(pack(1, LOW + 1).is_err());
        assert!(virt(0, 1).is_err());
    }

    #[test]
    fn the_virtual_mark_round_trips_and_a_torn_one_is_refused() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(read_virt_mark(d.path()), Ok(0));
        write_virt_mark(d.path(), 12345).unwrap();
        assert_eq!(read_virt_mark(d.path()), Ok(12345));
        let p = d.path().join("virt.ino");
        let mut b = std::fs::read(&p).unwrap();
        b[9] ^= 0xff;
        std::fs::write(&p, &b).unwrap();
        assert!(read_virt_mark(d.path()).is_err());
    }

    #[test]
    fn aliases_map_both_ways_and_drain() {
        let mut a = Aliases::default();
        let v = virt(2, 1).unwrap();
        a.insert(v, 2, 40);
        assert_eq!(a.meta_of(v), Some(40));
        assert_eq!(a.canon(2, 40), Some(v));
        assert_eq!(a.canon(3, 40), None);
        a.remove(v);
        assert_eq!(a.len(), 0);
        assert_eq!(a.canon(2, 40), None);
    }
}
