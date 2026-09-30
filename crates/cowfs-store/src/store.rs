//! The block store. See `docs/v1-store.md`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use crate::chunk::{chunks, Chunker};
use crate::error::{Error, Result};
use crate::fdcache::FdCache;
use crate::fsio::{Io, Trace};
use crate::index::{self, Index, Loc};
use crate::pack::{self, Event, PACK_HEADER_LEN};
use crate::record::{self, Codec, Header, HEADER_LEN};
use crate::types::{CorruptRegion, Damage, FsckReport, Gap, Options, RecoveryReport, Stats};
use crate::wm::{Mark, Wm};
use crate::{BlockId, ChunkRef, MAX_BLOCK_LEN, MAX_CHUNK_LEN};

const MAX_PACK_LIMIT: u64 = 1 << 31;

#[derive(Debug, Default)]
struct Counters {
    put_calls: AtomicU64,
    put_bytes: AtomicU64,
    dedup_hits: AtomicU64,
    dedup_bytes: AtomicU64,
    packs: AtomicU64,
    pack_bytes: AtomicU64,
}

#[derive(Debug)]
struct Writer {
    id: u32,
    file: Arc<File>,
    len: u64,
    sealed: BTreeMap<u32, u64>,
    /// `(pack, len)` up to which a completed `sync` has made the data and the watermark durable.
    synced: (u32, u64),
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
    io: Io,
    _lock: File,
    index: Index,
    reads: FdCache,
    writer: Mutex<Writer>,
    wm: Mutex<Wm>,
    checkpointing: Mutex<()>,
    dirty: AtomicBool,
    max_pack_size: u64,
    checkpoint_on_drop: bool,
    counters: Counters,
    damaged: RwLock<HashMap<BlockId, (u32, u64)>>,
    rescan: BTreeSet<u32>,
    recovery: RecoveryReport,
}

fn create_pack(io: &Io, store: &Path, id: u32) -> io::Result<File> {
    let path = pack::pack_path(store, id);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)?;
    io.created(&path);
    file.write_all_at(&pack::header_bytes(), 0)?;
    io.sync_file(&file, &path)?;
    io.sync_dir(&pack::pack_dir(store))?;
    Ok(file)
}

fn open_pack(store: &Path, id: u32) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(pack::pack_path(store, id))
}

fn locate_ok(loc: &Loc) -> bool {
    loc.ulen as usize <= MAX_BLOCK_LEN && loc.slen <= loc.ulen
}

/// A bad region found while scanning one pack.
struct Bad {
    offset: u64,
    len: u64,
    id: Option<BlockId>,
}

impl Store {
    /// Open the store at `dir`, creating it if absent, and repair any torn tail.
    pub fn open(dir: impl AsRef<Path>, options: Options) -> Result<Store> {
        Self::open_with(dir.as_ref(), options, Io::new(None, false))
    }

    /// Like [`Store::open`], but record every durability operation in `trace`. For tests.
    #[doc(hidden)]
    pub fn open_traced(dir: impl AsRef<Path>, options: Options, trace: Trace) -> Result<Store> {
        Self::open_with(dir.as_ref(), options, Io::new(Some(trace), false))
    }

    /// Like [`Store::open`], but never flush to disk. Only for tests that cannot afford fsync.
    #[doc(hidden)]
    pub fn open_unsynced(dir: impl AsRef<Path>, options: Options) -> Result<Store> {
        Self::open_with(dir.as_ref(), options, Io::new(None, true))
    }

    fn open_with(dir: &Path, options: Options, io: Io) -> Result<Store> {
        let dir = dir.to_path_buf();
        io.create_dir_durable(&pack::pack_dir(&dir))?;
        let lock_path = dir.join("LOCK");
        let lock_existed = lock_path.exists();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        if !lock_existed {
            io.created(&lock_path);
            io.sync_dir(&dir)?;
        }
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
        let mut lens: BTreeMap<u32, u64> = BTreeMap::new();
        for &id in &ids {
            lens.insert(id, fs::metadata(pack::pack_path(&dir, id))?.len());
        }

        let mut wm = Wm::open(&io, &dir)?;
        let mark = wm.mark();
        let mut recovery = RecoveryReport {
            watermark_missing: mark.is_none() && !ids.is_empty(),
            ..RecoveryReport::default()
        };
        let durable = |id: u32| -> Option<u64> {
            let m = mark?;
            Some(match id.cmp(&m.pack) {
                std::cmp::Ordering::Less => u64::MAX,
                std::cmp::Ordering::Equal => m.len,
                std::cmp::Ordering::Greater => 0,
            })
        };

        let index = Index::new();
        let mut starts: HashMap<u32, u64> = HashMap::new();
        if let Some(ck) = index::load(&dir) {
            let scanned: HashMap<u32, u64> = ck.packs.iter().copied().collect();
            let packs_ok = scanned
                .iter()
                .all(|(id, &n)| n >= PACK_HEADER_LEN && lens.get(id).is_some_and(|&l| l >= n));
            let entries_ok = ck.entries.iter().all(|(_, loc)| {
                locate_ok(loc)
                    && scanned.get(&loc.pack).is_some_and(|&n| {
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

        let mut damaged: HashMap<BlockId, (u32, u64)> = HashMap::new();
        let mut rescan = BTreeSet::new();
        let mut last_file: Option<File> = None;
        let mut last_damaged = false;
        for &id in &ids {
            let path = pack::pack_path(&dir, id);
            let file = open_pack(&dir, id)?;
            let mut len = lens.get(&id).copied().unwrap_or(0);
            let bad_pack = |reason| Error::BadPack {
                path: path.clone(),
                reason,
            };
            if len < PACK_HEADER_LEN {
                if Some(id) != last {
                    return Err(bad_pack("truncated header"));
                }
                io.truncate(&file, &path, 0)?;
                file.write_all_at(&pack::header_bytes(), 0)?;
                io.sync_file(&file, &path)?;
                len = PACK_HEADER_LEN;
            } else if !pack::header_ok(&file)? {
                return Err(bad_pack("bad header or unsupported version"));
            }
            if len > u64::from(u32::MAX) {
                return Err(bad_pack("pack larger than 4 GiB"));
            }
            let dur = durable(id);
            if let Some(d) = dur.filter(|&d| d != u64::MAX && len < d) {
                recovery.corrupt_synced.push(CorruptRegion {
                    pack: id,
                    offset: len,
                    len: d - len,
                    id: None,
                });
                rescan.insert(id);
                if Some(id) == last {
                    last_damaged = true;
                }
            }

            let start = starts.get(&id).copied().unwrap_or(PACK_HEADER_LEN);
            let mut bad: Vec<Bad> = Vec::new();
            let mut valid_end = start;
            let mut exhausted = false;
            let mut scanned = 0u64;
            pack::scan(&file, start, len, |event| {
                match event {
                    Event::Record {
                        offset,
                        header,
                        payload,
                    } => {
                        if record::verify(header, payload) {
                            scanned += 1;
                            valid_end = offset + header.total_len();
                            index.insert_verified(
                                header.id,
                                Loc {
                                    pack: id,
                                    offset: offset as u32,
                                    slen: header.slen,
                                    ulen: header.ulen,
                                    verified: true,
                                },
                            );
                        } else {
                            bad.push(Bad {
                                offset,
                                len: header.total_len(),
                                id: Some(header.id),
                            });
                        }
                    }
                    Event::Gap {
                        offset,
                        len: glen,
                        exhausted: ex,
                        ..
                    } => {
                        exhausted |= ex;
                        bad.push(Bad {
                            offset,
                            len: glen,
                            id: pack::peek_id(&file, offset, len),
                        });
                    }
                }
                Ok(())
            })?;
            recovery.records_scanned += scanned;

            let can_cut = Some(id) == last
                && !exhausted
                && valid_end < len
                && dur.is_some_and(|d| valid_end >= d);
            let cut = can_cut.then_some(valid_end);
            for b in bad.iter().filter(|b| cut.is_none_or(|c| b.offset < c)) {
                recovery.gaps.push(Gap {
                    pack: id,
                    offset: b.offset,
                    len: b.len,
                });
                rescan.insert(id);
                if Some(id) == last {
                    last_damaged = true;
                }
                if let Some(d) = dur.filter(|&d| b.offset < d) {
                    recovery.corrupt_synced.push(CorruptRegion {
                        pack: id,
                        offset: b.offset,
                        len: b.len.min(d - b.offset),
                        id: b.id,
                    });
                    if let Some(bid) = b.id {
                        damaged.insert(bid, (id, b.offset));
                    }
                }
            }
            if let Some(c) = cut {
                io.truncate(&file, &path, c)?;
                recovery.truncated_bytes += len - c;
                len = c;
            }
            if scanned > 0 || cut.is_some() {
                io.sync_file(&file, &path)?;
            }
            lens.insert(id, len);
            if Some(id) == last {
                last_file = Some(file);
            }
        }
        let counters = Counters::default();
        let (id, file, len) = match (last, last_file) {
            (Some(id), Some(f))
                if !last_damaged && lens.get(&id).copied().unwrap_or(0) < max_pack_size =>
            {
                let len = lens.get(&id).copied().unwrap_or(PACK_HEADER_LEN);
                (id, Arc::new(f), len)
            }
            _ => {
                let id = match last {
                    Some(l) => l
                        .checked_add(1)
                        .ok_or_else(|| io::Error::other("pack ids exhausted"))?,
                    None => 0,
                };
                let file = Arc::new(create_pack(&io, &dir, id)?);
                lens.insert(id, PACK_HEADER_LEN);
                (id, file, PACK_HEADER_LEN)
            }
        };
        let mut sealed = lens.clone();
        sealed.remove(&id);
        counters.packs.store(lens.len() as u64, Relaxed);
        counters.pack_bytes.store(lens.values().sum(), Relaxed);

        let fresh_or_clean = ids.is_empty() || (recovery.gaps.is_empty() && rescan.is_empty());
        if fresh_or_clean {
            wm.advance(&io, Mark { pack: id, len })?;
        }
        let synced = wm.mark().map_or((0, 0), |m| (m.pack, m.len));

        Ok(Store {
            dir: dir.clone(),
            reads: FdCache::new(dir, options.max_open_packs),
            io,
            _lock: lock,
            index,
            writer: Mutex::new(Writer {
                id,
                file,
                len,
                sealed,
                synced,
            }),
            wm: Mutex::new(wm),
            checkpointing: Mutex::new(()),
            dirty: AtomicBool::new(false),
            max_pack_size,
            checkpoint_on_drop: options.checkpoint_on_drop,
            counters,
            damaged: RwLock::new(damaged),
            rescan,
            recovery,
        })
    }

    fn writer(&self) -> MutexGuard<'_, Writer> {
        self.writer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What open found and repaired.
    pub fn recovery(&self) -> &RecoveryReport {
        &self.recovery
    }

    /// Store one block of at most [`MAX_BLOCK_LEN`] bytes and return its id.
    ///
    /// Storing bytes that already exist stores nothing, but an existing copy that was not read
    /// back in this session is checked against `data` first, and rewritten if it is damaged.
    /// The block is durable after the next [`Store::sync`].
    pub fn put(&self, data: &[u8]) -> Result<BlockId> {
        if data.len() > MAX_BLOCK_LEN {
            return Err(Error::BlockTooLarge(data.len()));
        }
        let id = BlockId::of(data);
        self.counters.put_calls.fetch_add(1, Relaxed);
        self.counters
            .put_bytes
            .fetch_add(data.len() as u64, Relaxed);
        if let Some(loc) = self.index.get(&id) {
            if loc.verified || self.confirm(id, loc, data) {
                self.dedup_hit(data.len());
                return Ok(id);
            }
        }
        let rec = record::encode(id, data)?;
        let mut w = self.writer();
        if self.index.get(&id).is_some_and(|l| l.verified) {
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
        self.counters
            .pack_bytes
            .fetch_add(rec.len() as u64, Relaxed);
        self.index.replace(
            id,
            Loc {
                pack: w.id,
                offset,
                slen: (rec.len() - HEADER_LEN) as u32,
                ulen: data.len() as u32,
                verified: true,
            },
        );
        drop(w);
        if !self.damaged_read().is_empty() {
            self.damaged
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&id);
        }
        self.dirty.store(true, Relaxed);
        Ok(id)
    }

    fn damaged_read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<BlockId, (u32, u64)>> {
        self.damaged.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn dedup_hit(&self, len: usize) {
        self.counters.dedup_hits.fetch_add(1, Relaxed);
        self.counters.dedup_bytes.fetch_add(len as u64, Relaxed);
    }

    /// True when the record at `loc` is intact and holds exactly `data`, whose hash is `id`.
    fn confirm(&self, id: BlockId, loc: Loc, data: &[u8]) -> bool {
        let Ok((header, buf)) = self.read_record(id, loc) else {
            return false;
        };
        let payload = &buf[HEADER_LEN..];
        let same = match header.codec {
            Codec::Raw => payload == data,
            Codec::Zstd => record::decode(&header, payload).is_ok_and(|d| d == data),
        };
        if same {
            self.index.mark_verified(&id, loc);
        }
        same
    }

    fn roll(&self, w: &mut Writer) -> Result<()> {
        self.io
            .sync_file(&w.file, &pack::pack_path(&self.dir, w.id))?;
        let id =
            w.id.checked_add(1)
                .ok_or_else(|| io::Error::other("pack ids exhausted"))?;
        let file = Arc::new(create_pack(&self.io, &self.dir, id)?);
        w.sealed.insert(w.id, w.len);
        w.id = id;
        w.file = file;
        w.len = PACK_HEADER_LEN;
        self.counters.packs.fetch_add(1, Relaxed);
        self.counters.pack_bytes.fetch_add(PACK_HEADER_LEN, Relaxed);
        Ok(())
    }

    /// Read the record `loc` names and check its structure and checksum, not its hash.
    fn read_record(&self, id: BlockId, loc: Loc) -> Result<(Header, Vec<u8>)> {
        let corrupt = |reason| Error::Corrupt {
            pack: loc.pack,
            offset: u64::from(loc.offset),
            reason,
        };
        if !locate_ok(&loc) {
            return Err(corrupt("index entry out of range"));
        }
        let file = self.reads.get(loc.pack).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => corrupt("pack file missing"),
            _ => Error::Io(e),
        })?;
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
        Ok((header, buf))
    }

    /// Read a block, verifying its checksum and hash. Never returns unverified data.
    pub fn get(&self, id: BlockId) -> Result<Vec<u8>> {
        let Some(loc) = self.index.get(&id) else {
            return Err(match self.damaged_read().get(&id) {
                Some(&(pack, offset)) => Error::Corrupt {
                    pack,
                    offset,
                    reason: "record damaged",
                },
                None => Error::NotFound(id),
            });
        };
        let (header, mut buf) = self.read_record(id, loc)?;
        let data = match header.codec {
            Codec::Raw => {
                buf.drain(..HEADER_LEN);
                buf
            }
            Codec::Zstd => {
                record::decode(&header, &buf[HEADER_LEN..]).map_err(|reason| Error::Corrupt {
                    pack: loc.pack,
                    offset: u64::from(loc.offset),
                    reason,
                })?
            }
        };
        if BlockId::of(&data) != id {
            return Err(Error::HashMismatch(id));
        }
        if !loc.verified {
            self.index.mark_verified(&id, loc);
        }
        Ok(data)
    }

    /// True if the block is indexed. Does not read or verify the block.
    pub fn contains(&self, id: BlockId) -> bool {
        self.index.get(&id).is_some()
    }

    /// Make every earlier `put` durable.
    pub fn sync(&self) -> Result<()> {
        self.sync_capture().map(drop)
    }

    /// Sync and return the pack lengths that are now durable. Data first, then the watermark.
    fn sync_capture(&self) -> Result<BTreeMap<u32, u64>> {
        let (file, id, len, lens, needed) = {
            let w = self.writer();
            (
                Arc::clone(&w.file),
                w.id,
                w.len,
                w.pack_lens(),
                w.synced < (w.id, w.len),
            )
        };
        if needed {
            self.io.sync_file(&file, &pack::pack_path(&self.dir, id))?;
            self.wm
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .advance(&self.io, Mark { pack: id, len })?;
            let mut w = self.writer();
            w.synced = w.synced.max((id, len));
        }
        Ok(lens)
    }

    /// Sync, then write `index.cix` so the next open only scans what was added after this call.
    pub fn checkpoint(&self) -> Result<()> {
        let _serial = self
            .checkpointing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.dirty.store(false, Relaxed);
        let result = self.checkpoint_locked();
        if result.is_err() {
            self.dirty.store(true, Relaxed);
        }
        result
    }

    fn checkpoint_locked(&self) -> Result<()> {
        let lens = self.sync_capture()?;
        let packs: Vec<(u32, u64)> = lens
            .iter()
            .map(|(&id, &n)| {
                let n = if self.rescan.contains(&id) {
                    PACK_HEADER_LEN
                } else {
                    n
                };
                (id, n)
            })
            .collect();
        let entries = self.index.snapshot(|loc| {
            !self.rescan.contains(&loc.pack)
                && lens.get(&loc.pack).is_some_and(|&n| {
                    u64::from(loc.offset) + HEADER_LEN as u64 + u64::from(loc.slen) <= n
                })
        });
        index::save(&self.io, &self.dir, &packs, &entries)?;
        Ok(())
    }

    /// Sizes and counters. Constant time.
    pub fn stats(&self) -> Stats {
        let t = self.index.totals();
        Stats {
            blocks: t.blocks,
            uncompressed_bytes: t.ulen,
            stored_bytes: t.stored,
            packs: self.counters.packs.load(Relaxed),
            pack_bytes: self.counters.pack_bytes.load(Relaxed),
            put_calls: self.counters.put_calls.load(Relaxed),
            put_bytes: self.counters.put_bytes.load(Relaxed),
            dedup_hits: self.counters.dedup_hits.load(Relaxed),
            dedup_bytes: self.counters.dedup_bytes.load(Relaxed),
        }
    }

    /// All indexed ids, in no particular order.
    pub fn iter_ids(&self) -> impl Iterator<Item = BlockId> {
        self.index.ids().into_iter()
    }

    /// Chunk `data` with FastCDC, store every chunk, and return the chunk list.
    ///
    /// Blocks stored before an error stay in the store as unreferenced blocks.
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
    ///
    /// Blocks stored before an error stay in the store as unreferenced blocks.
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
        let mut seen = HashSet::new();
        for (&pack_id, &len) in &lens {
            let file = self.reads.get(pack_id).map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => Error::Corrupt {
                    pack: pack_id,
                    offset: 0,
                    reason: "pack file missing",
                },
                _ => Error::Io(e),
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
