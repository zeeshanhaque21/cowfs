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
  SYNCED                durable watermark, two CRC-protected 32 byte slots
  index.cix             optional index checkpoint (rebuildable, see below)
  packs/pack-00000000.cpk
  packs/pack-00000001.cpk
  ...
```

All integers are little endian.

### Pack file

A pack starts with a 16 byte header, followed by records with no padding and no footer.

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | magic `COWPACK\0` |
| 8 | 4 | format version, `1` |
| 12 | 4 | reserved, zero |

### Record

Every record has a fixed 52 byte header followed by `stored_len` payload bytes.

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | magic `CWRB` |
| 4 | 1 | codec: `0` raw, `1` zstd |
| 5 | 3 | zero |
| 8 | 4 | `uncompressed_len`, at most 262144 |
| 12 | 4 | `stored_len` |
| 16 | 32 | `BlockId`, BLAKE3-256 of the uncompressed bytes |
| 48 | 4 | CRC32C over header bytes 0..48 followed by the payload |
| 52 | n | payload |

Validity rules: codec 0 needs `stored_len == uncompressed_len`.
Codec 1 needs `stored_len < uncompressed_len`.
The zero bytes must be zero.
A record that breaks a rule, or whose CRC fails, is invalid.
The zero length block (empty data) is a valid raw record.

A record is self-describing, so a pack can be parsed with no other file.
The format has no in-place mutation, no back pointers and no pack-level footer.
Compaction (#10) can therefore copy live records into a new pack id and delete the old pack.
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
- Creating a store or a pack fsyncs the new file and every directory entry that names it: the store directory and its parent, `packs/`, `LOCK`, `SYNCED`, and each pack.
  Renaming `index.cix` into place is followed by an fsync of the store directory.
- A crash may lose puts after the last `sync`.
  It never serves a torn record, because every record is verified by CRC on scan and by CRC plus BLAKE3 on read.
- `checkpoint` syncs, then writes `index.cix` (tmp file, fsync, rename, directory fsync).
  Drop calls it when the store was written to since the last checkpoint, unless `Options::checkpoint_on_drop` is false.
  A failed checkpoint on drop is ignored, which is harmless: the packs are the truth.

### The watermark

`SYNCED` holds `(pack, len)`: everything in packs below `pack`, and the first `len` bytes of pack `pack`, were fsynced before this value was written.
It exists so that recovery can tell a torn tail (bytes after the watermark, never promised durable) from corruption of synced data (bytes before it).
It has two 32 byte slots written alternately, each with a sequence number and a CRC, so a torn write of one slot leaves the other.

Ordering, enforced by tests through the fsync trace seam:

1. fsync the pack data.
2. Write the new watermark and fsync `SYNCED`.

A crash between 1 and 2 leaves the watermark lower than the truth.
That is safe: the extra bytes are treated as tail, and open indexes every valid record in them and only cuts what does not verify.
A watermark higher than the truth is impossible unless a disk lies.
If a pack is shorter than the watermark says, open reports that as corruption.
A missing or unreadable `SYNCED` next to existing packs is reported (`watermark_missing`) and nothing is truncated.

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

1. Create the store directories durably, take the lock, list `packs/pack-*.cpk` in id order and read the watermark.
2. Validate each pack header.
   A last pack shorter than 16 bytes is a crash between create and first write, and is reset to an empty pack.
   Any other bad header is an error, because it is not a pack we wrote.
3. Load `index.cix`.
   Discard it if the CRC fails, if a listed pack is missing or shorter than its recorded length, or if any entry lies outside its pack or claims a length above the maximum block size or above its uncompressed length.
   A discarded index means every pack is scanned from its start.
   Entries from a loaded index are trusted for location only and start unverified.
4. Scan each pack from its indexed length (or from the pack header) to its end.
   A record whose structure and CRC pass is decoded and its BLAKE3 compared with its id.
   Only a record that passes both is indexed, and it replaces an unverified entry for the same id.
   A record that fails either is quarantined: it is reported as a gap and never indexed, so a forged or embedded record cannot claim an id.
   An invalid record starts a search for the next position where a valid record begins, so one damaged record never hides the valid records after it.
5. The search has a bound.
   A candidate costs a payload read only if its header is structurally valid and fits in the pack.
   Each scan of a pack may read at most 64 MiB of payload for candidates, plus one linear pass over the bytes to find magic.
   When the budget runs out the rest of the pack is reported as one gap and is never truncated, because nothing is known about it.
   A 4 MiB flood of fake headers therefore opens in well under a second (test `f3_fake_header_flood_opens_quickly`, previously 14 s in a debug build and 3 s in release).
6. Classify each bad region with the watermark.
   A region that starts at or after the watermark, in the last pack, with nothing valid after it and a completed search, is a torn tail.
   The pack is truncated to the end of its last valid record and fsynced.
   A region that starts before the watermark is corruption of synced data.
   It is never truncated and its bytes stay on disk.
   It is listed in `RecoveryReport::corrupt_synced` with pack, offset, length and the id its header claims, when that header still parses.
   `get` of a claimed id returns `Error::Corrupt`, not `NotFound`, and `fsck` lists the region.
   Open still succeeds so that the other blocks stay readable and the damage can be inspected.
7. A pack that contains any bad region is not appended to.
   Open starts a new pack, and the next checkpoint marks the damaged pack for a rescan so the damage is reported again on every open.
8. Every pack that contributed records or a truncation is fsynced.
   If nothing was damaged, the watermark is advanced to the end of the active pack.
9. Append to the last pack, or start a new one if it is at least `max_pack_size` or was damaged.

`Store::recovery()` returns what steps 3 to 8 found.
Callers that must not serve a store with lost data (for example the mount layer) check `RecoveryReport::has_corruption()` and refuse to mount.
`has_corruption()` is true for any `corrupt_synced` entry, and for any gap when the watermark file was missing, since then nothing can be classified as torn.

A bit flip in a synced record is now corruption, not a torn tail: it is reported, the bytes are kept, and the block reads as an error.
Only bytes the store never promised durable are ever cut.
`fsck` and `get` never return wrong data in any case.

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
