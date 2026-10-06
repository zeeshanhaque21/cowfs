//! Inode number shapes and the alias table. See `docs/v1-core.md`, "Inode numbers".

use std::collections::HashMap;
// Only the test-only `write_virt_mark` writes a mark now; the production path only reads one.
#[cfg(test)]
use std::io::Write as _;

use cowfs_meta::SnapshotId;
use cowfs_vfs::{Error, Ino, Result, ROOT_INO};

/// Set on inode numbers handed out before meta assigned a real one.
pub(crate) const VIRT: u64 = 1 << 63;
const VIRT_A: &str = "virt.ino.a";
const VIRT_B: &str = "virt.ino.b";
const SHIFT: u32 = 40;
const LOW: u64 = (1 << SHIFT) - 1;
/// Snapshot ids must stay below this to fit in a meta-derived `Ino`. This is meta's own limit
/// (`cowfs_meta::SNAPSHOT_LIMIT`), which `Meta::pack_ino` enforces; core must not be stricter, or a
/// snapshot meta can name would be invisible here.
pub(crate) const MAX_SNAP: u64 = cowfs_meta::SNAPSHOT_LIMIT;
/// A virtual number spends its top bit on the virtual flag, so its snapshot id has 23 bits, not 24.
/// A store with more snapshots than that can still be read; creating a file in the later ones is
/// `NoSpace` (`virt`), which is loud rather than wrong.
pub(crate) const MAX_VIRT_SNAP: u64 = 1 << 23;

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
            snap: (ino >> SHIFT) & (MAX_VIRT_SNAP - 1),
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

// Encodes a legacy virtual number. No production caller left: creates go through meta's own
// reservation now. Kept so the shape tests can still exercise `classify`, `snap_of` and `Aliases`
// against the numbers a pre-meta store would hold.
#[cfg(test)]
pub(crate) fn virt(snap: u64, n: u64) -> Result<Ino> {
    if snap == 0 || snap >= MAX_VIRT_SNAP || n > LOW {
        return Err(Error::NoSpace);
    }
    Ok(VIRT | snap << SHIFT | n)
}

/// Virtual inode number to meta inode number and back, for inodes created by this mount session.
///
/// An entry lives for as long as the inode it names has a name, so a number a client already holds
/// keeps meaning the same inode for the whole session. Both directions are stored because `canon`
/// is on the `lookup` path.
///
/// `Clone` so a commit can take a copy of the map before opening meta's writer lock.
#[derive(Debug, Default, Clone)]
pub(crate) struct Aliases {
    fwd: HashMap<Ino, u64>,
    rev: HashMap<Ino, Ino>,
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
        self.rev.get(&pack(snap, m).ok()?).copied()
    }

    pub(crate) fn insert(&mut self, virt: Ino, snap: u64, m: u64) {
        self.fwd.insert(virt, m);
        // The reverse map is a bridge from a meta inode to the *different* number a caller holds.
        // A reservation-backed create hands out the packed meta number itself, so `virt` is already
        // canonical and a reverse entry would be `pm -> pm`. `canon` reads this map and `load_node`
        // reloads through its result, so a self-map makes that reload recurse into itself forever.
        // Leave the forward entry (the alias count and `meta_of` need it); the reverse lookup is a
        // miss, and `Inner::canon` falls through to `pack`, which is the same number.
        if let Ok(pm) = pack(snap, m) {
            if pm != virt {
                self.rev.insert(pm, virt);
            }
        }
    }

    pub(crate) fn remove(&mut self, virt: Ino) {
        if let (Some(m), Some(snap)) = (self.fwd.remove(&virt), snap_of(virt)) {
            if let Ok(pm) = pack(snap, m) {
                self.rev.remove(&pm);
            }
        }
    }

    pub(crate) fn purge_snapshot(&mut self, snap: u64) {
        self.fwd.retain(|v, _| snap_of(*v) != Some(snap));
        self.rev
            .retain(|pm, _| matches!(classify(*pm), Id::Meta { snap: s, .. } if s != snap));
    }

    pub(crate) fn len(&self) -> usize {
        self.fwd.len()
    }

    /// Bytes the two maps hold, from their bucket counts. Exact, unlike an RSS delta, which also
    /// carries the dentry table and redb's own growth.
    pub(crate) fn bytes(&self) -> usize {
        (self.fwd.capacity() + self.rev.capacity()) * (2 * size_of::<u64>() + 1)
    }
}

/// Reads the durable virtual-number high-water mark, 0 when there is none.
/// Where the durable virtual-number mark is read and written.
///
/// Two copies of the same 16-byte record (the value twice, so a torn write is detected), written
/// alternately through a temporary file and a rename, so at least one copy is always intact.
pub(crate) fn read_virt_mark(root: &std::path::Path) -> Mark {
    let mut best = 0u64;
    let mut any = false;
    let mut bad = false;
    for name in [VIRT_A, VIRT_B] {
        match std::fs::read(root.join(name)) {
            Ok(b) => match parse_mark(&b) {
                Some(v) => {
                    any = true;
                    best = best.max(v);
                }
                None => bad = true,
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => bad = true,
            Err(_) => bad = true,
        }
    }
    if any && !bad {
        return Mark::Value(best);
    }
    if any {
        return Mark::Damaged(best);
    }
    Mark::Missing
}

/// What the mark file says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    /// Both copies agree.
    Value(u64),
    /// At least one copy is intact and one is not: take the intact one.
    Damaged(u64),
    /// Nothing usable.
    Missing,
}

impl Mark {
    /// The counter to start from, with the reason to log when the mark was not intact.
    pub(crate) fn counter(&self, has_state: bool) -> (u64, Option<String>) {
        match *self {
            // a zero mark with committed state cannot be believed: a rolled-back or restored mark
            // looks exactly like this
            Mark::Value(0) if has_state => (
                SAFETY,
                Some(format!(
                    "virt.ino: zero with committed state, continuing from {SAFETY}"
                )),
            ),
            Mark::Value(v) => (v, None),
            Mark::Damaged(v) => (
                v,
                Some(format!(
                    "virt.ino: one copy is damaged, continuing from {v} (the other copy is intact)"
                )),
            ),
            Mark::Missing if has_state => (
                SAFETY,
                Some(format!(
                    "virt.ino: missing with committed state, continuing from {SAFETY}; \
                     inode numbers below it may have been handed out by an earlier session"
                )),
            ),
            Mark::Missing => (
                0,
                Some("virt.ino: missing and no committed state".to_string()),
            ),
        }
    }
}

/// Where the counter starts when the mark is lost: far above anything a session could have used, so
/// a reused number needs billions of creates in one mount.
pub(crate) const SAFETY: u64 = 1 << 32;

/// The low bits of a virtual inode number that hold the counter, for tests and diagnostics.
pub const VIRT_COUNTER_MASK: u64 = (1 << SHIFT) - 1;

fn parse_mark(b: &[u8]) -> Option<u64> {
    let v: [u8; 8] = b.get(..8)?.try_into().ok()?;
    let w: [u8; 8] = b.get(8..16)?.try_into().ok()?;
    let n = u64::from_le_bytes(v);
    (u64::from_le_bytes(w) == n).then_some(n)
}

/// Records `n` durably in the copy that does not hold the newest value, through a temporary file and
/// a rename, so the previous copy survives a crash anywhere in here.
///
/// Nothing writes the mark in production any more, but the read side is still live for stores
/// written before meta owned the reservation, so the tests need a way to lay one down.
#[cfg(test)]
pub(crate) fn write_virt_mark(root: &std::path::Path, n: u64) -> std::io::Result<()> {
    let mut b = [0u8; 16];
    b[..8].copy_from_slice(&n.to_le_bytes());
    b[8..].copy_from_slice(&n.to_le_bytes());
    let (newest, other) = newest_copy(root);
    let tmp = root.join(format!("{other}.tmp"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&b)?;
        crate::fsops::sync_file(&f, &tmp)?;
    }
    std::fs::rename(&tmp, root.join(other))?;
    crate::fsops::note("virt_mark_renamed");
    crate::fsops::sync_dir(root)?;
    // remember which copy is newest without a durable write: the value itself says
    let _ = newest;
    Ok(())
}

/// The copy that holds the highest intact value, so the next write goes to the other one.
#[cfg(test)]
fn newest_copy(root: &std::path::Path) -> (&'static str, &'static str) {
    let a = parse_mark(&std::fs::read(root.join(VIRT_A)).unwrap_or_default()).unwrap_or(0);
    let b = parse_mark(&std::fs::read(root.join(VIRT_B)).unwrap_or_default()).unwrap_or(0);
    if a >= b {
        (VIRT_A, VIRT_B)
    } else {
        (VIRT_B, VIRT_A)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark_seam() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn a_legacy_virtual_mark_syncs_its_bytes_then_rename_then_directory() {
        let _seam = mark_seam();
        let d = tempfile::tempdir().unwrap();
        write_virt_mark(d.path(), 12343).unwrap();
        write_virt_mark(d.path(), 12344).unwrap();
        crate::fsops::arm();
        let result = write_virt_mark(d.path(), 12345);
        let trace = crate::fsops::trace_take();
        crate::fsops::disarm();
        result.unwrap();
        let position = |prefix: &str| trace.iter().position(|e| e.starts_with(prefix)).unwrap();
        assert!(position("sync_file:virt.ino") < position("virt_mark_renamed"));
        assert!(position("virt_mark_renamed") < position("sync_dir:"));
        assert_eq!(read_virt_mark(d.path()), Mark::Value(12345));
    }

    #[test]
    fn a_legacy_virtual_mark_refuses_a_failed_directory_sync() {
        let _seam = mark_seam();
        let d = tempfile::tempdir().unwrap();
        crate::fsops::arm();
        crate::fsops::set_fault(crate::fsops::Fault::DirSync, &d.path().to_string_lossy(), 1);
        let result = write_virt_mark(d.path(), 12345);
        crate::fsops::disarm();
        assert!(result.is_err());
    }

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
    fn the_virtual_mark_round_trips_and_a_torn_one_falls_back_to_the_other() {
        let _seam = mark_seam();
        let d = tempfile::tempdir().unwrap();
        assert_eq!(read_virt_mark(d.path()), Mark::Missing);
        let (v, e) = read_virt_mark(d.path()).counter(false);
        assert_eq!(v, 0);
        assert!(e.unwrap().contains("missing"), "a missing mark is reported");
        write_virt_mark(d.path(), 12345).unwrap();
        write_virt_mark(d.path(), 12346).unwrap();
        assert_eq!(read_virt_mark(d.path()), Mark::Value(12346));
        // tear the copy that does not hold the newest value: the newest is intact
        std::fs::write(d.path().join(newest_copy(d.path()).1), b"").unwrap();
        assert_eq!(read_virt_mark(d.path()), Mark::Damaged(12346));
        // both gone with committed state: a safety margin, loudly
        let _ = std::fs::remove_file(d.path().join(VIRT_A));
        let _ = std::fs::remove_file(d.path().join(VIRT_B));
        let (v, e) = read_virt_mark(d.path()).counter(true);
        assert_eq!(v, SAFETY);
        assert!(e.unwrap().contains("missing"));
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

    /// The reserved-create regression: `commit_batch` aliases every committed create by its packed
    /// meta number, which for a reservation-backed create is the number the caller already holds, so
    /// `insert` was called with `virt == pack(snap, m)`. The reverse map then held `pm -> pm`, and
    /// `load_node` reloads through `canon`, so an uncached node recursed forever and aborted the
    /// process. A physical self-alias must make `canon` miss (the caller of `canon`, `Inner::canon`,
    /// then falls through to `pack`, the same number) while the forward map still carries the entry.
    #[test]
    fn a_physical_self_alias_does_not_create_a_reverse_self_map() {
        let mut a = Aliases::default();
        let pm = pack(2, 40).unwrap();
        a.insert(pm, 2, 40);
        // the forward map is the alias count and the `meta_of` bridge; it must be there
        assert_eq!(a.meta_of(pm), Some(40));
        assert_eq!(a.len(), 1, "the live inode still counts one alias");
        // the reverse lookup must NOT return the same number it was asked about, or the reload
        // through `canon` never terminates
        assert_eq!(
            a.canon(2, 40),
            None,
            "a physical number must not be its own reverse alias"
        );
        // a different meta inode is unaffected
        assert_eq!(a.canon(2, 41), None);
        // removal frees the one entry, and the absent reverse entry is not resurrected
        a.remove(pm);
        assert_eq!(a.len(), 0);
        assert_eq!(a.meta_of(pm), None);
    }

    /// The forward-only physical entry must coexist with a genuine virtual bridge in the same table,
    /// so a later legacy number does not resurrect the self-map and a physical removal does not drop
    /// the virtual entry's reverse bridge.
    #[test]
    fn a_physical_self_alias_does_not_disturb_a_virtual_bridge() {
        let mut a = Aliases::default();
        let v = virt(2, 1).unwrap();
        a.insert(v, 2, 40);
        let pm = pack(2, 40).unwrap();
        // the same meta inode seen under its physical number is the shape a reservation create makes
        a.insert(pm, 2, 40);
        assert_eq!(a.canon(2, 40), Some(v), "the virtual bridge still resolves");
        assert_eq!(a.len(), 2, "both aliases are live");
        a.remove(v);
        assert_eq!(
            a.canon(2, 40),
            None,
            "after the virtual alias is gone only the physical number is left, no bridge"
        );
        assert_eq!(a.len(), 1);
    }
}
