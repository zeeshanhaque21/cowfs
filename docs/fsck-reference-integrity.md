# fsck reference integrity

`fsck` answers two different questions, and until #84 it answered only one of them.

## Two kinds of check

- **Store-record integrity.** `Store::fsck` snapshots every pack length under the writer mutex, then scans each pack, re-decodes each record and re-hashes its data.
  It reports records, verified unique blocks, duplicate records, gaps, hash mismatches, bad payloads, and index entries that do not point at a verified record.
  It only sees records that are still on disk.
- **Filesystem-reference integrity.** A durable snapshot names blocks in its committed trees.
  `Core::fsck` now walks every durable snapshot root and reports each live block the store does not contain.

A store can be internally clean, every extant record hashing correctly, while a surviving snapshot references a block that is gone.
That is exactly the state an old reader's cache upgrade left behind (#84): the reader unlinked a block a fork still needed, the store acknowledged the loss, and `Store::fsck` reported clean.
A clean store-record check is not a clean filesystem.

## What `Core::fsck` checks now

1. Runs `Store::fsck` over the extant records.
2. Reads the durable snapshot list (`Meta::durable_snapshots`), the same set GC marks from.
3. Walks each durable snapshot's committed tree with one shared `Marker`, so a subtree shared between snapshots is visited once.
4. Filters the all-zero hole id of sparse files, then reports `Damage::MissingLiveBlock { id }` for any live id `Store::contains` is false for.

The result is a `cowfs_store::FsckReport` whose `damage` may hold both record-level and live-reference damage.
`is_clean` is true only when neither is present.

## Scope

- The walk covers **durable** references: blocks named by a committed, synced snapshot tree.
  This is the same set GC must treat as live, so a block reported missing would fail a read through a snapshot mount.
- It does **not** cover runtime-only references: blocks of an open unlinked file, or of a chunk list not yet committed.
  Those are not durable references and are protected at runtime by `Core::pinned_blocks` and the store's own mark, not by fsck.
  A block named only by an uncommitted tree is not reported; it becomes reportable once its snapshot commits.
- The report is bounded: at most `MAX_MISSING_LIVE_REFS` (256) `MissingLiveBlock` entries are listed, so a wholesale store loss cannot turn the answer into an unbounded enumeration.
  The walk itself still visits every live id; the bound is on what is listed, not on what is checked.
- Nothing is repaired, reaped, freed, or moved.
  The traversal is read-only over the committed trees.
  `Meta::live_blocks` syncs metadata before the walk, the same operation `Meta::check` performs, so the walk sees exactly the durable state.

## What an operator sees

- Library: `Core::fsck() -> Result<FsckReport, Error>`, with `Damage::MissingLiveBlock { id }` in `damage`.
- Control: `Handler::fsck` maps it to a `FsckProblem { kind: "missing_live_block", detail: "live reference to absent block <id>" }`.
  `FsckReport::ok` is false whenever `problems` is non-empty.
- CLI: `cowfs fsck` prints `PROBLEMS FOUND` and the problems, and **exits 1** when `ok` is false, so a script gating on the exit code cannot mistake a missing live block for a clean filesystem.
  A clean check exits 0.

## Contracts preserved

- A clean store with only live files stays clean: the walk finds every referenced id present.
- Hash corruption is still detected: the record-level scan is unchanged and its damage is reported alongside any live-reference damage.
- The GC gate and the GC source algorithm are untouched.
  Live-reference integrity is a read-only check over GC's own durable root set; it does not change what GC may free.
