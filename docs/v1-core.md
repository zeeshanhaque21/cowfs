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
| virtual | `1 << 63 \| (snapshot id << 40) \| n` | a file created through this `Core` that was not yet committed when its number was handed out |

The meta-derived form is computed, not stored, so it needs no table, is stable across restarts, and is unique across snapshots because the snapshot id is part of it.
Two snapshots that share content report different numbers for the same file, so `find -samefile` and `rsync -H` do not see them as hardlinks.
Snapshot roots are `(id << 40) | 1`.
An inode whose snapshot id is not a live snapshot is `Stale`.

The virtual form is `1 << 63 | (snapshot id << 40) | n`, where `n` comes from a counter.
The virtual form exists because `create`, `mkdir` and `symlink` must return an `Ino` at once, while the write-back layer commits them later, and meta only assigns inode numbers inside a transaction.
When the batch that creates the file commits, meta reports its real inode number and `Core` records the pair in an alias table (virtual to meta, and meta to virtual).
From then on `lookup`, `readdir` and every other path canonicalise the meta number to the virtual one, so a file has exactly one `Ino` while anything can still hold it.

**The counter is durable.** `<root>/virt.ino` holds the highest number handed out, written and synced in blocks of 2^20 before any number of the block is used (`Options::alias_batch` is unrelated; the block size is `VIRT_BLOCK`).
A crash can therefore waste numbers but never reuse one, and a number from an earlier session is `Stale` rather than another file's data.
The mark is written twice (value, then the same value again), so a torn write is refused at open instead of believed.
Tests: `virtual_inode_numbers_are_never_reused_across_a_restart` (a clean reopen) and `the_virtual_number_reservation_survives_a_crash` (a child process that aborts without syncing, three times in a row).

**Aliases are released.** Once a file is committed, has no unflushed bytes, is not unlinked, and no caller holds a reference or a handle, its alias is dropped and its number reverts to the meta-derived one.
The dentry cache stores meta-derived numbers only and canonicalises every hit, so releasing an alias cannot leave a cached name pointing at a number nothing knows (`DCache::retarget` re-points a committed create's entry, and only if it still names that inode, so a rename or unlink in between is not undone).
A released number used again as a directory gives `Stale`, not a silent `NotFound` (`Inner::is_released_virt`).
Tests: `aliases_drain_for_committed_files_with_no_references` (20,000 creates leave 1 alias and 2 nodes; 500,000 leave 130), `a_referenced_file_keeps_its_number_across_a_flush`, `live_blocks_filters_holes_and_yields_only_stored_blocks`.

Snapshot ids and meta inode numbers are never reused, so a number never names two files.
The meta-derived number is exactly `Meta::pack_ino(snapshot id, meta inode)`, so the layout is meta's and restart stable by construction.
A test (`an_inode_number_is_never_reused_for_another_file`) checks that no number names two files across unlink and create, snapshot delete and create, and two restarts.

## The synthetic root

`ROOT_INO` is a read-only directory.
Its entries are the snapshots, named as the snapshot is named, with the snapshot root directory's attributes.
Its cookies are snapshot ids (never reused, independent of inode numbers), so a listing is exactly-once under create and remove.
Its `nlink` is 2 plus the number of snapshots.
`create`, `mkdir`, `symlink`, `link`, `unlink`, `rmdir`, `rename`, `setattr` with a size, `write`, and `setxattr` on the root return `ReadOnly`; `rename` between two snapshots is `CrossDevice`; `link` across snapshots is `CrossDevice`.
`setattr` of mode or times on the root is accepted and ignored.

Snapshot names follow exactly the rule `cowfs-ctl` enforces (`crates/cowfs-ctl/src/validate.rs` on branch `v1/13-cli`), which is what the CLI's tests pin: non-empty, at most 255 bytes, no `/`, no control character (NUL, newline, ESC and tab included), no leading `.` (which also rules out `.`, `..`, `._*` and `.nfs*`).
The collision key of a name is NFC, lowercased, NFC again (`cowfs_core::name_key`), so `Café`, `cafe` + combining acute and `CAFÉ` are one name and a backend must refuse to hold two.
The rules and the test table are copied into one small module, `src/snapname.rs`, so the two can be reconciled into a shared crate when the branches land (see "Requests of store and meta").
The API takes `&str`, so a name is UTF-8 by construction; `validate_snapshot_name_bytes` is what an adapter or the control API calls on bytes that came off a wire, and it refuses non-UTF-8.
Test: `snapshot_names_follow_the_cli_rules`.

## Snapshot replacement is crash-safe

`cowfs-meta` has no atomic rename of a snapshot, so `Core` cannot replace a name in one transaction.
Every replacement (`rename_snapshot`, and `promote_base` which may overwrite) goes through `src/swap.rs`:

1. fork the source into a staging name (a failure here changes nothing),
2. write and sync the intent file `<root>/swap-<target>`, naming the staging and target snapshots,
3. remove the old target,
4. fork the staging snapshot into the target name,
5. remove the staging snapshot,
6. remove the intent file.

Any failure before step 3 leaves the old target untouched.
Any failure or crash from step 3 on leaves the intent file, and the next `Core::open` finishes steps 4 to 6 before it serves anything.
So a snapshot name never disappears without a record that explains it, and a failed swap never costs the old base.
The two forks change the snapshot id, so every inode number in the new snapshot differs from the old one's.

Test: `promote_base_survives_a_failure_at_every_step` injects a failure at each of the five steps (`Core::set_swap_fault`, a doc-hidden test seam), checks the content before the reopen, reopens, and checks that recovery completed the swap or left the old base, with no staging snapshot and no intent file left.

## One damaged block poisons one file, not the snapshot

A read-modify-write of a damaged chunk fails at the write, not later.
`Inner::verify_partial` checks every stored chunk that a write only partially covers before the write is recorded, so `write` returns `EIO` (`Error::Corrupt`) instead of silently writing new bytes around garbage; a fully covered chunk is never read, because its old bytes are not needed.
When a file's flush fails anyway (for example a store cut by an external tool), the file is poisoned: its node records the reason and every later operation on that file (`read`, `write`, `setattr` with a size, `fsync`, `flush`) returns it.
The rest of the snapshot's queue still commits, other files' `fsync` still makes their data durable, and `fork`, `readdir` barrier and `sync` on that snapshot keep working, because a poisoned file's data is simply not queued (its bytes stay in memory and stay counted in `Stats::dirty_bytes`).
The file's dirty bytes are dropped when the inode is reclaimed, that is when it is unlinked and the unlink commits; `Stats::poisoned` counts the events.

Tests: `a_damaged_chunk_fails_its_own_file_and_nothing_else`, `a_damaged_chunk_found_by_a_truncate_poisons_only_that_file`, `a_healthy_file_is_durable_after_its_fsync_while_another_is_poisoned`, `a_damaged_block_does_not_stop_a_promote`.

## Control plane

| Method | Cost | Notes |
|---|---|---|
| `create_snapshot(name)` | one durable meta commit | empty tree |
| `fork_snapshot(src, name)` | O(1): flush of `src`'s pending operations, then one meta row | writable clone |
| `remove_snapshot(name)` | proportional to nodes unique to it | refused with `Busy` while any handle is open in it |
| `rename_snapshot(old, new)` | the staged swap in `src/swap.rs` | crash-safe and error-safe (see "Snapshot replacement is crash-safe"); changes the snapshot id and so every inode number in it, refused while handles are open |
| `promote_base(src, base)` | the staged swap, replacing an existing `base` | crash-safe and error-safe; two O(1) forks |
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

The queue also has a hard bound: an operation that finds four times `max_pending_ops` queued flushes in its own thread, so the queue is bounded even with `Options::background` off.
Since the meta rewrite, meta has its own timer and group commit, so `Meta::sync` from the flusher and from `fsync` shares a commit with whatever else is pending.
`Core` only decides when its queue becomes a meta batch; when that batch becomes durable is meta's policy plus `fsync` and `sync`.

`flush(ino)` does nothing, as the trait allows.
`fsync(ino)` flushes that file's data, commits the snapshot's whole queue (so the file's name and its parents are durable), then calls `Meta::sync`, which runs the store sync first.
`data_only` does not change what is done, because a commit is one transaction either way.
Dropping the last `Core` handle stops the flusher, does the same as `sync`, and then calls `Meta::close` (whose error a drop cannot report; call `Core::sync` first when the result matters).

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
  A cached `child` is always a meta-derived number; a create that has not committed yet is the one exception and is re-pointed at its meta number when the batch commits (`DCache::retarget`), so a released virtual alias can never leave a cached name pointing at a number nothing knows.
  This is where the cargo warm-build lookups land (about 14,000 missing and 2,000 existing per rebuild, issue #18).
- Node table: inode to `Arc<Node>` (attributes, symlink target, file data), evicting nodes that are clean, unreferenced, have no handle and are not in use by a running operation.
- The pin set for GC is `Core::pinned_blocks`: every block of an open orphan and of a chunk list that is not committed yet.
  It answers exactly for the state at the end of the call, or `ControlError::Busy` if a node lock could not be taken within two seconds; it is never partial, and no lock of ours is held while it waits.
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

## Opening a damaged store

`Core::from_parts` refuses to open when `Store::recovery().has_corruption()` (durable bytes were lost to damage, or the watermark is missing and there are gaps): the error is `Corrupt`.
The store quarantines the damaged region and never serves it, so refusing is a policy choice of the mount layer, not a safety requirement.
Test: `a_store_that_lost_durable_data_is_not_opened`.

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

There is ONE global order, and it is enforced by construction rather than by inspection:

1. `SnapCtx::ns` (namespace), then `SnapCtx::flush`, then `SnapCtx::q`.
2. The node write lock of the file being written (`Node::st`), then the queue mutex, then a cache shard mutex (`nodes`, `dents`, `blocks`).
3. **The meta lock is a leaf.** No lock of ours is taken while a meta read or write transaction is open, and no lock of ours is held while a meta transaction is opened.
4. `snaps`, `aliases`, `handles`, `root_time`, `pressure`, `unsynced` and `last_error` are leaves: taken alone, or with the SnapCtx locks above them and never below them.

Rule 3 is the one that matters and the one that was broken twice:

- `ensure_file` and `ensure_target` used to hold the node write lock while reading a file's chunk list, while `commit` read every touched node's state inside the meta writer lock (`restore_state`).
  That is a cycle: a reader waits for the meta writer, the committer waits for the node.
  The critic reproduced it as a hang (`d1_deadlock_ensure_file_vs_commit`, "no progress for 10 s, progress=7211"), and the stack showed both sides: readers in `op_read -> ensure_file -> Snapshot::chunks` and the committer in `Core::flush -> commit -> Snapshot::batch -> mutate`.
  Now `ensure_file` reads meta first and publishes under the node lock, and `commit` takes a snapshot of every touched node's `(mode, atime, mtime)` (`Inner::restore_states`) before it opens the batch.
- `ShardMap::retain` used to take a blocking node read lock while holding a shard lock, and `try_reclaim` takes a node lock and then the shard lock.
  Every closure that runs under a shard lock now uses `try_read` and keeps the entry it cannot read, so a shard lock is never held across a node lock.

Full audit of every lock-taking function, generated by `python3 scripts/lock_audit.py --write` and
checked by `tests/critic2b.rs::every_lock_site_is_in_the_audit_table`, which fails if a function
that takes a lock is not listed here.

| Site | Locks held together | Order |
|---|---|---|
| `blocks::insert` | gens | leaf |
| `blocks::get` | gens | leaf |
| `blocks::clear` | gens | leaf |
| `dcache::shard` | leaf | 1 |
| `dcache::bump_all` | leaf | 1 |
| `dcache::len` | leaf | 1 |
| `dcache::purge_snapshot` | leaf | 1 |
| `dcache::shrink_all` | leaf | 1 |
| `file::truncate` | blocks | ? |
| `file::put_piece` | blocks | ? |
| `file::flush_extent` | blocks | ? |
| `file::verify_partial` | blocks | ? |
| `file::read_range` | blocks | ? |
| `inner::snapctx_id` | leaf | 1 |
| `inner::all_snaps` | leaf | 1 |
| `inner::reserve_virt` | virt_lock | leaf |
| `inner::drop_stale_aliases` | nodes, aliases | 2 then leaf |
| `inner::meta_of` | aliases | leaf |
| `inner::canon` | aliases | leaf |
| `inner::flushed_of` | leaf | 1 |
| `inner::load_node` | nodes, aliases, snap. | 2 then 3 then leaf |
| `inner::virt_committed_without_alias` | nodes, aliases | 2 then leaf |
| `inner::live` | st.rd | 2 |
| `inner::dir` | st.rd | 2 |
| `inner::shrink_nodes` | st.try_read, nodes | 2 |
| `inner::dent_lookup` | dents, snap. | 2 then 3 |
| `inner::ensure_file` | st.wr, st.rd, snap. | 2 then 3 |
| `inner::ensure_target` | st.wr, st.rd | 2 |
| `inner::preserve_orphan` | st.wr, st.rd | 2 |
| `inner::maybe_drop_alias` | nodes, aliases | 2 then leaf |
| `inner::try_reclaim` | st.wr, st.rd, nodes, aliases | 2 then leaf |
| `inner::queue_content` | sc.q | 1 |
| `inner::flush_node` | st.wr | 2 |
| `inner::flush_data` | sc.q, nodes, last_error | 1 then 2 then leaf |
| `inner::flush_snapshot` | sc.flush, last_error | 1 then leaf |
| `inner::flush_locked_snapshot` | sc.q | 1 |
| `inner::commit_batch` | sc.q, nodes, dents, aliases, unsynced | 1 then 2 then leaf |
| `inner::restore_state` | st.rd, nodes | 2 |
| `inner::commit` | aliases, snap. | 3 then leaf |
| `inner::sync_all` | unsynced | leaf |
| `inner::fsync_snapshot` | unsynced | leaf |
| `inner::barrier` | sc.flush, last_error | 1 then leaf |
| `inner::flush_namespace_locked` | sc.q | 1 |
| `inner::tick` | sc.q, unsynced | 1 then leaf |
| `inner::relieve` | pressure | leaf |
| `io::file_node` | st.rd | 2 |
| `io::op_read` | st.rd | 2 |
| `io::op_write` | sc.q, st.wr, st.rd, last_error | 1 then 2 then leaf |
| `io::op_setattr` | sc.q, st.wr, st.rd | 1 then 2 |
| `io::op_readlink` | st.rd | 2 |
| `io::op_open` | handles | leaf |
| `io::op_release` | nodes, handles | 2 then leaf |
| `io::op_getxattr` | st.rd | 2 |
| `io::op_listxattr` | st.rd, snap. | 2 then 3 |
| `io::op_setxattr` | sc.ns, st.wr | 1 then 2 |
| `io::xattr_exists` | st.rd, snap. | 2 then 3 |
| `io::op_removexattr` | sc.ns, st.wr | 1 then 2 |
| `lib::drop` | leaf | 1 |
| `lib::from_parts` | nodes, dents, aliases, handles, root_time, pressure, unsynced, last_error, virt_lock | 2 then leaf |
| `lib::fork_snapshot` | snap. | 3 |
| `lib::list_snapshots` | leaf | 1 |
| `lib::merkle_root` | snap. | 3 |
| `lib::last_flush_error` | last_error | leaf |
| `lib::live_blocks` | snap. | 3 |
| `lib::add_snap` | leaf | 1 |
| `lib::snap_by_name_raw` | leaf | 1 |
| `lib::check_new_name_except` | leaf | 1 |
| `lib::register` | root_time, last_error, snap. | 3 then leaf |
| `lib::unregister` | sc.ns, sc.flush, sc.q, st.try_read, nodes, dents, aliases, root_time | 1 then 2 then leaf |
| `lib::stats` | sc.q, nodes, dents, aliases | 1 then 2 then leaf |
| `lib::drop_caches` | st.try_read, nodes, dents | 2 |
| `node::try_read_for` | st.try_read | 2 |
| `ns::root_attr` | root_time | leaf |
| `ns::maybe_wake` | sc.q | 1 |
| `ns::op_getattr` | st.rd | 2 |
| `ns::op_lookup` | st.rd | 2 |
| `ns::root_lookup` | st.rd | 2 |
| `ns::op_readdir` | st.rd, dents | 2 |
| `ns::root_readdir` | leaf | 1 |
| `ns::make` | sc.ns, sc.q, st.wr, st.rd, nodes, dents | 1 then 2 |
| `ns::op_link` | sc.ns, sc.q, st.wr, st.rd, dents | 1 then 2 |
| `ns::op_unlink` | sc.ns, sc.q, st.wr, st.rd, dents | 1 then 2 |
| `ns::op_rmdir` | sc.ns, sc.q, st.wr, dents | 1 then 2 |
| `ns::require_empty` | st.rd | 2 |
| `ns::op_rename` | sc.ns, sc.q, st.wr, st.rd, dents | 1 then 2 |
| `ns::adjust_kids` | st.wr | 2 |
| `ns::rename_dir` | st.wr, dents | 2 |
| `ns::refresh_dir_attr` | st.wr, snap. | 2 then 3 |
| `swap::recover` | last_error | leaf |
| `swap::swap_snapshot` | last_error | leaf |
| `swap::stage_and_intent` | snap. | 3 |
| `swap::finish_swap` | snap. | 3 |
| `util::shard` | leaf | 1 |
| `util::bump_all` | leaf | 1 |
| `util::len` | leaf | 1 |
| `util::retain` | leaf | 1 |

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

State at the merge of `origin/v1/8-9-meta` (commit 9abe96e) and `origin/v1/7-block-store` (commit 1c00c1e), plus `origin/v1/vfs-test` (5cc6008) and `origin/v1/vfs-trait` (1825081).
None of these blocks the crate, each has a workaround stated here.

Landed and adopted: `before_sync` after the batch closure, `Meta::close`, `Meta::pack_ino`, the durable inode reservation (numbers are never reused), the store's `corrupt_synced` refusal, the store's v2 pack format and recovery classification, the new error variants (mapped in the error table), the store op log.
Landed and not yet used by `Core`: `Snapshot::chunk_range`, `Tx::splice_content` (with the version check) and `Error::NeedsRechunk`.
`Core` still reads a file's whole chunk list on first use and commits it with `set_content`, which is O(chunks in the file) per commit.
Moving to `chunk_range` and `splice_content` is the next step for multi-GiB files and is not needed at the measured sizes.

Not landed:

1. `Meta::rename_snapshot(id, new_name)` (atomic, keeps the id).
   Workaround: the staged swap with an intent record in `src/swap.rs` (see "Snapshot replacement is crash-safe"), which needs no meta change and survives a crash or an error at every step.
   It still changes the id (two forks), which the lead may or may not care about.
2. `Snapshot::batch_at(now: Timestamp, f)` or `Tx::set_now`, so that ctime (and creation times) of deferred operations are the times the operations happened.
   Workaround: atime and mtime are restored with `setattr`; ctime in meta is the batch time, up to `flush_interval` late.
   Cached ctime is exact while the node is cached.
3. A first-class hole flag in `ChunkRef` (`ChunkRef` is in the store crate).
   Workaround: an all-zero block id is a hole.
   GC (#10) must use `Core::live_blocks`, which filters hole refs, and must not free blocks named only by an open orphan or by an uncommitted chunk list (`Core::pinned_blocks`), and must not free a block put after its mark started.
4. `Meta::reserve_inodes(n)` or `Tx::create_with_ino`, which would remove the virtual inode numbers and the alias table.
   Meta now reserves durably inside itself, but a caller still cannot get a number before the transaction that creates the inode.
5. A crate for the snapshot-name rule shared by the backend and the control API.
   `cowfs_core::validate_snapshot_name`, `validate_snapshot_name_bytes` and `name_key` are copied from `cowfs-ctl`'s `validate.rs` with its test table (`src/snapname.rs`) because the two are on different branches; they should become one crate, and `validate.rs` should then depend on it instead of duplicating it.

## Decisions for the lead to review

1. Virtual inode numbers with an alias table, in place of asking meta for a reservation.
   The counter is durable in `<root>/virt.ino` and aliases are released once nothing can hold their number, so the table is bounded (130 aliases after 500,000 creates).
2. Directory rename, replacing a directory, and readdir of a directory with pending changes are barriers, but a barrier commits only the namespace.
   Unrelated files' dirty data is not flushed: a `readdir` of one directory with 48 MiB of dirty data in another file costs 0.14 ms at the median and never chunks the unrelated data (test `a_directory_barrier_does_not_flush_unrelated_file_data`; before the fix the same call flushed all 48 MiB and took 5.9 s on this loaded box).
3. Timestamps: atime and mtime exact, ctime in meta is batch time (request 2).
4. `rename_snapshot` and `promote_base` go through a staged swap with a crash-safe intent record; they change ids and are atomic only in the sense that a name is never lost (request 1).
5. Holes are zero-id chunk refs; `Core::live_blocks` filters them for GC (request 3).
6. No atime update on read.
7. Data lost on crash is bounded by the write-back limits above; content updates can commit later than later namespace operations.
8. Snapshot removal is refused while a handle is open in it, and makes its inode numbers `Stale` even when references remain.
9. `statfs` free space counts pack bytes, so blocks of a file that was flushed to the store and then deleted are only returned by GC (#10).
   The conformance check `statfs_free_after_unlink` passes only while the deleted file's data is unflushed, so the suite runs with the background flusher off (`tests/conformance.rs`).
   The lead should decide whether that check should stay at the `Cowfs` level before GC exists.
10. The `Core` open path refuses a store that lost durable data (see "Opening a damaged store").
11. Two conformance-relevant readings that changed with the revised suite: `readdir` with `max` 0 is `InvalidArgument`, and an xattr name with a NUL is `InvalidArgument`.
12. Snapshot names follow `cowfs-ctl`'s rule and collision key, copied into `src/snapname.rs` rather than depended on, until one shared crate exists (request 5).
13. A file whose flush fails is poisoned for the life of the mount: every operation on it reports the store error and the rest of the queue keeps committing.
14. Virtual inode numbers come from a durable mark in `<root>/virt.ino`, and an alias is released as soon as nothing can hold its number, so a number never names two files and the table stays bounded.

## Tests

All in `crates/cowfs-core/tests` unless noted.
Counts are from the runs recorded below (`cargo test -p cowfs-core` after merging the meta fixes).

| Category | File | Tests | What it proves |
| Unit | `src/` | 14 | inode shapes, alias table, virtual-number mark (including a torn one), error tables (meta and store, case by case), file extents, holes, truncate, append chunking, snapshot-name rules and collision keys |
| Conformance | `conformance.rs` | 132 pass, 2 heavy | the whole `cowfs-vfs-test` suite through a `Core` snapshot view (levels Posix, Portable, Cowfs), plus the 2 heavy checks run separately |
| Core behaviour | `core.rs` | 18 | mount root, snapshot rules, fork isolation both ways, persistence, dedup by byte counts, 1 TiB sparse file with RSS bound, forget accounting over 100,000 create and unlink cycles, batching, corrupt block is EIO, damaged store refused, Merkle root iff content, control plane, unlink while open, elision, background flusher, inode numbers never reused |
| Locks | `locks.rs` | 2 | the critic's deadlock repro as a test with a stack-dumping watchdog, plus a 60 s mixed stress of every multi-lock operation |
| Swap | `swap.rs` | 3 | failure injected at every step of a snapshot replacement, the intent record's recovery on reopen, a damaged store inside a swap, rename failure safety |
| Poison | `poison.rs` | 3 | a damaged chunk is EIO at the write, poisons only its own file, leaves other files' `fsync` durable, and does not stop a promote |
| Names and inode numbers | `names_ino.rs` | 4 | the CLI's name rules and collision keys, virtual numbers never reused across a clean reopen or a process that aborts |
| Aliases | `alias.rs` | 2 | the alias table stays bounded over 20,000 (and 500,000) creates, a referenced or open file keeps its number |
| Caches and blocks | `caches.rs` | 2 | a directory barrier does not flush unrelated data, `live_blocks` filters holes and yields only stored blocks |
| Ported from the critic | `critic.rs` | 3 pass, 4 heavy | truncate leaves no stale bytes, a store that lost durable data is refused by name, reader stall during a slow write; heavy: fsx, 180 s hammer, barrier storm, 500,000 files |
| Model | `model.rs` | 2 proptests | `Core` against `MemVfs` on random sequences including forks, handles, restarts and cache drops, with tiny and default cache sizes |
| Partial chunks | `chunks.rs` | 2 proptests, 2 boundary tests | random write, truncate and extend against a `Vec<u8>`, offsets and lengths on and around 16 KiB, 64 KiB and 256 KiB, with eager flushing and with cache drops; sequential appends dedup with a one-shot write; writes ending one byte before, at and one byte after real chunk boundaries read back byte for byte |
| Crash images | `crash.rs` | 2 | power-loss simulation, see below, plus a negative control that must fail; built through `Core::open_with_meta` so the store sync hook is wired by the production path |
| kill -9 | `kill9.rs` | 1 (plus the child) | SIGKILL of a writing process with the background flusher on, through `Core::open` |
| Stress | `stress.rs` | 3 | overlapping writers to one file, many files with snapshot forks and renames, directory churn, all under a deadlock watchdog |

The 8,000 hardlink pairs, delete-while-listing and the read-only-mode 0444 and 0400 checks (20 rounds of 8 MiB with fsync) are conformance checks and run through `Core`.

### Crash injection (`crash.rs`)

The metadata database sits on a recording redb backend and the store is a real directory.
A seeded workload of creates, overlapping writes, truncates, renames, unlinks, hardlinks, snapshot forks, `fsync` and flushes runs on a `Core`.
At random operation boundaries the test records the length of the backend log, a copy of the store files and what the model says is durable.
Each point becomes four crash images: metadata rebuilt from the log with everything, with only synced writes, and twice with synced writes plus a random subset of later writes each possibly torn; the store's last pack is cut at a random byte beyond the durable watermark, and the index checkpoint is kept or dropped at random.
Every image is reopened and must: open without corruption, pass `Meta::check`, have a clean `Store::fsck`, contain every snapshot that had an `fsync`, read back every file untouched since its `fsync` byte for byte (a dangling chunk is a read error and fails the test), read every other file without error, accept a new fsynced write, and survive a second reopen.
A file changed after its last `fsync` is only required to read without error, because either version is allowed.
The negative control runs the same workload with the store sync removed from the meta hook and must find a dangling chunk; it does (`Corrupt("block ... named by a file is missing")`), so the test can see the bug it guards against.

### Kill -9 (`kill9.rs`)

A child process runs a workload (truncate and rewrite of 40 files with sizes up to 600 KB, a create and unlink churn) with the background flusher at 30 ms and `fsync` every fifth step, printing progress.
The parent kills it at a random moment between 0.4 and 2.6 s, reopens, and checks: no corruption reported, `check()`, clean `fsck`, and every file whose last fsynced step is known holds one of the contents written at or after that step (or is empty from a truncate).

## Results

Machine: Apple M3 Max, APFS, shared with about 20 other sessions, load1 between 21 and 205 during the runs below, so every timing is a noisy lower bound.

| Run | Result |
|---|---|
| full verification command (fmt, clippy -D warnings, `cargo test --workspace`, `cargo doc`) after the store, meta, vfs-test and vfs merges | exit 0; 657 s wall at load1 32 to 56 |

The two mutants the first round of this review survived are now killed: `m01` (the store sync hook not wired in the production path) by `crash.rs`, which now goes through `Core::open_with_meta` so the wiring is in production code, and `m05` (a read-modify-write that drops the last byte) by the new real-chunk-boundary test in `chunks.rs`.

Defaults are smaller than these runs to keep `cargo test --workspace` short: 1 crash workload of 60 operations, 8 kill rounds, 32 and 24 proptest cases.
`COWFS_CRASH_SEEDS`, `COWFS_CRASH_OPS`, `COWFS_KILL_ROUNDS`, `PROPTEST_CASES`, `LOCK_STRESS_SECS`, `HAMMER_SECS`, `FSX_OPS`, `FSX_SEED`, `N_FILES` and `ALIAS_FILES` raise them.

## Measurements

### Benchmark

Source: `cargo run --release -p cowfs-core --example core_bench` (`COWFS_BENCH_QUICK=1` for a smoke run, `COWFS_BENCH_ONLY=seq` and `COWFS_BENCH_REPEAT=n` to run one section repeatedly).
Every timed batch ran under the shared CPU lock, n=5, median shown, ranges in the raw output.
Load1 was 54 at the start and 47 at the end and between 66 and 111 during the rows, so every row is flagged high load.
The baseline is the same operation with `std::fs` on the same APFS volume.
"time ratio" is the median time of cowfs-core divided by the median time of the baseline, so below 1 means faster than the baseline.

| Metric | cowfs-core | std::fs | time ratio |
| create 20,000 files (334 MiB, mixed sizes) then durable | 8,905 files/s | 4,002 files/s (no fsync) | 0.45 |
| lookup hit, cold caches, 10,000 names | 498,411 /s | 349,885 /s | 0.70 |
| lookup hit, warm | 667,000 /s | 365,445 /s | 0.55 |
| lookup miss, cold | 788,895 /s | 638,203 /s | 0.81 |
| lookup miss, warm (negative entries) | 2,718,869 /s | 460,138 /s | 0.17 |
| getattr of 10,000 held inodes, warm | 8,624,095 /s | 410,042 /s | 0.05 |
| cargo no-op lookup replay, cold (16,000 lookups: 2,000 hit, 14,000 miss, synthetic) | 697,884 /s | 545,933 /s | 0.78 |
| same replay, warm | 3,287,587 /s | 543,500 /s | 0.17 |
| sequential write 1 GiB with fsync | 239 MiB/s | 1,396 MiB/s | 5.84 |
| sequential read 1 GiB, caches dropped | 637 MiB/s | 11,326 MiB/s | 17.79 |
| random 4 KiB read, 256 MiB file, caches dropped | 16,003 /s | 914,042 /s | 57.12 |
| random 4 KiB write plus fsync, 5,000 ops | 2,554 /s | 35,638 /s | 13.95 |
| snapshot create (durable), 1,000 files | 95 /s (10 to 11 ms) | - | - |
| snapshot create (durable), 100,000 files | 71 /s (12 to 21 ms) | `cp -cR` 0.02 /s | 0.00 |
| floor: `Store::ingest_bytes` plus sync of 1 GiB, no Core | 347 MiB/s | - | - |
| floor: FastCDC plus BLAKE3 only | 658 MiB/s | - | - |

What they show and do not show:

- The lookup numbers are the ones the issue #18 workload depends on.
  Warm negative lookups are 0.33 us each and the synthetic replay of a cargo no-op build's lookup mix runs about 6 times faster than raw `std::fs` when warm and in the same range when cold.
  The replay is synthetic (a random mix of 2,000 hits and 14,000 misses over 200 directories), not a recording, so it is a rate, not a build time.
- The native baseline for data is the macOS page cache, which cowfs cannot match: sequential read is 17.8 times slower than a warm page-cache read and random 4 KiB reads 57 times, because each read of a cold block fetches, decompresses and hashes a whole block of up to 256 KiB.
  These rows are not within the 1.5x target of `docs/design.md` and the target is not claimed here.
  The target is about a `cargo build` and `git status`, which are dominated by the lookup rows and by writes of build output, neither of which this benchmark runs end to end.
- Sequential write is 239 MiB/s against a store-only floor of 347 MiB/s, at load up to 111 (range 3.7 to 8.2 s), so the gap to the floor is inside the noise and is not attributed to `Core`.
  In an earlier run at load 65 the two were 397 and 369 MiB/s.
- Snapshot create is flat between 1,000 and 100,000 files within the noise and is dominated by the durable commit.
  The 100,000-file tree took 3.1 s to build durably (32,657 files/s).
- One profile-guided fix was made: a cold lookup used to read meta twice (the lookup, then the inode for the node table).
  It now seeds the node table from the lookup's attributes.
  Its effect was not isolated at n=5 under this load, so no speedup is claimed; by construction it removes one meta read per cold hit.

### Lock discipline and stall measurements

| Measurement | Result |
|---|---|
| before the fix (same call, pre-`034db42` build) | flushed all 48 MiB (dirty 50,331,648 to 0) and took 5.95 s |
| same, 500,000 creates (`ALIAS_FILES=500000`) | 130 aliases, 131 nodes, RSS growth from 5,000 to 500,000 files 122 bytes per file; the residue is redb mapping a growing database file (967 bytes per file measured on `cowfs-meta` alone), not a per-file map |

The cold-read profile (`sample`, 26 windows of 2 s over a `core_bench --only seq` run at load1 86 to 98, 116,023 samples) splits the busy time as roughly 51 percent kernel write and read syscalls, 31 percent the benchmark's own data generator, 11 percent BLAKE3 over all samples and 6 percent `memcpy`; the windows that land entirely in the read are 84 to 87 percent `memcpy`.
So the read path's cost here is copying, not hashing, and the obvious cheap win is one copy too many (store decompression into the block cache, then the cache into the caller's buffer) rather than anything about the hash.
This was not pursued further, per the instruction not to spend long on it.

## Known gaps

- Chunk lists are loaded whole and committed whole (see "Requests of store and meta").
- Blocks of deleted data are only reclaimed by GC (#10), so `statfs` free space does not grow after a flushed file is deleted.
- Only `fsync` and the timers make data durable; `flush` does nothing.
- The virtual inode numbers need the `<root>/virt.ino` mark and the alias table still exists (both bounded now) until meta can hand out inode numbers ahead of a transaction.
- A poisoned file stays poisoned for the life of the mount; there is no repair operation, only removing it.
- The cold read path copies twice per chunk and the profile says that is where its time goes (see above).
- No mount adapter has been run against `Core` yet; the suite runs through the `Vfs` trait only.
