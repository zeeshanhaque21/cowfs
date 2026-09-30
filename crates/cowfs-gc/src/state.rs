//! Batched last-access times, kept in memory and flushed in batches.
//!
//! A read must not become a write, so an access only touches memory here.
//! The batch file is append-only and a torn last record is dropped on load, so a crash in the
//! middle of a flush costs the hints in that batch and nothing else.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use cowfs_store::BlockId;

const MAGIC: &[u8] = b"COWAT01";
const ENTRY: usize = 36;
const MAGIC_MARKS: &[u8] = b"COWMARK1";
const ROOT_ENTRY: usize = 32;

/// Seconds since the Unix epoch, 0 when the clock is before it or unreadable.
pub fn now() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs().min(u64::from(u32::MAX)) as u32)
}

/// Batched last-access times. In memory, bounded, flushed in batches, and only ever a hint.
#[derive(Debug)]
pub struct Hints {
    path: PathBuf,
    times: HashMap<BlockId, u32>,
    dirty: Vec<(BlockId, u32)>,
    cap: usize,
    dropped: u64,
}

impl Hints {
    /// Load the batch file. A missing, short or torn file is a shorter set, never an error: a lost
    /// hint only costs an ordering decision.
    pub fn load(dir: &Path, cap: usize) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join("atime.bin");
        let mut times = HashMap::new();
        if let Ok(bytes) = fs::read(&path) {
            let mut rest = bytes.get(MAGIC.len()..).unwrap_or(&[]);
            while let Some(head) = rest.first_chunk::<ENTRY>() {
                let id = BlockId::from_bytes(head[..32].try_into().unwrap_or([0; 32]));
                let at = u32::from_le_bytes(head[32..].try_into().unwrap_or([0; 4]));
                rest = &rest[ENTRY..];
                let e = times.entry(id).or_insert(at);
                *e = (*e).max(at);
            }
        }
        if times.len() > cap {
            times.retain(|_, v| *v > 0);
        }
        while times.len() > cap {
            let oldest = times.iter().min_by_key(|(_, v)| **v).map(|(k, _)| *k);
            match oldest {
                Some(k) => {
                    times.remove(&k);
                }
                None => break,
            }
        }
        Ok(Self {
            path,
            times,
            dirty: Vec::new(),
            cap,
            dropped: 0,
        })
    }

    /// Record an access. Never touches the disk.
    pub fn note(&mut self, id: BlockId) {
        let now = now();
        let known = self.times.contains_key(&id);
        if !known && self.times.len() >= self.cap {
            self.dropped += 1;
            return;
        }
        match self.times.entry(id) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                if *e.get() < now {
                    let old = e.insert(now);
                    self.dirty.push((id, old));
                }
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(now);
                self.dirty.push((id, 0));
            }
        }
    }

    /// The last access second recorded for a block, 0 when none.
    pub fn get(&self, id: &BlockId) -> u32 {
        self.times.get(id).copied().unwrap_or(0)
    }

    /// Number of tracked hints.
    pub fn tracked(&self) -> u64 {
        self.times.len() as u64
    }

    /// Hints not recorded because the cap was reached. A lost hint, never a lost block.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Mean last-access second of `ids`, counting an unrecorded block as 0, the coldest value
    /// there is. A caller sorts candidates by this, so a cycle reclaims cold packs first.
    ///
    /// This is a hint. It orders work; it never decides what is freed.
    pub fn coldness(&self, ids: &[BlockId], live_bytes: u64) -> u64 {
        if live_bytes == 0 {
            return 0;
        }
        if ids.is_empty() {
            // No records, or no hint recorded for any of them: treat the pack as cold.
            return 0;
        }
        let sum: u64 = ids.iter().map(|b| u64::from(self.get(b))).sum();
        sum / ids.len() as u64
    }

    /// Append the pending hints and fsync them. Returns the number of records written.
    pub fn flush(&mut self) -> io::Result<usize> {
        if self.dirty.is_empty() {
            return Ok(0);
        }
        let mut buf = Vec::with_capacity(MAGIC.len() + self.dirty.len() * ENTRY);
        buf.extend_from_slice(MAGIC);
        for (id, old) in &self.dirty {
            let at = self.times.get(id).copied().unwrap_or(*old);
            buf.extend_from_slice(id.as_bytes());
            buf.extend_from_slice(&at.to_le_bytes());
        }
        let existed = self.path.exists();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)?;
        let at = file.metadata()?.len();
        file.write_all_at(&buf, at)?;
        file.set_len(at + buf.len() as u64)?;
        file.sync_data()?;
        if !existed {
            if let Some(parent) = self.path.parent() {
                File::open(parent)?.sync_all()?;
            }
        }
        let n = self.dirty.len();
        self.dirty.clear();
        Ok(n)
    }
}

/// Snapshot roots an earlier cycle walked, and the blocks those walks yielded.
///
/// This is the persistent half of the incremental marking: a snapshot whose root is already here
/// is skipped whole, and the blocks its earlier walk yielded are still known live, so a block is
/// never condemned because the walk that found it did not run again.
#[derive(Debug)]
pub struct Marks {
    path: PathBuf,
    roots: HashSet<[u8; 32]>,
    blocks: HashSet<BlockId>,
    cap: usize,
    /// Set when the file held more ids than the cap. The set is then dropped, and the next cycle
    /// does a full walk, which is always correct.
    pub dropped: bool,
}

impl Marks {
    /// Load the file. A missing, short or corrupt file is an empty set: a full walk is always
    /// correct, so losing the incremental state costs time and nothing else.
    pub fn load(dir: &Path, cap: usize) -> Self {
        let path = dir.join("mark.bin");
        let mut out = Self {
            path,
            cap,
            dropped: false,
            roots: HashSet::new(),
            blocks: HashSet::new(),
        };
        let Ok(bytes) = fs::read(&out.path) else {
            return out;
        };
        let Some(body) = bytes.strip_prefix(MAGIC_MARKS) else {
            return out;
        };
        let Some((n_roots, rest)) = split_u64(body) else {
            return out;
        };
        let Some((n_blocks, rest)) = split_u64(rest) else {
            return out;
        };
        let (Some(root_bytes), Some(block_bytes)) = (
            n_roots.checked_mul(ROOT_ENTRY),
            n_blocks.checked_mul(ROOT_ENTRY),
        ) else {
            return out;
        };
        if rest.len() < root_bytes + block_bytes {
            return out;
        }
        let (roots, blocks) = rest.split_at(root_bytes);
        for c in roots.as_chunks::<ROOT_ENTRY>().0 {
            out.roots.insert(*c);
        }
        for c in blocks.as_chunks::<ROOT_ENTRY>().0 {
            if out.blocks.len() >= cap {
                out.dropped = true;
                break;
            }
            out.blocks.insert(BlockId::from_bytes(*c));
        }
        if out.roots.len() >= cap {
            out.dropped = true;
        }
        out
    }

    /// True when an earlier cycle already walked this root.
    pub fn has_root(&self, root: &[u8; ROOT_ENTRY]) -> bool {
        self.roots.contains(root)
    }

    /// Every block an earlier cycle yielded, so a skipped root's blocks stay live.
    pub fn blocks(&self) -> &HashSet<BlockId> {
        &self.blocks
    }

    /// Record a root an earlier cycle walked.
    pub fn add_root(&mut self, root: &[u8; ROOT_ENTRY]) {
        if self.roots.len() >= self.cap {
            self.dropped = true;
            return;
        }
        self.roots.insert(*root);
    }

    /// Record a block a walk yielded.
    pub fn add_block(&mut self, id: BlockId) {
        if self.blocks.len() >= self.cap {
            self.dropped = true;
            return;
        }
        self.blocks.insert(id);
    }

    /// Replace the file with the current set, dropping `dead` first.
    ///
    /// Written whole through a temporary name, fsynced and renamed, so a crash leaves the previous
    /// set or the new one, never a half-written mix that would let a later cycle condemn a block
    /// whose root it is about to skip.
    pub fn save(&mut self, dead: &HashSet<BlockId>) -> io::Result<()> {
        for id in dead {
            self.blocks.remove(id);
        }
        if self.dropped {
            let _ = fs::remove_file(&self.path);
            self.roots.clear();
            self.blocks.clear();
            return Ok(());
        }
        let mut buf = Vec::with_capacity(
            MAGIC_MARKS.len() + 16 + (self.roots.len() + self.blocks.len()) * ROOT_ENTRY,
        );
        buf.extend_from_slice(MAGIC_MARKS);
        buf.extend_from_slice(&(self.roots.len() as u64).to_le_bytes());
        buf.extend_from_slice(&(self.blocks.len() as u64).to_le_bytes());
        let mut roots: Vec<&[u8; 32]> = self.roots.iter().collect();
        roots.sort_unstable();
        for r in roots {
            buf.extend_from_slice(r);
        }
        let mut blocks: Vec<&BlockId> = self.blocks.iter().collect();
        blocks.sort_unstable();
        for b in blocks {
            buf.extend_from_slice(b.as_bytes());
        }
        let tmp = self.path.with_extension("tmp");
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&buf)?;
            f.sync_data()?;
        }
        fs::rename(&tmp, &self.path)?;
        if let Some(parent) = self.path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}

fn split_u64(b: &[u8]) -> Option<(usize, &[u8])> {
    let (head, rest) = b.split_at_checked(8)?;
    Some((
        usize::try_from(u64::from_le_bytes(head.try_into().ok()?)).ok()?,
        rest,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A root is 32 bytes of node hash. `NodeId` has no public constructor, so the tests use the
    /// same bytes the collector sees: `NodeId::as_bytes`.
    fn root(seed: u8) -> [u8; ROOT_ENTRY] {
        [seed; ROOT_ENTRY]
    }

    #[test]
    fn marks_survive_a_reload() {
        let d = tempfile::tempdir().unwrap();
        let b = BlockId::of(b"x");
        let mut m = Marks::load(d.path(), 16);
        m.add_root(&root(7));
        m.add_block(b);
        m.save(&HashSet::new()).unwrap();
        let back = Marks::load(d.path(), 16);
        assert!(back.has_root(&root(7)));
        assert!(back.blocks().contains(&b));
        assert!(!back.dropped);
    }

    #[test]
    fn a_different_root_is_not_treated_as_walked() {
        let d = tempfile::tempdir().unwrap();
        let mut m = Marks::load(d.path(), 16);
        m.add_root(&root(1));
        assert!(!m.has_root(&root(2)), "a changed root is walked again");
        assert!(m.has_root(&root(1)), "the same root is skipped");
    }

    #[test]
    fn a_real_node_id_round_trips_through_the_file() {
        // The bytes the collector writes are the bytes `NodeId::as_bytes` gives, so a root the
        // metadata crate produced is skipped on the next cycle.
        let dir = tempfile::tempdir().unwrap();
        let meta = cowfs_meta::Meta::open(
            dir.path().join("meta"),
            cowfs_meta::Options {
                background: false,
                ..cowfs_meta::Options::default()
            },
        )
        .unwrap();
        let snap = meta.new_snapshot("s").unwrap();
        let bytes = *snap.root().unwrap().as_bytes();
        let d = tempfile::tempdir().unwrap();
        let mut m = Marks::load(d.path(), 16);
        m.add_root(&bytes);
        m.save(&HashSet::new()).unwrap();
        assert!(Marks::load(d.path(), 16).has_root(&bytes));
    }

    #[test]
    fn a_dead_block_leaves_the_persisted_set() {
        let d = tempfile::tempdir().unwrap();
        let b = BlockId::of(b"x");
        let mut m = Marks::load(d.path(), 16);
        m.add_block(b);
        m.save(&HashSet::from([b])).unwrap();
        assert!(Marks::load(d.path(), 16).blocks().is_empty());
    }

    #[test]
    fn a_corrupt_file_is_an_empty_set_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("mark.bin"), b"not a mark file at all").unwrap();
        let m = Marks::load(d.path(), 16);
        assert!(m.blocks().is_empty());
        assert!(!m.dropped);
    }

    #[test]
    fn a_truncated_file_is_an_empty_set() {
        let d = tempfile::tempdir().unwrap();
        let mut m = Marks::load(d.path(), 16);
        m.add_block(BlockId::of(b"x"));
        m.save(&HashSet::new()).unwrap();
        let path = d.path().join("mark.bin");
        let mut b = fs::read(&path).unwrap();
        b.truncate(b.len() - 3);
        fs::write(&path, &b).unwrap();
        assert!(Marks::load(d.path(), 16).blocks().is_empty());
    }

    #[test]
    fn a_full_set_is_dropped_rather_than_written_huge() {
        let d = tempfile::tempdir().unwrap();
        let mut m = Marks::load(d.path(), 2);
        m.add_block(BlockId::of(b"a"));
        m.add_block(BlockId::of(b"b"));
        m.add_block(BlockId::of(b"c"));
        assert!(m.dropped);
        m.save(&HashSet::new()).unwrap();
        assert!(!d.path().join("mark.bin").exists());
    }

    #[test]
    fn notes_survive_a_reload_and_the_newest_wins() {
        let d = tempfile::tempdir().unwrap();
        let id = BlockId::of(b"a");
        let mut h = Hints::load(d.path(), 16).unwrap();
        h.note(id);
        assert_eq!(h.flush().unwrap(), 1);
        h.note(id);
        assert_eq!(
            h.flush().unwrap(),
            0,
            "the same second is not a second write"
        );
        let loaded = Hints::load(d.path(), 16).unwrap();
        assert_eq!(loaded.get(&id), now());
        assert_eq!(loaded.tracked(), 1);
    }

    #[test]
    fn a_torn_last_record_is_dropped() {
        let d = tempfile::tempdir().unwrap();
        let a = BlockId::of(b"a");
        let b = BlockId::of(b"b");
        let mut h = Hints::load(d.path(), 16).unwrap();
        h.note(a);
        h.note(b);
        h.flush().unwrap();
        let path = d.path().join("atime.bin");
        let mut bytes = fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 5);
        fs::write(&path, &bytes).unwrap();
        let loaded = Hints::load(d.path(), 16).unwrap();
        assert!(loaded.get(&a) > 0, "the intact record survives");
        assert_eq!(loaded.get(&b), 0, "the torn record is gone");
    }

    #[test]
    fn the_cap_drops_hints_and_counts_them() {
        let d = tempfile::tempdir().unwrap();
        let mut h = Hints::load(d.path(), 2).unwrap();
        for i in 0..5u8 {
            h.note(BlockId::of(&[i]));
        }
        assert_eq!(h.tracked(), 2);
        assert_eq!(h.dropped(), 3);
    }
}
