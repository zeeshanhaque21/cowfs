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

Status on main: a created inode now takes a number reserved from `Meta::reserve_inodes`, so it is born with its meta number and needs no alias (#140, #142).
The virtual shape, the alias table and `<root>/virt.ino` below remain for numbers issued before a reopen.
The rest of this section describes that scheme as designed, and has not been rewritten.

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

**The counter is durable.** `<root>/virt.ino` holds the highest number handed out, written and synced in blocks of 2^20 before any number of the block is used (the block size is `VIRT_BLOCK`; `Options::alias_limit` is unrelated).
A crash can therefore waste numbers but never reuse one, and a number from an earlier session is `Stale` rather than another file's data.
The mark is written twice (value, then the same value again), so a torn write is refused at open instead of believed.
Tests: `virtual_inode_numbers_are_never_reused_across_a_restart` (a clean reopen) and `the_virtual_number_reservation_survives_a_crash` (a child process that aborts without syncing, three times in a row).

**An alias lives as long as the inode has a name.** The rule is in `cowfs-vfs` under `Ino` and `forget`, and it is a session contract, not an optimisation:

> An `Ino` handed to a client keeps meaning the same inode for the rest of the mount session, for as long as that inode exists, whatever the adapter does with `forget`.

A stateless protocol has to be able to do that, because the client never tells the server it is done with a number.
`cowfs-nfs` hands out attributes and immediately calls `forget` (`crates/cowfs-nfs/src/adapter.rs:170`), because an NFS filehandle is just the `Ino` and nothing pins it.
So `forget` cannot mean "the inode may stop existing"; it means "the adapter is done with its own bookkeeping", and the release rule moved from "no caller holds a reference" to "the inode has no name left and nothing holds a handle".
Before this rule, issue #53: `mkdir d` through a real macOS NFS mount, forget it the way the adapter does, wait for the commit, and `mkdir d/e` fails with `Stale NFS file handle`, because the alias went away and the number reverted from `0x8000010000000001` to `0x10000000002`.
The reproducer needs no adapter: `crates/cowfs-core/tests/alias_session.rs`.

The release point is `Inner::try_reclaim`: an alias goes when its inode is unlinked, its removal is committed, and no handle is open. An NFS client holding a handle to an unlinked file still gets `ESTALE`, which is correct and is what POSIX allows.
`Inner::maybe_evict_node` still drops the *node* of a committed, clean, unreferenced file, so the node table stays bounded; the alias is what survives.
An unlinked inode nobody holds is `Stale`, not a silent `NotFound`.

Hardlinks and renames keep one number because the alias is keyed by the meta inode, and `canon` is applied on every `lookup`, `readdir` and dentry hit.
A client never sees two numbers for one inode in a session.
The `is_released_virt` check is gone with the rule it existed for: a released alias now means an unlinked inode, which `Inner::live` already answers `Stale`, so the second spelling was dead.
Tests: `a_forgotten_directory_keeps_its_number_after_the_commit`, `a_directory_created_through_the_session_still_takes_children_after_the_commit`, `a_rename_keeps_one_number`, `a_hardlink_keeps_one_number`, `an_unlinked_inode_goes_stale`, `a_number_never_changes_while_its_inode_has_a_name`, `live_blocks_filters_holes_and_yields_only_stored_blocks`.

Mutants for the rule: `a01_alias_released_on_commit` (release the alias on every reclaim), `a02_reclaim_keeps_nlink_zero` (reclaim a named inode), `a03_no_alias_ceiling` (never refuse a create past the ceiling), `a04_canon_returns_meta_number` (`canon` answers nothing, so a lookup names the file by its meta number), `a05_pinned_unlinked_is_stale` (the mirror: an unlinked file a handle is open on goes `Stale`).
All five are killed by assertion.
The whole sweep is 34 mutants, all killed; the `n12_released_virt_ok` entry went with the function it patched.

### What a session alias costs, and the ceiling

One entry per inode a session created that still has a name.
The table is two `HashMap`s: virtual to meta inode, and packed meta inode to virtual.
`Aliases::bytes` reports it from the bucket counts, which is exact; an RSS delta is not, since it also carries the dentry table and redb's own database growth.

Measured at 500,000 creates (`ALIAS_FILES=500000 cargo test -p cowfs-core --release --test alias -- --ignored alias_bytes_at_500k_creates`, `target/alias-500k.txt`):

| | |
|---|---|
| entries | 500,100 |
| bytes | 31,195,136 |
| bytes per entry | 62.4 |
| node table | 101 entries |
| process RSS per file | 600 bytes (includes redb and the dentry table, so an upper bound on everything, not on the alias) |

62.4 bytes per entry is what two `(u64, u64)` maps cost at 500,100 entries: 17 bytes per bucket (two `u64`s plus a control byte) and a power-of-two bucket count, so the worst case just past a doubling is 68 and the floor at a full load factor is 39.
31 MiB for half a million files is the price of the contract, and it is paid once per inode created, not once per lookup.

`Options::alias_limit` (default `1 << 20`, so about 65 MiB of alias table) is the ceiling on how many inodes one session may hold at once.
Past it, `create`, `mkdir` and `symlink` return `Error::NoSpace` and `last_error` names the ceiling, instead of handing out a number that would go stale later. That is the honest failure: a client gets `ENOSPC` on a new file and keeps every number it already holds.
Unlinking an inode frees its alias, so the ceiling is on live inodes and not on the session's total.
Test: `a_create_past_the_alias_ceiling_is_refused`, with the limit set to 4 so it is reachable without a million creates.

### FUSE

`cowfs-fuse` has its own inode table (`crates/cowfs-fuse/src/table.rs`) and the kernel's `FORGET` arrives when the kernel drops the inode, which is much later than the NFS case.
The same window exists in principle and the fix closes it: an alias now survives `forget` for as long as the inode has a name, so a FUSE client cannot see a number change under a cached dentry either.
No FUSE-only change was needed or made, and none of that crate was touched.

### The real mount still fails, for a different reason (#53 is not the whole story)

Verified on macOS 26 against `v1/daemon-core` merged into this branch, driving the real stack
(`cowfs-daemon --backend core`, a real `mount_nfs`, real `mkdir`):
`mkdir d` succeeds, `mkdir d/e` fails with `Stale NFS file handle`, and a `git clone` into the mount fails on its first `.git` entry.

With this branch's core, `mkdir d` now keeps its number (the core test proves it), and the failure moves one layer up, into `cowfs-nfs`:

```
SCRATCH side_getattr id=0x8000010000000001 -> taken as a sidecar
```

`cowfs_nfs::sidecar::SIDE_BIT` is `1 << 63` and `cowfs_core::ino::VIRT` is `1 << 63`, so **every virtual inode number the core hands out looks like an AppleDouble sidecar to the adapter** and is answered `ESTALE` without ever reaching the `Vfs`. Measured: 12 operations entered `Adapter::getattr` with a virtual number and 0 reached `Vfs::getattr`.

That is a namespace collision between two crates, so the fix belongs to whichever side gives up the bit, and this branch does not edit `cowfs-nfs`.
Two requests, in preference order:

1. `cowfs-nfs`: mint the sidecar bit from the adapter's own space instead of the `Ino`'s. The adapter already has a per-inode `parents` map and a handle codec with a keyed MAC, so a sidecar can be named by a handle the adapter mints, and `is_side` stops testing an `Ino` bit it does not own.
2. Failing that, `cowfs-core`: move `VIRT` off bit 63 (bit 62 is free, and `MAX_VIRT_SNAP` already accounts for the lost bit). That changes the durable `virt.ino` mark format, so it needs a one-line version bump in the mark and an explicit "an old mark is not a new mark" rule at open, since a number from a session with the old layout must not name a file under the new one.

The same collision is why writes into an existing directory work and creating a nested one does not: a file's own number is only used by `getattr` after `lookup`, and `cowfs-nfs` handles a lookup's result by returning the number it was given.

`the_core_backend_serves_real_bytes_and_survives_a_restart_and_a_kill` and `the_core_refuses_a_store_that_reports_damage_and_does_not_acknowledge_it` therefore still fail on macOS with this branch, for this reason and not the alias release. The other three end-to-end tests pass.

### What this costs the store and meta

Nothing.
Both the alias table and the durable counter are in `cowfs-core`, and `cowfs-meta` is unaware of the virtual form.

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

`Core::rename_snapshot` moves a name in one `Meta::rename_snapshot` transaction and keeps the snapshot id, so no swap is involved.
A refused rename (a name held by another snapshot, a bad name) writes nothing.
Tests: `a_rename_keeps_the_id_the_root_and_the_file_numbers`, `a_rename_that_fails_at_the_commit_changes_nothing`, `rename_snapshot_is_failure_safe`.

`promote_base` replaces an existing name with a clone of another snapshot.
A rename cannot do that, because it refuses a name another snapshot holds, so `promote_base` still goes through `src/swap.rs`:

1. fork the source into a staging name (a failure here changes nothing),
2. write and sync the intent file `<root>/swap-<target>`, naming the staging and target snapshots,
3. remove the old target,
4. fork the staging snapshot into the target name,
5. remove the staging snapshot,
6. remove the intent file.

Any failure before step 3 leaves the old target untouched.
Any failure or crash from step 3 on leaves the intent file, and the next `Core::open` finishes steps 4 to 6 before it serves anything.
So a snapshot name never disappears without a record that explains it, and a failed swap never costs the old base.
The one fork gives the new snapshot a new id, which it keeps under the target name, so every inode number in it differs from the old target's.
Import uses the same finish step.
A swap or replacing import of a name with a pending intent finishes that intent first (`Core::recover_target`), so a retry never deletes the only copy of the new tree.
After the intents, `Core::open` removes every staging snapshot no intent names (a crash during staging leaves one); it holds the store's flock, so no live operation owns a staging snapshot then.

Test: `promote_base_survives_a_failure_at_every_step` injects a failure at each of the five steps (`Core::set_swap_fault`, a doc-hidden test seam), checks the content before the reopen, reopens, and checks that recovery completed the swap or left the old base, with no staging snapshot and no intent file left.
`ingest_replacing_survives_a_failure_at_every_step` does the same for the replacing import (faults 2 to 4 of `replace_with_staged`), and the retry and orphan-sweep tests are in the same file.
The intent writer's temp file is `tmp-swap-<target>`, a prefix no intent (`swap-<target>`) can have, so a target named `base.tmp` is not mistaken for a temp file; a leftover `swap-<X>.tmp` from the older naming is dropped on open.

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
| `rename_snapshot(old, new)` | one meta transaction | keeps the snapshot id and every inode number in it; a handle open across it keeps working; a refused rename changes nothing |
| `promote_base(src, base)` | the staged swap, replacing an existing `base` | crash-safe and error-safe; two O(1) forks |
| `list_snapshots()` | one read | |
| `merkle_root(name)` | flush of that snapshot, then one read | root only covers committed state, so it flushes first |
| `sync()` | flush of everything, then `Meta::sync` | |
| `check()` | scan | pass-through to `Meta::check` after a flush |
| `fsck()` | scan | `Store::fsck`, then a walk of every durable snapshot root for live references to absent blocks |

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

A cold read moves each byte twice and cannot move it once: `pread` fills a buffer the store returns, and the caller copies that into its own `Vec`, because `Vfs::read` hands back an owned buffer while the cache holds an `Arc<Vec<u8>>` that outlives the call.
`Store::get` reads the 56-byte header on its own and then reads the payload straight into the buffer it returns, so the record is never read whole and shifted: draining the header off a record-sized buffer moved every byte of every block a second time.
One `pread` per block is the floor, since each block carries its own header.

Measured on the 512 MiB cold read of `examples/coldread` (`sample`, 1 ms, self time, idle and reopen noise excluded), the read path is verification-bound, not copy-bound: BLAKE3 76%, crc32c 6%, `pread` 10%, `memmove` 5%.
Removing the extra copy cut `memmove` 42% and total read-path work 2.2%, because it trades one `memmove` for one small `pread`.
Closing the gap to a native cold read needs verification to cost less, not fewer copies; `docs/v1-store.md` already records BLAKE3 alone at 1129 MiB/s against 939 MiB/s for a verified read of the same block.

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

0. **The reference gate** (`src/gate.rs`) is outermost.
   A thread enters it before it takes `SnapCtx::ns`, `SnapCtx::flush`, a node lock or meta's lock, and only while it may store a chunk or commit a chunk list: `flush_snapshot`, `barrier`, and `setattr` with a size.
   `Blocks::put` takes the gate's `Entry`, so a store write outside the gate does not compile.
   A caller that already holds a node write lock (`op_write`'s threshold flush, `relieve`) uses `try_enter` and leaves its bytes dirty when the gate is closed, so nothing parks at the gate while holding a node lock.
   The garbage collector's barrier closes the gate (`docs/gc-core-integration.md`).
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
checked by `python3 scripts/lock_audit.py --check` in the CI lint job, which fails if this table is not
exactly what the generator produces from `crates/cowfs-core/src`.

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
| `file::punch` | blocks | ? |
| `file::put_piece` | blocks | ? |
| `file::flush_extent` | blocks | ? |
| `file::verify_partial` | blocks | ? |
| `file::read_range` | blocks | ? |
| `gate::enter` | leaf | 1 |
| `gate::try_enter` | leaf | 1 |
| `gate::take` | leaf | 1 |
| `gate::waiting` | leaf | 1 |
| `gate::drop` | leaf | 1 |
| `gate::a_nested_enter_is_admitted_while_a_barrier_drains` | leaf | 1 |
| `inner::snapctx_id` | leaf | 1 |
| `inner::all_snaps` | leaf | 1 |
| `inner::take_reserved` | leaf | 1 |
| `inner::lose_the_next_insert` | leaf | 1 |
| `inner::meta_of` | aliases | leaf |
| `inner::canon` | aliases | leaf |
| `inner::flushed_of` | leaf | 1 |
| `inner::load_node` | nodes, aliases, snap. | 2 then 3 then leaf |
| `inner::live` | st.rd | 2 |
| `inner::dir` | st.rd | 2 |
| `inner::shrink_nodes` | st.try_read, nodes | 2 |
| `inner::dent_lookup` | dents, snap. | 2 then 3 |
| `inner::ensure_file` | st.wr, st.rd, snap. | 2 then 3 |
| `inner::ensure_target` | st.wr, st.rd | 2 |
| `inner::preserve_orphan` | st.wr, st.rd | 2 |
| `inner::maybe_evict_node` | nodes | 2 |
| `inner::try_reclaim` | st.wr, st.rd, nodes, aliases | 2 then leaf |
| `inner::queue_content` | sc.q | 1 |
| `inner::flush_node` | st.wr | 2 |
| `inner::flush_data` | sc.q, nodes, last_error | 1 then 2 then leaf |
| `inner::take_flush_fault` | leaf | 1 |
| `inner::flush_snapshot` | sc.flush, last_error | 1 then leaf |
| `inner::flush_locked_snapshot` | sc.q | 1 |
| `inner::commit_batch` | sc.q, nodes, dents, aliases, unsynced | 1 then 2 then leaf |
| `inner::restore_state` | st.rd, nodes | 2 |
| `inner::op_times` | st.rd, nodes | 2 |
| `inner::commit` | aliases, snap. | 3 then leaf |
| `inner::finish_sync` | unsynced, last_error | leaf |
| `inner::barrier` | sc.flush, last_error | 1 then leaf |
| `inner::flush_namespace_locked` | sc.q | 1 |
| `inner::tick` | sc.q, unsynced | 1 then leaf |
| `inner::relieve` | pressure | leaf |
| `io::file_node` | st.rd | 2 |
| `io::op_read` | st.rd | 2 |
| `io::op_write` | sc.q, st.wr, st.rd, last_error | 1 then 2 then leaf |
| `io::op_setattr` | sc.q, st.wr, st.rd | 1 then 2 |
| `io::op_fallocate` | sc.q, st.wr | 1 then 2 |
| `io::op_readlink` | st.rd | 2 |
| `io::op_open` | handles | leaf |
| `io::op_release` | nodes, handles | 2 then leaf |
| `io::op_getxattr` | st.rd | 2 |
| `io::op_listxattr` | st.rd, snap. | 2 then 3 |
| `io::op_setxattr` | sc.ns, st.wr, snap. | 1 then 2 then 3 |
| `io::op_removexattr` | sc.ns, st.wr | 1 then 2 |
| `lib::shutdown` | leaf | 1 |
| `lib::from_parts` | nodes, dents, aliases, handles, root_time, pressure, unsynced, last_error | 2 then leaf |
| `lib::fork_snapshot` | snap. | 3 |
| `lib::move_name` | root_time | leaf |
| `lib::list_snapshots` | leaf | 1 |
| `lib::merkle_root` | snap. | 3 |
| `lib::missing_live_refs` | snap. | 3 |
| `lib::last_flush_error` | last_error | leaf |
| `lib::alias_table` | aliases | leaf |
| `lib::set_load_node_contention` | leaf | 1 |
| `lib::set_flush_fault` | leaf | 1 |
| `lib::live_blocks` | snap. | 3 |
| `lib::add_snap` | leaf | 1 |
| `lib::snap_by_name_raw` | leaf | 1 |
| `lib::check_new_name_except` | leaf | 1 |
| `lib::register` | root_time, last_error, snap. | 3 then leaf |
| `lib::unregister` | sc.ns, sc.flush, sc.q, st.try_read, nodes, dents, aliases, root_time | 1 then 2 then leaf |
| `lib::stats` | sc.q, nodes, dents, aliases | 1 then 2 then leaf |
| `lib::health` | sc.q, nodes, last_error | 1 then 2 then leaf |
| `lib::unpoison` | sc.q | 1 |
| `lib::drop_caches` | st.try_read, nodes, dents | 2 |
| `node::try_read_for` | st.try_read | 2 |
| `ns::root_attr` | root_time | leaf |
| `ns::maybe_wake` | sc.q | 1 |
| `ns::op_getattr` | st.rd | 2 |
| `ns::op_lookup` | st.rd | 2 |
| `ns::root_lookup` | st.rd | 2 |
| `ns::op_readdir` | st.rd, dents | 2 |
| `ns::root_readdir` | leaf | 1 |
| `ns::make` | sc.ns, sc.q, st.wr, st.rd, nodes, dents, aliases, last_error | 1 then 2 then leaf |
| `ns::op_link` | sc.ns, sc.q, st.wr, st.rd, dents | 1 then 2 |
| `ns::op_unlink` | sc.ns, sc.q, st.wr, st.rd, dents | 1 then 2 |
| `ns::op_rmdir` | sc.ns, sc.q, st.wr, dents | 1 then 2 |
| `ns::barrier_if_needed` | st.rd | 2 |
| `ns::require_empty` | st.rd | 2 |
| `ns::op_rename` | sc.ns, sc.q, st.wr, st.rd, dents | 1 then 2 |
| `ns::adjust_kids` | st.wr | 2 |
| `ns::rename_dir` | st.wr, nodes, dents | 2 |
| `ns::refresh_dir_attr` | st.wr, snap. | 2 then 3 |
| `swap::recover_intent` | last_error | leaf |
| `swap::recover` | last_error | leaf |
| `swap::swap_snapshot` | last_error | leaf |
| `swap::stage_and_intent` | snap. | 3 |
| `util::lk` | leaf | 1 |
| `util::try_lk` | leaf | 1 |
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

State when this section was written: the merge of `origin/v1/8-9-meta` (commit 9abe96e) and `origin/v1/7-block-store` (commit 1c00c1e), plus `origin/v1/vfs-test` (5cc6008) and `origin/v1/vfs-trait` (1825081).
The list below is updated to main as of 2026-10-08.

Landed and adopted: `before_sync` after the batch closure, `Meta::close`, `Meta::pack_ino`, the durable inode reservation (numbers are never reused), the store's `corrupt_synced` refusal, the store's v2 pack format and recovery classification, the new error variants (mapped in the error table), the store op log.
Landed and not yet used by `Core`: `Snapshot::chunk_range`, `Tx::splice_content` (with the version check) and `Error::NeedsRechunk`.
`Core` still reads a file's whole chunk list on first use and commits it with `set_content`, which is O(chunks in the file) per commit.
Moving to `chunk_range` and `splice_content` is the next step for multi-GiB files and is not needed at the measured sizes.

Requests recorded in #42, and where each stands on main:

1. `Meta::rename_snapshot(id, new_name)`: landed (#137) and used by `Core::rename_snapshot` (#141).
   `promote_base` replaces an existing name, so it still uses the staged swap; the swap forks once and renames the staged snapshot into place.
2. `Tx::set_now`: landed (#136).
   `Core` stamps each deferred operation with the ctime its cached node holds.
   `Snapshot::batch_at` was not added, because `set_now` covers the need.
3. A hole flag in `ChunkRef`: landed (#138).
   The metadata walk skips holes on its own.
   `Core::live_blocks` keeps its own hole filter as defence in depth, and must not free blocks named only by an open orphan or by an uncommitted chunk list (`Core::pinned_blocks`), nor a block put after its mark started.
4. `Meta::reserve_inodes(n)`: landed (#140), and `Core` creates at reserved numbers (#142).
   The alias table and `<root>/virt.ino` are still read on open, for numbers issued before a reopen.
   They are not removed yet.
5. A crate for the snapshot-name rule: landed (#99) as `cowfs-snapname`.
   `cowfs-ctl` and `cowfs-core` hold thin wrappers, and `cowfs-daemon/tests/snapname_drift.rs` checks that they agree.

## Decisions for the lead to review

1. Virtual inode numbers with an alias table, in place of asking meta for a reservation (superseded for new creates by `Meta::reserve_inodes`; kept for numbers issued before a reopen).
   The counter is durable in `<root>/virt.ino` and aliases are released once nothing can hold their number, so the table is bounded (130 aliases after 500,000 creates).
2. Directory rename, replacing a directory, and readdir of a directory with pending changes are barriers, but a barrier commits only the namespace.
   Unrelated files' dirty data is not flushed: a `readdir` of one directory with 48 MiB of dirty data in another file costs 0.14 ms at the median and never chunks the unrelated data (test `a_directory_barrier_does_not_flush_unrelated_file_data`; before the fix the same call flushed all 48 MiB and took 5.9 s on this loaded box).
3. Timestamps: atime and mtime exact, and ctime is the time of the operation, not of the batch (`Tx::set_now`).
4. `rename_snapshot` is one meta transaction and keeps the id.
   `promote_base` goes through a staged swap with a crash-safe intent record; it changes ids and is atomic only in the sense that a name is never lost.
5. Holes carry an explicit flag in `ChunkRef`; `Core::live_blocks` still filters them for GC.
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
| Swap | `swap.rs` | 12 | failure injected at every step of a snapshot replacement and of a replacing import, the intent record's recovery on reopen, a same-name retry over a pending intent, the orphan staging sweep, a damaged store inside a swap, rename failure safety |
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
- Blocks of deleted data are reclaimed by GC (#10, `Core::collector`), so `statfs` free space does not grow after a flushed file is deleted until a collection runs.
- Only `fsync` and the timers make data durable; `flush` does nothing.
- Virtual inode numbers use `<root>/virt.ino.a` and `<root>/virt.ino.b` reservations until meta can allocate durable inode numbers ahead of a transaction.
- `Core::unpoison` requeues retained bytes after the cause is repaired; it cannot reconstruct bytes lost outside the mount.
- The cold read path copies twice per chunk and the profile says that is where its time goes (see above).
- No mount adapter has been run against `Core` yet; the suite runs through the `Vfs` trait only.

## Round-2 revalidation

Main at `48645b9` and block-store at `c4eb089` are merged into the integration branch.
The store's new quarantine error maps to retryable VFS I/O, not file corruption.
No store or metadata source was hand-edited for this revalidation.

The original two survivors now fail targeted assertions: `b03_no_rollback` leaves an intent and hidden staging state; `b09_transient_poisons` stops before using the transient retry budget.
The second mutant changes the retry decision, not the eventual poison classification.
The original watchdog-only outcomes were rerun rather than counted as assertion kills.
Of the original 30 mutants, 26 have regression kills, three survive (`n02_swap_intent_no_fsync`, `n04_virt_mark_no_dir_fsync`, `b11_load_node_upserts`), and one is invalid (`b07_unregister_locked_meta` has an unclosed delimiter).
`n02` still calls file fsync but ignores its error; the existing tests do not inject that error.
`n04` needs a directory-durability power-loss model; a process kill does not establish that guarantee.
`b11` still lacks deterministic contention coverage of the final cache-insertion retry.
Each rerun has its own target directory and a total hard timeout, and restores its source in `finally`.

Polling `Core::health` previously drained dirty-file work; a failing regression now verifies repeated polls and durable readback without repair.
The checkpoint harness previously selected no child test and deleted its fixture before reopening it.
It now retains the fixture, requires a real abort and checkpoint marker, frames output away from the test-harness prefix, and models successful promotion without removing the source from the oracle.
A controlled unchanged-versus-replaced-target spike demonstrates why the old promotion expectations were invalid.
Earlier green checkpoint runs do not establish crash coverage.
Pre-removal swap refusals at steps 1, 2 and 3 now roll back; a regression demonstrated step 1 completing on reopen before this fix.

The two post-store macOS fsx runs each completed 100,000 operations, seeds 1 and 2, in 430.13 s and 410.12 s respectively.
The 180 s macOS mixed hammer completed 725,491,433 global progress ticks with a largest sampled gap of 452 ms.
This is one run, not a per-worker bound, and includes setup and drain time.
The Linux 3 s representative hammer failed with a 12,678 ms gap; its 180 s batch was not launched after the failed sample.
No lock-specific cause or cross-platform liveness pass is claimed from that result.
The directory-barrier test now asserts unchanged dirty bytes exactly; timing remains diagnostic because an unrelated scheduling delay made its maximum-time ratio fail without flushing data.

`flush_boundary.rs` SIGKILLs its verified child immediately before and immediately after the real store-sync hook inside a pending metadata flush.
Both boundaries reopen with clean metadata and fsck, preserve the untouched fsynced file, and expose a complete old or new replacement and namespace batch.
This is process-crash coverage at two boundaries, not a power-loss model.
Linux PID verification reads the selected `/proc/<pid>/cmdline` directly because `ps -p` hung while scanning an unrelated blocked process's environment.
The VM recipe uses only `/home/zeeshanhaque/cowfs-core-work`, its local target and temporary directory, and disables core dumps for its own test processes.
Linux crate validation uses 100 invariant iterations and three checkpoints; macOS's default invariant run uses 400 iterations and twelve checkpoints.

Reproduce with `sh scripts/verify-core.sh` and `orb -m cowfs-spike3 sh /Users/zeeshanhaque/.treehouse/cowfs-7c1bf8/9/cowfs/scripts/validate-core-linux.sh crate`.
The `hammer` phase requires its representative run to pass before launching 180 s.
Raw local evidence is in `target/fsx-round2-seed{1,2}.log`, `target/hammer-macos180.log`, `target/hammer-linux180.log`, `target/mutants.out`, and each retained `target/mutants/<name>/run.log`.
After #24 lands, re-merge any newer store revision and rerun both workspace platforms, fsx, flush boundaries, checkpoint invariants and the unresolved Linux hammer before claiming an integration pass.
Explicit hole markers, durable inode reservations, shared snapshot-ID bounds, and atomic snapshot replacement remain metadata requests in #42.

## Round-3 mutants

Tracked in #49, branched from merged `main` at `cdc90d3`.
All 30 mutants are killed by an assertion now; the three survivors of round 2 and the one invalid entry are fixed.

`cowfs_core::fsops` is a `doc(hidden)` test seam with two inert parts, both off unless a test arms them.
A fault rule makes the next `times` `sync`s of a file or directory whose path contains a substring fail, and a trace records those calls in order.
`write_intent` and `write_virt_mark` now go through the seam, so the durability orderings the design argument depends on are observable:

- `n02_swap_intent_no_fsync` is killed by `a_swap_refuses_when_the_intent_file_cannot_be_made_durable` (an intent file that cannot be made durable stops the swap, with the mount unchanged) and by `the_intent_file_is_durable_before_the_victim_snapshot_is_removed` (the trace shows the record's own file sync, then its rename, then the directory sync, then the victim unregister).
  Making the intent directory sync a checked call rather than a swallowed one is a real change: a rename that cannot be made durable is now a refused swap, not a silent one.
- `n04_virt_mark_no_dir_fsync` is killed by `a_reservation_refuses_when_its_directory_cannot_be_made_durable` and by `a_new_virtual_reservation_is_durable_before_any_of_its_numbers_is_handed_out` (the trace shows the mark's file sync, its rename, the directory sync, and only then a number from the new reservation is handed out).
- `b11_load_node_upserts` is killed by `a_node_load_that_exhausts_its_retry_budget_fails_closed`.
  `Core::set_load_node_contention` makes the next `tries` node-table insertions lose their race by bumping the shard epoch between the epoch read and the insert, which is the contention the retry loop exists for, without a timing race.
  The test arms it across a core reopen, so the first lookup builds its node from meta, and asserts the exhausted budget is `Stale` rather than a node built from meta.
  It does not prove data loss, because a dirty node is never evicted from the table while its bytes are unflushed; it proves the load fails closed, which is the property the code claims.
- `b07_unregister_locked_meta` is valid again: it now takes the namespace and flush locks immediately before the metadata commit, which compiles and keeps the original defect.
  `SnapshotLockProbe` holds a snapshot's own locks after it leaves the table, and `removing_a_snapshot_releases_its_locks_before_its_metadata_commit` uses a slow store-sync hook to hold the commit open, so the locks are observed free throughout.
  The test is wall-clock sensitive: it passes when the commit is held long enough to sample and would miss a mutant that held the locks for a shorter window.

A proptest seed found while this work ran (`cc 1c48a6cc`, persisted in `model.proptest-regressions`) failed against the memory model, not against the core.
It reproduces on `main` with the same seed, so it is a harness bug: a held op that hit no live handle on its own side still entered the operation log, and a later fork replayed it against a handle only the memory model had.
`run` now logs a held op only when it had a live handle.

Reproduce the four with `python3 scripts/mutants.py n02_swap_intent_no_fsync n04_virt_mark_no_dir_fsync b11_load_node_upserts b07_unregister_locked_meta`.
Each run has its own target directory, a total hard timeout, and restores its source in `finally`.
A timeout counts as UNKNOWN, never as killed.
