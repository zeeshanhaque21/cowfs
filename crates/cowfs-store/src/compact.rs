//! Pack compaction: copy a pack's live records into a new pack, then drop the old one.
//!
//! Design: `docs/v1-gc.md`.
//!
//! The store has `put` and no `delete`, so a block leaves the store only when the pack that holds
//! it is rewritten without it. That is what this file does, in steps a caller can drive and a
//! crash can interrupt at any of:
//!
//! 1. [`Store::plan_pack`] counts a pack's live and dead bytes without writing.
//! 2. [`Store::begin_compaction`] creates a fresh pack and works out what to copy and what to
//!    condemn.
//! 3. [`Store::copy_batch`] copies a bounded number of records, byte for byte, so nothing is
//!    decoded, rehashed or recompressed.
//! 4. [`Store::finish_compaction`] fsyncs the new pack, repoints the index and rewrites
//!    `index.cix`.
//! 5. [`Store::discard_pack`] unlinks the old pack, drops the condemned index entries and fixes
//!    the watermark base, but only once the caller has confirmed that no condemned id became live.
//!
//! A crash between any two steps leaves a consistent store: the copies are real records that `open`
//! finds and indexes, the old pack stays until it is deliberately unlinked, and an unlinked pack
//! is recorded in `ACKED` before it disappears so a later `open` never calls its absence
//! corruption.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ack;
use crate::error::{Error, Result};
use crate::fsio;
use crate::index::Loc;
use crate::pack::{self, PACK_HEADER_LEN};
use crate::record::{Header, HEADER_LEN};
use crate::BlockId;
use crate::Store;

/// Bytes a batch copies at most when the caller does not choose.
const DEFAULT_BATCH: u64 = 4 << 20;

/// One pack, its current length, and whether appends go to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackInfo {
    /// Pack id.
    pub id: u32,
    /// File length in bytes.
    pub len: u64,
    /// True when `put` appends to this pack.
    pub active: bool,
}

/// What one scan of a pack found, counted against the caller's live set. Reading it writes nothing.
/// Markers the durability tests place in the op log, so an ordering can be asserted across a
/// process exit, which no crash test can observe.
const MARK_UNLINK: u64 = 9_001;
const MARK_AFTER_DIRSYNC: u64 = 9_002;
const MARK_BEFORE_SYNC: u64 = 9_101;
const MARK_AFTER_SYNC: u64 = 9_102;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PackPlan {
    /// Pack id.
    pub id: u32,
    /// File length when the pack was scanned.
    pub len: u64,
    /// Valid records found, duplicates included.
    pub records: u64,
    /// Bytes of records whose block the caller's live set accepts.
    pub live_bytes: u64,
    /// Bytes of records whose block the live set does not accept.
    pub dead_bytes: u64,
    /// Bytes that belong to no valid record. Never copied, so rewriting drops them.
    pub gap_bytes: u64,
    /// True when the pack holds damage to bytes an earlier `sync` made durable, or when the
    /// scanner gave up before the end. A corrupt pack is never a rewrite candidate.
    pub corrupt: bool,
    /// Bytes kept in `<pack>.torn-*` sidecars next to it. Reported, never removed.
    pub quarantined_bytes: u64,
}

impl PackPlan {
    /// Bytes in valid records, live plus dead.
    pub fn record_bytes(&self) -> u64 {
        self.live_bytes.saturating_add(self.dead_bytes)
    }

    /// Fraction of record bytes that are dead, 0 when the pack holds no records.
    pub fn dead_ratio(&self) -> f64 {
        let all = self.record_bytes();
        if all == 0 {
            0.0
        } else {
            self.dead_bytes as f64 / all as f64
        }
    }
}

/// What a finished copy produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rewrite {
    /// Pack that was read.
    pub from: u32,
    /// Pack that was written.
    pub to: u32,
    /// Records copied.
    pub records: u64,
    /// Bytes written to the new pack, excluding its 16 byte header.
    pub bytes: u64,
    /// File length of the new pack, header included, or 0 when nothing was written.
    pub file_bytes: u64,
    /// Block ids the new pack does not hold, for [`Store::discard_pack`].
    pub condemned: Vec<BlockId>,
}

/// A copy in progress: created by [`Store::begin_compaction`], driven by
/// [`Store::copy_batch`], closed by [`Store::finish_compaction`].
#[derive(Debug)]
pub struct Compaction {
    from: u32,
    to: u32,
    source: Arc<File>,
    /// The pack being written. `None` until the first record is copied, and forever when the
    /// source holds nothing live.
    target: Option<Arc<File>>,
    len: u64,
    ids: Vec<BlockId>,
    locs: Vec<Loc>,
    condemned: Vec<BlockId>,
    at: usize,
    moved: Vec<(BlockId, Loc)>,
    source_len: u64,
}

impl Compaction {
    /// Pack being read.
    pub fn from(&self) -> u32 {
        self.from
    }

    /// Pack being written, equal to [`Compaction::from`] while no record has been copied.
    pub fn to(&self) -> u32 {
        self.to
    }

    /// True once every live record has been copied.
    pub fn is_complete(&self) -> bool {
        self.at >= self.ids.len()
    }

    /// Bytes the source pack holds, as it was when it was planned.
    pub fn source_len(&self) -> u64 {
        self.source_len
    }

    /// Bytes the new pack holds, excluding its header.
    pub fn written(&self) -> u64 {
        self.len - PACK_HEADER_LEN
    }

    /// True once the copy has created its target pack on disk.
    pub fn target_created(&self) -> bool {
        self.target.is_some()
    }

    /// Bytes the remaining records will add to the new pack.
    pub fn outstanding_bytes(&self) -> u64 {
        self.locs[self.at..]
            .iter()
            .map(|l| HEADER_LEN as u64 + u64::from(l.slen))
            .sum()
    }

    /// Block ids the new pack will not hold.
    pub fn condemned(&self) -> &[BlockId] {
        &self.condemned
    }
}

/// Bytes kept in the `<pack>.torn-*` sidecars of one pack.
fn torn_bytes(dir: &Path, id: u32) -> u64 {
    let prefix = format!("pack-{id:08}.cpk.torn-");
    fs::read_dir(pack::pack_dir(dir)).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(&prefix))
            })
            .filter_map(|e| e.metadata().ok().map(|m| m.len()))
            .sum()
    })
}

/// Read the record `loc` names and check its structure and checksum. Never returns bad bytes.
fn read_record(file: &File, id: BlockId, loc: Loc) -> Result<Vec<u8>> {
    let corrupt = |reason| Error::Corrupt {
        pack: loc.pack,
        offset: u64::from(loc.offset),
        reason,
    };
    if loc.ulen as usize > crate::MAX_BLOCK_LEN || loc.slen > loc.ulen {
        return Err(corrupt("index entry out of range"));
    }
    let mut buf = vec![0u8; HEADER_LEN + loc.slen as usize];
    file.read_exact_at(&mut buf, u64::from(loc.offset))?;
    let (head, payload) = buf.split_at(HEADER_LEN);
    let raw: &[u8; HEADER_LEN] = head.try_into().map_err(|_| corrupt("short header"))?;
    let header = Header::parse(raw).map_err(corrupt)?;
    if header.id != id || header.slen != loc.slen || header.ulen != loc.ulen {
        return Err(corrupt("record does not match index"));
    }
    if Header::expected_crc(raw, payload) != header.crc {
        return Err(corrupt("checksum mismatch"));
    }
    Ok(buf)
}

impl Store {
    /// Every pack on disk, in id order, with its length and whether `put` appends to it.
    pub fn packs(&self) -> Result<Vec<PackInfo>> {
        let g = self.guts();
        let active = self.active_pack();
        let mut out = Vec::new();
        for entry in fs::read_dir(pack::pack_dir(g.dir))? {
            let entry = entry?;
            let Some(id) = entry.file_name().to_str().and_then(pack::parse_pack_name) else {
                continue;
            };
            out.push(PackInfo {
                id,
                len: entry.metadata()?.len(),
                active: Some(id) == Some(active),
            });
        }
        out.sort_by_key(|p| p.id);
        Ok(out)
    }

    /// Size of one pack on disk, 0 when it is not there.
    pub fn pack_len(&self, id: u32) -> u64 {
        let g = self.guts();
        fs::metadata(pack::pack_path(g.dir, id)).map_or(0, |m| m.len())
    }

    /// Scan one pack and count its live and dead record bytes. Writes nothing.
    ///
    /// `live` decides per record on the id the record's own header claims, so the count does not
    /// depend on the index.
    /// `live_ids` is appended with the id of every live record, for a caller that orders its work
    /// by something the plan does not carry, such as last-access times. Ids may repeat when a
    /// pack holds duplicate records of one block.
    pub fn plan_pack(
        &self,
        id: u32,
        live: &dyn Fn(BlockId) -> bool,
        live_ids: &mut Vec<BlockId>,
    ) -> Result<PackPlan> {
        let g = self.guts();
        let path = pack::pack_path(g.dir, id);
        let file = File::open(&path)?;
        let len = file.metadata()?.len();
        let mut plan = PackPlan {
            id,
            len,
            quarantined_bytes: torn_bytes(g.dir, id),
            ..PackPlan::default()
        };
        pack::scan(&file, PACK_HEADER_LEN, len, |event| {
            match event {
                pack::Event::Record { header, .. } => {
                    plan.records += 1;
                    let n = header.total_len();
                    if live(header.id) {
                        plan.live_bytes += n;
                        live_ids.push(header.id);
                    } else {
                        plan.dead_bytes += n;
                    }
                }
                pack::Event::Gap { len, exhausted, .. } => {
                    plan.gap_bytes += len;
                    plan.corrupt |= exhausted;
                }
            }
            Ok(())
        })?;
        plan.corrupt |= self.recovery().corrupt_synced.iter().any(|c| c.pack == id);
        plan.corrupt |= self.recovery().missing_synced.contains(&id);
        Ok(plan)
    }

    /// Create a pack for the copy to go into, and decide what to copy and what to condemn.
    ///
    /// The target is a real pack with a real header, so a crash at any later point leaves records
    /// `open` finds and indexes. Every live record indexed in the source is copied; every dead one
    /// is condemned. A record whose id is indexed in another pack is left alone, so a duplicate can
    /// never steal an index entry from a good copy.
    pub fn begin_compaction(
        &self,
        plan: &PackPlan,
        live: &dyn Fn(BlockId) -> bool,
    ) -> Result<Compaction> {
        let g = self.guts();
        if plan.corrupt {
            return Err(Error::BadPack {
                path: pack::pack_path(g.dir, plan.id),
                reason: "pack holds damage to durable bytes",
            });
        }
        let source = Arc::new(File::open(pack::pack_path(g.dir, plan.id))?);
        let mut ids = Vec::new();
        let mut locs = Vec::new();
        let mut condemned = Vec::new();
        for (id, loc) in self.guts().index.snapshot(|loc| loc.pack == plan.id) {
            if live(id) {
                ids.push(id);
                locs.push(loc);
            } else {
                condemned.push(id);
            }
        }
        condemned.sort_unstable();
        Ok(Compaction {
            from: plan.id,
            source,
            // A pack with nothing live in it needs no copy, so no new pack is made for it.
            target: None,
            to: plan.id,
            len: PACK_HEADER_LEN,
            ids,
            locs,
            condemned,
            at: 0,
            moved: Vec::new(),
            source_len: plan.len,
        })
    }

    /// Copy up to `budget` bytes of live records into the new pack. True when every record is
    /// copied.
    ///
    /// `budget` of 0 copies up to 4 MiB. A record is never split, so one record past the budget
    /// still finishes the batch.
    pub fn copy_batch(&self, c: &mut Compaction, budget: u64) -> Result<bool> {
        let budget = if budget == 0 { DEFAULT_BATCH } else { budget };
        let mut used = 0;
        // Built once per call, not per record: only the crash-model log reads the name.
        let mut target_path: Option<PathBuf> = None;
        while c.at < c.ids.len() {
            let id = c.ids[c.at];
            let loc = c.locs[c.at];
            let need = HEADER_LEN as u64 + u64::from(loc.slen);
            if used > 0 && used + need > budget {
                break;
            }
            let raw = read_record(&c.source, id, loc)?;
            if c.target.is_none() {
                let (to, file) = self.new_pack()?;
                c.to = to;
                c.target = Some(file);
            }
            let target = Arc::clone(c.target.as_ref().unwrap_or_else(|| unreachable!()));
            let at = u32::try_from(c.len).map_err(|_| Error::Corrupt {
                pack: c.to,
                offset: c.len,
                reason: "new pack past the 4 GiB limit",
            })?;
            let g = self.guts();
            let path = target_path.get_or_insert_with(|| pack::pack_path(g.dir, c.to));
            g.io.write_at(&target, path, c.len, &raw)?;
            c.len += need;
            c.moved.push((
                id,
                Loc {
                    pack: c.to,
                    offset: at,
                    slen: loc.slen,
                    ulen: loc.ulen,
                    verified: true,
                },
            ));
            c.at += 1;
            used += need;
        }
        Ok(c.is_complete())
    }

    /// Make the copy durable, repoint the index at it and rewrite `index.cix`.
    ///
    /// The source pack is untouched, so a crash after this returns loses nothing: the index names
    /// bytes that are already fsynced, and the source is still there to fall back on.
    pub fn finish_compaction(&self, c: &Compaction) -> Result<Rewrite> {
        let g = self.guts();
        // Nothing live in the source: there is no copy to make durable, only the free to come.
        if let Some(target) = &c.target {
            let path = pack::pack_path(g.dir, c.to);
            fsio::mark(MARK_BEFORE_SYNC);
            g.io.sync_file(target, &path)?;
            g.io.sync_dir(&pack::pack_dir(g.dir))?;
            fsio::mark(MARK_AFTER_SYNC);
            for (id, loc) in &c.moved {
                g.index.replace(*id, *loc);
            }
            self.finish_pack(c.to, c.len)?;
            self.checkpoint()?;
        }
        Ok(Rewrite {
            from: c.from,
            to: c.to,
            records: c.moved.len() as u64,
            bytes: if c.target.is_some() {
                c.len - PACK_HEADER_LEN
            } else {
                0
            },
            file_bytes: if c.target.is_some() { c.len } else { 0 },
            condemned: c.condemned.clone(),
        })
    }

    /// On-disk file length of a pack by id, or 0 when it is not there. Used to account a partial
    /// copy whose target pack was created and never finished.
    pub fn pack_file_len(&self, id: u32) -> u64 {
        fs::metadata(pack::pack_path(self.guts().dir, id)).map_or(0, |m| m.len())
    }

    /// Unlink a pack whose live records were copied, and drop the index entries of the ids it no
    /// longer holds. Returns the bytes freed.
    ///
    /// Only call this once no condemned id is live any more. The pack is recorded in `ACKED`
    /// before it is unlinked, so a crash in the middle is never mistaken for lost data, and the
    /// watermark base is lowered so the gap is not reported as a missing pack.
    pub fn discard_pack(&self, id: u32, condemned: &[BlockId]) -> Result<u64> {
        let d = self.discard(id, condemned)?;
        match d.durability_error {
            Some(e) => Err(e),
            None => Ok(d.removed_bytes),
        }
    }

    /// Discard a pack, reporting what it really did even when it failed after the unlink.
    ///
    /// [`Self::discard_pack`] cannot express that: it returns only `Err` once the pack is gone,
    /// which loses the file length of a pack the store has already unlinked. A caller that only
    /// credits bytes on `Ok` then reports no removal for a removal that happened, and a pack the
    /// unlink removed stays named in the writer's in-memory map.
    ///
    /// So the effect and the failure are separate fields: `Err` means the unlink did not happen and
    /// nothing changed on disk, while `Ok` means it did, with `removed_bytes` its file length.
    /// `durability_error` then carries a step that failed *after* the pack was gone, which the
    /// caller must still surface: the bytes are freed, but the unlink is not confirmed durable.
    pub fn discard(&self, id: u32, condemned: &[BlockId]) -> Result<Discarded> {
        self.sync()?;
        let g = self.guts();
        for b in condemned {
            if g.index.get(b).is_some_and(|l| l.pack == id) {
                g.index.remove(b);
            }
        }
        let path = pack::pack_path(g.dir, id);
        // Two collectors on one store is not a supported configuration, but it must not corrupt
        // anything: if the pack is already gone the second caller has nothing to do. It still has to
        // drop the id from the writer's map, or `fsck` walks a map naming a file that is not there
        // and fails forever.
        let Ok(meta) = fs::metadata(&path) else {
            self.forget_pack(id);
            return Ok(Discarded::default());
        };
        let len = meta.len();
        // The watermark floor goes above this pack before the file is unlinked, and durably. A crash
        // after that leaves a file the next open does not scan, which converges on a later cycle; a
        // crash with the gap still in the scan range would be reported as a lost pack, which is a
        // false loss report about bytes that were copied first.
        {
            let mut wm =
                g.wm.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(m) = wm.mark() {
                let lowest = self.packs()?.first().map_or(id, |p| p.id);
                let base = wm.base().max(lowest).max(id.saturating_add(1));
                let next = wm.next_id();
                wm.reset(m, base, next)?;
            }
        }
        fsio::mark(MARK_UNLINK);
        match g.io.remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.forget_pack(id);
                return Ok(Discarded::default());
            }
            Err(e) => return Err(e.into()),
        }
        // The pack is gone from disk from here on, so the writer's map must stop naming it even if a
        // later step fails: `fsck` walks that map, and an id whose file is gone fails the check
        // permanently rather than only until the next cycle.
        self.forget_pack(id);
        let mut out = Discarded {
            removed_bytes: len,
            durability_error: None,
        };
        if let Err(e) = g.io.sync_dir(&pack::pack_dir(g.dir)) {
            out.durability_error = Some(e.into());
            return Ok(out);
        }
        fsio::mark(MARK_AFTER_DIRSYNC);
        // Only now, with the file really gone, is a whole-pack acceptance true. Written while the
        // pack was still there it would be a wildcard: `find` treats a zero nonce as matching any,
        // so it would swallow damage reported against this pack later, and the pack id is never
        // reused, so nothing can legitimately come back to that id.
        let mut entries = ack::load(g.dir);
        entries.push(ack::Entry {
            pack: id,
            nonce: 0,
            state: ack::State::Acked,
            offset: ack::WHOLE_PACK.0,
            len: ack::WHOLE_PACK.1,
            id: None,
        });
        if let Err(e) = ack::save(g.io, g.dir, entries) {
            out.durability_error = Some(e.into());
        }
        Ok(out)
    }
}

/// What one [`Store::discard`] actually did.
///
/// The effect and the failure are separate because they answer different questions: `removed_bytes`
/// is what the store really freed, and `durability_error` is a step that failed after the pack was
/// already gone, so the removal happened but is not confirmed durable. A caller that drops
/// `removed_bytes` when `durability_error` is set under-reports a reclaim that happened; a caller
/// that drops `durability_error` reports a removal it cannot vouch for.
#[derive(Debug, Default)]
pub struct Discarded {
    /// The pack file's length, which the unlink removed. Zero when the pack was already gone, so a
    /// caller credits exactly one removal per pack that actually left the store.
    pub removed_bytes: u64,
    /// A failure that happened after the unlink. The pack is gone; its removal is not confirmed
    /// durable and the acceptance record that says so was not written.
    pub durability_error: Option<Error>,
}
