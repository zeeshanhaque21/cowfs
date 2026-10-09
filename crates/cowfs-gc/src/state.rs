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
/// Marks cache format version.
///
/// `COWMARK3` records which blocks belong to which walked root.
/// `COWMARK2` stored one flat block list under the whole root list, so every block was credited to
/// every root, and a removed root's blocks stayed live under a surviving root's key forever.
/// `COWMARK1` was worse: a collector before the walked-root fix could pair a listed root key with a
/// different, newly committed root's blocks.
/// Neither older layout carries the per-root association, so neither can be reconstructed into one
/// and both are rejected: every root is walked in full, which is always correct.
/// `COWMARK4` is `COWMARK3`'s layout plus a trailing BLAKE3 hash of every byte before it. Without
/// it a same-length corruption (bit rot, a zeroed span) parsed as a valid but different set, and a
/// later cycle skipped a root's walk and freed live blocks. `COWMARK3` has no hash, so it is
/// rejected like the older layouts: its roots are walked in full once.
/// The cache is derived data: discarding it costs a walk and nothing else.
const MAGIC_MARKS: &[u8] = b"COWMARK4";
/// Length of the trailing BLAKE3 hash.
const HASH_LEN: usize = 32;
const ROOT_ENTRY: usize = 32;
/// A root key and its block count, ahead of that root's own block list.
const GROUP_ENTRY: usize = ROOT_ENTRY + 8;

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
///
/// Each root keeps its own block list, and a root is only ever recorded from a walk that started
/// with an empty marker, so a recorded list is that root's complete reachable set. A flat union
/// would not do: it cannot say whose blocks a removed snapshot had, so those blocks stay live under
/// a survivor's key and no cycle can free them.
#[derive(Debug)]
pub struct Marks {
    path: PathBuf,
    /// Which blocks each walked root contributed. Per root, not one flat set: a flat set cannot
    /// drop a removed snapshot's blocks, because it cannot tell whose they were.
    roots: BTreeMap<[u8; 32], HashSet<BlockId>>,
    /// Recorded (root, block) pairs, which is what the cap bounds and what the file size is made of.
    /// Counted as it changes so `add_block` does not rebuild a union set per block.
    assoc: usize,
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
            assoc: 0,
            dropped: false,
            roots: BTreeMap::new(),
        };
        let Ok(bytes) = fs::read(&out.path) else {
            return out;
        };
        out.parse(&bytes);
        out
    }

    /// Fill in the associations a well-formed file carries, or leave the set empty.
    ///
    /// The file is walked as a strict sequence of `[root][count][blocks]` groups and must end exactly
    /// where the last group ends. Anything else - an old format, a torn tail, a count that runs past
    /// the end, a repeated root - leaves the set empty, so every root is walked in full. Nothing is
    /// committed until the whole file parses: a partial read trimmed into a smaller valid-looking set
    /// would leave a root credited with only some of its blocks, and a later cycle would skip that
    /// root's walk and free the rest.
    fn parse(&mut self, bytes: &[u8]) {
        // The hash covers the magic too, and is checked before any byte of the body is believed.
        let Some((covered, hash)) = bytes.split_at_checked(bytes.len().saturating_sub(HASH_LEN))
        else {
            return;
        };
        if hash.len() != HASH_LEN || blake3::hash(covered).as_bytes() != hash {
            return;
        }
        let Some(body) = covered.strip_prefix(MAGIC_MARKS) else {
            return;
        };
        let Some((n_roots, mut rest)) = split_u64(body) else {
            return;
        };
        let mut roots: BTreeMap<[u8; 32], HashSet<BlockId>> = BTreeMap::new();
        let mut assoc = 0usize;
        for _ in 0..n_roots {
            if rest.len() < GROUP_ENTRY {
                return;
            }
            let (key_bytes, tail) = rest.split_at(ROOT_ENTRY);
            let Some((n_blocks, tail)) = split_u64(tail) else {
                return;
            };
            let Some(wanted) = n_blocks.checked_mul(ROOT_ENTRY) else {
                return;
            };
            if tail.len() < wanted {
                return;
            }
            let (block_bytes, tail) = tail.split_at(wanted);
            rest = tail;
            // A recorded root with no blocks is never written, because it carries nothing a later
            // cycle could seed from, so one here is a malformed file and not an empty tree.
            if n_blocks == 0 || roots.len() >= self.cap || assoc + n_blocks > self.cap {
                return;
            }
            let mut set = HashSet::with_capacity(n_blocks);
            for b in block_bytes.as_chunks::<ROOT_ENTRY>().0 {
                set.insert(BlockId::from_bytes(*b));
            }
            let key: [u8; ROOT_ENTRY] = key_bytes.try_into().unwrap_or([0; ROOT_ENTRY]);
            if roots.insert(key, set).is_some() {
                return;
            }
            assoc += n_blocks;
        }
        // Trailing bytes mean the root count and the body disagree about what was written.
        if !rest.is_empty() {
            return;
        }
        self.roots = roots;
        self.assoc = assoc;
    }

    pub fn n_blocks(&self) -> usize {
        let mut seen = HashSet::new();
        for s in self.roots.values() {
            seen.extend(s.iter().copied());
        }
        seen.len()
    }

    /// True when an earlier cycle already walked this root and its recorded blocks survived.
    ///
    /// A root with no recorded blocks is never true. Trusting one would skip its walk and seed live
    /// from nothing, which frees exactly what it should have protected, and an empty set here can
    /// only come from a malformed file: a walk that legitimately reaches no blocks is not recorded.
    ///
    /// Never true once anything was dropped: a walk can record a root and then overflow the cap
    /// before recording its blocks, and a cycle that trusted that root would skip the walk and seed
    /// live from an empty set, which frees exactly what it should have protected.
    pub fn has_root(&self, root: &[u8; ROOT_ENTRY]) -> bool {
        !self.dropped && self.roots.get(root).is_some_and(|s| !s.is_empty())
    }

    /// The blocks an earlier cycle yielded for this root, so a skipped root's blocks stay live.
    pub fn blocks_of(&self, root: &[u8; ROOT_ENTRY]) -> Option<&HashSet<BlockId>> {
        self.roots.get(root)
    }

    /// Record a root an earlier cycle walked, with nothing to seed from yet.
    pub fn add_root(&mut self, root: &[u8; ROOT_ENTRY]) {
        if self.roots.len() >= self.cap {
            self.dropped = true;
            return;
        }
        self.roots.entry(*root).or_default();
    }

    /// Record a block a walk yielded for this root.
    pub fn add_block(&mut self, root: &[u8; ROOT_ENTRY], id: BlockId) {
        if self.assoc >= self.cap {
            self.dropped = true;
            return;
        }
        if self.roots.entry(*root).or_default().insert(id) {
            self.assoc += 1;
        }
    }

    /// Drop the roots `keep` rejects, and the blocks only they held.
    pub fn retain_roots(&mut self, keep: &dyn Fn(&[u8; 32]) -> bool) {
        let mut gone = 0usize;
        self.roots.retain(|r, s| {
            if keep(r) {
                true
            } else {
                gone += s.len();
                false
            }
        });
        self.assoc -= gone;
    }

    /// Replace the file with the current set, dropping `dead` first.
    ///
    /// Written whole through a temporary name, fsynced and renamed, so a crash leaves the previous
    /// set or the new one, never a half-written mix that would let a later cycle condemn a block
    /// whose root it is about to skip. A block two roots share is written under both: what is
    /// recorded has to be each root's own set, not the union, which is the whole point.
    pub fn save(&mut self, dead: &HashSet<BlockId>) -> io::Result<()> {
        if !dead.is_empty() {
            let mut gone = 0usize;
            for set in self.roots.values_mut() {
                let before = set.len();
                set.retain(|b| !dead.contains(b));
                gone += before - set.len();
            }
            self.assoc -= gone;
        }
        // A root that recorded nothing cannot seed a later cycle, so it is not written and the
        // next cycle walks it again. That also keeps every written root non-empty, which is what
        // makes an empty set in a file mean corruption rather than an empty tree.
        self.roots.retain(|_, s| !s.is_empty());
        if self.dropped {
            let _ = fs::remove_file(&self.path);
            self.roots.clear();
            self.assoc = 0;
            return Ok(());
        }
        let mut buf = Vec::with_capacity(
            MAGIC_MARKS.len() + 8 + self.roots.len() * GROUP_ENTRY + self.assoc * 32 + HASH_LEN,
        );
        buf.extend_from_slice(MAGIC_MARKS);
        buf.extend_from_slice(&(self.roots.len() as u64).to_le_bytes());
        for (r, set) in &self.roots {
            buf.extend_from_slice(r);
            buf.extend_from_slice(&(set.len() as u64).to_le_bytes());
            let mut blocks: Vec<&BlockId> = set.iter().collect();
            blocks.sort_unstable();
            for b in blocks {
                buf.extend_from_slice(b.as_bytes());
            }
        }
        let hash = blake3::hash(&buf);
        buf.extend_from_slice(hash.as_bytes());
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
        m.add_block(&root(1), BlockId::of(b"a"));
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
        m.add_block(&bytes, BlockId::of(b"x"));
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

    /// Build a marks file by hand, so a test can state a byte shape no writer produces.
    fn file(groups: &[(&[u8; ROOT_ENTRY], &[&BlockId])], magic: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(magic);
        buf.extend_from_slice(&(groups.len() as u64).to_le_bytes());
        for (root, blocks) in groups {
            buf.extend_from_slice(*root);
            buf.extend_from_slice(&(blocks.len() as u64).to_le_bytes());
            for b in *blocks {
                buf.extend_from_slice(b.as_bytes());
            }
        }
        if magic == MAGIC_MARKS {
            seal(&mut buf);
        }
        buf
    }

    /// Append the trailing hash, so a test can build a structurally wrong body that is still sealed
    /// and reaches the parser, instead of being stopped at the hash.
    fn seal(buf: &mut Vec<u8>) {
        let h = blake3::hash(buf);
        buf.extend_from_slice(h.as_bytes());
    }

    /// B2: an old-format marks file (a collector before the walked-root fix) may pair a listed root
    /// with a different root's blocks. It carries no version apart from a magic that says which
    /// collector wrote it, so the loader must reject the whole old format and walk in full rather
    /// than trust the wrong association.
    #[test]
    fn an_old_format_marks_file_is_not_reused() {
        let d = tempfile::tempdir().unwrap();
        let k1 = root(1);
        let b = BlockId::of(b"b");
        // `COWMARK1` is the shape the old collector wrote after the walked-root race: one root key,
        // then one flat block list of the union. `COWMARK2` is the same shape, and it is the one this
        // issue is about: every block credited to every root, so a removed root's blocks stayed live.
        for magic in [&b"COWMARK1"[..], &b"COWMARK2"[..]] {
            let mut buf = Vec::new();
            buf.extend_from_slice(magic);
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
        }

        // The current format carries the per-root group and does round-trip, so the rejection is the
        // format version and not the payload.
        fs::write(
            d.path().join("mark.bin"),
            file(&[(&k1, &[&b])], MAGIC_MARKS),
        )
        .unwrap();
        let m = Marks::load(d.path(), 16);
        assert!(m.has_root(&k1), "the current format is reused");
        assert!(m.blocks_of(&k1).unwrap().contains(&b));
    }

    /// Issue 82: two roots, and each keeps its own blocks. A flat union credited to both would make
    /// dropping one root keep the other's blocks alive.
    #[test]
    fn each_root_keeps_only_its_own_blocks_across_a_reload() {
        let d = tempfile::tempdir().unwrap();
        let (k1, k2) = (root(1), root(2));
        let (a, b, shared) = (BlockId::of(b"a"), BlockId::of(b"b"), BlockId::of(b"shared"));
        let mut m = Marks::load(d.path(), 16);
        m.add_root(&k1);
        m.add_block(&k1, a);
        m.add_block(&k1, shared);
        m.add_root(&k2);
        m.add_block(&k2, b);
        m.add_block(&k2, shared);
        m.save(&HashSet::new()).unwrap();

        let back = Marks::load(d.path(), 16);
        let one = back.blocks_of(&k1).unwrap();
        let two = back.blocks_of(&k2).unwrap();
        assert!(
            one.contains(&a) && !one.contains(&b),
            "root 1 kept its own block"
        );
        assert!(
            two.contains(&b) && !two.contains(&a),
            "root 2 kept its own block"
        );
        assert!(
            one.contains(&shared) && two.contains(&shared),
            "a block two roots share is credited to both"
        );

        // Dropping one root takes only that root's exclusive blocks with it.
        let mut m = back;
        m.retain_roots(&|r| *r == k2);
        assert!(m.blocks_of(&k1).is_none());
        let left = m.blocks_of(&k2).unwrap();
        assert!(
            left.contains(&shared),
            "the shared block stays for the root that kept it"
        );
        assert_eq!(left.len(), 2);
    }

    /// A malformed file is a full walk, never a smaller set that looks valid. Trimming a partial
    /// read would leave a root credited with only some of its blocks, and trusting that root would
    /// skip its walk and free the rest.
    #[test]
    fn a_malformed_file_is_never_a_smaller_valid_set() {
        let d = tempfile::tempdir().unwrap();
        let (k1, k2) = (root(1), root(2));
        let (a, b) = (BlockId::of(b"a"), BlockId::of(b"b"));
        let good = file(&[(&k1, &[&a]), (&k2, &[&b])], MAGIC_MARKS);
        let raw = good[..good.len() - HASH_LEN].to_vec();

        // A count that runs past the end of the file, which is what a torn write looks like.
        let mut over = raw.clone();
        let n_at = MAGIC_MARKS.len();
        over[n_at..n_at + 8].copy_from_slice(&9u64.to_le_bytes());
        // Trailing bytes the root count does not account for.
        let mut extra = raw.clone();
        extra.extend_from_slice(b"tail");
        // The same root twice, where the second group would silently overwrite the first.
        let mut twice = raw.clone();
        let mut second = Vec::new();
        second.extend_from_slice(&k1);
        second.extend_from_slice(&1u64.to_le_bytes());
        second.extend_from_slice(b.as_bytes());
        twice.extend_from_slice(&second);
        // A root with no blocks, which no writer emits and which would seed a skipped walk with
        // nothing.
        let mut empty_group = raw.clone();
        empty_group.extend_from_slice(&k1);
        empty_group.extend_from_slice(&0u64.to_le_bytes());

        for (what, mut bytes) in [
            ("a count past the end", over),
            ("trailing bytes", extra),
            ("a repeated root", twice),
            ("a root with no blocks", empty_group),
        ] {
            seal(&mut bytes);
            fs::write(d.path().join("mark.bin"), &bytes).unwrap();
            let m = Marks::load(d.path(), 16);
            assert_eq!(m.n_blocks(), 0, "{what} must not yield a partial set");
            assert!(!m.has_root(&k1), "{what} must not leave a trusted root");
            assert!(!m.dropped, "{what} is an empty set, not a dropped one");
        }

        // The intact file is the control: it is the same bytes with nothing wrong.
        fs::write(d.path().join("mark.bin"), &good).unwrap();
        let m = Marks::load(d.path(), 16);
        assert_eq!(m.n_blocks(), 2);
        assert!(m.has_root(&k1) && m.has_root(&k2));
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
        let m = Marks::load(d.path(), 16);
        assert_eq!(m.n_blocks(), 0);
        assert!(!m.has_root(&root(3)), "a torn tail leaves no trusted root");
    }

    /// Issue 288: a same-length corruption must be rebuilt, never trusted. Each case starts from a
    /// good file and damages it without changing the length where it can.
    #[test]
    fn a_damaged_file_is_never_trusted() {
        let d = tempfile::tempdir().unwrap();
        let mut m = Marks::load(d.path(), 64);
        let k = root(5);
        for i in 0..8u8 {
            m.add_root(&k);
            m.add_block(&k, BlockId::of(&[i]));
        }
        m.save(&HashSet::new()).unwrap();
        let path = d.path().join("mark.bin");
        let good = fs::read(&path).unwrap();
        assert!(
            Marks::load(d.path(), 64).has_root(&k),
            "control: intact file is reused"
        );

        let mut flip = good.clone();
        flip[good.len() / 2] ^= 1;
        let mut zeroed = good.clone();
        let at = MAGIC_MARKS.len() + 8 + GROUP_ENTRY;
        zeroed[at..at + 64].fill(0);
        let mut bad_hash = good.clone();
        *bad_hash.last_mut().unwrap() ^= 0xff;
        let mut trailing = good.clone();
        trailing.extend_from_slice(b"garbage");
        let mut prefix_junk = b"x".to_vec();
        prefix_junk.extend_from_slice(&good);
        // A COWMARK3 file (no hash) with a valid layout: the previous format, never trusted.
        let mut v3 = good[..good.len() - HASH_LEN].to_vec();
        v3[..MAGIC_MARKS.len()].copy_from_slice(b"COWMARK3");

        for (what, bytes) in [
            ("a bit flip", flip),
            ("a zeroed 64-byte span", zeroed),
            ("a corrupt hash", bad_hash),
            ("trailing garbage", trailing),
            ("a leading byte", prefix_junk),
            ("a truncated tail", good[..good.len() - 1].to_vec()),
            ("an empty file", Vec::new()),
            ("the old unhashed format", v3),
        ] {
            fs::write(&path, &bytes).unwrap();
            let m = Marks::load(d.path(), 64);
            assert!(!m.has_root(&k), "{what} must not leave a trusted root");
            assert_eq!(m.n_blocks(), 0, "{what} must not yield a partial set");
            assert!(!m.dropped);
        }
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
