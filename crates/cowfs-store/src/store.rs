//! The block store. See `docs/v1-store.md`.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use crate::chunk::{chunks, Chunker};
use crate::error::{Error, Result};
use crate::index::{self, Index, Loc};
use crate::pack::{self, Event, PACK_HEADER_LEN};
use crate::record::{self, Codec, Header, HEADER_LEN};
use crate::{BlockId, ChunkRef, MAX_BLOCK_LEN, MAX_CHUNK_LEN};

const MAX_PACK_LIMIT: u64 = 1 << 31;

/// Settings for [`Store::open`].
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Start a new pack when appending would pass this size. Clamped to 2 GiB.
    pub max_pack_size: u64,
    /// Write the index checkpoint when the store is dropped after writes.
    pub checkpoint_on_drop: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            max_pack_size: 256 << 20,
            checkpoint_on_drop: true,
        }
    }
}

/// A run of bytes in a pack that belongs to no valid record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gap {
    /// Pack id.
    pub pack: u32,
    /// Offset of the first bad byte.
    pub offset: u64,
    /// Number of bad bytes.
    pub len: u64,
}

/// What [`Store::open`] found and repaired.
#[derive(Clone, Debug, Default)]
pub struct RecoveryReport {
    /// The index checkpoint was valid and used.
    pub index_loaded: bool,
    /// Records found by scanning packs (beyond the checkpoint).
    pub records_scanned: u64,
    /// Bytes cut from the end of the last pack as a torn tail.
    pub truncated_bytes: u64,
    /// Damaged regions that were skipped but not removed.
    pub gaps: Vec<Gap>,
}

/// Counters and sizes, see [`Store::stats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Unique blocks in the index.
    pub blocks: u64,
    /// Sum of uncompressed sizes of unique blocks.
    pub uncompressed_bytes: u64,
    /// Sum of record sizes (header and payload) of indexed blocks.
    pub stored_bytes: u64,
    /// Number of pack files.
    pub packs: u64,
    /// Total size of all pack files.
    pub pack_bytes: u64,
    /// `put` calls since open.
    pub put_calls: u64,
    /// Bytes given to `put` since open.
    pub put_bytes: u64,
    /// `put` calls that found the block already stored.
    pub dedup_hits: u64,
    /// Bytes not stored because of those hits.
    pub dedup_bytes: u64,
}

/// A problem found by [`Store::fsck`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Damage {
    /// Bytes that are not part of a valid record.
    Gap {
        /// Pack id.
        pack: u32,
        /// Offset of the first bad byte.
        offset: u64,
        /// Number of bad bytes.
        len: u64,
    },
    /// A record whose data does not hash to its id.
    HashMismatch {
        /// Pack id.
        pack: u32,
        /// Record offset.
        offset: u64,
        /// The id the record claims.
        id: BlockId,
    },
    /// A record with a valid checksum whose payload does not decode.
    BadPayload {
        /// Pack id.
        pack: u32,
        /// Record offset.
        offset: u64,
        /// The id the record claims.
        id: BlockId,
    },
    /// An index entry that does not point at a verified record for that id.
    IndexEntry {
        /// The indexed id.
        id: BlockId,
    },
}

/// Result of [`Store::fsck`].
#[derive(Clone, Debug, Default)]
pub struct FsckReport {
    /// Pack files scanned.
    pub packs: u64,
    /// Bytes scanned.
    pub bytes_scanned: u64,
    /// Valid records found, duplicates included.
    pub records: u64,
    /// Distinct blocks whose data re-hashed correctly.
    pub blocks_verified: u64,
    /// Valid records for an id that already had one.
    pub duplicate_records: u64,
    /// Everything wrong.
    pub damage: Vec<Damage>,
}

impl FsckReport {
    /// True when nothing is wrong.
    pub fn is_clean(&self) -> bool {
        self.damage.is_empty()
    }
}

#[derive(Debug, Default)]
struct Counters {
    put_calls: AtomicU64,
    put_bytes: AtomicU64,
    dedup_hits: AtomicU64,
    dedup_bytes: AtomicU64,
}

#[derive(Debug)]
struct Writer {
    id: u32,
    file: Arc<File>,
    len: u64,
    sealed: BTreeMap<u32, u64>,
}

impl Writer {
    fn pack_lens(&self) -> BTreeMap<u32, u64> {
        let mut lens = self.sealed.clone();
        lens.insert(self.id, self.len);
        lens
    }
}

/// A content-addressed block store rooted at one directory.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    _lock: File,
    index: Index,
    packs: RwLock<HashMap<u32, Arc<File>>>,
    writer: Mutex<Writer>,
    checkpointing: Mutex<()>,
    dirty: AtomicBool,
    max_pack_size: u64,
    checkpoint_on_drop: bool,
    counters: Counters,
    recovery: RecoveryReport,
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

fn create_pack(store: &Path, id: u32) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(pack::pack_path(store, id))?;
    file.write_all_at(&pack::header_bytes(), 0)?;
    file.sync_all()?;
    sync_dir(&pack::pack_dir(store))?;
    Ok(file)
}

impl Store {
    /// Open the store at `dir`, creating it if absent, and repair any torn tail.
    pub fn open(dir: impl AsRef<Path>, options: Options) -> Result<Store> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(pack::pack_dir(&dir))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.join("LOCK"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(Error::Locked(dir)),
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
        let max_pack_size = options.max_pack_size.clamp(1, MAX_PACK_LIMIT);

        let mut ids = Vec::new();
        for entry in fs::read_dir(pack::pack_dir(&dir))? {
            if let Some(id) = entry?.file_name().to_str().and_then(pack::parse_pack_name) {
                ids.push(id);
            }
        }
        ids.sort_unstable();
        let last = ids.last().copied();

        let mut files: BTreeMap<u32, Arc<File>> = BTreeMap::new();
        let mut lens: BTreeMap<u32, u64> = BTreeMap::new();
        for &id in &ids {
            let path = pack::pack_path(&dir, id);
            let file = OpenOptions::new().read(true).write(true).open(&path)?;
            let mut len = file.metadata()?.len();
            let bad = |reason| Error::BadPack {
                path: path.clone(),
                reason,
            };
            if len < PACK_HEADER_LEN {
                if Some(id) != last {
                    return Err(bad("truncated header"));
                }
                file.set_len(0)?;
                file.write_all_at(&pack::header_bytes(), 0)?;
                file.sync_all()?;
                len = PACK_HEADER_LEN;
            } else if !pack::header_ok(&file)? {
                return Err(bad("bad header or unsupported version"));
            }
            if len > u64::from(u32::MAX) {
                return Err(bad("pack larger than 4 GiB"));
            }
            files.insert(id, Arc::new(file));
            lens.insert(id, len);
        }

        let index = Index::new();
        let mut recovery = RecoveryReport::default();
        let mut starts: HashMap<u32, u64> = HashMap::new();
        if let Some(ck) = index::load(&dir) {
            let scanned: HashMap<u32, u64> = ck.packs.iter().copied().collect();
            let packs_ok = scanned
                .iter()
                .all(|(id, &n)| n >= PACK_HEADER_LEN && lens.get(id).is_some_and(|&l| l >= n));
            let entries_ok = ck.entries.iter().all(|(_, loc)| {
                scanned.get(&loc.pack).is_some_and(|&n| {
                    u64::from(loc.offset) >= PACK_HEADER_LEN
                        && u64::from(loc.offset) + HEADER_LEN as u64 + u64::from(loc.slen) <= n
                })
            });
            if packs_ok && entries_ok {
                for (id, loc) in ck.entries {
                    index.insert_if_absent(id, loc);
                }
                starts = scanned;
                recovery.index_loaded = true;
            }
        }

        for (&id, file) in &files {
            let len = lens.get(&id).copied().unwrap_or(PACK_HEADER_LEN);
            let start = starts.get(&id).copied().unwrap_or(PACK_HEADER_LEN);
            let mut cut_at = None;
            let mut touched = false;
            pack::scan(file, start, len, |event| {
                touched = true;
                match event {
                    Event::Record { offset, header, .. } => {
                        recovery.records_scanned += 1;
                        index.insert_if_absent(
                            header.id,
                            Loc {
                                pack: id,
                                offset: offset as u32,
                                slen: header.slen,
                                ulen: header.ulen,
                            },
                        );
                    }
                    Event::Gap {
                        offset,
                        len: gap_len,
                        trailing,
                    } => {
                        if trailing && Some(id) == last {
                            cut_at = Some(offset);
                        } else {
                            recovery.gaps.push(Gap {
                                pack: id,
                                offset,
                                len: gap_len,
                            });
                        }
                    }
                }
                Ok(())
            })?;
            if let Some(at) = cut_at {
                file.set_len(at)?;
                recovery.truncated_bytes += len - at;
                lens.insert(id, at);
            }
            if touched {
                file.sync_data()?;
            }
        }

        let mut sealed = lens.clone();
        let mut packs: HashMap<u32, Arc<File>> = files.into_iter().collect();
        let (id, file, len) = match last {
            Some(id) if lens.get(&id).copied().unwrap_or(0) < max_pack_size => {
                sealed.remove(&id);
                let len = lens.get(&id).copied().unwrap_or(PACK_HEADER_LEN);
                match packs.get(&id) {
                    Some(f) => (id, Arc::clone(f), len),
                    None => return Err(io::Error::other("active pack missing").into()),
                }
            }
            _ => {
                let id = match last {
                    Some(l) => l
                        .checked_add(1)
                        .ok_or_else(|| io::Error::other("pack ids exhausted"))?,
                    None => 0,
                };
                let file = Arc::new(create_pack(&dir, id)?);
                packs.insert(id, Arc::clone(&file));
                (id, file, PACK_HEADER_LEN)
            }
        };

        Ok(Store {
            dir,
            _lock: lock,
            index,
            packs: RwLock::new(packs),
            writer: Mutex::new(Writer {
                id,
                file,
                len,
                sealed,
            }),
            checkpointing: Mutex::new(()),
            dirty: AtomicBool::new(false),
            max_pack_size,
            checkpoint_on_drop: options.checkpoint_on_drop,
            counters: Counters::default(),
            recovery,
        })
    }

    fn writer(&self) -> MutexGuard<'_, Writer> {
        self.writer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn pack_file(&self, id: u32) -> Option<Arc<File>> {
        let packs = self.packs.read().unwrap_or_else(PoisonError::into_inner);
        packs.get(&id).cloned()
    }

    /// What open found and repaired.
    pub fn recovery(&self) -> &RecoveryReport {
        &self.recovery
    }

    /// Store one block of at most [`MAX_BLOCK_LEN`] bytes and return its id.
    ///
    /// Storing bytes that already exist stores nothing. The block is durable after the next [`Store::sync`].
    pub fn put(&self, data: &[u8]) -> Result<BlockId> {
        if data.len() > MAX_BLOCK_LEN {
            return Err(Error::BlockTooLarge(data.len()));
        }
        let id = BlockId::of(data);
        self.counters.put_calls.fetch_add(1, Relaxed);
        self.counters
            .put_bytes
            .fetch_add(data.len() as u64, Relaxed);
        if self.index.get(&id).is_some() {
            self.dedup_hit(data.len());
            return Ok(id);
        }
        let rec = record::encode(id, data)?;
        let mut w = self.writer();
        if self.index.get(&id).is_some() {
            drop(w);
            self.dedup_hit(data.len());
            return Ok(id);
        }
        if w.len > PACK_HEADER_LEN && w.len + rec.len() as u64 > self.max_pack_size {
            self.roll(&mut w)?;
        }
        let offset = u32::try_from(w.len).map_err(|_| io::Error::other("pack offset overflow"))?;
        if let Err(e) = w.file.write_all_at(&rec, w.len) {
            let _ = w.file.set_len(w.len);
            return Err(e.into());
        }
        w.len += rec.len() as u64;
        self.index.insert_if_absent(
            id,
            Loc {
                pack: w.id,
                offset,
                slen: (rec.len() - HEADER_LEN) as u32,
                ulen: data.len() as u32,
            },
        );
        self.dirty.store(true, Relaxed);
        Ok(id)
    }

    fn dedup_hit(&self, len: usize) {
        self.counters.dedup_hits.fetch_add(1, Relaxed);
        self.counters.dedup_bytes.fetch_add(len as u64, Relaxed);
    }

    fn roll(&self, w: &mut Writer) -> Result<()> {
        w.file.sync_data()?;
        let id =
            w.id.checked_add(1)
                .ok_or_else(|| io::Error::other("pack ids exhausted"))?;
        let file = Arc::new(create_pack(&self.dir, id)?);
        self.packs
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, Arc::clone(&file));
        w.sealed.insert(w.id, w.len);
        w.id = id;
        w.file = file;
        w.len = PACK_HEADER_LEN;
        Ok(())
    }

    /// Read a block, verifying its checksum and hash. Never returns unverified data.
    pub fn get(&self, id: BlockId) -> Result<Vec<u8>> {
        let loc = self.index.get(&id).ok_or(Error::NotFound(id))?;
        let corrupt = |reason| Error::Corrupt {
            pack: loc.pack,
            offset: u64::from(loc.offset),
            reason,
        };
        let file = self
            .pack_file(loc.pack)
            .ok_or(corrupt("pack file missing"))?;
        let mut buf = vec![0u8; HEADER_LEN + loc.slen as usize];
        file.read_exact_at(&mut buf, u64::from(loc.offset))
            .map_err(|e| match e.kind() {
                io::ErrorKind::UnexpectedEof => corrupt("record extends past end of pack"),
                _ => Error::Io(e),
            })?;
        let (head, payload) = buf.split_at(HEADER_LEN);
        let raw: &[u8; HEADER_LEN] = head.try_into().map_err(|_| corrupt("short header"))?;
        let header = Header::parse(raw).map_err(corrupt)?;
        if header.id != id || header.slen != loc.slen || header.ulen != loc.ulen {
            return Err(corrupt("record does not match index"));
        }
        if Header::expected_crc(raw, payload) != header.crc {
            return Err(corrupt("checksum mismatch"));
        }
        let data = match header.codec {
            Codec::Raw => {
                buf.drain(..HEADER_LEN);
                buf
            }
            Codec::Zstd => record::decode(&header, payload).map_err(corrupt)?,
        };
        if BlockId::of(&data) != id {
            return Err(Error::HashMismatch(id));
        }
        Ok(data)
    }

    /// True if the block is indexed. Does not read or verify the block.
    pub fn contains(&self, id: BlockId) -> bool {
        self.index.get(&id).is_some()
    }

    /// Make every earlier `put` durable.
    pub fn sync(&self) -> Result<()> {
        let file = Arc::clone(&self.writer().file);
        file.sync_data()?;
        Ok(())
    }

    /// Sync, then write `index.cix` so the next open only scans what was added after this call.
    pub fn checkpoint(&self) -> Result<()> {
        let _serial = self
            .checkpointing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (lens, entries) = {
            let w = self.writer();
            w.file.sync_data()?;
            self.dirty.store(false, Relaxed);
            (w.pack_lens(), self.index.entries())
        };
        let packs: Vec<(u32, u64)> = lens.into_iter().collect();
        if let Err(e) = index::save(&self.dir, &packs, &entries) {
            self.dirty.store(true, Relaxed);
            return Err(e.into());
        }
        Ok(())
    }

    /// Sizes and counters.
    pub fn stats(&self) -> Stats {
        let lens = self.writer().pack_lens();
        let mut s = Stats {
            packs: lens.len() as u64,
            pack_bytes: lens.values().sum(),
            put_calls: self.counters.put_calls.load(Relaxed),
            put_bytes: self.counters.put_bytes.load(Relaxed),
            dedup_hits: self.counters.dedup_hits.load(Relaxed),
            dedup_bytes: self.counters.dedup_bytes.load(Relaxed),
            ..Stats::default()
        };
        for (_, loc) in self.index.entries() {
            s.blocks += 1;
            s.uncompressed_bytes += u64::from(loc.ulen);
            s.stored_bytes += HEADER_LEN as u64 + u64::from(loc.slen);
        }
        s
    }

    /// All indexed ids, in no particular order.
    pub fn iter_ids(&self) -> impl Iterator<Item = BlockId> {
        self.index.entries().into_iter().map(|(id, _)| id)
    }

    /// Chunk `data` with FastCDC, store every chunk, and return the chunk list.
    pub fn ingest_bytes(&self, data: &[u8]) -> Result<Vec<ChunkRef>> {
        chunks(data)
            .map(|c| {
                Ok(ChunkRef {
                    id: self.put(c)?,
                    len: c.len() as u32,
                })
            })
            .collect()
    }

    /// Chunk everything `reader` yields, store every chunk, and return the chunk list.
    pub fn ingest<R: Read>(&self, mut reader: R) -> Result<Vec<ChunkRef>> {
        let chunker = Chunker::new();
        let window = 4 * MAX_CHUNK_LEN;
        let mut buf: Vec<u8> = Vec::new();
        let mut start = 0;
        let mut eof = false;
        let mut out = Vec::new();
        loop {
            if !eof && buf.len() - start < MAX_CHUNK_LEN {
                buf.drain(..start);
                start = 0;
                let want = window - buf.len();
                let got = reader.by_ref().take(want as u64).read_to_end(&mut buf)?;
                eof = got < want;
            }
            if start == buf.len() {
                return Ok(out);
            }
            let n = chunker.cut(&buf[start..]);
            out.push(ChunkRef {
                id: self.put(&buf[start..start + n])?,
                len: n as u32,
            });
            start += n;
        }
    }

    /// Re-hash every block and re-check every record checksum, without changing anything.
    pub fn fsck(&self) -> Result<FsckReport> {
        let lens = self.writer().pack_lens();
        let mut report = FsckReport::default();
        let mut verified: HashMap<(u32, u32), BlockId> = HashMap::new();
        let mut seen = std::collections::HashSet::new();
        for (&pack_id, &len) in &lens {
            let file = self.pack_file(pack_id).ok_or(Error::Corrupt {
                pack: pack_id,
                offset: 0,
                reason: "pack file missing",
            })?;
            pack::scan(&file, PACK_HEADER_LEN, len, |event| {
                match event {
                    Event::Record {
                        offset,
                        header,
                        payload,
                    } => {
                        report.records += 1;
                        match record::decode(header, payload) {
                            Ok(data) if BlockId::of(&data) == header.id => {
                                verified.insert((pack_id, offset as u32), header.id);
                                if seen.insert(header.id) {
                                    report.blocks_verified += 1;
                                } else {
                                    report.duplicate_records += 1;
                                }
                            }
                            Ok(_) => report.damage.push(Damage::HashMismatch {
                                pack: pack_id,
                                offset,
                                id: header.id,
                            }),
                            Err(_) => report.damage.push(Damage::BadPayload {
                                pack: pack_id,
                                offset,
                                id: header.id,
                            }),
                        }
                    }
                    Event::Gap { offset, len, .. } => report.damage.push(Damage::Gap {
                        pack: pack_id,
                        offset,
                        len,
                    }),
                }
                Ok(())
            })?;
            report.packs += 1;
            report.bytes_scanned += len;
        }
        for (id, loc) in self.index.entries() {
            let in_scan = lens
                .get(&loc.pack)
                .is_some_and(|&l| u64::from(loc.offset) < l);
            if in_scan && verified.get(&(loc.pack, loc.offset)) != Some(&id) {
                report.damage.push(Damage::IndexEntry { id });
            }
        }
        Ok(report)
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if self.checkpoint_on_drop && self.dirty.load(Relaxed) {
            let _ = self.checkpoint();
        }
    }
}
