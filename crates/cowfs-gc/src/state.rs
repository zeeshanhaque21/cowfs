//! Batched last-access times, kept in memory and flushed in batches.
//!
//! A read must not become a write, so an access only touches memory here.
//! The batch file is append-only and a torn last record is dropped on load, so a crash in the
//! middle of a flush costs the hints in that batch and nothing else.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use cowfs_store::BlockId;

const MAGIC: &[u8] = b"COWAT01";
const ENTRY: usize = 36;
/// Marks cache format version. Bumped from `COWMARK1` because a collector before the walked-root
/// fix could write a listed root key paired with a different, newly committed root's blocks.
/// Such a file is a wrong association that would make a later cycle skip a root's walk and free
/// the blocks only that root referenced, so an old file must be ignored and walked in full.
/// The cache is derived data: discarding it costs a walk and nothing else.
const MAGIC_MARKS: &[u8] = b"COWMARK2";
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
        self.note_at(id, now());
    }

    /// `note` with the second supplied, so the rule can be tested without racing the wall clock.
    fn note_at(&mut self, id: BlockId, now: u32) {
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
        let mut buf = Vec::with_capacity(self.dirty.len() * ENTRY);
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
        let mut at = file.metadata()?.len();
        // The header is a property of the file, not of an append. Writing it on every flush left a
        // copy in front of every later record, and `load`, which strips it once, then read each
        // header as the first bytes of a block id and invented a phantom hint per flush.
        if at == 0 {
            file.write_all_at(MAGIC, 0)?;
            at = MAGIC.len() as u64;
        }
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
    /// Which blocks each walked root contributed. Per root, not one flat set: a flat set cannot
    /// drop a removed snapshot's blocks, because it cannot tell whose they were.
    roots: BTreeMap<[u8; 32], HashSet<BlockId>>,
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
            roots: BTreeMap::new(),
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
            if out.roots.len() >= cap {
                out.dropped = true;
                break;
            }
            out.roots.insert(*c, HashSet::new());
        }
        for c in blocks.as_chunks::<ROOT_ENTRY>().0 {
            if out.roots.is_empty() || out.n_blocks() >= cap {
                out.dropped = true;
                break;
            }
            let id = BlockId::from_bytes(*c);
            // The file does not say which root a block came from, so it is credited to every root
            // that was walked. Over-crediting only costs a walk later; under-crediting would free a
            // live block, so this errs the safe way.
            for set in out.roots.values_mut() {
                set.insert(id);
            }
        }
        out
    }

    pub fn n_blocks(&self) -> usize {
        let mut seen = HashSet::new();
        for s in self.roots.values() {
            seen.extend(s.iter().copied());
        }
        seen.len()
    }

    /// True when an earlier cycle already walked this root *and* its recorded blocks survived.
    ///
    /// Never true once anything was dropped: a walk can record a root and then overflow the cap
    /// before recording its blocks, and a cycle that trusted that root would skip the walk and seed
    /// live from an empty set, which frees exactly what it should have protected.
    pub fn has_root(&self, root: &[u8; ROOT_ENTRY]) -> bool {
        !self.dropped && self.roots.contains_key(root)
    }

    /// The blocks an earlier cycle yielded for this root, so a skipped root's blocks stay live.
    pub fn blocks_of(&self, root: &[u8; ROOT_ENTRY]) -> Option<&HashSet<BlockId>> {
        self.roots.get(root)
    }

    /// Record a root an earlier cycle walked.
    pub fn add_root(&mut self, root: &[u8; ROOT_ENTRY]) {
        if self.roots.len() >= self.cap {
            self.dropped = true;
            return;
        }
        self.roots.entry(*root).or_default();
    }

    /// Record a block a walk yielded for this root.
    pub fn add_block(&mut self, root: &[u8; ROOT_ENTRY], id: BlockId) {
        if self.n_blocks() >= self.cap {
            self.dropped = true;
            return;
        }
        self.roots.entry(*root).or_default().insert(id);
    }

    /// Drop the roots `keep` rejects, and the blocks only they held.
    pub fn retain_roots(&mut self, keep: &dyn Fn(&[u8; 32]) -> bool) {
        self.roots.retain(|r, _| keep(r));
    }

    /// Replace the file with the current set, dropping `dead` first.
    ///
    /// Written whole through a temporary name, fsynced and renamed, so a crash leaves the previous
    /// set or the new one, never a half-written mix that would let a later cycle condemn a block
    /// whose root it is about to skip.
    pub fn save(&mut self, dead: &HashSet<BlockId>) -> io::Result<()> {
        if !dead.is_empty() {
            for set in self.roots.values_mut() {
                set.retain(|b| !dead.contains(b));
            }
            self.roots.retain(|_, s| !s.is_empty());
        }
        if self.dropped {
            let _ = fs::remove_file(&self.path);
            self.roots.clear();
            return Ok(());
        }
        let mut all: HashSet<BlockId> = HashSet::new();
        for set in self.roots.values() {
            all.extend(set.iter().copied());
        }
        let mut buf = Vec::with_capacity(
            MAGIC_MARKS.len() + 16 + (self.roots.len() + all.len()) * ROOT_ENTRY,
        );
        buf.extend_from_slice(MAGIC_MARKS);
        buf.extend_from_slice(&(self.roots.len() as u64).to_le_bytes());
        buf.extend_from_slice(&(all.len() as u64).to_le_bytes());
        for r in self.roots.keys() {
            buf.extend_from_slice(r);
        }
        let mut blocks: Vec<&BlockId> = all.iter().collect();
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
        m.add_block(&root(7), b);
        m.save(&HashSet::new()).unwrap();
        let back = Marks::load(d.path(), 16);
        assert!(back.has_root(&root(7)));
        assert!(back.blocks_of(&root(7)).unwrap().contains(&b));
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
        m.add_root(&root(3));
        m.add_block(&root(3), b);
        m.save(&HashSet::from([b])).unwrap();
        assert!(Marks::load(d.path(), 16).n_blocks() == 0);
    }

    #[test]
    fn a_corrupt_file_is_an_empty_set_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("mark.bin"), b"not a mark file at all").unwrap();
        let m = Marks::load(d.path(), 16);
        assert!(m.n_blocks() == 0);
        assert!(!m.dropped);
    }

    /// B2: an old-format marks file (a collector before the walked-root fix) may pair a listed root
    /// with a different root's blocks. It carries no version apart from a magic that says which
    /// collector wrote it, so the loader must reject the whole old format and walk in full rather
    /// than trust the wrong association.
    #[test]
    fn an_old_format_marks_file_is_not_reused() {
        let d = tempfile::tempdir().unwrap();
        // Build a well-formed COWMARK1 file by hand: one root K1, one block B, exactly the shape the
        // old collector wrote after the walked-root race (the listed key with the new root's blocks).
        let k1 = root(1);
        let b = BlockId::of(b"b");
        let mut buf = Vec::new();
        buf.extend_from_slice(b"COWMARK1");
        buf.extend_from_slice(&1u64.to_le_bytes());
        buf.extend_from_slice(&1u64.to_le_bytes());
        buf.extend_from_slice(&k1);
        buf.extend_from_slice(b.as_bytes());
        fs::write(d.path().join("mark.bin"), &buf).unwrap();

        let m = Marks::load(d.path(), 16);
        assert!(
            !m.has_root(&k1),
            "an old-format cache must not be trusted: the root is walked again"
        );
        assert_eq!(m.n_blocks(), 0, "no old block association is honoured");
        assert!(!m.dropped);

        // The same shape written under the current magic does round-trip, so the rejection is the
        // format version and not the payload.
        let mut cur = Vec::new();
        cur.extend_from_slice(MAGIC_MARKS);
        cur.extend_from_slice(&1u64.to_le_bytes());
        cur.extend_from_slice(&1u64.to_le_bytes());
        cur.extend_from_slice(&k1);
        cur.extend_from_slice(b.as_bytes());
        fs::write(d.path().join("mark.bin"), &cur).unwrap();
        let m = Marks::load(d.path(), 16);
        assert!(m.has_root(&k1), "the current format is reused");
        assert!(m.blocks_of(&k1).unwrap().contains(&b));
    }

    #[test]
    fn a_truncated_file_is_an_empty_set() {
        let d = tempfile::tempdir().unwrap();
        let mut m = Marks::load(d.path(), 16);
        m.add_root(&root(3));
        m.add_block(&root(3), BlockId::of(b"x"));
        m.save(&HashSet::new()).unwrap();
        let path = d.path().join("mark.bin");
        let mut b = fs::read(&path).unwrap();
        b.truncate(b.len() - 3);
        fs::write(&path, &b).unwrap();
        assert!(Marks::load(d.path(), 16).n_blocks() == 0);
    }

    #[test]
    fn a_full_set_is_dropped_rather_than_written_huge() {
        let d = tempfile::tempdir().unwrap();
        let mut m = Marks::load(d.path(), 2);
        m.add_root(&root(1));
        m.add_block(&root(1), BlockId::of(b"a"));
        m.add_block(&root(1), BlockId::of(b"b"));
        m.add_block(&root(1), BlockId::of(b"c"));
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
        // A repeat is a second record only when the stored second has advanced. `note` stamps the
        // wall clock, so calling it again and racing the second boundary makes the flush count
        // vary between runs. Drive the stored time directly instead, which is what the rule reads.
        let stamped = h.get(&id);
        // A repeat is a record only when the stored second has advanced. `note` stamps the wall
        // clock, so calling it again races the second boundary and the count varies between runs.
        // Stamp the stored time forward instead, which is the whole of what `note` reads.
        h.times.insert(id, stamped);
        h.dirty.clear();
        h.note_at(id, stamped);
        assert_eq!(
            h.flush().unwrap(),
            0,
            "a repeat without a newer second is not a write"
        );
        h.note_at(id, stamped + 1);
        assert_eq!(h.flush().unwrap(), 1, "a newer second is a write");
        let loaded = Hints::load(d.path(), 16).unwrap();
        assert!(loaded.get(&id) >= stamped, "the newest record survives");
        assert_eq!(loaded.tracked(), 1, "one block, however many records");
    }

    #[test]
    fn a_header_is_written_once_not_once_per_flush() {
        let d = tempfile::tempdir().unwrap();
        let mut h = Hints::load(d.path(), 16).unwrap();
        let mut expected = 0u64;
        let mut flushes = 0u32;
        for i in 0..5u8 {
            // Two blocks per round, and a flush after each note, so the file sees ten appends.
            h.note(BlockId::of(&[i]));
            expected += h.flush().unwrap() as u64;
            flushes += 1;
            h.note(BlockId::of(&[i, 1]));
            expected += h.flush().unwrap() as u64;
            flushes += 1;
        }
        assert_eq!(flushes, 10, "ten flushes");
        assert_eq!(
            fs::metadata(d.path().join("atime.bin")).unwrap().len(),
            MAGIC.len() as u64 + expected * ENTRY as u64,
            "one header, not one per flush"
        );
        let loaded = Hints::load(d.path(), 16).unwrap();
        assert_eq!(loaded.tracked(), 10, "no phantom hints from the headers");
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
