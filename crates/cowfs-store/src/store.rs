//! The block store. See `docs/v1-store.md`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, Instant};

use crate::ack::{self, Entry};
use crate::chunk::{chunks, Chunker};
use crate::error::{Error, Result};
use crate::fdcache::FdCache;
use crate::fsio::{Io, Trace};
use crate::index::{self, Index, Loc};
use crate::pack::{self, Deep, Event, PACK_HEADER_LEN};
use crate::record::{self, Codec, Header, HEADER_LEN};
use crate::types::{
    CorruptRegion, Damage, FsckReport, Gap, Options, RecoveryReport, SalvageReport, Stats,
    TornSidecar,
};
use crate::wm::{Mark, Wm};
use crate::{BlockId, ChunkRef, MAX_BLOCK_LEN, MAX_CHUNK_LEN};

const MAX_PACK_LIMIT: u64 = 1 << 31;
/// Most bytes of a torn tail kept in a `.torn-<n>` file next to the pack.
const TORN_KEEP: u64 = 1 << 20;
/// Most missing pack ids listed in a recovery report.
const MAX_LISTED: usize = 1 << 16;
/// How long `open` waits for a lock whose owner is closing before it reports the store as busy.
/// A forked child inherits the lock, so this only covers the in-process release, which is
/// sub-millisecond. It is not a fix for an inherited lock.
const LOCK_WAIT: Duration = Duration::from_millis(50);
/// How often that wait looks again.
const LOCK_POLL: Duration = Duration::from_millis(2);

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
    /// Lowest pack id this store may create. Ids below it are never reused.
    next_id: u32,
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
    index: Index,
    reads: FdCache,
    writer: Mutex<Writer>,
    wm: Mutex<Wm>,
    checkpointing: Mutex<()>,
    dirty: AtomicBool,
    /// Set once the shutdown work has run, so `close` and `Drop` never repeat it.
    finished: AtomicBool,
    max_pack_size: u64,
    checkpoint_on_drop: bool,
    counters: Counters,
    damaged: RwLock<HashMap<BlockId, (u32, u64)>>,
    /// Packs whose records the index cannot be trusted for, so a checkpoint must not record them.
    rescan: Mutex<BTreeSet<u32>>,
    recovery: RecoveryReport,
    /// The store lock, declared last so it is the last descriptor to go: every other handle the
    /// store owns is closed before the lock is released, so a reopen never races a half-closed
    /// store. `None` only between `close` taking it and the field dropping.
    _lock: Option<File>,
}

/// The pid recorded in the lock file, so a refusal can name the process that holds it.
fn lock_holder(lock: &File) -> Option<u32> {
    let len = lock.metadata().ok()?.len();
    if len == 0 || len > 24 {
        return None;
    }
    let mut b = vec![0u8; len as usize];
    lock.read_exact_at(&mut b, 0).ok()?;
    std::str::from_utf8(&b).ok()?.trim().parse().ok()
}

/// Record the owner's pid in the lock file. The write is not synced: the flock, not this text, is
/// what excludes a second writer, and the text is only there to name the holder.
fn record_holder(lock: &File) {
    let pid = std::process::id().to_string();
    let _ = lock.set_len(pid.len() as u64);
    let _ = lock.write_at(pid.as_bytes(), 0);
}

/// Write a fresh pack header and return the resulting length.
fn reset_header(io: &Io, dir: &Path, file: &mut File, id: u32) -> io::Result<u64> {
    let path = pack::pack_path(dir, id);
    io.write_at(file, &path, 0, &pack::header_bytes(pack::new_nonce(id)))?;
    io.sync_file(file, &path)?;
    Ok(PACK_HEADER_LEN)
}

/// Record how much of `id` was durable, after a torn tail was cut.
///
/// It goes in its own file, written whole and renamed into place, because rewriting any byte of a
/// sealed pack would open durable data to a torn write.
fn set_durable(io: &Io, dir: &Path, id: u32, durable: u64) -> io::Result<()> {
    let mut b = [0u8; 12];
    b[..8].copy_from_slice(&durable.to_le_bytes());
    let crc = crc32c::crc32c(&b[..8]);
    b[8..].copy_from_slice(&crc.to_le_bytes());
    io.log_whole(&pack::pack_dir(dir), &pack::cut_name(id), &b);
    io.write_whole(&pack::pack_dir(dir), &pack::cut_name(id), &b)
}

/// Write a pack header again after the old one was lost. Only the zeros go, so no durable byte of
/// a record can be touched by it.
fn rewrite_header(io: &Io, dir: &Path, id: u32) -> io::Result<u32> {
    let path = pack::pack_path(dir, id);
    let file = open_pack(dir, id)?;
    let nonce = pack::new_nonce(id);
    io.write_at(&file, &path, 0, &pack::header_bytes(nonce))?;
    io.sync_file(&file, &path)?;
    Ok(nonce)
}

/// Create pack `id`, or reuse an empty leftover from a failed earlier attempt.
/// On failure nothing is left behind, so the caller can simply try again.
fn create_pack(io: &Io, store: &Path, id: u32) -> io::Result<File> {
    let path = pack::pack_path(store, id);
    let (file, reuse) = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(f) => {
            io.created(&path);
            (f, false)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let f = open_pack(store, id)?;
            if f.metadata()?.len() > PACK_HEADER_LEN {
                return Err(e);
            }
            (f, true)
        }
        Err(e) => return Err(e),
    };
    let mut file = file;
    let mut init = || -> io::Result<u64> {
        if reuse {
            io.truncate(&file, &path, 0)?;
        }
        reset_header(io, store, &mut file, id)
    };
    let made = init().and_then(|_| io.sync_dir(&pack::pack_dir(store)));
    match made {
        Ok(()) => Ok(file),
        Err(e) => {
            let _ = fs::remove_file(&path);
            Err(e)
        }
    }
}

/// Every sidecar in the store, oldest first. A pack id is never reused, so a higher pack id is a
/// later crash, and inside a pack a higher index is a later one still.
fn torn_index(dir: &Path) -> Vec<(u32, u32, u64)> {
    let Ok(rd) = fs::read_dir(pack::pack_dir(dir)) else {
        return Vec::new();
    };
    let mut v: Vec<(u32, u32, u64)> = rd
        .filter_map(|e| {
            let e = e.ok()?;
            let n = e.file_name().to_string_lossy().into_owned();
            let (head, tail) = n.split_once(".torn-")?;
            Some((
                pack::parse_pack_name(head)?,
                tail.parse().ok()?,
                e.metadata().map_or(0, |m| m.len()),
            ))
        })
        .collect();
    v.sort_unstable();
    v
}

fn torn_path(dir: &Path, pack: u32, n: u32) -> PathBuf {
    let mut p = pack::pack_path(dir, pack).into_os_string();
    p.push(format!(".torn-{n}"));
    PathBuf::from(p)
}

/// Copy a discarded tail to a sidecar, fsync it and its directory entry, then enforce retention.
/// Fails the whole open when the sidecar cannot be written, because cutting without evidence would
/// destroy the only copy.
fn save_torn(
    io: &Io,
    dir: &Path,
    pack: u32,
    file: &File,
    tail: (u64, u64),
    options: &Options,
    recovery: &mut RecoveryReport,
) -> Result<()> {
    let (from, len) = tail;
    let n = (0u32..)
        .find(|n| !torn_path(dir, pack, *n).exists())
        .unwrap();
    let path = torn_path(dir, pack, n);
    let take = (len - from).min(TORN_KEEP);
    let mut buf = vec![0u8; take as usize];
    file.read_exact_at(&mut buf, from)?;
    let quarantine = |e: &dyn std::fmt::Display| Error::Quarantine {
        pack,
        offset: from,
        reason: e.to_string(),
    };
    let f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| quarantine(&e))?;
    io.created(&path);
    let written = io
        .write_at(&f, &path, 0, &buf)
        .and_then(|()| io.sync_file(&f, &path));
    if let Err(e) = written {
        let _ = fs::remove_file(&path);
        return Err(quarantine(&e));
    }
    io.sync_dir(&pack::pack_dir(dir))?;
    recovery.torn_sidecars.push(TornSidecar {
        pack,
        offset: from,
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        bytes: take,
    });

    // Retention is for the whole store, not for one pack, so a crash loop cannot fill the disk.
    let mut all = torn_index(dir);
    let keep = options.max_torn_sidecars.max(1);
    let mut total: u64 = all.iter().map(|(_, _, n)| *n).sum();
    while all.len() > keep || (total > options.max_torn_sidecar_bytes && all.len() > 1) {
        let (pack, i, size) = all.remove(0);
        if fs::remove_file(torn_path(dir, pack, i)).is_ok() {
            io.sync_dir(&pack::pack_dir(dir))?;
            recovery.sidecars_pruned += 1;
            total = total.saturating_sub(size);
        }
    }
    Ok(())
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
    total: u64,
    id: BlockId,
    slen: u32,
    ulen: u32,
    bytes: Vec<u8>,
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
        // `flock` lives on the open file description, so a bare `fork` keeps the store locked until
        // the child exits. `O_CLOEXEC` does not help that, so the wait below only covers the
        // in-process release, and the holder's pid is reported so the cause is visible.
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(
                        LOCK_POLL.min(deadline.saturating_duration_since(Instant::now())),
                    );
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(Error::Locked {
                        dir,
                        holder: lock_holder(&lock),
                    });
                }
                Err(TryLockError::Error(e)) => return Err(e.into()),
            }
        }
        record_holder(&lock);
        let max_pack_size = options.max_pack_size.clamp(1, MAX_PACK_LIMIT);

        let mut ids = Vec::new();
        for entry in fs::read_dir(pack::pack_dir(&dir))? {
            if let Some(id) = entry?.file_name().to_str().and_then(pack::parse_pack_name) {
                ids.push(id);
            }
        }
        ids.sort_unstable();
        let mut lens: BTreeMap<u32, u64> = BTreeMap::new();
        let mut nonces: HashMap<u32, u32> = HashMap::new();
        let mut head_lost: Vec<u32> = Vec::new();
        let mut cuts: Vec<CorruptRegion> = Vec::new();
        let mut sealed_len: HashMap<u32, u64> = HashMap::new();
        let mut wm = Wm::open(&io, &dir)?;
        for &id in &ids {
            let path = pack::pack_path(&dir, id);
            let mut file = open_pack(&dir, id)?;
            let mut len = fs::metadata(&path)?.len();
            if len < PACK_HEADER_LEN {
                if Some(id) != ids.last().copied() {
                    return Err(Error::BadPack {
                        path,
                        reason: "truncated header",
                    });
                }
                // A crash between extending a pack file and writing its header leaves a short or
                // zero-filled file. It never held data, so it is reset like a fresh pack.
                if wm
                    .mark()
                    .is_some_and(|m| m.pack >= id && m.len > PACK_HEADER_LEN)
                {
                    ack::save(
                        &io,
                        &dir,
                        vec![Entry {
                            pack: id,
                            nonce: 0,
                            state: ack::State::Pending,
                            offset: 0,
                            len: PACK_HEADER_LEN,
                            id: None,
                        }],
                    )?;
                }
                io.truncate(&file, &path, 0)?;
                len = reset_header(&io, &dir, &mut file, id)?;
            } else {
                match pack::header_check(&file)? {
                    Ok(nonce) => {
                        nonces.insert(id, nonce);
                    }
                    Err("empty pack (no data was written)") => {
                        // The header sector was lost: a crash between creating the file and writing
                        // it, or a torn write to sector zero. Records past it are still scanned, so
                        // whatever verifies is kept. The header is written again after the scan.
                        head_lost.push(id);
                    }
                    Err(_) => {
                        // A torn or rotted header is not fatal: the records past it are still
                        // scanned and the header is written again afterwards.
                        head_lost.push(id);
                    }
                }
            }
            if len > u64::from(u32::MAX) {
                return Err(Error::BadPack {
                    path,
                    reason: "pack larger than 4 GiB",
                });
            }
            lens.insert(id, len);
        }
        for &id in &ids {
            if let Some(n) = pack::cut_len(&dir, id) {
                sealed_len.insert(id, n);
            }
        }
        let last = ids.last().copied();

        let mark = wm.mark();
        let mut acked = ack::load(&dir);
        let table = ack::Table::new(&acked);
        // Packs an operator accepted as wholly lost, so a file that reappears at one of those ids
        // is a restore rather than a torn tail. A pending entry does not count: the loss is still
        // being reported, and that report is what makes a restore visible.
        let known: HashSet<u32> = acked
            .iter()
            .filter(|e| {
                e.state.accepted() && e.offset == ack::WHOLE_PACK.0 && e.len == ack::WHOLE_PACK.1
            })
            .map(|e| e.pack)
            .collect();
        // Nothing is reported missing above the highest id that a file or an acceptance names.
        let mut recovery = RecoveryReport {
            watermark_missing: mark.is_none() && !ids.is_empty(),
            ..RecoveryReport::default()
        };
        // The pack `put` appends to is the highest one, which is not always the one the watermark
        // names: a rollover creates the next pack and appends to it before any `sync` runs.
        let active = ids.last().copied();
        // How much of a pack a completed `sync` promised, and so how much damage is corruption and
        // how much is a torn tail. A cut label only speaks for a pack the watermark has moved past.
        //
        // A mark that fell back to an older slot, or that is behind a pack the store already knows
        // about, cannot speak for the bytes above it, so they are unclassifiable rather than a torn
        // tail. A pack above the mark with no cut label and no record of its own is one the
        // operator restored, so it gets the same treatment.
        let durable = |id: u32| -> u64 {
            let sealed = sealed_len.get(&id).copied();
            match mark {
                Some(m) if m.pack == id => m.len,
                Some(m) if m.pack > id => sealed.unwrap_or(u64::MAX),
                Some(_) => sealed.unwrap_or_else(|| {
                    if wm.uncertain() || known.contains(&id) {
                        u64::MAX
                    } else {
                        0
                    }
                }),
                None if Some(id) == active => PACK_HEADER_LEN,
                None => sealed.unwrap_or(u64::MAX),
            }
        };
        // A pack id is never reused, so every id above the high-water is free and everything at or
        // below it is either a pack we know or a loss we already reported.
        let mut next_id = wm
            .next_id()
            .max(last.map_or(0, |l| l.saturating_add(1)))
            .max(acked.iter().map(|e| e.pack).max().map_or(0, |p| p + 1));
        if mark.is_some() || !ids.is_empty() {
            let lo = if mark.is_some() {
                wm.base()
            } else {
                ids.first().copied().unwrap_or(0)
            };
            // Only the mark promises a pack, and the high-water says how far the store ever
            // allocated, so the listing covers every id the store could have promised a file for.
            // An id the high-water passed but that no file or acceptance names was reserved and
            // never created, which is not a loss.
            let hi = mark
                .map_or_else(|| last.unwrap_or(0), |m| m.pack)
                .min(wm.next_id().saturating_sub(1))
                .max(lo);
            recovery.missing_synced = (lo..=hi)
                .filter(|_| mark.is_some() || wm.next_id() == 0)
                .filter(|p| !lens.contains_key(p))
                .filter(|&p| !ack::covers_pack(&acked, p))
                .take(MAX_LISTED)
                .collect();
        }
        for p in &recovery.missing_synced {
            next_id = next_id.max(p.saturating_add(1));
        }

        let index = Index::new();
        let mut starts: HashMap<u32, u64> = HashMap::new();
        if let Some(ck) = index::load(&dir) {
            let scanned: HashMap<u32, (u64, u32)> =
                ck.packs.iter().map(|p| (p.0, (p.1, p.2))).collect();
            // The pack must exist, be at least as long as the checkpoint says, and carry the same
            // creation nonce, so a stale checkpoint cannot validate against a different pack.
            let packs_ok = scanned.iter().all(|(id, &(n, nonce))| {
                n >= PACK_HEADER_LEN
                    && lens.get(id).is_some_and(|&l| l >= n)
                    && nonces.get(id).is_some_and(|&w| w == nonce || w == 0)
            });
            let entries_ok = ck.entries.iter().all(|(_, loc)| {
                locate_ok(loc)
                    && scanned.get(&loc.pack).is_some_and(|&(n, _)| {
                        u64::from(loc.offset) >= PACK_HEADER_LEN
                            && u64::from(loc.offset) + HEADER_LEN as u64 + u64::from(loc.slen) <= n
                    })
            });
            // A watermark that fell back to an older slot means the checkpoint may name bytes the
            // store cannot vouch for any more, so the packs are read again instead.
            if packs_ok && entries_ok && !wm.uncertain() {
                for (id, loc) in ck.entries {
                    index.insert_if_absent(id, loc);
                }
                starts = scanned.into_iter().map(|(id, (n, _))| (id, n)).collect();
                recovery.index_loaded = true;
            }
        }

        let mut regions: Vec<CorruptRegion> = Vec::new();
        let mut rescan = BTreeSet::new();
        let mut last_file: Option<File> = None;
        let mut last_damaged = false;
        let mut relocated: Vec<Tail> = Vec::new();
        let mut cut_at: Option<(u32, u64)> = None;
        let mut have_tail = false;
        for &id in &ids {
            let path = pack::pack_path(&dir, id);
            let file = open_pack(&dir, id)?;
            let mut len = lens.get(&id).copied().unwrap_or(0);
            let is_active = Some(id) == active;
            let dur = durable(id);
            if dur != u64::MAX && len < dur {
                regions.push(CorruptRegion {
                    pack: id,
                    offset: len,
                    len: dur - len,
                    id: None,
                    unclassified: false,
                });
                rescan.insert(id);
                last_damaged |= is_active;
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
                                // The whole record, header included, so it can be written to a
                                // new pack byte for byte.
                                let mut bytes = vec![0u8; header.total_len() as usize];
                                file.read_exact_at(&mut bytes, offset)?;
                                tail.push(Tail {
                                    total: header.total_len(),
                                    id: header.id,
                                    slen: header.slen,
                                    ulen: header.ulen,
                                    bytes,
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
                // Bytes at or past the durable length were never promised, so they are a torn
                // tail to be cut, not corruption. That holds for the active pack past the
                // watermark, and for a retired pack whose cut was interrupted.
                if dur != u64::MAX && offset >= dur && !exhausted && torn_from.is_none() {
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
                last_damaged |= is_active;
                regions.push(CorruptRegion {
                    pack: id,
                    offset: b.offset,
                    len: if b.offset < dur {
                        b.len.min(dur - b.offset)
                    } else {
                        b.len
                    },
                    id: b.id,
                    // Damage the watermark cannot vouch for is a loss of unknown promise, not a
                    // broken promise, and is reported as such until somebody accepts it.
                    unclassified: dur == u64::MAX || wm.uncertain(),
                });
            }
            if let Some(t) = torn_from {
                // Anything the watermark cannot speak for is unclassifiable, so the loss is
                // recorded and made durable before the bytes are cut.
                if dur == u64::MAX || recovery.watermark_missing || wm.uncertain() {
                    let cut = CorruptRegion {
                        pack: id,
                        offset: t,
                        len: len - t,
                        id: None,
                        unclassified: true,
                    };
                    if !ack::find(&table, id, 0, t, len - t, None)
                        .is_some_and(|e| e.state.accepted())
                    {
                        ack::save(
                            &io,
                            &dir,
                            vec![Entry {
                                pack: id,
                                nonce: 0,
                                state: ack::State::Unclassified,
                                offset: t,
                                len: len - t,
                                id: None,
                            }],
                        )?;
                        cuts.push(cut);
                    }
                }
                save_torn(&io, &dir, id, &file, (t, len), &options, &mut recovery)?;
                // The label is what lets a later open tell a torn tail from corruption once this
                // pack is no longer the active one, and what lets an interrupted cut be finished.
                set_durable(&io, &dir, id, t)?;
                if tail.is_empty() {
                    // Nothing verifiable past the tear, so it goes now.
                    io.truncate(&file, &path, t)?;
                } else {
                    // Verifiable records sit past the tear. Copy them into a new pack and fsync it
                    // there first, so this pack never loses a byte in the middle of a move. A pack
                    // that was already retired can land here too: a crash between labelling it and
                    // moving its records leaves the records here and nowhere else.
                    relocated = tail;
                    have_tail = true;
                    cut_at = Some((id, t));
                }
                recovery.torn_tail_discarded += len - t;
                len = t;
            }
            if scanned > 0 || torn_from.is_some() {
                io.sync_file(&file, &path)?;
            }
            lens.insert(id, len);
            if is_active {
                last_file = Some(file);
            }
        }
        recovery.truncated_bytes = recovery.torn_tail_discarded;
        let counters = Counters::default();
        let (id, file, mut len) = match (last, last_file) {
            (Some(id), Some(f))
                if !last_damaged
                    && !have_tail
                    && lens.get(&id).copied().unwrap_or(0) < max_pack_size =>
            {
                let len = lens.get(&id).copied().unwrap_or(PACK_HEADER_LEN);
                (id, Arc::new(f), len)
            }
            _ => {
                let id = next_id;
                next_id = id
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("pack ids exhausted"))?;
                wm.raise_next(next_id)?;
                let file = Arc::new(create_pack(&io, &dir, id)?);
                lens.insert(id, PACK_HEADER_LEN);
                (id, file, PACK_HEADER_LEN)
            }
        };
        if have_tail {
            // The recovered records go into the new pack, in order, and the writer starts after them.
            let path = pack::pack_path(&dir, id);
            for r in &relocated {
                io.write_at(&file, &path, len, &r.bytes)?;
                index.insert_verified(
                    r.id,
                    Loc {
                        pack: id,
                        offset: len as u32,
                        slen: r.slen,
                        ulen: r.ulen,
                        verified: true,
                    },
                );
                len += r.total;
            }
            io.sync_file(&file, &path)?;
            // The recovered records are durable in the new pack, so the old pack may now be cut.
            if let Some((old, at)) = cut_at {
                let old_path = pack::pack_path(&dir, old);
                let f = open_pack(&dir, old)?;
                io.truncate(&f, &old_path, at)?;
                io.sync_file(&f, &old_path)?;
            }
            recovery.recovered_from_tail += relocated.len() as u64;
            recovery.records_scanned += relocated.len() as u64;
            lens.insert(id, len);
        }

        let mut sealed = lens.clone();
        sealed.remove(&id);
        counters.packs.store(lens.len() as u64, Relaxed);
        counters.pack_bytes.store(lens.values().sum(), Relaxed);

        // Damage in durable bytes stays reported until a caller accepts it: a pending entry is
        // written now, so a later open still knows the bytes are gone.
        let mut damaged: HashMap<BlockId, (u32, u64)> = HashMap::new();
        let mut fresh: Vec<Entry> = Vec::new();
        let mut still_pending = Vec::new();
        for r in regions {
            if let Some(x) = r.id {
                damaged.insert(x, (r.pack, r.offset));
            }
            let nonce = nonces.get(&r.pack).copied().unwrap_or(0);
            if ack::find(&table, r.pack, nonce, r.offset, r.len, r.id)
                .is_some_and(|e| e.state.accepted())
            {
                recovery.acknowledged.push(r);
                continue;
            }
            let elsewhere = |x: BlockId| {
                index.get(&x).is_some_and(|l| {
                    let at = u64::from(l.offset);
                    if l.pack == r.pack && at >= r.offset && at < r.offset + r.len {
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
                // The block has a verified copy elsewhere, so the region is repaired, not lost.
                // It is recomputed on every open, so no entry is written.
                recovery.superseded.push(r);
                continue;
            }
            fresh.push(Entry {
                pack: r.pack,
                nonce,
                state: if r.unclassified {
                    ack::State::Unclassified
                } else {
                    ack::State::Pending
                },
                offset: r.offset,
                len: r.len,
                id: r.id,
            });
            still_pending.push(r);
        }
        for p in &recovery.missing_synced {
            fresh.push(Entry {
                pack: *p,
                nonce: 0,
                state: ack::State::Pending,
                offset: ack::WHOLE_PACK.0,
                len: ack::WHOLE_PACK.1,
                id: None,
            });
        }
        for c in &cuts {
            fresh.push(Entry {
                pack: c.pack,
                nonce: 0,
                state: ack::State::Unclassified,
                offset: c.offset,
                len: c.len,
                id: None,
            });
            still_pending.push(*c);
        }
        // A pending entry from an earlier open whose region no longer shows up is still a loss.
        for e in &acked {
            if e.state.accepted() {
                continue;
            }
            let found = still_pending
                .iter()
                .any(|r| r.pack == e.pack && r.offset == e.offset && r.len == e.len)
                || recovery.missing_synced.contains(&e.pack);
            if e.len == ack::WHOLE_PACK.1 {
                // A missing pack is reported as missing, not as a corrupt region.
                continue;
            }
            if ack::find(&table, e.pack, e.nonce, e.offset, e.len, e.id)
                .is_some_and(|a| a.state.accepted())
            {
                // A newer entry accepted this exact region.
                continue;
            }
            let repaired = e.id.is_some_and(|id| {
                index.get(&id).is_some_and(|l| {
                    !l.verified || {
                        let at = u64::from(l.offset);
                        l.pack != e.pack || at < e.offset || at >= e.offset + e.len
                    }
                })
            });
            if !found && !repaired {
                still_pending.push(CorruptRegion {
                    pack: e.pack,
                    offset: e.offset,
                    len: e.len,
                    id: e.id,
                    unclassified: e.state == ack::State::Unclassified,
                });
            }
        }
        recovery.corrupt_synced = still_pending;
        if !fresh.is_empty() {
            ack::save(&io, &dir, fresh)?;
            acked.extend(ack::load(&dir));
        }
        for &id in &head_lost {
            let nonce = rewrite_header(&io, &dir, id)?;
            nonces.insert(id, nonce);
        }

        // Every byte scanned above was fsynced and any torn tail is gone, so nothing unresolved
        // sits below this mark. A mark that is already ahead of the packs is left alone.
        let here = Mark { pack: id, len };
        let first = lens.keys().next().copied().unwrap_or(id);
        let reserved_start = mark.map_or(first, |m| m.pack.saturating_add(1));
        let unused: Vec<Entry> = (reserved_start..id)
            .filter(|p| !lens.contains_key(p) && !recovery.missing_synced.contains(p))
            .map(|pack| Entry {
                pack,
                nonce: 0,
                state: ack::State::Acked,
                offset: ack::WHOLE_PACK.0,
                len: ack::WHOLE_PACK.1,
                id: None,
            })
            .collect();
        if !unused.is_empty() {
            ack::save(&io, &dir, unused)?;
        }
        let base = recovery
            .missing_synced
            .iter()
            .filter(|p| **p < next_id)
            .min()
            .copied()
            .or_else(|| wm.base().checked_add(0))
            .unwrap_or(0);
        if wm.mark().is_none() {
            if first == wm.base() {
                wm.init(here)?;
            } else {
                wm.reset(here, first, next_id)?;
                wm.reset(here, first, next_id)?;
            }
        } else if !recovery.missing_synced.is_empty() {
            wm.reset(here, base, next_id)?;
        } else {
            wm.advance(here)?;
            wm.raise_next(next_id)?;
        }
        let synced = wm
            .mark()
            .filter(|m| *m <= here)
            .map_or((0, 0), |m| (m.pack, m.len));

        Ok(Store {
            dir: dir.clone(),
            reads: FdCache::new(dir, options.max_open_packs),
            io,
            index,
            writer: Mutex::new(Writer {
                id,
                file,
                len,
                sealed,
                synced,
                next_id,
            }),
            wm: Mutex::new(wm),
            checkpointing: Mutex::new(()),
            dirty: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            max_pack_size,
            checkpoint_on_drop: options.checkpoint_on_drop,
            counters,
            damaged: RwLock::new(damaged),
            rescan: Mutex::new(rescan),
            recovery,
            _lock: Some(lock),
        })
    }

    /// Creation nonce of every pack, read from its header.
    fn nonces(&self) -> HashMap<u32, u32> {
        let mut out = HashMap::new();
        let Ok(rd) = fs::read_dir(pack::pack_dir(&self.dir)) else {
            return out;
        };
        for e in rd.flatten() {
            let Some(id) = e.file_name().to_str().and_then(pack::parse_pack_name) else {
                continue;
            };
            if let Ok(f) = File::open(e.path()) {
                if let Ok(Ok(n)) = pack::header_check(&f) {
                    out.insert(id, n);
                }
            }
        }
        out
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join(index::FILE_NAME)
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
        if let Err(e) = self
            .io
            .write_at(&w.file, &pack::pack_path(&self.dir, w.id), w.len, &rec)
        {
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

    /// The id of a pack this store may create: above every pack it knows and never reused.
    fn alloc_id(&self, w: &mut Writer) -> Result<u32> {
        let above = w.pack_lens().keys().next_back().copied().unwrap_or(0);
        let id = w.next_id.max(above.saturating_add(1));
        let next = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("pack ids exhausted"))?;
        let mut wm = self.wm.lock().unwrap_or_else(PoisonError::into_inner);
        let start = above
            .saturating_add(1)
            .max(wm.mark().map_or(0, |m| m.pack.saturating_add(1)));
        let unused = (start..id)
            .map(|pack| Entry {
                pack,
                nonce: 0,
                state: ack::State::Acked,
                offset: ack::WHOLE_PACK.0,
                len: ack::WHOLE_PACK.1,
                id: None,
            })
            .collect::<Vec<_>>();
        if !unused.is_empty() {
            ack::save(&self.io, &self.dir, unused)?;
        }
        wm.raise_next(next)?;
        w.next_id = next;
        Ok(id)
    }

    fn roll(&self, w: &mut Writer) -> Result<()> {
        self.io
            .sync_file(&w.file, &pack::pack_path(&self.dir, w.id))?;
        let id = self.alloc_id(w)?;
        let file = Arc::new(create_pack(&self.io, &self.dir, id)?);
        w.sealed.insert(w.id, w.len);
        w.id = id;
        w.next_id = id.saturating_add(1);
        w.file = file;
        w.len = PACK_HEADER_LEN;
        self.counters.packs.fetch_add(1, Relaxed);
        self.counters.pack_bytes.fetch_add(PACK_HEADER_LEN, Relaxed);
        Ok(())
    }

    /// Create a pack above every other one, for compaction to write into.
    ///
    /// It comes from the same allocator as a rollover, so the writer never collides with it, and the
    /// id is recorded as used, so a later roll steps over it instead of reusing it.
    pub fn new_pack(&self) -> Result<(u32, Arc<File>)> {
        let mut w = self.writer();
        let id = self.alloc_id(&mut w)?;
        let file = Arc::new(create_pack(&self.io, &self.dir, id)?);
        w.sealed.insert(id, PACK_HEADER_LEN);
        w.next_id = id.saturating_add(1);
        self.counters.packs.fetch_add(1, Relaxed);
        self.counters.pack_bytes.fetch_add(PACK_HEADER_LEN, Relaxed);
        Ok((id, file))
    }

    /// Record the real length of a pack written by [`Store::new_pack`].
    pub fn finish_pack(&self, id: u32, len: u64) -> Result<()> {
        let mut w = self.writer();
        let old = w.sealed.insert(id, len).unwrap_or(0);
        self.counters
            .pack_bytes
            .fetch_add(len.saturating_sub(old), Relaxed);
        Ok(())
    }

    /// Id of the pack `put` appends to.
    pub fn active_pack(&self) -> u32 {
        self.writer().id
    }

    /// The durable watermark, for a collector to use as the epoch of its cycle.
    pub fn epoch(&self) -> Option<(u32, u64)> {
        let w = self.writer();
        (w.synced.1 > 0).then_some(w.synced)
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
        let short = |e: io::Error| match e.kind() {
            io::ErrorKind::UnexpectedEof => corrupt("record extends past end of pack"),
            _ => Error::Io(e),
        };
        // The header is read on its own so each payload byte is read once, straight into the buffer
        // it is returned in. Reading the whole record first and draining the header off the front
        // moved every byte of every block a second time.
        let mut raw = [0u8; HEADER_LEN];
        file.read_exact_at(&mut raw, u64::from(loc.offset))
            .map_err(short)?;
        let header = Header::parse(&raw).map_err(corrupt)?;
        if header.id != id || header.slen != loc.slen || header.ulen != loc.ulen {
            return Err(corrupt("record does not match index"));
        }
        let body = u64::from(loc.offset) + HEADER_LEN as u64;
        let data = match header.codec {
            Codec::Raw => {
                let mut v = vec![0u8; header.ulen as usize];
                file.read_exact_at(&mut v, body).map_err(short)?;
                if Header::expected_crc(&raw, &v) != header.crc {
                    return Err(corrupt("checksum mismatch"));
                }
                v
            }
            Codec::Zstd => {
                let mut payload = vec![0u8; header.slen as usize];
                file.read_exact_at(&mut payload, body).map_err(short)?;
                if Header::expected_crc(&raw, &payload) != header.crc {
                    return Err(corrupt("checksum mismatch"));
                }
                record::decode(&header, &payload).map_err(corrupt)?
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
                .advance(Mark { pack: id, len })?;
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
        let nonces = self.nonces();
        let rescan = self.rescan.lock().unwrap_or_else(PoisonError::into_inner);
        let packs: Vec<(u32, u64, u32)> = lens
            .iter()
            .map(|(&id, &n)| {
                let n = if rescan.contains(&id) {
                    PACK_HEADER_LEN
                } else {
                    n
                };
                (id, n, nonces.get(&id).copied().unwrap_or(0))
            })
            .collect();
        let entries = self.index.snapshot(|loc| {
            !rescan.contains(&loc.pack)
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

    /// Blocks that `open` found damaged in this session, for a caller that wants to put them again.
    ///
    /// A block that has been put again is not in the list, so a caller can work through it until
    /// the list is empty.
    pub fn damaged_blocks(&self) -> Vec<BlockId> {
        let d = self.damaged.read().unwrap_or_else(PoisonError::into_inner);
        d.keys().copied().collect()
    }

    /// Accept the damage `open` reported, so later opens stop reporting it.
    ///
    /// Every region in `recovery().corrupt_synced` and every missing pack is recorded in `ACKED`
    /// as accepted, and the index checkpoint is dropped, because a pack may be gone and its id is
    /// never reused. The damaged bytes stay on disk and the blocks stay unreadable until they are
    /// put again. Returns the number of entries recorded.
    pub fn acknowledge_corruption(&self) -> Result<usize> {
        let r = &self.recovery;
        let nonces = self.nonces();
        let mut entries: Vec<Entry> = r
            .corrupt_synced
            .iter()
            .map(|c| Entry {
                pack: c.pack,
                // The real creation nonce and the real claimed id, so the entry cannot go on
                // hiding damage in a different pack that reappears with the same id.
                nonce: nonces.get(&c.pack).copied().unwrap_or(0),
                state: ack::State::Acked,
                offset: c.offset,
                len: c.len,
                id: c.id,
            })
            .collect();
        // A whole-pack entry outlives the file it described, so it is only written while the pack
        // is really gone. A pack that is present is reported as corruption, not as a lost pack.
        entries.extend(
            r.missing_synced
                .iter()
                .filter(|&&pack| !pack::pack_path(&self.dir, pack).exists())
                .map(|&pack| Entry {
                    pack,
                    nonce: 0,
                    state: ack::State::Acked,
                    offset: ack::WHOLE_PACK.0,
                    len: ack::WHOLE_PACK.1,
                    id: None,
                }),
        );
        if entries.is_empty() {
            return Ok(0);
        }
        self.sync()?;
        let n = entries.len();
        ack::save(&self.io, &self.dir, entries)?;
        // A stale checkpoint can name a pack that no longer exists, so it must not be trusted.
        if self.index_path().exists() {
            fs::remove_file(self.index_path())?;
            self.io.sync_dir(&self.dir)?;
        }
        // A pack that is on disk again was never a lost pack, so its acceptance is not recorded and
        // the watermark base is only lowered for packs that are really gone.
        if r.missing_synced
            .iter()
            .any(|&p| !pack::pack_path(&self.dir, p).exists())
        {
            let (mark, base, next) = {
                let w = self.writer();
                let base = r
                    .missing_synced
                    .iter()
                    .copied()
                    .min()
                    .unwrap_or_else(|| w.pack_lens().keys().next().copied().unwrap_or(0));
                (
                    Mark {
                        pack: w.id,
                        len: w.len,
                    },
                    base,
                    w.next_id,
                )
            };
            self.wm
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .reset(mark, base, next)?;
            let mut w = self.writer();
            w.synced = w.synced.max((mark.pack, mark.len));
        }
        Ok(n)
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
            pack::scan_deep(&file, len, |event| {
                match event {
                    Deep::Record { offset, header } => {
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
                    Deep::Gap { offset, len } => report.damaged.push(Gap {
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
            // Every pack was read from end to end and every record that verified is now indexed,
            // so a checkpoint can record them and a later open does not have to rescan.
            self.rescan
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clear();
        }
        Ok(report)
    }
}

impl Store {
    /// Do the shutdown work once, under the lock, and report it.
    ///
    /// `close` and `Drop` both call this. It runs at most once, so nothing is ever written after
    /// the lock has been released.
    fn finish(&self) -> Result<()> {
        if self.finished.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        if self.checkpoint_on_drop && self.dirty.load(Relaxed) {
            self.checkpoint()?;
        }
        Ok(())
    }

    /// Release the lock now. `close` calls it; a drop leaves it to the field, which drops last.
    fn release_lock(&mut self) -> io::Result<()> {
        match self._lock.take() {
            Some(f) => f.unlock(),
            None => Ok(()),
        }
    }

    /// Flush, release the lock and report anything that went wrong.
    ///
    /// A drop does the same work and cannot report it, so a caller that has to know should call
    /// this. It is the only way to finish a store whose drop would have complained.
    pub fn close(mut self) -> Result<()> {
        let flushed = self.finish();
        let released = self.release_lock();
        flushed.and(released.map_err(Error::from))
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.finish();
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
