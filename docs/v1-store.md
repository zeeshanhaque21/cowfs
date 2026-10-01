# v1 block store (`cowfs-store`)

Issue: #7.
Contract: `docs/v1-architecture.md`, section "cowfs-store contract".
This note records the byte layout, the index choice, the durability and concurrency model, the recovery algorithm and the API.
It does not change any settled decision in `docs/design.md`.
The target platforms are Linux and macOS.
The crate uses positional file I/O (`std::os::unix::fs::FileExt`), so it does not build on Windows, which is out of v1.
The crate declares `rust-version = "1.89"` because `File::try_lock`, used for `LOCK`, was stabilized in 1.89.
Only the declared field enforces this: the crate was not built with a 1.89 toolchain, and its tests use newer std APIs.

## Layout on disk

```
<store>/
  LOCK                  advisory lock, held while a Store is open
  SYNCED                durable watermark, pack base and pack id high-water, two CRC-protected 32 byte slots
  ACKED                 optional list of pending and accepted losses (see "Repair and acknowledgement")
  index.cix             optional index checkpoint (rebuildable, see below)
  packs/pack-00000000.cpk
  packs/pack-00000000.cpk.cut     optional durable length of a pack whose torn tail was cut
  packs/pack-00000000.cpk.torn-0  optional preserved bytes of a discarded torn tail
  packs/pack-00000001.cpk
  ...
```

All integers are little endian.

### Pack file

A pack starts with a 16 byte header, followed by records with no padding and no footer.

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | magic `COWPACK\0` |
| 8 | 4 | format version, `2` |
| 12 | 4 | creation nonce, never 0 |

The nonce is written once, into a file that is empty at the time, and never rewritten.
It is what binds a checkpoint to one specific pack: a stale checkpoint that names pack `n` cannot
validate against a different pack that happens to have that id.
It is not a secret and not integrity protected; flipping it does not lose data, it only makes the
checkpoint stale.
Packs written by the version that had a zero there are read unchanged, and a checkpoint made by
that version carries nonce 0, so both keep working.

The durable length of a cut pack is kept beside the pack in `<pack>.cut` (12 bytes: the length and
its CRC), written whole and renamed into place.
Rewriting any byte of a sealed pack would open durable data to a torn write, so the label is a
separate file.

### Record

Every record has a fixed 56 byte header followed by `stored_len` payload bytes.

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | magic `CWRB` |
| 4 | 1 | codec: `0` raw, `1` zstd |
| 5 | 3 | zero |
| 8 | 4 | `uncompressed_len`, at most 262144 |
| 12 | 4 | `stored_len` |
| 16 | 32 | `BlockId`, BLAKE3-256 of the uncompressed bytes |
| 48 | 4 | header CRC32C over bytes 0..48 |
| 52 | 4 | record CRC32C over bytes 0..48 followed by the payload |
| 56 | n | payload |

Validity rules: codec 0 needs `stored_len == uncompressed_len`.
Codec 1 needs `stored_len < uncompressed_len`.
The zero bytes must be zero.
A record that breaks a rule, or whose header CRC or record CRC fails, is invalid.
The header CRC lets a scanner reject a fake or damaged header without reading its payload.
The zero length block (empty data) is a valid raw record.

A record is self-describing, so a pack can be parsed with no other file.
The format has no in-place mutation, no back pointers and no pack-level footer.
Compaction (#10) can therefore copy live records into a new pack id and delete the old pack.
Format version 2 added the header CRC.
A pack with another version is refused at open with a clear error (`unsupported pack format version`): version 1 was never released, so there is no reader for it.
The `index.cix` checkpoint must be dropped or rewritten by the compaction step, because it names pack ids and offsets.

## Compression and hashing

- FastCDC v2020 (crate `fastcdc` 5), min 16 KiB, average 64 KiB, max 256 KiB, normalization level 1, default gear table and seed 0.
  The chunker only reads bytes inside the current chunk's first 256 KiB, so boundaries do not depend on read buffering.
- The id is BLAKE3-256 of the uncompressed bytes.
- Payload is zstd level 3.
  It is stored raw unless the compressed size is at most 95% of the input.
- `get` checks the CRC, decompresses, checks the decoded length, then re-hashes and compares with the requested id.
  Any failure is an error and no data is returned.

## Index

The in-memory index is a sharded hash map from `BlockId` to `(pack, offset, stored_len, uncompressed_len)`.
It costs about 16 bytes of location per block plus the map overhead, which is fine for millions of blocks.

Packs are the only source of truth.
The index is a cache that is rebuilt by scanning packs whenever it is missing or wrong.

Choice of index persistence: a single checkpoint file, not redb.
- The store needs point lookups only, so an embedded transactional KV adds a large dependency for no gain.
- A KV write on every new block would sit on the ingest hot path.
- The checkpoint is written only by `checkpoint()` and on drop, and costs one sequential write.
- Losing or corrupting it costs one scan of the packs and never data.

`index.cix` layout:

| Field | Size |
|---|---|
| magic `COWIDX01` | 8 |
| pack count | 4 |
| entry count | 8 |
| per pack: pack id (4), indexed length (8) | 12 each |
| per entry: id (32), pack (4), offset (4), stored_len (4), uncompressed_len (4) | 48 each |
| CRC32C of everything above | 4 |

It is written to a temporary file, synced, renamed over `index.cix`, and the directory is synced.
A checkpoint is taken only after the packs it describes are synced, so every entry names durable bytes.

## Durability

- `put` writes the record with one positional write and returns.
  The data is in the OS page cache, is visible to every reader in the process, and is not yet durable.
- `sync` fsyncs the active pack, and only then advances the watermark in `SYNCED` and fsyncs that file.
  A pack is fsynced before the next one is created, so a sealed pack is always durable.
  After `sync` returns, every `put` that returned earlier is durable.
  A `sync` with nothing new since the last one does no I/O.
- Rolling to a new pack is transactional.
  If creating the next pack fails (for example `EMFILE`), the half-made file is removed, no counter or writer state changes, and the error is returned.
  A later `put` retries and succeeds once resources return.
  An empty leftover pack file at the next id is reused, and one that holds data is never overwritten (the roll fails instead).
- Creating a store or a pack fsyncs the new file and every directory entry that names it: the store directory and its parent, `packs/`, `LOCK`, `SYNCED`, and each pack.
  Renaming `index.cix` into place is followed by an fsync of the store directory.
- A crash may lose puts after the last `sync`.
  It never serves a torn record, because every record is verified by CRC on scan and by CRC plus BLAKE3 on read.
- `checkpoint` syncs, then writes `index.cix` (tmp file, fsync, rename, directory fsync).
  Drop calls it when the store was written to since the last checkpoint, unless `Options::checkpoint_on_drop` is false.
  A failed checkpoint on drop is ignored, which is harmless: the packs are the truth.

### The watermark

`SYNCED` holds `(pack, len)`, `base`, the lowest pack id that must exist, and `next`, the lowest
pack id this store may create.
Missing packs between `base` and the highest known pack are reported as `missing_synced`.
`next` is what makes a pack id permanent: it never falls, it is fsynced before the pack file that
uses the id is created, and it is raised again whenever the watermark advances.
So an id that has ever been used is never handed out again, not after a pack is deleted, not after
an acknowledgement, and not after a loss.
The trade is that the pack files alone are not enough to tell a deleted pack from one that never
existed: if `SYNCED` is lost, the allocator falls back to "one above the highest pack on disk" and
can reuse an id. That case is covered by the pack nonce, which stops a stale checkpoint from
validating against the recreated pack.
`(pack, len)` means: everything in packs below `pack`, and the first `len` bytes of pack `pack`, were fsynced before this value was written.
It exists so that recovery can tell a torn tail (bytes after the watermark, never promised durable) from corruption of synced data (bytes before it).
It has two 32 byte slots written alternately, each with a sequence number and a CRC, so a torn write of one slot leaves the other.

Ordering, enforced by tests through the fsync trace seam:

1. fsync the pack data.
2. Write the new watermark and fsync `SYNCED`.

A crash between 1 and 2 leaves the watermark lower than the truth.
That is safe: the extra bytes are treated as tail, and open indexes every valid record in them and only cuts what does not verify.
A watermark higher than the truth is impossible unless a disk lies.
If a pack is shorter than the watermark says, open reports that as corruption.
A missing or unreadable `SYNCED` next to existing packs is reported (`watermark_missing`), and the last pack is then treated as unclassifiable: only its header was promised durable.
Any damage in it is reported, and because the store cannot say whether those bytes were ever durable, cutting them is recorded as a pending loss that stays reported until somebody accepts it.
Open never advances the watermark over a region that is not resolved (cut or verified), and never lowers it.
When the watermark names a pack that is not on disk, open reports `missing_synced` and `sync` keeps fsyncing whatever it appends.

## Concurrency

- Any number of threads may call any method on a shared `&Store`.
- Hashing and compression run outside every lock.
- Reads take a shard read lock, then use `pread` on a shared file handle.
  Reads never block on writers.
- Read handles come from a bounded LRU cache (`Options::max_open_packs`, default 64), so a store with thousands of packs needs few descriptors.
  The writer holds one extra handle for the active pack.
  On `EMFILE` or `ENFILE` the cache is emptied and the open is retried once.
- Appends are serialized by one writer mutex.
  It protects the append offset and pack rollover, and is held only for the write, never while copying the index or fsyncing for a checkpoint.
  The block is looked up again under that mutex, so two threads racing on the same new block store it once.
- Index inserts happen inside the writer mutex, after the record is fully written.
  A reader that sees an index entry can therefore always read its bytes.
- `LOCK` holds an exclusive `flock` (`File::try_lock`), so a second `Store::open` on the same directory fails with `Error::Locked`, in this process and in another one.
  The kernel drops the lock when the process dies, including `kill -9`, so a crashed store reopens.
  Both cases are tested.
  Locking on a network filesystem was not tested and is not claimed.
- `stats` is constant time: counters are kept current on every insert.
  `checkpoint` and `iter_ids` copy the index one shard at a time, so a writer waits at most for one shard, and never for the whole copy.
  With 1,000,000 indexed ids a checkpoint takes about 0.5 s while puts continue with a worst single put of 13 to 20 ms (test `puts_keep_flowing_during_a_checkpoint_of_a_million_ids`).
  Before the change the worst put was 67 to 127 ms and 1,000 `stats` calls took over a minute.
- `put` of an id that is already indexed does one of two things.
  If this session already verified that copy (it wrote it, read it back with `get`, or hash-verified it while rebuilding), it returns at once.
  Otherwise it reads the stored record, checks structure and CRC, decodes it, and compares the bytes with the bytes being put.
  That comparison proves the stored copy hashes to the id, because the caller's bytes were just hashed to get the id.
  On success it caches the verified bit in the index entry, in memory only.
  On failure it writes a fresh record and the new entry replaces the bad one.
  So `put` never returns success for an id whose stored block is unreadable.
  Only the first duplicate put of each block per session pays a read.
  The alternative, refusing to dedup against entries that came from a rebuild, would have rewritten every block after any index loss.

## Recovery on open

Two kinds of damage are kept apart in `RecoveryReport`.

- A torn tail is bytes after the durable watermark in the last pack.
  The store never promised them, so a crash may leave any prefix, any hole and any junk there.
  Open cuts them and reports the size in `torn_tail_discarded`.
  This is informational and is not corruption.
- Corruption is damage to bytes that a completed `sync` had made durable, or a pack that is shorter or missing compared with the watermark.
  It is listed in `corrupt_synced` and `missing_synced`, and `has_corruption()` is true.

Steps:

1. Create the store directories durably, take the lock, list `packs/pack-*.cpk` in id order, read `SYNCED` and `ACKED`.
2. Validate each pack header.
   A last pack shorter than 16 bytes is a crash between create and first write, and is reset to an empty pack.
   Any other bad header or unknown version is an error.
3. Report packs that the watermark says must exist (`base` up to the highest pack) and are gone, unless `ACKED` covers them.
4. Load `index.cix`.
   Discard it if the CRC fails, if a listed pack is missing or shorter than its recorded length, or if any entry lies outside its pack or claims a length above the maximum block size or above its uncompressed length.
   A discarded index means every pack is scanned from its start.
   Entries from a loaded index are trusted for location only and start unverified.
5. Scan each pack from its indexed length (or from the pack header) to its end.
   A record whose header CRC, structure and record CRC pass is decoded and its BLAKE3 compared with its id.
   Only a record that passes all of them is indexed, and it replaces an unverified entry for the same id.
   A record that fails is quarantined: reported as a gap, never indexed, so a forged or embedded record cannot claim an id.
6. Resynchronising after a bad region searches for the next position whose header CRC passes.
   A candidate costs a payload read only if its header CRC passes and its record fits in the pack, and only a header written by us passes.
   Accidental damage (crashes, bit rot) cannot produce a header with a valid CRC, so the search is linear in the bytes scanned.
   Against crafted input the payload bytes read for candidates are capped at twice the pack bytes plus 1 MiB.
   Only when that cap is hit is the rest of the pack reported as one gap, and then it is never cut.
   A damaged record therefore never hides the valid records after it: only records that overlap damaged bytes are lost.
7. Classify each bad region with the watermark.
   Any pack, active or not, has a durable length: the watermark for the pack it names, its `.cut`
   label if it was cut, and its whole length otherwise. A pack above the watermark was written
   after the last completed `sync`, so nothing in it was promised and its durable length is zero.
   The first bad region at or after that length starts the torn tail.
   Everything from there is preserved (first 1 MiB) in `<pack>.torn-<n>`, then:
   - nothing verifiable past the tear: the pack is cut at that point and fsynced;
   - something verifiable past the tear: the records are read into memory, written into a new pack
     with a fresh id and fsynced there, and only then is the old pack cut (`recovered_from_tail`).
     Copy-then-swap, so a crash in the middle of a move never leaves a record half written, and a
     crash before the new pack is durable leaves the old pack whole and the next open repeats it.
   A bad region before that length is corruption: never cut, never erased, listed in
   `corrupt_synced` with pack, offset, length and the id its header claims when that still parses.
   `get` of such an id returns `Error::Corrupt`, and `fsck` lists the region.
   A cut made while the watermark file was missing cannot be classified, so it is recorded as a
   pending loss with `CorruptRegion::unclassified` set and stays reported until it is accepted.
8. A pack that contains any bad region is not appended to: open starts a new pack.
9. Every pack that contributed records or a cut is fsynced, then the watermark is advanced to the end of the active pack.
   Nothing unresolved sits below it, so a later `sync` can never bless leftover damage as durable.
   A pre-existing mark that is already ahead of the packs is left alone, and the writer's own synced position starts at zero so `sync` still fsyncs.

`Store::recovery()` returns what steps 3 to 9 found.
Callers that must not serve a store with lost data (the mount layer) check `RecoveryReport::has_corruption()` and refuse to mount.
`has_corruption()` is true for any `corrupt_synced` or `missing_synced` entry, and, when the watermark file itself is missing, for any damage in the last pack, since then nothing can be classified as torn.

### What open does not check

Open does not re-read data that the index checkpoint covers, so bit rot in checkpointed data is not visible in `RecoveryReport`.
It shows up on `get` (an error, never wrong data) and in `Store::verify_all()`, which is `fsck`.
A mount that wants a clean bill of health calls `verify_all()`.
Open stays O(records since the last checkpoint) on purpose.
Data that a pack holds between the watermark and its true end (sealed, but past the watermark of an earlier mark) is likewise not re-read.

### Repair and acknowledgement

- Re-`put` of a block whose stored copy is damaged writes a fresh record, and later opens report the old region as `superseded` (informational) instead of corruption.
  This works with and without the index, because the check finds a verified copy of the claimed id elsewhere.
- A region whose header is destroyed cannot be matched to a block.
  `Store::acknowledge_corruption()` appends the current `corrupt_synced` regions and missing packs to `ACKED`, fsynced, and lowers the watermark base for missing packs.
  Later opens list them under `acknowledged` and `has_corruption()` is false.
  The bytes stay on disk and the blocks stay unreadable until they are put again.
- `Store::salvage()` slides over every byte of every pack and indexes every record that verifies:
  header structure, both checksums and the BLAKE3 of the payload.
  It trusts nothing a header claims, so a flood of forged headers with valid checksums costs one
  payload read each instead of hiding the real records, and it is linear in the pack size.
  It repairs index entries that cannot be read and clears the rescan set, so a checkpoint written
  after it keeps the salvaged records. It never writes to a pack.
- `Store::new_pack()` creates a pack for compaction to write into. It comes from the same allocator
  as a rollover, so the two never collide, and the id is recorded as used so a later rollover steps
  over it. Any code that creates packs must use it.

### Threat model

cowfs assumes a single user on one host and no attacker.
A header carries two checksums, so damage from a crash or bit rot cannot produce a header that
passes them, and a record is only ever served after its payload hash matches its id.
An attacker who can write to the store directory can rewrite a whole pack, including checksums,
and can then claim any id for any bytes; nothing in this crate defends against that, and no format
field is a secret.
What the checksums and the work bound do buy is that crafted input cannot make open read without
limit: the payload bytes read for header candidates are capped at twice the pack bytes plus 1 MiB,
and salvage, which has no bound, is a repair tool an operator runs on a store that already needs it.

### A known window: rot after verification

`put` trusts the in-memory verified bit of an entry that this session already wrote, read or verified.
If the stored bytes rot after that, `put` still returns Ok and `get` returns `Error::Corrupt`.
The bit is not persisted, so after a reopen the next `put` re-reads the record, sees the damage and rewrites it.
Callers that need certainty within a session use `get` or `verify_all`.

## fsck

`fsck` snapshots each pack length under the writer mutex, then scans every pack, re-decodes each record and re-hashes its data.
It reports records, verified unique blocks, duplicate records, damaged regions (including regions the open scan skipped), hash mismatches, and index entries that do not point to a verified record.
It is safe to run while other threads use the store.
It only reads.

## Packs

`Options::max_pack_size` defaults to 256 MiB and is clamped to 2 GiB, so record offsets fit in 32 bits.
`get` rejects an index entry whose lengths exceed the maximum block size, or whose stored length exceeds its uncompressed length, before it allocates anything.
A record is never split across packs.
A pack always accepts at least one record, so a record may push a pack past the limit when the limit is tiny.

## API

```rust
Store::open(dir, Options) -> Result<Store>             // Options: max_pack_size, checkpoint_on_drop, max_open_packs
Store::put(&self, &[u8]) -> Result<BlockId>            // at most 256 KiB, idempotent
Store::get(&self, BlockId) -> Result<Vec<u8>>          // verified
Store::contains(&self, BlockId) -> bool
Store::ingest(&self, impl Read) -> Result<Vec<ChunkRef>>
Store::ingest_bytes(&self, &[u8]) -> Result<Vec<ChunkRef>>
Store::sync(&self) -> Result<()>
Store::checkpoint(&self) -> Result<()>
Store::stats(&self) -> Stats
Store::iter_ids(&self) -> impl Iterator<Item = BlockId>
Store::fsck(&self) -> Result<FsckReport>
Store::recovery(&self) -> &RecoveryReport              // .has_corruption(), .corrupt_synced, .gaps
Store::verify_all(&self) -> Result<FsckReport>        // same as fsck
Store::acknowledge_corruption(&self) -> Result<usize>  // accept reported losses, see Recovery
Store::salvage(&self) -> Result<SalvageReport>         // re-index every verifiable record
chunks(&[u8]) -> impl Iterator<Item = &[u8]>           // FastCDC 16/64/256 KiB
```

`Store::open_traced` and `Store::open_unsynced` are `#[doc(hidden)]` test seams: the first records every fsync, rename, create and truncate in order, the second turns fsync into a no-op for tests that cannot afford it.
`BlockId` and `ChunkRef` are unchanged from the skeleton.
`ingest` and `ingest_bytes` leave blocks stored before an error in the store as unreferenced blocks, for garbage collection.
An empty input to `ingest` gives an empty chunk list.

## Not in this crate

Garbage collection and compaction (#10), last-access times, tiering and any remote backend.
Only `put`, no `delete`: blocks leave the store only through compaction.

## Measured performance

Tool: `cargo run --release -p cowfs-store --example bench -- <data-dir> <store-dir> [MiB] [runs]`.
Data: every second file of `RuView/v2/target` (a Rust build output), 3,921 files, 1,037 MiB, 15,029 chunks, average chunk 72,405 bytes.
Machine: Apple M3 Max, shared with about 19 other sessions.
Load average during the last full run went from 35 to 140, so every figure is a noisy lower bound.
n is 5 for every row and the median is shown.

| Metric | Median | Range | Target | Verdict |
|---|---|---|---|---|
| Baseline: read the files, warm | 2198 MiB/s | 562 to 2290 | - | - |
| Baseline: raw write plus fdatasync | 2828 MiB/s | 2373 to 3120 | - | - |
| FastCDC only, 1 thread | 1267 MiB/s | 1163 to 1415 | - | - |
| BLAKE3 of whole files, 1 thread | 943 MiB/s | 847 to 967 | - | - |
| Chunk plus hash, 1 thread | 573 MiB/s | 428 to 640 | 800 | missed |
| zstd-3 of the chunks, 1 thread | 264 MiB/s | 202 to 287 | - | - |
| Ingest without sync, 1 thread | 198 MiB/s | 169 to 215 | - | - |
| Ingest plus sync, 1 thread | 212 MiB/s | 196 to 242 | 10 to 48 per core (spike 1) | met |
| Ingest plus sync, 8 threads | 709 MiB/s | 633 to 833 | - | - |
| Verified read, 1 thread, real mix of blocks | 394 MiB/s | 285 to 419 | 1024 | missed |
| Verified read, 8 threads | 2656 MiB/s | 1916 to 3198 | - | - |
| Re-ingest of all duplicates, 1 thread | 588 MiB/s | 544 to 633 | - | - |
| Re-ingest of all duplicates, 8 threads | 4293 MiB/s | 4047 to 4468 | - | - |
| Index lookups, 1 thread | 25.4 M/s | 18.7 to 25.8 | - | - |

Where the time goes:

- Chunk plus hash is the sum of two serial passes, `1/(1/1267 + 1/943)` is about 540 MiB/s, which matches the measurement.
  BLAKE3 alone tops out near 1 GiB/s per thread here, so 800 MiB/s cannot be reached without overlapping the two passes on separate threads.
- Ingest is bound by zstd-3 (264 MiB/s) plus chunk and hash.
  `1/(1/573 + 1/264)` is about 181 MiB/s against a measured 198.
  The append and the fsync cost almost nothing (212 with sync against 198 without).
- Verified reads of a 141 KiB incompressible block run at 939 MiB/s against 1129 MiB/s for BLAKE3 alone, so the hash sets the ceiling.
  A 165 KiB compressible block runs at 474 MiB/s, the sum of zstd decompression (896 MiB/s) and BLAKE3 (1153 MiB/s).
  The 1 GiB/s per thread target for verified reads is therefore out of reach for compressed blocks on this machine, and only close for raw ones.
- Data on this corpus compresses 3.24x after dedup (320 MiB stored for 1,037 MiB of input, with 1,676 of 15,029 puts deduplicated).

## Contract targets that are missed

Two targets in `docs/v1-architecture.md` are missed single-threaded on this Mac, and the store does not claim them.

- Chunk plus hash at least 800 MiB/s per thread: measured 573 to 597 MiB/s.
  FastCDC and BLAKE3 are two serial passes that top out near 1.3 to 1.5 GiB/s and 0.9 to 1.3 GiB/s.
  Their harmonic sum is about 540 to 710 MiB/s.
- Verified read at least 1 GiB/s per thread: measured 394 to 472 MiB/s on a real mix of blocks.
  BLAKE3 alone caps a hot raw block near 1.1 GiB/s, and compressed blocks add zstd decompression at about 0.85 GiB/s.
  The contract requires the hash on every read, so skipping it to reach the number is not allowed.

Recommendation for the lead, not applied to `docs/v1-architecture.md`: restate both targets per 8 threads instead of per thread.
The independent critic measured verified reads at 3413 MiB/s on 8 threads, and the builder measured 2656 MiB/s under heavier load.
Ingest already scales the same way (709 to 1172 MiB/s on 8 threads against about 200 on one).
Every thread of a mount serving reads or an import runs the chunker, the hasher and the decoder independently, so the 8 thread figure is the one that matters for cowfs.

## Performance before and after the critic fixes

Same tool and data as above, one run of each binary back to back under the shared CPU lock, n=5 per row, median shown.
Machine load average was 84 to 117 at the start and 49 to 118 at the end, so every figure is a noisy lower bound and differences under about 15% are not meaningful.
"Before" is commit `3696e42`, "after" is the fixed code.

| Metric | Before | After |
|---|---|---|
| Chunk plus hash, 1 thread | 567 MiB/s | 636 MiB/s |
| Ingest plus sync, 1 thread | 204 MiB/s | 209 MiB/s |
| Ingest plus sync, 8 threads | 503 MiB/s | 1000 MiB/s |
| Verified read, 1 thread | 612 MiB/s | 525 MiB/s |
| Verified read, 8 threads | 3711 MiB/s | 4170 MiB/s |
| Re-ingest of duplicates, 1 thread, warm session | 774 MiB/s | 901 MiB/s |
| Re-ingest of duplicates, 1 thread, first pass after reopen | 689 MiB/s | 686 MiB/s |
| Re-ingest of duplicates, 8 threads | 4943 MiB/s | 5604 MiB/s |
| Index lookups, 1 thread | 24.9 M/s | 31.4 M/s |

Verify-on-dedup costs nothing measurable: the first duplicate pass after a reopen reads and compares every stored block, and runs at the same speed as the old trust-the-index path, because that pass is bound by chunking and hashing.
Single-thread verified read moved from 612 to 525 MiB/s, which is inside the load noise of these runs but is not proven to be noise.

## Round 2: measured results

### Simulated power loss

The harness (`tests/round2.rs`, `power_loss_orderings`, ported from the critic) runs a random put, sync and checkpoint history, copies the files, then damages the copy the way a crash can: the active pack is cut at a random length at or after the watermark, random 512 byte or 4096 byte sectors past the watermark are zeroed or filled with junk, and the watermark file is left alone, has its newest slot torn, or has both slots torn.
It then opens the copy, checks that every acknowledged block reads back with the right bytes, writes five more blocks, syncs, checkpoints, reopens, and requires a clean report.
`COWFS_POWERLOSS_SEEDS=400` gives the full 1800 cases.
The default is 60 seeds (270 cases).

| Code | Cases | Acked blocks lost | Wrong bytes served | Reported corruption after one more sync |
|---|---|---|---|---|
| Before (85727dc, critic's run) | 1800 | 0 | 0 | 271, about 15% (watermark intact or newest slot torn: 113 of 1600; both slots torn: 158 of 200) |
| After | 1800 | 0 | 0 | 0 |

With both watermark slots torn, open cannot tell a torn tail from corruption, so it reports the damage once (`watermark_missing` and `has_corruption()`), cuts the tail into a sidecar, rewrites the watermark, and the next open is clean.
That happened in 158 of the 200 such cases and in none of the 1600 cases where at least one slot survived.
The first mark of a store is written into both slots, so a single torn write cannot destroy it.

### Full-size pack

`examples/big_pack.rs`: one 256 MiB pack of 4096 incompressible 64 KiB blocks, release build, machine load 15 to 35.

| Case | Open time | Readable | Expected lost | Wrongly lost |
|---|---|---|---|---|
| Clean, index loaded | 1 ms | 4096 | 0 | 0 |
| Clean, index deleted (full rebuild, every block hash-verified) | 227 ms | 4096 | 0 | 0 |
| 1 MiB zeroed at offset 1 MiB | 238 ms | 4079 | 17 | 0 |
| 2 bytes flipped in each of the first 1100 records | 311 ms | 2996 | 1100 | 0 |
| 2 bytes flipped in each of the first 3000 records | 342 ms | 1096 | 3000 | 0 |
| 2 bytes flipped in every other record | 1.11 s | 2048 | 2048 | 0 |

Before, the second case lost all 4095 blocks, about 3000 of them valid, to the 64 MiB resync budget.
A 4 MiB flood of fake headers opens in well under a second, and a flood of headers that pass their own CRC is bounded by the payload cap (tests `f3_fake_header_flood_opens_quickly` and `a_flood_of_forged_headers_with_valid_checksums_opens_fast`).

### Throughput before and after round 2

Commit 85727dc against this code, same data as above, n=5 per row.
The machine is shared and other sessions moved the load between runs, so the figures are not precise.

| Metric, MiB/s | Before, pass 1 (load 35 to 16) | After, pass 1 | Before, pass 2 (load 56 to 65) | After, pass 2 | Before, pass 3 (load 64 to 71) | After, pass 3 |
|---|---|---|---|---|---|---|
| Chunk plus hash, 1 thread | 945 | 946 | 720 | 650 | 718 | 488 |
| Ingest plus sync, 1 thread | 287 | 191 | 34 | 36 | 98 | 49 |
| Ingest plus sync, 8 threads | 1108 | 1016 | 165 | 109 | 82 | 191 |
| Verified read, 1 thread | 771 | 521 | 364 | 372 | 249 | 479 |
| Verified read, 8 threads | 4305 | 3106 | 1988 | 2558 | 764 | 2458 |
| Re-ingest of duplicates, 1 thread, warm session | 1004 | 745 | 586 | 550 | 443 | 735 |
| Re-ingest of duplicates, 1 thread, first pass after reopen | 1065 | 783 | 280 | 288 | 251 | 550 |

Only pass 1 ran on a moderately quiet machine, and it shows the new code 25 to 35% slower on single-thread ingest, verified read and duplicate re-ingest.
Passes 2 and 3 show no consistent direction.
So a real slowdown on those paths is neither proven nor ruled out.
The changes on those paths are small (two CRC32C calls over 48 bytes per record, one more field in the index entry), and the clean pass was the first run of the session, which also favours the first binary.
A quiet-machine A/B was not possible.

### Verify-on-dedup with a cold cache

Not measured.
Purging the OS page cache needs root, and a file set larger than this Mac's 128 GiB of RAM is not practical.
The in-cache cost is the row "first pass after reopen" above: the first duplicate `put` of each block per session reads the stored record and compares it, and that pass ran at 250 to 1065 MiB/s in the three passes, compared with 443 to 1004 for the warm-session pass.
With a cold cache the first pass after every reopen costs one full sequential read of the stored bytes of every block that is deduplicated, so it is bound by the disk read speed, not by the hash.

### Mutation testing

`tests/mutate.py` applies 50 textual mutations one at a time to a scratch copy, runs the store suite with a separate `CARGO_TARGET_DIR` per mutant, and records `target/mut/results.txt`.
Results on the final code: 48 mutants killed, 2 survived (W7 and H5 after the reruns that followed the first pass).
Survivors:

- W7 (the `durable` helper returns 0 instead of `u64::MAX` for sealed packs): equivalent.
  The value is only used for the last pack, so it has no observable effect.
- H5 (`get` skips the index length range check): equivalent.
  A loaded index is validated at open, and `put` only writes correct lengths, so no entry reaches `get` with an out-of-range length.
  It stays as a second line of defence.

The earlier labels N17, N18, N19 and N22 were from round 1 on commit 85727dc: N17 is the padding check (now H2), N18 is the decoded-length check at `record.rs:128`, `Ok(v) if v.len() == header.ulen as usize => Ok(v)` replaced by `Ok(v) => Ok(v)` (now H3), N19 is `if !locate_ok(&loc)` at `store.rs:499` in `get` replaced by `if false` (now H5), and N22 is the `header.id != id || header.slen != loc.slen || header.ulen != loc.ulen` check at `store.rs:515` replaced by `if false` (now H4).

A known gap that no mutant can show: a sealed pack that is cut short at a record boundary is only noticed through the index checkpoint, because nothing else records a sealed pack's length.
Without the index, open sees a shorter but well-formed pack.

## Round 3: measured results

All runs on the Apple M3 Max, `cargo -j4`, release build for the long runs, with other work on the
machine. Every figure below is from a run whose output was read, not from an estimate.

### Crash-reopen model (`tests/crash.rs`)

A history of puts, syncs and checkpoints, a power cut at any operation, then open, and again until
a full recovery, with the checkpoint written at random points.
`C7C_SEEDS=1000` is 4000 cases, about 12 minutes.

| Cases | Acked blocks lost | Wrong bytes served | False corruption report | fsck dirty | Open failures |
|---|---|---|---|---|---|
| 800 (default) | 0 | 0 | 0 | 0 | 0 |
| 4000 (`C7C_SEEDS=1000`) | 0 | 0 | 0 | 0 | 0 |

The same harness on the pre-round-3 code also reported 0 lost and 0 false corruption on its 800
seeds, so the new recovery rules did not cost that property; the round-3 tests are what make the
losses that *were* possible before impossible.

### Power-loss model (`tests/round2.rs`)

`COWFS_POWERLOSS_SEEDS=400` is 1800 cases, 324 s.
0 acked blocks lost, 0 wrong bytes, 0 reports of damage that a sync had not promised.

### Eight threads for three minutes (`tests/stress.rs`)

`C7C_SECS=180`, 50 KB packs, two read handles open, puts, gets, syncs, checkpoints, fsck, salvage
and acknowledgements all at once.

```
ops=29638 errors=0 model=13262 stats.blocks=13262 iter_ids=13262
fsck.verified=13262 fsck.dupes=0 stats.packs=1280 disk.packs=1280
stats.pack_bytes=58828455 disk.bytes=58828455
reopen ok, corruption=false torn=0
```

### Mutation testing, round 3 (`tests/mutate3.py`)

14 mutations of the round-3 code, run against `round3`, `round2`, `store` and `durability`, plus the
library unit tests for the ones they cover. Results in `target/mut3/results.txt`.

Killed: N02 (a checkpoint may validate against a recreated pack), N05 (the work bound refuses a
candidate that exactly fits), N06 (a repairing `put` leaves the block in the damaged list), N07 (an
acknowledgement does not sync first), N08 (a pack with a lost header is refused again), N09 (salvage
trusts the header instead of the payload hash), N10 (sidecars are never pruned), N12 (an
unclassifiable cut is not remembered), N14 (salvage does not clear the rescan set).

Survivors, all three equivalent:

- N01 (the allocator ignores the `ACKED` pack ids): equivalent while the watermark holds.
  Losing `SYNCED` loses the high-water, and then `base` and `acked` are what still keep an
  acknowledged id from coming back, so this is the second line of the same property.
- N03 (records are moved down in reverse order): equivalent.
  Relocation writes whole records into a new pack and indexes them by id; nothing reads them in
  file order, and the copy never overlaps its source.
- N11 (a pack above the watermark uses 0 instead of its cut label): equivalent.
  A pack above the watermark was written after the last `sync`, so treating all of it as
  unpromised is the same answer the label gives.

Two mutations from the critic's list are answered by superseding rather than by a new test: M01 and
M02 (dropping or ignoring `base`) target a field whose role changed. The slot now carries the pack
id high-water instead, `base` is the lowest pack that must exist, and a missing pack stays reported
because an `ACKED` entry must be accepted before it is silent, which `d4` and `p13` cover.
M04 (moving records down in place) is gone with the in-place move: recovery copies into a new pack
and only then cuts, which `f4_two_torn_regions_survive_a_crash_at_every_write` exercises at every
write of the recovery.
