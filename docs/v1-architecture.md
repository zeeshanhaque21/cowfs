# v1 architecture

This document turns `docs/design.md` into a crate layout and contracts.
It does not change any settled decision in `docs/design.md`.
The spike results in `docs/spikes/` are the evidence behind the numbers here.

## Crates

| Crate | Issue | Role |
|---|---|---|
| `cowfs-store` | #7, later #10 | Chunking, BLAKE3, zstd, append-only packs, block index, fsck |
| `cowfs-meta` | #8, #9 | redb-backed inode tables, directories, Merkle root, O(1) snapshots |
| `cowfs-gc` | #10 | Mark and sweep over snapshot roots (not yet created) |
| `cowfs-fuse`, `cowfs-nfs` | #11, #12 | Mount adapters (not yet created) |
| `cowfs-cli` | #13, #14 | CLI, control API, `import` (not yet created) |

Dependency direction: mounts and CLI depend on `meta` and `store`.
`meta` depends on `store` for the `BlockId` and `ChunkRef` types only.
`meta` never writes blocks.
The caller ingests file data through `store` and hands `meta` the resulting chunk list.
Keeping the two apart lets each be tested and crash-tested alone.

Shared types live in `cowfs-store`:

- `BlockId`: BLAKE3-256 of the uncompressed bytes of a block.
- `ChunkRef { id, len }`: one chunk of a file.

## cowfs-store contract

### Chunking

- FastCDC with minimum 16 KiB, average 64 KiB, maximum 256 KiB, as measured in spike 1.
- A file shorter than the minimum is one block, so small files are stored whole.
- Chunking is deterministic: the same bytes give the same boundaries on every run and on every platform.
- Dedup is computed on uncompressed bytes, and compression is applied after chunking.

### Blocks

- A block is at most 256 KiB of uncompressed data.
- Compression is zstd level 3, applied per block.
  A block is stored raw when compression saves less than 5%.
- The hash is verified on every read.
  A mismatch is an error and never returns data.
- `put` is idempotent: putting bytes that already exist stores nothing and returns the existing `BlockId`.

### Packs and durability

- Blocks live in append-only pack files.
  Each record is self-describing: magic, `BlockId`, uncompressed length, codec, stored length, a checksum over the record header and payload, then the payload.
- Packs are the source of truth.
  The block index (`BlockId` to pack, offset, stored length, uncompressed length) can always be rebuilt by scanning the packs.
  The index may live in redb or in another structure, as long as losing it never loses data.
- On open, the store scans each pack's tail beyond the last indexed offset, truncates a torn final record, and never serves a record whose checksum fails.
- Durability is explicit.
  `put` may return before the data is durable, and `sync` makes every prior `put` durable.
  Losing a bounded amount of recent unsynced data on a crash is acceptable.
  A torn or corrupt record being served is not.
- Packs roll over at a size chosen by the implementation and documented (a few hundred MiB is expected).

### API sketch (the implementation may refine names, not semantics)

- `Store::open(dir, options)`, `put(&self, &[u8]) -> BlockId`, `get(&self, BlockId) -> Vec<u8>`, `contains`, `sync`, `stats`, `iter_ids`.
- A streaming ingest helper that chunks a `Read` and returns the `Vec<ChunkRef>`.
- `fsck`: re-hash every block and check every record checksum, returning a report.
- Safe for concurrent use from many threads.

### Hooks left for #10 (garbage collection)

Deleting blocks means rewriting packs without the dead blocks.
The pack format must allow a compaction pass that copies live records into a new pack and removes the old one atomically.
The compaction itself is #10 and is not part of #7.

### Performance targets

- Chunk plus hash at least 800 MiB/s per thread.
- End to end ingest (chunk, hash, dedup lookup, compress, append) at least as fast per core as the spike 1 tool, which ran at 165 to 770 MiB/s across 16 threads on the real corpus.
- Read of a cached block dominated by hashing, and at least 1 GiB/s per thread for verified reads of large blocks on this Mac.
- These are targets to measure against and to report honestly, not claims.
  The benchmark must use real data (for example a slice of a Rust `target/` directory) and report the do-nothing baseline (raw copy speed).

### Tests required

- Unit and property tests for chunk boundaries (determinism, min and max bounds, concatenation of chunks equals the input).
- Round trip for many sizes including 0, 1, min-1, min, max, max+1, and multi-MiB.
- Dedup: the same data twice stores once.
- Corruption: a flipped bit anywhere in a stored record is detected on read and by fsck, never returned as data.
- Crash injection: for a pack with several records, truncate at every byte offset inside the last record and inside a middle record, then reopen.
  Every block that was synced before the cut must read back correctly, no torn record may be served, and the store must keep accepting writes.
- Model-based property test: random sequences of put, get, sync, and simulated crash compared against an in-memory model.
- Concurrency stress: many threads putting overlapping data.
- Index loss: delete the index, reopen, and every block is still readable.

## cowfs-meta contract

### Required behaviour (from `docs/design.md`)

- A Merkle tree of directories and files, with a writable snapshot that is an O(1) metadata copy.
- redb for metadata, with crash-consistent transactions.
  A torn or corrupt tree is never acceptable, and bounded loss of recent writes is.
- Full POSIX semantics available to the mount layer: hardlinks, symlinks, xattrs, mode bits, timestamps, rename.
- Single-user model: everything is owned by the mounter.

### The constraint the design has to solve

A pure content-addressed Merkle tree cannot express hardlinks.
Two names for one inode must show the same later writes, but two identical content hashes are just equal, not the same file.
Spike 2 found that rustc incremental compilation hardlinks its `.o` files, so hardlinks are required.
So the design needs an inode layer whose identity is separate from content.

One workable shape, offered as a starting point and not a decision:
persistent (copy-on-write) maps for the inode table and for directory entries, where a snapshot copies the two map roots, writes copy the path to the changed leaf, hardlinks are two directory entries naming one inode id, and the Merkle root is a hash over the map roots for verification.
The `cowfs-meta` author must write the actual design, with the alternatives considered and why one was chosen, in `docs/v1-meta.md`, before writing the storage code.

### Requirements for the design note

- Snapshot creation cost is independent of tree size (measure it on 1,000, 100,000 and 1,000,000 nodes).
- A write to one file in a large tree touches only the path to it, and the cost is measured.
- Reference counting or an equivalent so that #10 can find live blocks by walking snapshot roots, and can skip subtrees already marked.
- Inode numbers stable within a snapshot and unique across snapshots' live files, suitable for NFS file handles and FUSE.
- Every mutating operation batch commits in one redb write transaction, with a documented durability policy for high-rate operations.
- A crash-injection harness that kills the process or truncates or tears the redb file at many points and checks the tree still opens and passes a consistency check.
- Directory iteration is stable and resumable by a position cookie that does not depend on inode numbers (the spike 2 readdir bug: duplicates and errors when hardlinked names shared an inode-based cookie).

### Tests required

- Unit and property tests against an in-memory model of a POSIX tree: random sequences of create, write, mkdir, rename, link, unlink, symlink, snapshot.
- Snapshot isolation: writes to a snapshot never appear in its source and the reverse.
- Hardlink semantics: write through one name, read through another, unlink one, the other survives, nlink is correct, across directories and across a snapshot boundary.
- Rename over an existing file and over an empty directory, rename into itself rejected.
- Crash consistency as described above, with consistency checks after every simulated crash.
- Directory listing while entries are being removed.

## Cross-cutting rules

- Every crate is `#![deny(unsafe_code)]` unless it has a documented reason (mount adapters may need it).
- CI runs `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace` on Linux and macOS.
- Tests use `tempfile`, never a fixed path, and leave nothing behind.
- Nothing in these crates depends on the spikes.
- Performance claims must come from a benchmark with n of at least 5, a stated baseline, and the machine load recorded.
