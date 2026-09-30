# cowfs-core design

Issue: #26.
Contract: `docs/v1-architecture.md`, `crates/cowfs-vfs` (the `Vfs` trait), `docs/v1-store.md`, `docs/v1-meta.md`.
This note is the design.
Numbers in "Measurements" come from `crates/cowfs-core/examples/bench.rs` and are the only performance claims made here.

## What the crate is

`Core` opens one directory holding a block store and a metadata database, and implements `Vfs` over them.
It is also the control plane: snapshot create, fork, remove, rename, promote, list, Merkle root, sync, check, fsck.
The control methods are inherent methods of `Core` and are not part of `Vfs`.

```
Core::open(dir, Options)  ->  <dir>/store/  (cowfs-store)
                              <dir>/meta.redb  (cowfs-meta)
```

`cowfs_core::SnapshotView` is a `Vfs` whose root is one snapshot's root directory, so that the conformance suite (which creates files directly under `ROOT_INO`) and single-tree consumers can use a snapshot as a plain filesystem.
`Core` itself is the mount: its root is the synthetic snapshot directory.

## Inode numbers

`Ino` is a `u64` with three shapes.

| Shape | Value | Meaning |
|---|---|---|
| mount root | `1` | synthetic directory that lists snapshots |
| meta-derived | `(snapshot id << 40) \| meta inode`, snapshot id in `1..2^23`, meta inode below `2^40` | a file that exists in meta |
| virtual | `1 << 63 \| n` | a file created through this `Core` that was not yet committed when its number was handed out |

The meta-derived form is computed, not stored, so it needs no table, is stable across restarts, and is unique across snapshots because the snapshot id is part of it.
Two snapshots that share content report different numbers for the same file, so `find -samefile` and `rsync -H` do not see them as hardlinks.
Snapshot roots are `(id << 40) | 1`.
An inode whose snapshot id is not a live snapshot is `Stale`.

The virtual form exists because `create`, `mkdir` and `symlink` must return an `Ino` at once, while the write-back layer commits them later, and meta only assigns inode numbers inside a transaction.
A virtual number is allocated from a counter.
When the batch that creates the file commits, meta reports its real inode number and `Core` records the pair in an alias table (virtual to meta, and meta to virtual).
From then on `lookup`, `readdir` and every other path canonicalise the meta number to the virtual one, so a file has exactly one `Ino` for as long as the mount lives.
The alias entry is dropped when the file is deleted and unreferenced.
An alias costs about 40 bytes per file created by this mount session that still exists.
Virtual numbers do not survive a restart: after a restart the same file has its meta-derived number.
No adapter keeps file handles across a restart of the process that owns the mount, so this is documented and accepted.
Snapshot ids and meta inode numbers are never reused, so a number never names two files.

## The synthetic root

`ROOT_INO` is a read-only directory.
Its entries are the snapshots, named as the snapshot is named, with the snapshot root directory's attributes.
Its cookies are snapshot ids (never reused, independent of inode numbers), so a listing is exactly-once under create and remove.
Its `nlink` is 2 plus the number of snapshots.
`create`, `mkdir`, `symlink`, `link`, `unlink`, `rmdir`, `rename`, `setattr` with a size, `write`, and `setxattr` on the root return `ReadOnly`; `rename` between two snapshots is `CrossDevice`; `link` across snapshots is `CrossDevice`.
`setattr` of mode or times on the root is accepted and ignored.

Snapshot names: 1 to 255 bytes, valid UTF-8, no `/`, no NUL, not `.` or `..`, not starting with `._` (AppleDouble) or `.nfs` (NFS silly rename).
The adapters and the OS create names such as these, so allowing them would make a snapshot that a mount cannot see.

## Control plane

| Method | Cost | Notes |
|---|---|---|
| `create_snapshot(name)` | one durable meta commit | empty tree |
| `fork_snapshot(src, name)` | O(1): flush of `src`'s pending operations, then one meta row | writable clone |
| `remove_snapshot(name)` | proportional to nodes unique to it | refused with `Busy` while any handle is open in it |
| `rename_snapshot(old, new)` | fork plus remove | changes the snapshot id and so every inode number in it, refused while handles are open. Request 1 below removes this. |
| `promote_base(src, base)` | fork plus remove of the old `base` | not atomic across a crash: between the two steps there is no `base`. Request 1. |
| `list_snapshots()` | one read | |
| `merkle_root(name)` | flush of that snapshot, then one read | root only covers committed state, so it flushes first |
| `sync()` | flush of everything, then `Meta::sync` | |
| `check()`, `fsck()` | scan | pass-through to `Meta::check` and `Store::fsck` after a flush |

Snapshot create is O(1) because meta's is.
`Core` adds a flush of the source's pending operations, which is proportional to what is pending and bounded by the write-back limits, never to tree size.
Other snapshots' directory handles, dentry entries and node entries are untouched by any of these, because every cache key contains the snapshot id.
Removing a snapshot purges only entries whose key carries its id, and from then on every inode number in it is `Stale`.

## Write-back layer

Meta commits cost about 2 ms each and a build creates tens of thousands of files, so namespace operations and small writes are applied to memory at once and committed to meta in batches.

### State

For each mounted snapshot there is a `SnapCtx` with:

- an operation queue (`Vec<Op>`), guarded by one small mutex that is never held across IO;
- a namespace lock, held only by operations that change directory entries, and only for in-memory work plus at most one meta point read;
- a flush lock, held by whoever is committing a batch, so there is one committer per snapshot;
- three counters: `seq` (bumped for every mutation), `drained` (the value of `seq` when the queue was last taken) and `flushed` (the value when the last batch committed).

Operations recorded in the queue: create (file, directory or symlink), link, unlink, rmdir, rename, and set-content (chunk list plus size).
Everything else a batch has to write (mode, atime, mtime) is not an operation: the nodes an operation touched are recorded, and at the end of the batch their cached mode, atime and mtime are written with one `setattr` each.
That keeps timestamps at the time the operation happened and not at the time the batch ran (meta stamps `Tx::now`, the start of the batch; ctime cannot be set, see request 2).

Nodes and dentries carry the `seq` of their last mutation.
An entity is dirty exactly while its `seq` is above `flushed`, which is computed and never stored, so a commit marks nothing.
Dirty entries are never evicted, so a read that hits the cache sees every uncommitted change.
A read that misses reads meta, which holds every change that is not pending, and that is the right answer for anything not in the cache.

### What is deferred, what is a barrier

Deferred (memory only, one queued op): `create`, `mkdir`, `symlink`, `link`, `unlink` of a non-directory, `rename` of a non-directory, `write`, `setattr`.
`rmdir` is deferred when emptiness can be decided without a commit: the directory has no pending entry changes and a meta `readdir` of it returns nothing, or it was created by this session and its exact child count is zero.
Otherwise it flushes first (a barrier) and then decides.

Barrier (flush the snapshot, then run the operation against meta directly): directory rename, rename that replaces a directory, `readdir` of a directory that has pending entry changes, and xattr writes on a file that has no meta inode yet.
A barrier is one batch of everything pending plus the operation, so a burst of barriers still commits far fewer transactions than operations.
`readdir` of a directory without pending entry changes does not flush.

### Flush policy (exact bounds)

A snapshot's queue is committed when any of these holds:

1. the number of queued operations reaches `Options::max_pending_ops` (default 4096), checked by the operation that queued it, which wakes the background flusher;
2. the oldest queued operation is `Options::flush_interval` old (default 500 ms), checked by the background flusher every `flush_interval / 2`;
3. dirty data exceeds `Options::max_dirty_bytes` (default 128 MiB), in which case the writer flushes data itself;
4. a barrier operation, `fsync`, `sync`, a control operation that needs a consistent view, or drop of the last `Core` handle.

Data (`write`) is buffered per file.
A file's buffer is chunked, hashed and put in the store when it reaches `Options::file_flush_bytes` (default 4 MiB), when the snapshot is flushed, or on `fsync` of that file.
Putting in the store is not durable and is not visible to meta.

The background flusher also calls `Meta::sync` when a non-durable commit is older than `Options::sync_interval` (default 1 s).
`Meta` decides for itself to commit durably every `sync_every_ops` transactions or `sync_interval`, whichever comes first, and only when a mutation arrives.
So after `fsync` returns, the file's data, its name and its attributes are durable.
Without `fsync`, a crash loses at most: the operations queued and not yet committed (at most `flush_interval` old, at most `max_pending_ops` of them), plus committed batches that meta has not made durable (at most `sync_interval` old, plus one flusher tick).
That is about 2 seconds of writes when idle-free, and never more than `max_pending_ops` operations plus meta's own bound.
What survives is always a prefix of the operations of each snapshot, cut at a batch boundary.
Every operation is atomic: a rename, a create, or a write that reached the set-content stage is either fully there or fully absent.
File content updates are committed when the file's data is flushed, which can be later than namespace operations that followed the write, as with delayed allocation elsewhere.

`flush(ino)` does nothing, as the trait allows.
`fsync(ino)` flushes that file's data, commits the snapshot's whole queue (so the file's name and its parents are durable), then calls `Meta::sync`, which runs the store sync first.
`data_only` does not change what is done, because a commit is one transaction either way.
Dropping the last `Core` handle stops the flusher and does the same as `sync`.

### Ordering with the store (no dangling chunk)

A meta commit must never be durable while naming a chunk that is not.
Every chunk is `Store::put` before the set-content operation that names it is queued.
`Options::before_sync` of meta is set to `Store::sync`, and meta runs it before every durable commit, including its own periodic ones and `Meta::sync`.
A durable meta state therefore only names blocks that were put before it, and `Store::sync` made every earlier `put` durable.
A commit that is not durable can be rolled back by a crash, and then names nothing.
The crash tests check this by cutting the store and meta files and then walking every chunk list.

## File data

A regular file is a list of `ChunkRef` plus a size.
Bytes in `[sum of chunk lengths, size)` are an implicit trailing hole.
A hole inside the file is a chunk ref whose id is all zero bytes (`BlockId::from_bytes([0; 32])`), with any length up to 1 GiB, that reads as zeros and is never put in the store.
That lets a write at the end of a 1 TiB sparse file cost a handful of chunk refs and no memory.
`ChunkRef.id` all-zero can never be a real BLAKE3 output in practice, and #10 has to skip it when marking (request 3).

Each loaded file keeps `Arc<[ChunkRef]>`, the prefix ends of its chunks (binary search from offset to chunk), and its dirty extents: a sorted map of disjoint, non-adjacent `(offset, bytes)` runs.

### Write

1. Take the file's write lock.
2. Extend the size if the write ends past it (the gap is a hole, nothing is allocated).
3. Merge the bytes into the extent map (sequential appends extend the last extent in place).
4. Update mtime and ctime, record the node dirty.
5. Release the lock, then apply memory pressure if needed.

Writers to one file are serialised by that lock, so an overlapping write is applied wholly before or after another one, never torn.
Writers to different files do not contend.
Readers of a file take the read lock only long enough to copy the chunk list handle and the overlapping dirty bytes, then fetch blocks with no lock held.

### Flush of file data (partial-chunk write algorithm)

For each dirty extent `[a, b)`, in order:

1. If `a` is past the end of the chunk list, a hole ref covers the gap (`[total, a)`), and the region starts at `a`.
   If `a == total`, the last existing chunk is pulled into the region so that content-defined boundaries continue as if the file had been written in one go, which is what makes an identical file written in different ways dedup.
2. Otherwise the region starts at the start of the chunk that contains `a`.
   The old bytes before `a` in that chunk are read (verified) and prepended.
3. If `b` falls inside a chunk, the old bytes after `b` in that chunk are read and appended, and the region ends at that chunk's end.
   Chunks wholly inside `[a, b)` are not read.
4. The region is cut with the store's FastCDC, and each piece is `put`.
5. The chunk refs of the replaced chunks are replaced by the new ones.

Only the modified region is re-chunked.
The tail of a region is a forced boundary at an old boundary, so a region can leave one short chunk; repeated overwrite of one place cannot grow the list, because the region always covers whole old chunks.
After all extents, the new chunk list, size and a set-content operation are queued, replacing an earlier queued set-content for the same inode.
Chunk list, size and times are committed by one meta operation, in one transaction.

### Truncate

Shrink: flush the file's data, drop chunks past the new size, and rewrite the one chunk that contains the new size (its head is re-put).
Grow: raise the size (a trailing hole).
Truncate to zero drops the list.
Nothing proportional to the size of a hole is ever allocated or read.

### Read

Chunk data comes through a block cache (`Options::block_cache_bytes`, default 128 MiB, two-generation) in front of `Store::get`, which verifies BLAKE3.
A verification failure is `Error::Corrupt` (EIO), never wrong data, and the cache never holds an unverified block.
Blocks that were just put are cached.

## Caches and invalidation

- Dentry cache: per directory, name to `Some(child, kind)` or `None` (a negative entry), bounded to `Options::dentry_cache` entries, evicting clean entries only.
  This is where the cargo warm-build lookups land (about 14,000 missing and 2,000 existing per rebuild, issue #18).
- Node table: inode to `Arc<Node>` (attributes, symlink target, file data), evicting nodes that are clean, unreferenced, have no handle and are not in use by a running operation.
- Block cache as above.

Every mutation goes through `Core`, so the caches stay exact without invalidation messages.
The one race is a reader that misses, reads meta before a commit, and inserts the stale answer after that commit made the entry clean and evictable.
Each cache shard has an epoch that increases on every mutation, on every commit and on every eviction, and a fill is only inserted if the epoch did not move since the reader started.
Control operations that change a snapshot's content from the outside (remove) purge entries by snapshot id; fork and create do not touch existing snapshots.

## Handles, forget, unlink while open

- `lookup`, `create`, `mkdir`, `symlink`, `link` add one reference to the returned inode.
  `forget(ino, n)` removes `n`.
  `open` adds a handle, `release` removes it.
  Removing more than exists is counted (`Stats::forget_underflows`) and clamped, never wrapped.
- A node stays in memory while it has a reference, a handle, dirty state or is in flight.
- Deleting the last name (`unlink`, a replacing `rename`, `rmdir`) makes the node an orphan: `nlink` 0.
  If it has a reference or handle, its chunk list, symlink target and xattrs are loaded first, so it stays fully readable and writable.
  Its data is still chunked into the store, but no set-content operation is queued, because meta drops the inode with its last name.
  When the last reference and handle go, the node is removed and every operation on it is `Stale`.
- An orphan node is not dropped while its removal is uncommitted, so a not-yet-committed unlink cannot be undone by a later reload from meta.
- A file created and unlinked within one batch window, with no other structural operation on it, is removed from the queue and never reaches meta (its inode number is simply never assigned).
- GC (#10) must treat the chunks of open orphans and of blocks put after its mark started as live: `Core::pinned_blocks()` returns the chunk ids held by open orphans and by files with an uncommitted chunk list.
- Data of an orphan is only in the store and in memory, so it is lost on a crash, as POSIX allows.

## Errors

One table, `error::from_meta` and `error::from_store`, tested case by case.

| Source | `cowfs_vfs::Error` |
|---|---|
| meta `NotFound` | `NotFound` (or `Stale` where the inode itself is the subject) |
| meta `Exists`, `NotDir`, `IsDir`, `NotEmpty`, `NameTooLong`, `NoAttr` | the same names |
| meta `Invalid`, `TooBig` | `InvalidArgument`, `Range` |
| meta `NoSuchSnapshot`, `SnapshotExists` | `Stale`, `Exists` |
| meta `Corrupt`, `Inconsistent` | `Corrupt` |
| meta `Storage`, `Hook` | `Io` |
| store `HashMismatch`, `Corrupt` | `Corrupt` |
| store `NotFound` | `Corrupt` (a chunk that meta names must exist) |
| store `Io` | `Io` (`NoSpace` for ENOSPC) |
| store `BlockTooLarge`, `BadPack`, `Locked` | `Io` |

## Concurrency and locks

Order: namespace lock, then flush lock, then node locks, then the queue mutex, then a cache shard mutex.
No lock is held across a store `get` for a read, or across a meta commit, except the flush lock (which only other committers wait on) and the write lock of the one file being flushed to the store.
Lookup, getattr, read and readdir of a clean directory take no namespace or flush lock, so they are never queued behind a commit.
A batch commit is done with no namespace lock held, so namespace operations continue while it runs.

## Cost

| Operation | Cost |
|---|---|
| lookup, cache hit | one shard lock |
| lookup, miss | one meta read (about 41 us at 1M inodes), then cached |
| create, mkdir, symlink, link, unlink, file rename | O(1) memory, one queued op |
| batch commit | one meta transaction for the whole batch |
| snapshot create | O(1) plus the flush of pending ops |
| write | O(bytes) copy |
| flush of file data | O(dirty bytes), plus at most two boundary chunks read per extent |
| truncate | O(size of one chunk), plus the flush of dirty data |
| read | O(bytes read) plus block fetches |
| readdir | O(entries returned); a barrier if the directory has pending changes |
| rmdir | O(1) when deferred, else one barrier |

## Requests of store and meta

None of these blocks the crate, each has a workaround stated here.

1. `Meta::rename_snapshot(id, new_name)` (atomic, keeps the id).
   Workaround: fork plus remove, which changes ids and is not atomic.
   Also a way to replace snapshot `base` by another in one transaction for `promote_base`.
2. `Snapshot::batch_at(now: Timestamp, f)` or `Tx::set_now`, so that ctime (and creation times) of deferred operations are the times the operations happened.
   Workaround: atime and mtime are restored with `setattr`; ctime in meta is the batch time, up to `flush_interval` late.
   Cached ctime is exact while the node is cached.
3. GC (#10) must skip chunk refs with an all-zero id (holes), must not free blocks named only by an open orphan or by an uncommitted chunk list (`Core::pinned_blocks`), and must not free a block put after its mark started.
   A first-class hole flag in `ChunkRef` would be cleaner; `ChunkRef` is in the store crate.
4. `Meta::reserve_inodes(n)` (or `Tx::create_with_ino`), which would remove the virtual inode numbers and the alias table.
5. `Snapshot::chunks` reads the whole chunk list.
   A ranged read (`chunks_at(ino, offset)`) would keep the first read of a multi-GiB file from loading every segment.
   Not needed at the measured sizes.

## Decisions for the lead to review

1. Virtual inode numbers with an alias table, in place of asking meta for a reservation.
2. Directory rename, replacing a directory, and readdir of a directory with pending changes are barriers.
3. Timestamps: atime and mtime exact, ctime in meta is batch time (request 2).
4. `rename_snapshot` and `promote_base` change ids and are not atomic (request 1).
5. Holes are zero-id chunk refs (request 3).
6. No atime update on read.
7. Data lost on crash is bounded by the write-back limits above; content updates can commit later than later namespace operations.
8. Snapshot removal is refused while a handle is open in it, and makes its inode numbers `Stale` even when references remain.

## Tests

Listed with results in the "Measurements and results" section once they have run.
