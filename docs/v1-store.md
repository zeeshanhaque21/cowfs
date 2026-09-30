# v1 block store (`cowfs-store`)

Issue: #7.
Contract: `docs/v1-architecture.md`, section "cowfs-store contract".
This note records the byte layout, the index choice, the durability and concurrency model, the recovery algorithm and the API.
It does not change any settled decision in `docs/design.md`.
The target platforms are Linux and macOS.
The crate uses positional file I/O (`std::os::unix::fs::FileExt`), so it does not build on Windows, which is out of v1.

## Layout on disk

```
<store>/
  LOCK                  advisory lock, held while a Store is open
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
- `sync` calls `sync_data` on the active pack.
  A pack is synced before the next one is created, so a sealed pack is always durable.
  After `sync` returns, every `put` that returned earlier is durable.
- Creating a pack syncs the new file and the `packs/` directory.
- A crash may lose puts after the last `sync`.
  It never serves a torn record, because every record is verified by CRC on scan and by CRC plus BLAKE3 on read.
- `checkpoint` syncs, then writes `index.cix`.
  Drop calls it when the store was written to since the last checkpoint, unless `Options::checkpoint_on_drop` is false.

## Concurrency

- Any number of threads may call any method on a shared `&Store`.
- Hashing and compression run outside every lock.
- Reads take a shard read lock, then use `pread` on a shared file handle.
  Reads never block on writers.
- Appends are serialized by one writer mutex.
  It protects the append offset, pack rollover and the checkpoint's consistent view.
  The block is looked up again under that mutex, so two threads racing on the same new block store it once.
- Index inserts happen inside the writer mutex, after the record is fully written.
  A reader that sees an index entry can therefore always read its bytes.
- `LOCK` holds an exclusive advisory lock, so a second `Store::open` on the same directory, in this or another process, fails with `Error::Locked`.

## Recovery on open

1. Take the lock and list `packs/pack-*.cpk` in id order.
2. Validate each pack header.
   A last pack shorter than 16 bytes is a crash between create and first write, and is reset to an empty pack.
   Any other bad header is an error, because it is not a pack we wrote.
3. Load `index.cix`.
   Discard it if the CRC fails, if a listed pack is missing or shorter than its recorded length, or if an entry lies outside its pack.
   A discarded index means every pack is scanned from its start.
4. Scan each pack from its indexed length (or from the pack header) to its end.
   A record that is complete and passes its CRC is indexed, first occurrence of an id wins.
   An invalid record starts a search for the next byte position where a valid record begins.
   The bytes skipped are reported as a gap and nothing in them is served.
   Resynchronizing means one damaged record never hides the valid records after it.
5. If the scan of the last pack ends in an invalid region with no valid record after it, that region is a torn tail.
   The pack is truncated to the end of its last valid record and synced.
   A torn region in an earlier pack is only reported, never truncated.
6. Every pack that contributed records or a truncation in steps 4 and 5 is synced, so a checkpoint never claims bytes that are still only in the page cache.
7. Append to the last pack, or start a new one if it is at least `max_pack_size`.

`Store::recovery()` returns what step 3 to 5 found.

A bit flip in the final synced record is indistinguishable from a torn write, so a full rebuild truncates it.
The recovery report shows the truncated byte count.
`fsck` and `get` never return wrong data in any case.

## fsck

`fsck` snapshots each pack length under the writer mutex, then scans every pack, re-decodes each record and re-hashes its data.
It reports records, verified unique blocks, duplicate records, damaged regions, hash mismatches, and index entries that do not point to a verified record.
It is safe to run while other threads use the store.
It only reads.

## Packs

`Options::max_pack_size` defaults to 256 MiB and is clamped to 2 GiB, so record offsets fit in 32 bits.
A record is never split across packs.
A pack always accepts at least one record, so a record may push a pack past the limit when the limit is tiny.

## API

```rust
Store::open(dir, Options) -> Result<Store>
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
Store::recovery(&self) -> &RecoveryReport
chunks(&[u8]) -> impl Iterator<Item = &[u8]>           // FastCDC 16/64/256 KiB
```

`BlockId` and `ChunkRef` are unchanged from the skeleton.
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
