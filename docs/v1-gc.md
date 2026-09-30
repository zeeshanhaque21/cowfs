# v1 garbage collection (`cowfs-gc`)

Issue: #10.
Contract: `docs/design.md`, section "Garbage collection".
This note records the decisions.
It does not change any settled decision in `docs/design.md`.

## The problem in one paragraph

`cowfs-store` has `put` and no `delete`, so a block leaves the store only when the pack that
holds it is rewritten without it.
That makes the collector a two-part problem: decide which blocks are dead, and then reclaim the
bytes of a pack by copying its live records elsewhere and unlinking it.
The first part is a reachability problem over the metadata.
The second part is a durability problem.
Criterion 3 is zero data loss, so the durability argument is the one that matters and it is
written out in full below.

## Crate and dependencies

`cowfs-gc` depends on `cowfs-store` and `cowfs-meta`.
It does not depend on `cowfs-core`.
Blocks that only memory names are reached through the [`ExtraRoots`](#extraroots) trait, which
`cowfs-core` implements for its `Core`.

## ExtraRoots

```rust
pub trait ExtraRoots {
    fn pinned_blocks(&self) -> Vec<BlockId>;
    fn reference_barrier(&self) -> Option<Box<dyn Barrier + Send>> { None }
}
```

`pinned_blocks` returns the chunk ids of open orphans and of files whose chunk list is not
committed yet, which is what `Core::pinned_blocks` already provides.

`reference_barrier` returns a guard that, while alive, keeps the reference side still: no new
metadata commit can make a new block reference visible.
`cowfs-core` returns its flusher lock.
`None` means the caller offers no ordering, and the collector then reports candidates and
reclaims nothing (see "The barrier is required").

## Mark

Marking walks `Snapshot::live_blocks` for every live snapshot and unions the ids, plus
`pinned_blocks`.

Three details are already handled by the crates this one calls, and are not re-implemented here.

- A node is added to the shared `Marker` only after everything below it was yielded, and a
  snapshot whose root is in the marker is skipped whole.
  So one `Marker` shared across all snapshots makes snapshots that share subtrees cost only their
  differing paths.
  This is the incremental marking of `docs/design.md`.
- `live_blocks` calls `sync` first, so the walk sees the state a crash would leave.
- A chunk id equal to `BlockId::of(&[])` is a hole, not a block.
  It is dropped from the live set and is never demanded of the store.

The live set is a `HashSet<BlockId>`.
Its peak size is the number of live blocks, so the collector takes memory proportional to the live
set, about 60 bytes per live block.
That is the same order as the store's own index, and it is reported in `GcReport::live_blocks`.

## The race this design has to close

Write a file whose content is a block that already exists as garbage.
`put` deduplicates onto the old record and returns its id.
The commit makes the block live.
If the collector decided that block was dead a moment earlier, and unlinks the pack that holds it,
the commit has just referenced a block that no longer exists.

No amount of waiting fixes this.
A metadata commit can land at any instant, so a "grace period" alone cannot bound the window.
The window is closed by making the reference side still for the instant of the free.

Two facts make the window small enough to hold.

- A block is only re-referenced by a write that deduplicates onto it, so the risk is confined to
  blocks that are already garbage.
- The parts of the cycle that are long (the mark, the pack copies) do not need the barrier.
  Only the last step does, and that step is an incremental re-walk plus a few unlinks.

## The protocol

A cycle is these steps.
`W` marks a window in which the reference side is held still by a barrier.

1. **Freeze, no barrier.** Read `Meta::durable_snapshots` for the roots, read `pinned_blocks`, and
   read the store's durable watermark `(pack, len)`.
   That watermark is the cycle's **epoch**.
2. **Mark, no barrier.** Walk every root with one shared `Marker` and union `pinned_blocks`.
   The live set is complete for every root that existed at the freeze.
3. **Choose candidates, no barrier.** For every pack, one scan counts its live and dead record
   bytes with respect to the live set.
   A pack is a candidate when its dead bytes reach `dead_ratio` of its record bytes and it has at
   least `min_dead_bytes` dead.
   The active pack is never a candidate.
   A pack that open reported as corrupt is never a candidate.
4. **Copy, no barrier.** Each candidate is rewritten into a fresh pack: every record whose id is in
   the live set is copied byte for byte, so no block is decompressed, rehashed or recompressed.
   The new pack is fsynced, then the index entries of the copied ids are repointed at it, then
   `index.cix` is rewritten.
   The old pack stays on disk.
5. **`W` Verify and unlink.** Take the barrier.
   Re-read the roots and `pinned_blocks`, re-walk only the roots that the `Marker` has not already
   seen, and union them into the live set.
   Then, for every candidate:
   - if every condemned id is still absent from the live set, unlink the old pack, fsync
     `packs/`, drop the index entries of the condemned ids, and lower the watermark `base` to the
     lowest pack that still exists;
   - if any condemned id is now live, leave the old pack alone and report the pack as skipped.
   Its records are still indexed, so every live block stays readable.
   Release the barrier.

The epoch rule from the task is a second, independent guard: a record at or above the epoch
watermark was written after the cycle's freeze, so it is never condemned.
It shrinks the sweep set and costs nothing.

### Why step 5 is enough

Between step 2 and step 5 a write can reference a block that the mark did not see.
That block was `put` in that interval.
If the put wrote a new record, that record is at or above the epoch and is never condemned.
If the put deduplicated onto an old record, the block is in some pack, and step 5 finds it reachable
and leaves that pack in place.
So a block can be condemned only if no write referenced it before the barrier was taken, which is
exactly the condition for its being garbage.

### Why the copies are outside the barrier

A put during step 4 is handled the same way: the new record is above the epoch, or the dedup hit
lands in a pack that step 5 will refuse to unlink.
So the copies need no barrier and the barrier is held only for step 5.
The benchmark in this document measures the write stall that step 5 costs.

### The barrier is required

Without `reference_barrier` there is no sound way to free anything, and pretending otherwise
would trade criterion 3 for reclaimed bytes.
So a collector with no barrier marks, reports, and reclaims nothing, and says so in
`GcReport::skipped`.
This is a decision for the lead: `cowfs-core` needs about ten lines to implement
`reference_barrier` for its flusher lock, and then collection works.

## Access-time hints

`Gc::note_access` records the last-access epoch second of a block in memory.
A read never writes: the map is only touched, and flushed in batches.

- In memory: `HashMap<BlockId, u32>`, capped at `Options::max_hints` (default 1,048,576).
  When the cap is hit, new ids are not recorded and `GcReport::hints_dropped` counts them.
  A dropped hint is a lost optimisation, never a lost block.
- On disk: `<state>/atime.bin`, 36 byte records of a 32 byte id and a 4 byte epoch second,
  appended, each append fsynced, a torn last record dropped on load, and the maximum per id kept.
  Flushing happens at the end of a cycle, and on `Gc::flush_hints`.
- Hints are a **hint only**. They order sweep candidates so the coldest pack is rewritten first,
  and they can raise a pack's effective dead ratio by a small amount (`Options::cold_dead_bonus`).
  They are never the reason a block is freed: reachability decides that, and the barrier in step 5
  re-checks reachability.

## Sweep policy

- A pack is rewritten when its dead record bytes are at least `Options::dead_ratio` of its record
  bytes (default 0.5) and at least `Options::min_dead_bytes` (default 8 MiB) are dead.
- The order is coldest first, by the mean last-access second of the pack's live records, so a run
  under a write load reclaims cold packs first.
- `Options::io_budget_bytes` bounds the bytes copied per cycle (default 2 GiB, 0 means no bound),
  which bounds a cycle's duration and its I/O rate.
- Progress is reported through `Options::progress` after each pack, and
  `Options::cancelled` is polled before each pack and inside the copy loop, so a client can cancel
  and the store is left consistent: a half-copied new pack is a pack with no index entries, and
  open ignores it and the next cycle reuses or removes it.

## What the collector does not do

- It does not decrypt, tier or move blocks to slower storage.
- It does not delete a pack that open reported as corrupt, and it does not touch a `.torn-N`
  sidecar: those bytes stay on disk and their size is reported in `GcReport::quarantined_bytes`.
- It does not repair a store. `Store::salvage` and `Store::acknowledge_corruption` do that.
- It does not run on a store whose `RecoveryReport::has_corruption` is true.
  A collector that runs over known data loss can turn loss into untracked loss.

## API

```rust
Gc::open(state_dir, &Store, &Meta, Options) -> Result<Gc>
Gc::collect(&self, roots: Option<&dyn ExtraRoots>) -> Result<GcReport>   // dry run when Options::dry_run
Gc::note_access(&self, BlockId)
Gc::flush_hints(&self) -> Result<usize>
Gc::cancel(&self)                                     // same as Options::cancelled
Gc::set_progress(&self, impl FnMut(&Progress) + Send + Sync)

pub struct GcReport {
    pub marked: u64, pub marked_skipped_roots: u64, pub pinned: u64,
    pub live_blocks: usize, pub store_blocks: u64,
    pub candidates: u64, pub candidate_bytes: u64,
    pub freed_bytes: u64, pub packs_rewritten: u64, pub packs_unlinked: u64,
    pub records_copied: u64, pub bytes_copied: u64,
    pub skipped: Vec<Skipped>, pub quarantined_bytes: u64,
    pub hints_tracked: u64, pub hints_dropped: u64, pub hints_flushed: u64,
    pub errors: Vec<String>,
    pub dry_run: bool, pub mark_reused_roots: u64, pub barrier: bool,
}
```

`Gc::open` takes the state directory, not the store directory, so this crate writes nothing into
the store except through the store.

## Store API this design needs, and why

`cowfs-store` keeps every field of `Store` private, so the compaction entry points below are
`pub(crate)`-reachable only from inside the store crate.
They are added in `crates/cowfs-store/src/compact.rs`, with one `pub(crate) fn guts(&self)` in
`store.rs` that bundles the fields, so no private field is named outside `store.rs`.

- `Store::packs() -> Vec<PackInfo>`: the pack ids, their current lengths, and which one is active.
- `Store::epoch() -> Option<(u32, u64)>`: the durable watermark, the cycle's epoch.
- `Store::plan_pack(id, live) -> PackPlan`: one scan of a pack, counting live and dead record bytes.
- `Store::rewrite_pack(&PackPlan, live) -> Rewrite`: copy the live records into a new pack, fsync
  it, repoint the index, and rewrite `index.cix`. Does not unlink anything.
- `Store::discard_pack(id, &condemned) -> u64`: unlink a pack, fsync `packs/`, drop the condemned
  index entries, and lower the watermark base. Only valid after `rewrite_pack` and only when no
  condemned id became live.

## Decisions for the lead to review

1. The barrier is required for any free, and `cowfs-core` has to provide it.
   The alternative (proceed without it) risks silent data loss and is not implemented.
2. The live set and the already-marked roots are persisted in `<state>/mark.bin`, rewritten
   atomically at the end of a cycle, and dropped when they exceed `Options::max_persisted_blocks`.
   Within a cycle, skipping is `cowfs-meta`'s `Marker`.
3. Condemning a block never makes it unreadable early.
   It stays readable until its pack is unlinked in step 5, and a pack that cannot be unlinked is
   left fully in place.
4. A cancelled cycle leaves copied packs behind with no index entries.
   Open ignores them and the next cycle reuses them, so cancellation costs disk, never data.
5. Records are copied byte for byte.
   Compaction is bounded by the read and write path, never by compression.

## Tests

- `tests/mark.rs`: shared subtrees are walked once, unchanged roots are skipped, holes are not
  demanded, pinned blocks are marked.
- `tests/model.rs`: model-based property test over random put, reference, unreference, snapshot
  create and remove, and collect.
  After every collect, every block the model says is live reads back.
- `tests/race.rs`: writers, snapshot creation and removal, and collect all at once, with a barrier
  that behaves like `cowfs-core`'s flusher lock.
  Afterwards every block any snapshot references reads back.
- `tests/crash.rs`: crash injection at every durability event of a compaction, then reopen.
  A negative control shows the test fails when the unlink is moved before the new index is durable.
- `tests/kill9.rs`: a child runs writes and collect, the parent SIGKILLs it at a random moment,
  120 rounds.
- `tests/control.rs`: a dry run leaves the store directory byte-identical, a cancelled collect
  leaves it consistent, removing a snapshot and collecting frees exactly the expected bytes.
- `tests/corrupt.rs`: a corrupt pack is reported and never compacted, quarantined bytes are
  reported and kept.
