# cowfs-meta design

Issues: #8 (Merkle tree, O(1) writable snapshots) and #9 (redb metadata, crash consistency).
Contract: `docs/v1-architecture.md`, section "cowfs-meta contract".
This note is the design.
Numbers in the "Measurements" section come from `crates/cowfs-meta/examples/bench.rs` and are the only performance claims made here.

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
| inode | 0x01 | none | version, file type, mode, nlink, size, atime, mtime, ctime, parent inode (directories), next cookie (directories) |
| dirent by name | 0x02 | name bytes | child inode, file type, cookie |
| dirent by cookie | 0x03 | cookie, 8 bytes big-endian | child inode, file type, name |
| xattr | 0x04 | xattr name bytes | value bytes |
| chunk segment | 0x05 | segment index, 4 bytes big-endian | up to 128 chunk refs, each 32-byte block id and 4-byte length |
| symlink target | 0x06 | none | target bytes |

The root directory is inode 1.
There is no inode 0.

Limits: names are 1 to 255 bytes, not containing `/` or NUL; xattr names are at most 255 bytes; xattr values at most 64 KiB; a symlink target at most 4096 bytes.
A file's chunk list is split into segments of 128 chunks so that a file of tens of thousands of chunks never puts a huge value into one tree node, and a change to one region rewrites only the affected segments.
Setting the content of a file compares the old and new segments and writes only the ones that differ.

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
Every node read re-hashes and compares against the id it was looked up by, so a bit flip is a `Corrupt` error and never data.

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

A global counter in the `meta` table hands out inode numbers.
The counter and the inode records it numbers commit in the same redb transaction, so a crash never leaves a used number unrecorded, and a number is never reused.
Inode numbers are never reused after unlink either, so a stale NFS handle to a deleted file fails cleanly.

Snapshots share inode numbers at the moment of cloning.
The same inode number in two snapshots means "the same file as of the clone", which can diverge later.
The identity of a live file is therefore the pair (snapshot id, inode number).
Snapshot ids are also never reused.
A mount adapter must put both in NFS file handles and must derive `st_ino` from both, otherwise tools that compare `(dev, ino)` would see files in two clones as hardlinks of each other.
This is the reading of "unique across snapshots' live files" that an O(1) snapshot allows.
The lead should confirm it.

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
A rename gives the moved entry a new cookie in its destination directory.
POSIX leaves it unspecified whether a listing in progress shows a renamed entry.
Cookie 0 means "start".
`.` and `..` are not returned by `readdir`.
`lookup` resolves `.` and `..`, and directory inodes record their parent, so an adapter can synthesize them.

### Reading and the API surface

- `Meta`: opens or creates a database, lists and removes snapshots, checks consistency, syncs.
- `Snapshot`: a cheap, cloneable, `Send + Sync` handle to one snapshot with `&self` methods.
  Read methods open a redb read transaction.
  Write methods run one redb write transaction and update that snapshot's root.
- `Snapshot::batch`: runs a closure over a `Tx` that has the same write methods, in one transaction that is aborted if the closure returns an error.

redb allows one writer and many concurrent readers, and cowfs-meta inherits that.
Writers to any snapshot are serialized.
Readers never block and never see a partial transaction.

### Durability policy

Every mutating call, and every `batch`, is one redb write transaction and is atomic.
The transaction commits with `Durability::None` unless one of these holds, in which case it commits with `Durability::Immediate` (fsync, two-phase commit):

1. The call is a snapshot operation (create root snapshot, create snapshot, remove snapshot).
2. The number of mutating transactions since the last durable commit has reached `Options::sync_every_ops` (default 256).
3. The time since the last durable commit has reached `Options::sync_interval` (default 1 second).
4. The caller called `sync()`.

A durable commit makes every earlier non-durable commit durable too.
The exact bound on lost recent writes after a crash is: at most `sync_every_ops - 1` mutating transactions, and, while mutations keep arriving, at most `sync_interval` of wall time plus one transaction.
If mutations stop, the last fewer than `sync_every_ops` transactions stay unsynced until `sync()`, dropping the handle, or the next mutation.
There is no background thread.
The mount layer is expected to call `sync()` on fsync, on unmount, and on a timer.
Dropping the last handle performs a final `sync()`, because a non-durable commit is otherwise lost on a clean exit.

A crash discards a suffix of the transaction history.
It never exposes part of a transaction.
That is the crash-consistency argument below.

Ordering with the block store: a chunk list committed durably must not reference blocks that are not durable.
`Options::before_sync` is a hook called immediately before every durable commit, and the mount layer sets it to `Store::sync`.
An explicit `sync()` calls it too.
If the hook fails, the durable commit does not happen and the error is returned.

### Crash-consistency argument

All persistent state lives in one redb database file.
Tree nodes, reference counts, snapshot rows, the inode counter and the format version are all rows in tables of that database, and every mutating call changes all the rows it needs in one redb write transaction.
So the rows that a crash exposes are the rows of some committed prefix of the transaction history, because redb makes each transaction atomic and orders commits.
redb's commit protocol publishes a new root by flipping a single byte after the pages it names are written and synced, and verifies page checksums on repair.
We enable two-phase commit on durable commits so that the primary commit slot is valid without relying on checksums.
A reopened database is therefore a snapshot of the history at some transaction boundary that is at least as recent as the last durable commit that completed before the crash.
Within that state, the node tree is a set of immutable content-addressed nodes.
Every node in `nodes` was written with all its children in the same transaction or earlier, and reference counts change in the same transaction as the nodes they count, so counts and nodes cannot disagree.
`check()` re-derives all of this from scratch.
The tests cover this claim (see "Tests").
They do not prove it.

### Consistency check

`Meta::check()` runs in one read transaction and verifies:

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
   A file's size is at least the sum of its chunk lengths.
   All inode numbers are below the global counter.
4. The snapshot name index and the snapshot table agree.

## Tests

- Model-based property test against an in-memory POSIX tree over random sequences of every operation, checking every result and `check()` after each step.
- Snapshot isolation in both directions, hardlink semantics, rename edge cases, listing under removal.
- Tree-level property test with a tiny node size to force deep trees, splits, merges, and root collapse.
- Crash injection through redb's `StorageBackend`: a recording backend logs every write, set_len, and sync.
  Crash images are built from the log at hundreds of points with three policies: everything up to the crash point with the last write torn, only synced writes, and all synced writes plus a random subset of unsynced writes each possibly torn.
  Each image is reopened, `check()`ed, and its snapshot roots must equal the state at a committed transaction boundary that is between the last durable commit before the crash point and the last commit begun.
- A kill -9 test: a child process runs a deterministic mutation loop and reports progress on a pipe.
  The parent kills it at random moments, reopens, runs `check()`, and requires the state to match a replay of the same workload at a step that is at least the last reported durable step.
- No-panic test: truncated and bit-flipped copies of a database file either fail to open, fail `check()` or an operation with an error, or pass `check()`.
  They never panic.

## Decisions for the lead to review

1. Non-canonical roots (alternative E).
2. `(snapshot id, inode)` is the identity of a live file, and adapters must combine them.
3. Default durability numbers: 256 transactions or 1 second.
4. The `before_sync` hook as the way to order store syncs before metadata syncs.
5. No unlink-while-open support in this crate.
6. Only regular files, directories, and symlinks: no device nodes, FIFOs, or sockets, and no uid or gid.
7. `setattr(size)` shrinks only to a chunk boundary, and any other shrink is `set_content` after the caller has written the new tail chunk into the store.
8. Removing a snapshot frees its unique nodes in one write transaction, so removing a large snapshot holds the writer lock for a while.

## Measurements

Filled in from `crates/cowfs-meta/examples/bench.rs`.
