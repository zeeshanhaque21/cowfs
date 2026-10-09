# cowfs-meta design

Issues: #8 (Merkle tree, O(1) writable snapshots) and #9 (redb metadata, crash consistency).
Contract: `docs/v1-architecture.md`, section "cowfs-meta contract".
This note is the design.
Numbers in the "Measurements" section come from `crates/cowfs-meta/examples/bench.rs` and are the only performance claims made here.

## Changes since first review

An independent review of the first version (PR 25) found ordering, inode reuse, scaling and test-quality problems.
This list is for `cowfs-core`, which builds on the first version's API.
Everything not listed here keeps its signature and meaning.

Signature changes and additions:

- `Options` gained `ack: Ack`, `background: bool`, `max_pending_bytes: usize`, `ino_block: u64`.
  Code that builds it with `..Options::default()` still compiles; a full struct literal does not.
  `node_cache` now defaults to 32768 nodes.
- `Error` gained `Conflict`, `NeedsRechunk`, `LimitExceeded`, `Reentrant`, `Closed`, `Format`.
  A `match` on it needs new arms.
  A file that is not a cowfs-meta database, or has another format version, is now `Format` (it was `Corrupt` for a version mismatch and was silently initialised for a foreign redb file).
  `Corrupt` is now also returned for redb's own corruption errors (it was `Storage`).
- The chunk-list rows changed from segments to per-chunk extents, and the inode record grew.
  The format version is 2: a database from the first version is refused with `Error::Format`.
  `CHUNKS_PER_SEGMENT` is gone; `INO_LIMIT` and `SNAPSHOT_LIMIT` are new.
- New: `Meta::close() -> Result<()>`, `Meta::open_recover`, `Meta::durable_snapshots`, `Meta::reap_step`, `Meta::reap_all`, `Meta::pending_reap`, `Meta::pack_ino`, `Meta::unpack_ino`, `Snapshot::chunk_range`, `Snapshot::content_version`, `Snapshot::splice_content`, and the matching `Tx` methods (`Tx::readlink` too), `Ack`, `Recovery`, `ChunkRange`.

Semantic changes:

- Hook ordering (F1): the hook now runs after the batch closure and before the durable commit, once per commit, and also on every `sync()`, even with nothing pending.
  It may now run on the background thread, so it must be thread-safe (it already had to be `Send + Sync`).
  A hook that calls this crate gets `Error::Reentrant`.
- Applied versus durable: a mutating call is visible to every reader when it returns and becomes durable by the policy in "Applied and durable".
  `Snapshot::info`, `Snapshot::root` and `Meta::snapshots` report the applied root.
  `Meta::durable_snapshots` reports the file's state.
  `Snapshot::root()` on a snapshot with pending changes hashes the changed nodes, so its cost grows with the pending change, not the tree.
- `close()` and drop (F2): both run the hook and commit.
  If the hook or the commit fails, pending changes are discarded and never written.
  After `close()` every mutation returns `Error::Closed`.
  A `Snapshot` handle keeps the file open, so the file closes when the last `Meta` and `Snapshot` clone is gone.
- Inode numbers (F3): never reused, also across crashes; after a crash the first new number may be up to `ino_block` above the last handed out, so numbers are not contiguous.
  `pack_ino`/`unpack_ino` give a restart-stable `u64`.
  A `Tx` that fails still consumes the numbers it allocated.
- Timer (F4): the background thread flushes idle changes after `sync_interval`.
  With `background: false` the caller must call `sync()`.
- `Ack::Durable` (F11): each call returns after a durable commit; concurrent callers share one hook run and one fsync.
- `check()` and `live_blocks()` call `sync()` first, so they run the hook.
  `live_blocks` walks the durable state.
- `remove_snapshot` (F13) is durable on return and returns quickly; the nodes are freed afterwards in steps of at most 256 nodes.
  `Meta::pending_reap` reports the queue.
- Same-directory `rename` keeps the entry's cookie (F12); a cross-directory rename assigns a new cookie.
- `setattr(size)` inside a chunk returns `Error::NeedsRechunk` (F12).
- Chunk lists (F7): one extent row per chunk keyed by byte offset.
  `set_content` still replaces the whole list; use `chunk_range` and `splice_content(ino, expected_version, start, end, new_chunks, new_size)` for large files.
  `start` and `end` are byte offsets on chunk boundaries; a splice before the end of the covered range must keep its byte length.
- A detected corrupt read makes the handle refuse writes with `Corrupt` until it is reopened (F8).
- `Snapshot` lookups take one descent for the directory entry and one for the child inode (F10).
  Invalidation `cowfs-core` needs for a dentry cache above this API: a cached `(snapshot id, dir, name) -> Attr` is valid until a mutating call on that snapshot returns (any `batch`, `create`, `rename`, and so on) or the snapshot is removed.
  `Snapshot::root()` changes on every applied change and can serve as a cheap version stamp, at the hashing cost above; a per-snapshot counter is not provided.
  A forked snapshot starts with the same tree, so entries are valid in the fork until the fork mutates.

## Problem

`cowfs-meta` stores the directory tree, inode attributes, xattrs, symlink targets, and each file's chunk list.
It must give:

- an O(1) writable snapshot (a clone of the whole tree, cost independent of tree size),
- a Merkle root per snapshot,
- hardlinks, symlinks, xattrs, mode bits, timestamps, atomic rename,
- inode numbers that are stable and never reused, usable in NFS file handles,
- a cookie-resumable directory listing that does not depend on inode numbers,
- crash consistency: after any crash the tree is some committed state, never a torn or corrupt one,
- a way for garbage collection (#10) to find every live block and skip subtrees it already marked.

A pure content-addressed Merkle tree (a git-style tree where a directory hash covers its entries' hashes) cannot express hardlinks.
Two names for one inode must show the same later writes.
Two equal hashes are only equal, not the same file.
If a file's state lives inside its parent directory's node, a write through name A has to rewrite the path to name B in some other directory, and nothing in the tree points from the file to its other names.
Spike 2 found that rustc incremental compilation hardlinks its `.o` files, so this cannot be dropped.
So file identity (the inode) has to live in a layer that is separate from directory structure, and that layer must itself be cloneable in O(1).

## Alternatives considered

### A. Git-style Merkle tree with an inode indirection kept in one mutable table

Directories are content-addressed nodes mapping name to inode number.
Inode records (attributes, chunk list) live in one mutable redb table keyed by inode number.
A snapshot is the root hash of the directory tree plus a copy of the inode table.

Rejected.
The inode table is the part that has to be cloned, and a mutable redb table cannot be cloned in O(1).
Making it a persistent structure (option D) solves that problem, and then the content-addressed directory tree is redundant: the directory entries can live in the same persistent structure.
It would also put two Merkle mechanisms, two garbage collectors, and two crash-consistency arguments in one crate.

### B. One redb table per snapshot, copied on snapshot

Simplest reads and writes.
A snapshot copies every row.
O(n) in the size of the tree, so it fails the O(1) requirement outright.
At 1,000,000 nodes a snapshot would take seconds and double the metadata.
Rejected.

### C. Versioned rows with a generation lineage (MVCC)

Key is `(inode, generation)`.
A snapshot creates a new generation that points at its parent generation.
A read walks up the ancestry until it finds a row.
Snapshot creation is O(1) and writes touch one row.

Rejected for four reasons.
Read cost grows with the depth of the snapshot lineage, and the design's clone-of-clone workflow makes lineages deep.
Deleting a snapshot means finding rows that no descendant needs, which is a scan.
There is no Merkle root: hashes would have to be maintained per generation, and the hash of a directory would change when a hardlinked file changes through another name.
Garbage collection cannot skip already-marked subtrees, because there are no shared subtrees to identify, only rows.

### D. Persistent (copy-on-write) ordered map, content-addressed, stored in redb (chosen)

One ordered key-value map holds everything about a tree: inode records, directory entries, xattrs, symlink targets, chunk lists.
The map is a B+tree whose nodes are immutable and content-addressed.
A snapshot is the root node id.
A write copies the path from the leaf to the root and never modifies a node in place.
This is the shape the contract sketched, with one map instead of two, so that one root hash covers everything and one refcount scheme covers everything.

### E. Prolly tree (content-defined node boundaries) instead of an ordinary B+tree

Gives a canonical structure: equal contents produce equal roots regardless of history.
It costs a rolling-hash boundary rule, more complex local re-chunking on insert and delete, and less predictable node sizes.
Rejected for v1 because no requirement needs canonical roots.
The chosen tree is not canonical.
Two snapshots with equal contents built in different orders can have different roots.
Equal roots imply equal contents.
Nothing in the crate promises the converse.
This is a decision for the lead to review.

### F. HAMT (hash-keyed trie) instead of an ordered tree

Canonical and simple.
An ordered scan is not possible, and directory listing, prefix scans over an inode's records, and cookie resumption all need order.
Rejected.

## Chosen design

### Data model

One map per snapshot.
Keys are byte strings, ordered lexicographically.
Every key starts with the 8-byte big-endian inode number, followed by a 1-byte record kind, followed by a kind-specific suffix.
Clustering by inode number keeps all records of one inode adjacent, and keeps all entries of one directory adjacent.

| Kind | Byte | Suffix | Value |
|---|---|---|---|
| inode | 0x01 | none | version, file type, mode, nlink, size, atime, mtime, ctime, parent inode (directories), next cookie (directories), covered bytes and content version (files) |
| dirent by name | 0x02 | name bytes | child inode, file type, cookie |
| dirent by cookie | 0x03 | cookie, 8 bytes big-endian | child inode, file type, name |
| xattr | 0x04 | xattr name bytes | value bytes |
| chunk extent | 0x05 | byte offset of the chunk, 8 bytes big-endian | one chunk ref: 32-byte block id and 4-byte length |
| symlink target | 0x06 | none | target bytes |

The root directory is inode 1.
There is no inode 0.

Limits: names are 1 to 255 bytes, not containing `/` or NUL; xattr names are at most 255 bytes; xattr values at most 64 KiB; a symlink target at most 4096 bytes.
A file's chunk list is one row per chunk, keyed by the chunk's byte offset.
A chunk is an extent: `[offset, offset + len)`.
Extents are contiguous from offset 0, and their total length is the inode's `covered`, which is at most `size` (the rest is a trailing hole).
Because the rows are keyed by offset inside the same B+tree, appending a chunk, replacing a run of chunks, or truncating at a chunk boundary touches only the leaves on those keys and the path to the root.
Cost is proportional to the chunks changed plus the tree depth, not to the file.
`set_content` (whole list, last writer wins) compares old and new extents and writes only the rows that differ.
The `content version` in the inode record is bumped by every content change and is the compare-and-swap token of `splice_content`.

### Tree nodes

Nodes are B+tree nodes with a size limit (default 4096 bytes encoded, chosen at database creation and stored in the database).
A leaf holds entries (key, value).
An internal node holds entries (lower-bound key, child node id).
All leaves are at the same depth.
A node splits by encoded size when it exceeds the limit.
Deleting the last entry of a node removes the node.
A node below a quarter of the limit merges with a sibling when the result fits within three quarters of the limit.
A root with one child collapses into that child.
An entry larger than the limit sits alone in its node.

Encoding: tag byte, entry count, an offset table, then the entries, so that a lookup does a binary search on the serialized bytes without decoding the node.
The node id is BLAKE3 (keyed derivation context `cowfs-meta node v1`) of the encoded bytes.
An internal node's bytes contain its children's ids, so the id of a node commits to its whole subtree.
The id of the root node is the Merkle root of the snapshot.
A node is re-hashed and compared against the id it was looked up by each time it is loaded from the database, so a bit flip is a `Corrupt` error and never data.
Verified nodes are kept in a bounded in-memory cache (`Options::node_cache`, default 16384 nodes).
Because nodes are immutable and named by their hash, a cache entry never goes stale.

### Snapshots are O(1)

A snapshot is a row in the `snapshots` table: id, name, root node id, creation time, parent snapshot id.
Creating a snapshot from another reads one row, inserts one row, and increments one reference count.
It touches no tree node, so its cost is independent of tree size.
The benchmark measures this at 1,000, 100,000 and 1,000,000 nodes.

Snapshots are independent and equal.
There is no head and no base.
A write to a snapshot replaces that snapshot's root id.
The source snapshot still points at the old root, so nothing written to one appears in the other, in either direction.

### A write touches only a path

An operation such as `create` reads the parent inode, writes the new inode record, two directory entries, and the updated parent record.
Those are a handful of keys, mostly in the same one or two leaves.
The tree edits happen in memory on copies of only the nodes along those paths.
When the transaction finishes, the modified nodes are encoded, hashed bottom-up, and written as new nodes.
Untouched siblings are not read, written, or re-counted.
The depth is about log base 50 of the entry count, so 1,000,000 files need 3 levels.
A single write in the 1,000,000-node tree is measured in the benchmark.

A `batch` runs several operations in one redb write transaction and one materialization, so a burst of operations pays for each modified node once.

### Inode numbers

Inode numbers come from a counter that is reserved durably in blocks (`Options::ino_block`, default 16384; each reservation is a durable commit of its own, so a larger block means fewer latency spikes and a larger number gap after a crash).
The durable record `ino_reserved` is always above every number ever handed to a caller: before handing out the first number of a new block, the store commits the new high-water mark in its own redb transaction (durable, no chunk references, so no hook).
After a crash the counter restarts at `ino_reserved`, so the first number handed out is above every number a caller could have seen.
A clean `close()` or drop writes the exact counter, so a normal restart wastes nothing.
A failed batch consumes the numbers it allocated; they are not returned.
Numbers are never reused after unlink either, so a stale NFS handle to a deleted file fails cleanly.
The limit is 2^40 inode numbers (`INO_LIMIT`); reaching it returns `Error::LimitExceeded` on the operation that needs a new number.

Snapshot ids are also never reused (the counter is part of every snapshot commit) and are below 2^24 (`SNAPSHOT_LIMIT`); creating the 2^24th snapshot returns `Error::LimitExceeded`.

Snapshots share inode numbers at the moment of cloning.
The same inode number in two snapshots means "the same file as of the clone", which can diverge later.
The identity of a live file is therefore the pair (snapshot id, inode number).
`Meta::pack_ino(snapshot, ino) -> Option<u64>` packs the pair as `snapshot << 40 | ino` and `Meta::unpack_ino` reverses it.
The mapping is a pure function: it needs no table and is the same after a restart.
`pack_ino` returns `None` only for values the store never hands out (snapshot 0, or a value at or above its limit).
A mount adapter should use the packed value for `st_ino` and NFS file handles, otherwise tools that compare `(dev, ino)` would see files in two clones as hardlinks of each other.

### Hardlinks and `nlink`

A hardlink is two directory entries that name one inode number.
`nlink` is a field of the inode record.
`link` inserts a dirent pair and rewrites the inode record.
`unlink` removes the dirent pair, decrements `nlink`, and deletes the inode's records (inode, xattrs, chunk segments, symlink target) when `nlink` reaches zero.
A write through any name changes the one inode record and its chunk segments, so every name sees it.

Across a snapshot boundary, both snapshots initially share the inode record.
After a `link` or `unlink` in one snapshot, its inode record has a new `nlink`, and the other snapshot's record is unchanged.
After a write in one snapshot, its chunk segments differ, and the other's are unchanged.
Before any divergence the chunk list is literally the same tree node in both snapshots.
Directories cannot be hardlinked.
A directory's `nlink` is 2 plus its number of subdirectories, kept up to date by `mkdir`, `rmdir`, and `rename`.

An inode whose `nlink` reaches zero is deleted immediately.
Unlink-while-open is the mount adapter's job (issue #20): `unlink` returns the removed inode's attributes and chunk list so an adapter can keep them for open handles.
This is a known gap for the adapters and is not solved here.

### Reachability, reference counts, and garbage collection

Each stored node has a reference count in the `refs` table: the number of parent-slot references from other stored nodes plus the number of snapshot roots naming it.
A node exists in `nodes` exactly when its count is at least one.
Because nodes are content-addressed, two snapshots that share a subtree, or two histories that produce the same node, share one stored node and count both.

Materialization at the end of a transaction accumulates count changes in a map.
A new node adds one to each of its children, unless a node with that id already exists, in which case its children were already counted.
The new root adds one, and the old root subtracts one.
Increments and decrements on the same untouched sibling cancel before any write.
Then nodes whose final count is zero are deleted, and their children are decremented in turn.
Only nodes on replaced paths are ever read or freed.
Removing a snapshot decrements its root, and frees exactly the nodes no other snapshot shares.
That free is proportional to the nodes unique to the removed snapshot.

`live_blocks(snapshot, marker)` walks the tree and yields the block id of every chunk ref.
A `Marker` is a set of node ids already visited.
Before descending into a node the walker checks the marker and skips the node and its whole subtree if present.
Snapshots that share subtrees with an already-walked snapshot cost only the differing paths.
The benchmark measures a walk with and without skipping after a small change.
The marker is valid for the tree state it was built from.
Block ids inside a skipped subtree were already yielded by the earlier walk, so a garbage collector that unions results over one marker sees the complete live set.

### Directory cookies

Each directory inode has a `next_cookie` counter starting at 1.
Adding a directory entry assigns it the next cookie and stores it in both the by-name and the by-cookie records.
Cookies never change while an entry exists and are never reused within a directory.
`readdir(dir, cookie, max)` scans by-cookie records with a cookie greater than the given one, so resuming after any cookie returns the entries added later and skips nothing that remained.
Removing entries while a listing is in progress cannot duplicate or drop the survivors, because each survivor has one fixed cookie.
Two hardlinked names in the same directory have different cookies, so the spike 2 bug (an inode-based cookie shared by names of one inode) cannot occur.
A rename inside one directory keeps the entry's cookie (the entry keeps its place in listings, so a listing that renames every entry it sees still terminates).
A rename into another directory gives the entry a new cookie there, at the end of that directory's order.
POSIX leaves it unspecified whether a listing in progress shows a moved entry.
Cookie 0 means "start".
`.` and `..` are not returned by `readdir`.
`lookup` resolves `.` and `..`, and directory inodes record their parent, so an adapter can synthesize them.

### Reading and the API surface

- `Meta`: opens or creates a database (`open`, `open_recover`), lists and removes snapshots, `sync`, `close`, `check`, `reap_step`.
- `Snapshot`: a cheap, cloneable, `Send + Sync` handle to one snapshot with `&self` methods.
- `Snapshot::batch`: runs a closure over a `Tx` that has the same operations, applied atomically.
  If the closure returns `Err` or panics, none of its changes are kept.
- Content: `chunks`, `chunk_range(ino, start, end)`, `content_version`, `set_content`, and `splice_content(ino, expected_version, start, end, new_chunks, new_size)`, a compare-and-swap that returns `Error::Conflict` when the version moved on.
- `setattr(size)` shrinks a file only to a chunk boundary; any other shrink returns the distinct `Error::NeedsRechunk` and the caller re-chunks the tail, `put`s the new tail block, and splices.

### Applied and durable

A mutating call is applied to an in-memory copy of the snapshot's tree (only the nodes on touched paths are copied).
Every reader sees it at once and never sees part of a batch.
It reaches redb only in a durable commit, which writes all pending snapshot trees, their reference counts, the snapshot rows and the inode reservation in one redb transaction with two-phase commit.
`Snapshot::info` and `Snapshot::root` report the applied root.
`Meta::durable_snapshots` reports what the file holds, which is what a crash right now would leave.

A durable commit starts when any of these is true:

1. A snapshot is created, forked or removed (these are durable on return).
2. `sync_every_ops` applied calls are pending (default 256; a batch counts as one call).
3. `max_pending_bytes` of changes are pending (default 32 MiB), which also bounds memory.
4. The oldest pending change is `sync_interval` old (default 1 s).
   A background thread owned by `Meta` enforces this even when no further call arrives.
   It holds no strong reference to the handle and stops when the last handle is dropped.
   `Options::background = false` disables it, and then the caller drives `sync()`.
5. The caller calls `sync()` or `close()`, or `Options::ack = Ack::Durable` makes each call wait for a commit.

`Ack::Durable` uses group commit: a caller whose change is applied waits for the next commit; the first waiter becomes the leader, waits up to 2 ms for callers that are already inside `batch` to apply, and then runs one hook and one fsync for all of them.
Every caller returns only after its change is durable.

The bound on lost recent writes after a crash is the changes applied since the last durable commit: fewer than `sync_every_ops` calls and less than `max_pending_bytes` of changes, and, with the background thread on, no older than `sync_interval` plus one commit time.
With the thread off, the age is unbounded until the next call, `sync()` or `close()`.

### Ordering with the block store (the hook)

A chunk list committed durably must not reference blocks that are not durable.
`Options::before_sync` is the hook, and the mount layer sets it to `Store::sync`.
The precise guarantee:

- Every durable commit is preceded by exactly one run of the hook, and the hook returns before the redb transaction begins.
- The hook runs after every batch closure whose changes the commit carries has returned, because a commit takes the same writer lock a closure holds while it runs.
  So a closure that `put`s a block and then references it can never become durable before the block is: the block was `put` before the closure returned, and the hook (which syncs the store) runs after that.
- A hook error aborts the commit: nothing changes on disk, the changes stay applied in memory, and the error is returned to the caller of `sync`, `close`, or a durable-ack call.
  The timer retries every `sync_interval`.
- `sync()` runs the hook even when nothing is pending, so `Meta::sync` can be used as "sync the store, then the metadata".
- The hook must not call this crate; a call from inside it returns `Error::Reentrant` (checked per thread) instead of deadlocking.
- Dropping the last handle runs the same path as `close()`.
  If the hook fails, or the commit fails, the pending changes are discarded, never written: the store closes without redb's own close-time commit making them durable.
  `close()` reports the error; drop cannot.
- Inode reservations (F3) are committed without the hook.
  They carry no chunk references.

### Crash-consistency argument

All persistent state lives in one redb database file.
Tree nodes, reference counts, snapshot rows, the inode reservation, the removal queue and the format header are rows of that database, and each durable commit changes all the rows it needs in one redb write transaction.
So the rows that a crash exposes are the rows of some committed prefix of the commit history, because redb makes each transaction atomic and orders commits.
redb publishes a new root by flipping a single byte after the pages it names are written and synced.
We enable two-phase commit so that the primary commit slot is valid without relying on checksums.
A reopened database is therefore the state at some commit boundary at least as recent as the last durable commit that completed before the crash.
Within that state, the node tree is a set of immutable content-addressed nodes written with all their children in the same transaction or earlier, and reference counts change in the same transaction as the nodes they count, so counts and nodes cannot disagree.
`check()` re-derives all of this from scratch.
The tests cover this claim (see "Tests").
They do not prove it.

### A lost fsync (F5)

On macOS, Rust std uses `fcntl(F_FULLFSYNC)` for `File::sync_all` (seen as `fcntl` under `File::sync_all` in a sample profile of a redb commit here) and, from its source as I recall it and not re-checked in this session, for `sync_data` too, so redb's syncs should reach the platform's strongest guarantee (unverified for `sync_data`).
A disk that acknowledges a flush it did not perform is outside POSIX and outside this crate's model.
What redb does then: if the newest commit slot does not verify and two-phase commit was used, `open` fails with "Primary is corrupted despite 2-phase commit" (redb's `do_repair`).
`Meta::open` keeps that behaviour: it fails closed, never guesses.
`Meta::open_recover(path, opts)` is the explicit, never automatic path.
It copies the file to `<path>.pre-recover`, clears the two-phase flag and sets the recovery flag in the redb header, which makes redb verify the primary slot and fall back to the previous commit's slot.
It returns a `Recovery` report (rolled back or not, the backup path, the recovered snapshots).
If recovery fails, it restores the file from the backup.
Everything committed after the previous commit is lost, and the caller decides whether to accept that.
The mitigation for the failure mode itself is the backup copy: take a copy of the file (or use the store's own backups) before running with a disk whose flushes are in doubt.

### Consistency check

`Meta::check()` first calls `sync()`, then runs in one read transaction and verifies:

1. Every snapshot root exists.
   Every reachable node hashes to its id, has sorted keys, has separators consistent with its children, and all leaves have the same depth.
2. Recomputed reference counts equal the `refs` table exactly.
   No node exists that no snapshot reaches, and no reference count exists for a missing node.
3. For every snapshot: every directory entry names an existing inode of the recorded type, and its by-cookie twin exists and agrees.
   Every cookie is below its directory's `next_cookie`.
   `nlink` equals the number of directory entries naming a file or symlink, and equals 2 plus the number of subdirectories for a directory.
   Every non-root inode is named by at least one entry and is reachable from the root, so there are no orphaned records and no detached directories.
   A directory has exactly one parent entry and its recorded parent matches.
   xattr, chunk, and symlink records belong to an inode of the right type.
   A file's extents are contiguous from offset 0, their total equals `covered`, and `covered` is at most `size`.
   All inode numbers are below `ino_reserved`.
4. The snapshot name index and the snapshot table agree.
5. Roots waiting in the removal queue are counted like snapshot roots, so reference counts stay exact while a removed snapshot is freed in steps.

Memory holds per-inode counters and per-node counters (no data, no directory rows: a directory's entries are checked with a streaming multiset hash).
Measured: +230 MB of resident memory for 1,001,000 inodes, which is not the bounded-memory checker the review asked for (see Measurements).

## Tests

`cargo test -p cowfs-meta` runs all of these except the ones marked ignored.

- `tests/model.rs`: model-based property test against an in-memory POSIX tree over random sequences of every operation (including `splice_content` with stale versions and misaligned ranges), checking every result and `check()` after each step.
- `tests/posix.rs`: snapshot isolation in both directions, hardlink semantics, rename edge cases, listing under removal, atomic batches.
- `tests/crash.rs`, `tests/critic.rs`: crash injection through redb's `StorageBackend`.
  A recording backend logs every write, set_len and sync.
  Crash images are built at every N-th log event (`CRIT_STRIDE`, default 4, `1` for every event) under five loss policies: prefix with a torn last write, only fsynced writes, 512-byte sector shredding with random subset and reorder, 4 KiB sector shredding, and lost-last-fsync.
  Each image is reopened, `check()`ed, and its snapshot roots and a full content digest must equal a committed boundary between the last durable commit before the crash point and the next commit.
  The lost-last-fsync images that redb refuses must recover through `open_recover` to a committed boundary.
  Workloads: a random mix, snapshot create/fork/remove, and a removed snapshot freed in several reap steps.
- `tests/kill9.rs`: a child process mutates in a loop, the parent SIGKILLs it, reopens, checks, and compares with a replay.
- `tests/corrupt.rs`: truncated and bit-flipped database files give an error or a valid tree, never a panic.
- `tests/review.rs`: one regression test per review finding:
  F1 hook ordering with a store whose blocks are durable only after sync (1000 commits, 0 dangling), the same with the real `cowfs-store`, hook count per commit and the `sync_every_ops` boundary, `sync()` with nothing pending runs the hook, the timer flushes idle changes and retries after a hook failure, inode numbers never reused after a crash at every event (two block sizes), snapshot ids and `pack_ino`, hook re-entry, group commit, rename-while-listing terminates, `NeedsRechunk`, `splice_content` compare-and-swap, append cost in bytes written (10 vs 50,000 chunks), incremental snapshot removal, background reaper, foreign redb and non-redb files refused and untouched, fail-closed after a detected corrupt read, transient flips during writes.
  `hammer_120s` is ignored: `cargo test -p cowfs-meta --release --test review hammer -- --ignored --nocapture`.
- `src/check.rs` tests: `check()` negative tests that corrupt a refcount, an nlink, a directory nlink, a dangling entry, an orphan inode, a half directory entry, an extent gap and a node's bytes, through a test-only backdoor, and require the specific report.
- `src/ptree.rs`, `src/node.rs`, `src/error.rs` unit tests: CLOCK eviction bound, node parse of garbage, the panic guard and error mapping.

Heavy runs (release):

- `cargo test -p cowfs-meta --release --test crash -- --ignored --nocapture`
- `CRIT_STRIDE=1 cargo test -p cowfs-meta --release --test critic -- --nocapture`
- `COWFS_KILL_ROUNDS=300 cargo test -p cowfs-meta --release --test kill9 -- --nocapture --test-threads 1`
- `COWFS_CORRUPT_SCALE=5 cargo test -p cowfs-meta --release --test corrupt -- --nocapture`

## Decisions for the lead to review

1. Non-canonical roots (alternative E).
2. `(snapshot id, inode)` is the identity of a live file; `pack_ino` is the restart-stable packing (24 bits snapshot, 40 bits inode).
3. Mutations are applied in memory and reach redb only in durable commits (count, bytes, timer, sync, close, or durable ack).
   A crash loses the applied-but-not-durable changes.
4. The hook runs after the closure and before the redb transaction, once per commit.
5. No unlink-while-open support in this crate.
6. Regular files, directories, symlinks, and special files (fifo, socket, character and block device nodes, see `docs/special-files-107.md`): no uid or gid.
   A special file has no data; a device record is 8 bytes longer and carries its device number.
   A store is created at format version 2 and the commit that first persists a special file writes version 3, which a build that only knows version 2 refuses up front.
7. `setattr(size)` shrinks only to a chunk boundary, otherwise `NeedsRechunk`.
8. Removed snapshots are freed by the reaper in steps of at most 256 nodes per transaction, so the writer stall is one step, not the tree.
   Until the reaper finishes, the removed snapshot's nodes still occupy file space.
9. BLAKE3 runs the portable code path; enabling its `neon` feature is a workspace dependency change left to the lead.
10. redb panics on damaged pages are caught and reported as `Corrupt`; a detected corrupt read makes the handle refuse all writes until it is reopened.
11. `Meta::open_recover` exists for a lost fsync; it loses the newest commit and never runs by itself.

## Robustness against a damaged file

redb 4.3 panics on some damaged pages instead of returning an error, including inside `Database::drop`.
Every entry point that touches redb runs inside a guard that turns such a panic into `Error::Corrupt`, and the database handle is wrapped so its close-time commit cannot panic out of `drop`.
Panics from caller code (a `batch` closure) are not swallowed: they abort the transaction and are re-raised.
The damaged-file test covers truncations and bit flips and requires an error or a valid tree.

## Measurements

Durable operations and random-access lookups lead this table; hot-cache and applied-only figures are not substitutes for either.
These are historical shared-load measurements, not quiet-host acceptance gates or measurements of the current main revision.

Every number below comes from `crates/cowfs-meta/examples/review_bench.rs` (modes `lookup`, `create`, `group`, `splice`, `rm`, `check`), the first design's `examples/bench.rs`, and the review's `critic_perf` for the "before" splice and remove rows.
Machine: Apple M3 Max, APFS, shared with other agents and busy: load1 was 25 to 100 during all runs, so every row is flagged high load and absolute numbers are pessimistic.
Each row is n=5 (n=7 for lookups) repetitions of the stated batch under the shared CPU lock (the "before" remove_snapshot and chunk rows are n=5 runs of `critic_perf`, also under the lock).
Tree: 1,000 files per directory, one 4 KiB chunk per file.

| metric | before | after |
|---|---|---|
| durable create, 1 thread | about 12 to 20 ms (one fsync each) | 19.3 ms (52 per s) |
| durable create, 2 / 8 / 32 threads, group commit | none | 18.4 ms per op (54 per s) / 5.5 ms (182 per s) / 1.2 ms (827 per s) |
| lookup, 1M inodes, random file, node cache warm | not separately measured; original mixed lookup 38.9 to 41.6 us | 18.6 us (12.2 to 28.5) |
| lookup, 1M inodes, random file, node cache cold (fresh open) | not measured | 28.8 us (27.4 to 62.1) |
| lookup, 1M inodes, random file, node cache off | not measured | 56.6 us (48.6 to 72.4) |
| lookup, 1M inodes, 1000 fixed names (hot) | 38.9 to 41.6 us (measured by the reviewer and by the first bench, one mixed pattern) | 1.9 us (range 1.6 to 4.7) |
| lookup, 1M inodes, absent name | not measured | 3.0 us (2.7 to 4.6) |
| create, one call per file, unbatched | 1,775 us (563 per s), 10,000 creates about 17 s | 208.6 us (4,793 per s; range 180 to 250 us), applied in memory, durable by policy |
| create, 1000 per batch | 10.2 us | 35.4 us (under load 73 to 76; the first number was under load 102, not comparable) |
| append one chunk, file of 1k / 10k / 100k / 1M chunks | 106 us / 393 us / 3.68 ms / set_content alone 1.48 s at 1M (whole list) | `splice_content`: 12.6 / 25.3 / 24.4 / 20.6 us |
| replace one mid-file chunk, same sizes | not possible without the whole list | `splice_content`: 57 / 63 / 71 / 74 us |
| append via `chunks()` + `set_content`, same sizes (kept for comparison) | as above | 0.61 ms / 5.6 ms / 353 ms / 1,110 ms |
| remove_snapshot, 1M inodes: call returns after | 933 ms to 1.9 s (median 980 ms) | 19.7 ms (18.3 to 33.8) |
| worst create latency by a concurrent writer while a 1M-inode removal is being reaped | 0.89 to 1.9 s (as long as the removal) | 4.9 ms (1.2 to 13.2) |
| worst create latency including the durable commit of `remove_snapshot` itself | same | 28 ms (13.8 to 43.8) |
| time until all nodes of the removed 1M-inode snapshot are freed | 0.9 s (inline) | 6.2 s (5.9 to 8.0), in the background |
| `check()`, 1M inodes | 2.5 s, 747 MB resident (reviewer, one run) | 4.2 s (3.2 to 5.0), +230 MB resident (302 to 532 MB) |

### Separate round-2 reviewer report

[Issue #40](https://github.com/zeeshanhaque21/cowfs/issues/40) records the critic's separate measurements at load 28 to 85.
Its issue body does not state the repetition count; these are attributed historical reports, not fresh reruns or replacements for the implementation benchmark rows above.

| reviewer metric | figure reported in #40 | comparison limit |
|---|---|---|
| durable single-thread create | 32.8 ms | implementation benchmark reports 19.3 ms in a different run |
| random lookup | 24 us median, range 15 to 56 us | the 1.9 us hot lookup is not a random-access figure |
| remove_snapshot, 200k inodes | 44 ms | implementation benchmark's 19.7 ms row uses 1M inodes in a different run |

Neither workload size nor host load is matched across these reports, so the discrepancies are not measured regressions or speedups.
No current-main performance benefit is established by this documentation correction.

What these show and do not show:

- Hot lookup is single digit microseconds.
  A random lookup in a 1M-inode tree is 18.6 us, not single digit: the node cache (32,768 nodes of 4 KiB) holds a small part of a 520 MB tree, so most random lookups read and hash nodes.
  That target is met only for a hot working set.
- Unbatched mutation is no longer bounded by fsync, because it does not wait for one.
  The price is the loss window in "Applied and durable".
  With `Ack::Durable` each call waits for an fsync, and group commit raises throughput with concurrency (52 per s for one thread, 827 per s for 32), which is fsync-bound on this disk (raw redb one-row fsync commit 10 to 12 ms).
- `splice_content` cost is flat from 1k to 1M chunks within the noise; the whole-list path grows linearly (0.6 ms to 1.1 s).
  The bytes written by an append are flat as well: 33 KB at 10 chunks, 140 KB at 50,000, 197 KB at 100,000 (redb page and path overhead, a leaf-to-root path).
- Writer stall during removal is bounded by one reap step of at most 256 nodes or 4 ms of work; the remaining 28 ms in the whole window is the durable commit that `remove_snapshot` itself performs, which is above the 20 ms target at load 70.
- `check()` is slower in this run than in the review's (load differs: 54 here) and uses less memory, but it is not bounded-memory: +230 MB at 1M inodes.
  The target (streaming, bounded) was not reached.
- The 120 s hammer (`cargo test -p cowfs-meta --release --test review hammer -- --ignored --nocapture`, load about 48): 138,573 batches, 8,828 snapshot reads, 0 torn reads, `check()` clean, worst single write 1.85 s.
  The cause of that worst case is not attributed (candidates: a durable fork or removal commit, or writer starvation on the session lock with four readers); no measurement here separates them.
- Not measured: memory of the node cache, recovery time after an unclean shutdown (the review measured 4.8 s at 1M inodes for redb's repair; nothing here changes it), concurrent readers during writes.
