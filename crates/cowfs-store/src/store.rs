//! The block store. See `docs/v1-store.md`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use crate::ack::{self, Entry};
use crate::chunk::{chunks, Chunker};
use crate::error::{Error, Result};
use crate::fdcache::FdCache;
use crate::fsio::{Io, Trace};
use crate::index::{self, Index, Loc};
use crate::pack::{self, Event, PACK_HEADER_LEN};
use crate::record::{self, Codec, Header, HEADER_LEN};
use crate::types::{
    CorruptRegion, Damage, FsckReport, Gap, Options, RecoveryReport, SalvageReport, Stats,
};
use crate::wm::{Mark, Wm};
use crate::{BlockId, ChunkRef, MAX_BLOCK_LEN, MAX_CHUNK_LEN};

const MAX_PACK_LIMIT: u64 = 1 << 31;
/// Most bytes of a torn tail kept in a `.torn-<n>` file next to the pack.
const TORN_KEEP: u64 = 1 << 20;
/// Most missing pack ids listed in a recovery report.
const MAX_LISTED: usize = 1 << 16;

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

/// Create pack `id`, or reuse an empty leftover from a failed earlier attempt.
/// On failure nothing is left behind, so the caller can simply try again.
fn create_pack(io: &Io, store: &Path, id: u32) -> io::Result<File> {
    let path = pack::pack_path(store, id);
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(f) => {
            io.created(&path);
            f
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let f = open_pack(store, id)?;
            if f.metadata()?.len() > PACK_HEADER_LEN {
                return Err(e);
            }
            f
        }
        Err(e) => return Err(e),
    };
    let init = || -> io::Result<()> {
        file.set_len(0)?;
        file.write_all_at(&pack::header_bytes(), 0)?;
        io.sync_file(&file, &path)?;
        io.sync_dir(&pack::pack_dir(store))
    };
    if let Err(e) = init() {
        let _ = fs::remove_file(&path);
        return Err(e);
    }
    Ok(file)
}

/// The id the next pack must use, stepping over packs this store created.
///
/// Compaction creates packs above the active one and records them in `sealed`, so `active + 1`
/// can already be a pack of ours and must be stepped over rather than refused. A file at that id
/// that this store does not know is foreign, and the caller keeps the old contract for it: an
/// empty one is reused, one that holds data makes the roll fail.
fn next_pack_id(w: &Writer, after: u32) -> Result<u32> {
    let mut id = after
        .checked_add(1)
        .ok_or_else(|| io::Error::other("pack ids exhausted"))?;
    while w.sealed.contains_key(&id) {
        id = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("pack ids exhausted"))?;
    }
    Ok(id)
}

/// Read the record `loc` names and check that it decodes to data hashing to `id`.
fn verify_at(dir: &Path, id: BlockId, loc: Loc) -> bool {
    if !locate_ok(&loc) {
        return false;
    }
    let Ok(file) = File::open(pack::pack_path(dir, loc.pack)) else {
        return false;
    };
    let mut buf = vec![0u8; HEADER_LEN + loc.slen as usize];
    if file.read_exact_at(&mut buf, u64::from(loc.offset)).is_err() {
        return false;
    }
    let (head, payload) = buf.split_at(HEADER_LEN);
    let Ok(raw) = <&[u8; HEADER_LEN]>::try_from(head) else {
        return false;
    };
    let Ok(header) = Header::parse(raw) else {
        return false;
    };
    header.id == id
        && header.slen == loc.slen
        && header.ulen == loc.ulen
        && Header::expected_crc(raw, payload) == header.crc
        && record::verify(&header, payload)
}

/// Keep the first megabyte of a discarded tail for forensics. Best effort.
fn save_torn(dir: &Path, pack: u32, file: &File, from: u64, len: u64) {
    let n = (0u32..)
        .find(|n| !torn_path(dir, pack, *n).exists())
        .unwrap_or(0);
    let take = (len - from).min(TORN_KEEP) as usize;
    let mut buf = vec![0u8; take];
    if file.read_exact_at(&mut buf, from).is_ok() {
        let _ = fs::write(torn_path(dir, pack, n), &buf);
    }
}

fn torn_path(dir: &Path, pack: u32, n: u32) -> PathBuf {
    let mut p = pack::pack_path(dir, pack).into_os_string();
    p.push(format!(".torn-{n}"));
    PathBuf::from(p)
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

/// A verified record that sits after a torn region and will be moved down over it.
struct Tail {
    offset: u64,
    total: u64,
    id: BlockId,
    slen: u32,
    ulen: u32,
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
        let acked = ack::load(&dir);
        let mut recovery = RecoveryReport {
            watermark_missing: mark.is_none() && !ids.is_empty(),
            ..RecoveryReport::default()
        };
        // A pack below the last one is sealed, and a pack is only sealed after an fsync.
        // In the last pack only bytes below the watermark are known durable.
        let durable = |id: u32| -> u64 {
            if Some(id) != last {
                return u64::MAX;
            }
            match mark {
                Some(m) if m.pack == id => m.len,
                Some(m) if m.pack > id => u64::MAX,
                _ => PACK_HEADER_LEN,
            }
        };
        if mark.is_some() || !ids.is_empty() {
            let lo = if mark.is_some() {
                wm.base()
            } else {
                ids.first().copied().unwrap_or(0)
            };
            let hi = mark.map_or(0, |m| m.pack).max(last.unwrap_or(0));
            recovery.missing_synced = (lo..=hi)
                .filter(|p| !lens.contains_key(p))
                .filter(|&p| !ack::covers(&acked, p, ack::WHOLE_PACK.0, ack::WHOLE_PACK.1))
                .take(MAX_LISTED)
                .collect();
        }

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

        let mut regions: Vec<CorruptRegion> = Vec::new();
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
            let is_last = Some(id) == last;
            if len < PACK_HEADER_LEN {
                if !is_last {
                    return Err(bad_pack("truncated header"));
                }
                io.truncate(&file, &path, 0)?;
                file.write_all_at(&pack::header_bytes(), 0)?;
                io.sync_file(&file, &path)?;
                len = PACK_HEADER_LEN;
            } else if let Err(reason) = pack::header_check(&file)? {
                return Err(bad_pack(reason));
            }
            if len > u64::from(u32::MAX) {
                return Err(bad_pack("pack larger than 4 GiB"));
            }
            let dur = durable(id);
            if dur != u64::MAX && len < dur {
                regions.push(CorruptRegion {
                    pack: id,
                    offset: len,
                    len: dur - len,
                    id: None,
                });
                rescan.insert(id);
                last_damaged |= is_last;
            }

            let start = starts.get(&id).copied().unwrap_or(PACK_HEADER_LEN);
            let mut bad: Vec<Bad> = Vec::new();
            let mut torn_from: Option<u64> = None;
            let mut tail: Vec<Tail> = Vec::new();
            let mut scanned = 0u64;
            let mut exhausted = false;
            pack::scan(&file, start, len, |event| {
                let (offset, blen, bid) = match event {
                    Event::Record {
                        offset,
                        header,
                        payload,
                    } => {
                        if record::verify(header, payload) {
                            if torn_from.is_some() {
                                tail.push(Tail {
                                    offset,
                                    total: header.total_len(),
                                    id: header.id,
                                    slen: header.slen,
                                    ulen: header.ulen,
                                });
                            } else {
                                scanned += 1;
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
                            }
                            return Ok(());
                        }
                        (offset, header.total_len(), Some(header.id))
                    }
                    Event::Gap {
                        offset,
                        len: glen,
                        exhausted: ex,
                    } => {
                        exhausted |= ex;
                        (offset, glen, pack::peek_id(&file, offset, len))
                    }
                };
                if is_last && offset >= dur && !exhausted && torn_from.is_none() {
                    torn_from = Some(offset);
                }
                bad.push(Bad {
                    offset,
                    len: blen,
                    id: bid,
                });
                Ok(())
            })?;
            recovery.records_scanned += scanned;

            for b in bad
                .iter()
                .filter(|b| torn_from.is_none_or(|t| b.offset < t))
            {
                recovery.gaps.push(Gap {
                    pack: id,
                    offset: b.offset,
                    len: b.len,
                });
                rescan.insert(id);
                last_damaged |= is_last;
                regions.push(CorruptRegion {
                    pack: id,
                    offset: b.offset,
                    len: if b.offset < dur {
                        b.len.min(dur - b.offset)
                    } else {
                        b.len
                    },
                    id: b.id,
                });
            }
            if let Some(t) = torn_from {
                save_torn(&dir, id, &file, t, len);
                let mut w = t;
                let mut buf = Vec::new();
                for r in &tail {
                    buf.resize(r.total as usize, 0);
                    file.read_exact_at(&mut buf, r.offset)?;
                    file.write_all_at(&buf, w)?;
                    index.insert_verified(
                        r.id,
                        Loc {
                            pack: id,
                            offset: w as u32,
                            slen: r.slen,
                            ulen: r.ulen,
                            verified: true,
                        },
                    );
                    w += r.total;
                }
                io.truncate(&file, &path, w)?;
                recovery.torn_tail_discarded += len - w;
                recovery.recovered_from_tail += tail.len() as u64;
                recovery.records_scanned += tail.len() as u64;
                len = w;
            }
            if scanned > 0 || torn_from.is_some() {
                io.sync_file(&file, &path)?;
            }
            lens.insert(id, len);
            if is_last {
                last_file = Some(file);
            }
        }
        recovery.truncated_bytes = recovery.torn_tail_discarded;

        let mut damaged: HashMap<BlockId, (u32, u64)> = HashMap::new();
        for r in regions {
            if let Some(x) = r.id {
                damaged.insert(x, (r.pack, r.offset));
            }
            if ack::covers(&acked, r.pack, r.offset, r.len) {
                recovery.acknowledged.push(r);
                continue;
            }
            let elsewhere = |x: BlockId| {
                index.get(&x).is_some_and(|l| {
                    let at = u64::from(l.offset);
                    let inside = l.pack == r.pack && at >= r.offset && at < r.offset + r.len;
                    if inside {
                        return false;
                    }
                    if !l.verified && !verify_at(&dir, x, l) {
                        return false;
                    }
                    index.mark_verified(&x, l);
                    true
                })
            };
            if r.id.is_some_and(elsewhere) {
                recovery.superseded.push(r);
                continue;
            }
            recovery.corrupt_synced.push(r);
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

        // Every byte scanned above was fsynced and any torn tail is gone, so nothing unresolved
        // sits below this mark. A mark that is already ahead of the packs is left alone.
        let here = Mark { pack: id, len };
        if wm.mark().is_none() {
            wm.init(&io, here)?;
        } else {
            wm.advance(&io, here)?;
        }
        let synced = wm
            .mark()
            .filter(|m| *m <= here)
            .map_or((0, 0), |m| (m.pack, m.len));

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
        let id = next_pack_id(w, w.id)?;
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

impl Store {
    /// Same as [`Store::fsck`]: re-read and re-hash every block of every pack.
    ///
    /// `open` does not re-read data covered by the index checkpoint, so bit rot there shows up
    /// only on `get` or here. Run this when a clean bill of health matters.
    pub fn verify_all(&self) -> Result<FsckReport> {
        self.fsck()
    }

    /// Accept the damage `open` reported, so later opens stop reporting it.
    ///
    /// Every region in `recovery().corrupt_synced` is recorded in the `ACKED` file, and when the
    /// watermark named packs that are gone it is lowered to what exists. The damaged bytes stay on
    /// disk and the blocks in them stay unreadable until they are `put` again. Returns the number
    /// of entries recorded. A damaged region whose block has a verified copy elsewhere needs no
    /// acknowledgement: it is reported as `superseded`.
    pub fn acknowledge_corruption(&self) -> Result<usize> {
        let r = &self.recovery;
        let mut entries: Vec<Entry> = r
            .corrupt_synced
            .iter()
            .map(|c| Entry {
                pack: c.pack,
                offset: c.offset,
                len: c.len,
            })
            .collect();
        entries.extend(r.missing_synced.iter().map(|&pack| Entry {
            pack,
            offset: ack::WHOLE_PACK.0,
            len: ack::WHOLE_PACK.1,
        }));
        if entries.is_empty() {
            return Ok(0);
        }
        self.sync()?;
        ack::append(&self.io, &self.dir, &entries)?;
        if !r.missing_synced.is_empty() {
            let (mark, base) = {
                let w = self.writer();
                let base = w.pack_lens().keys().next().copied().unwrap_or(0);
                (
                    Mark {
                        pack: w.id,
                        len: w.len,
                    },
                    base,
                )
            };
            self.wm
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .reset(&self.io, mark, base)?;
            let mut w = self.writer();
            w.synced = w.synced.max((mark.pack, mark.len));
        }
        // The handle keeps reporting the loss it was asked about: `recovery` is what `open` found,
        // and rewriting that would make a report disagree with the file. A reopened store reads the
        // `ACKED` file and is clean.
        Ok(entries.len())
    }

    /// Re-index every verifiable record in every pack, including damaged ones.
    ///
    /// A record is indexed when its structure, checksum and hash all check out. It replaces an
    /// index entry only when that entry cannot be read. Nothing is written to the packs.
    /// Right after an `open` that rebuilt the index there is nothing left to find.
    pub fn salvage(&self) -> Result<SalvageReport> {
        let lens = self.writer().pack_lens();
        let mut report = SalvageReport::default();
        for (&pack_id, &len) in &lens {
            let file = self.reads.get(pack_id)?;
            pack::scan(&file, PACK_HEADER_LEN, len, |event| {
                match event {
                    Event::Record {
                        offset,
                        header,
                        payload,
                    } if record::verify(header, payload) => {
                        report.records += 1;
                        let loc = Loc {
                            pack: pack_id,
                            offset: offset as u32,
                            slen: header.slen,
                            ulen: header.ulen,
                            verified: true,
                        };
                        match self.index.get(&header.id) {
                            None => {
                                self.index.insert_verified(header.id, loc);
                                report.newly_indexed += 1;
                            }
                            Some(cur)
                                if (cur.pack, cur.offset) != (pack_id, loc.offset)
                                    && !cur.verified
                                    && self.get(header.id).is_err() =>
                            {
                                self.index.replace(header.id, loc);
                                report.repaired += 1;
                            }
                            Some(_) => {}
                        }
                    }
                    Event::Record { offset, header, .. } => report.damaged.push(Gap {
                        pack: pack_id,
                        offset,
                        len: header.total_len(),
                    }),
                    Event::Gap { offset, len, .. } => report.damaged.push(Gap {
                        pack: pack_id,
                        offset,
                        len,
                    }),
                }
                Ok(())
            })?;
        }
        if report.newly_indexed + report.repaired > 0 {
            self.dirty.store(true, Relaxed);
            let mut d = self.damaged.write().unwrap_or_else(PoisonError::into_inner);
            d.retain(|id, _| self.index.get(id).is_none());
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

/// Store internals `crate::compact` needs, bundled so that module never names a private field.
#[derive(Debug)]
pub(crate) struct Guts<'a> {
    pub(crate) dir: &'a Path,
    pub(crate) io: &'a Io,
    pub(crate) index: &'a Index,
    pub(crate) wm: &'a Mutex<Wm>,
}

impl Store {
    pub(crate) fn guts(&self) -> Guts<'_> {
        Guts {
            dir: &self.dir,
            io: &self.io,
            index: &self.index,
            wm: &self.wm,
        }
    }

    /// Id of the pack `put` appends to.
    pub(crate) fn active_pack(&self) -> u32 {
        self.writer().id
    }

    /// The durable watermark: everything below it was fsynced by a completed `sync`.
    ///
    /// A collector takes it as the epoch of its cycle: a record at or above it was written after
    /// the collector froze, so no reference to it can have been seen by the mark yet.
    /// `None` before anything is synced.
    pub fn epoch(&self) -> Option<(u32, u64)> {
        let w = self.writer();
        (w.synced.1 > 0).then_some(w.synced)
    }

    /// Create a pack that `put` will not append to, for compaction to write into.
    ///
    /// The id is above every pack the store knows, and is recorded in `sealed`, so the writer's
    /// next rollover steps over it rather than refusing to create it.
    pub fn new_pack(&self) -> Result<(u32, Arc<File>)> {
        let mut w = self.writer();
        let id = next_pack_id(&w, w.pack_lens().keys().next_back().copied().unwrap_or(0))?;
        let file = Arc::new(create_pack(&self.io, &self.dir, id)?);
        w.sealed.insert(id, PACK_HEADER_LEN);
        self.counters.packs.fetch_add(1, Relaxed);
        self.counters.pack_bytes.fetch_add(PACK_HEADER_LEN, Relaxed);
        Ok((id, file))
    }

    /// Record the real length of a pack compaction just finished writing.
    pub fn finish_pack(&self, id: u32, len: u64) -> Result<()> {
        let mut w = self.writer();
        let old = w.sealed.insert(id, len).unwrap_or(0);
        self.counters
            .pack_bytes
            .fetch_add(len.saturating_sub(old), Relaxed);
        Ok(())
    }

    /// Forget a pack that no longer exists: drop it from the writer's map and recount the sizes.
    pub(crate) fn forget_pack(&self, id: u32) {
        self.writer().sealed.remove(&id);
        let mut packs = 0u64;
        let mut bytes = 0u64;
        if let Ok(entries) = fs::read_dir(pack::pack_dir(&self.dir)) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_str()
                    .and_then(pack::parse_pack_name)
                    .is_some()
                {
                    packs += 1;
                    bytes += entry.metadata().map_or(0, |m| m.len());
                }
            }
        }
        self.counters.packs.store(packs, Relaxed);
        self.counters.pack_bytes.store(bytes, Relaxed);
    }
}
