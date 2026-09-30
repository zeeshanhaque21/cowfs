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
- Progress is reported through `Gc::set_progress` after each pack copied and after each pack
  unlinked, and `Gc::cancel` is polled before each pack, inside the copy loop, and before each
  unlink, so a client can cancel from its own progress handler and the store is left consistent: a
  half-copied new pack is a pack with no index entries, and open indexes it as ordinary data.
- A cancel is **sticky** until `Gc::resume`. A client that cancels once and then polls `collect`
  must not get a second full cycle by accident.
- A cycle that cannot free, because the caller offered no barrier, does not copy either. A copy
  nobody unlinks only costs space.
- The I/O budget is spent on **copied bytes**, not on packs looked at. A pack of pure garbage is a
  candidate and copies nothing, which is correct: it is unlinked without a new pack.

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
   The alternative, proceed without it, risks silent data loss and is not implemented.
   A collector with no barrier marks, reports its candidates and touches nothing.
2. Cross-cycle incrementality is at **root** granularity: a snapshot whose root an earlier cycle
   walked is skipped whole. Within a cycle it is at **node** granularity through `cowfs-meta`'s
   `Marker`, which is where the measured 4.8x comes from.
   A persistent set of *node* ids would make a single changed file cheap too, but `Marker` does not
   expose its contents, so that needs a change in `cowfs-meta`.
   Until then a change to one snapshot costs one full walk of that snapshot.
3. Condemning a block never makes it unreadable early.
   It stays readable until its pack is unlinked in step 5, and a pack that cannot be unlinked is
   left fully in place.
4. Records are copied byte for byte.
   Compaction is bounded by the read and write path, never by compression.
5. Reclaimable space is bounded by how **lumpy** the dead data is, not by how much of it there is.
   A pack is only rewritten when it is mostly dead, so live data spread evenly through every pack
   leaves most of the garbage in place. That is the correct trade: rewriting a pack that is mostly
   live costs a copy and frees almost nothing.
6. `Store::finish_compaction` checkpoints `index.cix` per pack, which is O(entries) per pack and
   therefore quadratic over a sweep. Dropping it to one checkpoint per cycle, or making the
   checkpoint incremental, is the first thing to fix if a sweep over many packs is slow.
7. One cycle runs at a time per collector: a second `collect` waits.
   Two overlapping cycles each hold their own picture of what is live, and the second to unlink a
   pack can free a block the first has just decided to keep.
   The concurrency suite found exactly that, twice in eight runs, before the cycle was serialised.
   A shared `Gc` is `Sync` and safe to call from any number of threads; it just runs them one at a
   time. Two `Gc` handles over one store are still not a supported configuration.

## Measurements

Tool: `cargo run --release -p cowfs-gc --example gc_bench -- <mark|compact|garbage> <blocks> <n>`.
Corpus is generated: 16 directories of `blocks/16` files of 64 KiB, half compressible, so a pack
holds a realistic mix.

Machine: Apple M3 Max, APFS, shared with other agents.
Load1 during the runs below was 32 to 60, so every figure is a noisy lower bound and differences
under about 20% are not meaningful. n is 5 for every row and the median is shown.
The mark and compact rows are at `blocks` 25,000, which is about 33,500 live blocks.

| Metric | Median | Range | Baseline | n |
|---|---|---|---|---|
| Mark, 33,516 live blocks, 6 snapshots, one shared `Marker` | 0.017 s | 0.011 to 0.035 | 0.101 s with a fresh `Marker` per snapshot, 6.0x more yields (33,581 against 201,296) | 5 |
| Compaction throughput, records copied | 2.1 MiB/s | 1.2 to 2.7 | 2,230,744 bytes copied per run, 8 MiB packs | 5 |
| Collect a store that is 90% garbage, `dead_ratio` 0.5 | 35.194 s | 24.509 to 47.779 | 5,476.0 MiB of packs down to 1,566.8 MiB, 20,506,826,760 bytes freed | 5 |
| The same at `dead_ratio` 0.1, on a smaller corpus | 2.936 s | 2.815 to 3.263 | 876.4 MiB down to 257.1 MiB, 3,264,322,230 bytes freed | 5 |

The mark row is the one that matters for the design: the incremental marking of `docs/design.md`
is real, and 6.0x is what six snapshots sharing a tree buy at 33,516 live blocks.
The absolute times here are much lower than an earlier draft of this table claimed
(0.463 s to mark, 1.761 s to sweep). Those figures were taken from a corpus generator whose
per-directory seed was `d * 1000 + i`, so once a directory held more than 1,000 files the seeds
overlapped across directories and the store deduplicated them away. `blocks` therefore stopped
scaling the corpus at around 23,500 live blocks, and the 33,525 the old table named was not
reachable at all. The generator is fixed (`d * files + i`), and every row above is re-measured
on the merged tree.
The 1,000,000-block mark of the contract was not run: creating a million 64 KiB files takes longer
than a session, and the trend from 21,496 to 53,619 live blocks is linear with no knee.

The compaction row is low, and the reason is in the decision above: a per-pack checkpoint of
`index.cix` and a per-pack scan of the candidate set.
Both are O(entries) per pack, so a sweep over many packs is quadratic.
The garbage rows are the evidence, and they are worse than this table first suggested: the
`dead_ratio` 0.5 row sweeps 5,476 MiB of packs and takes 35.194 s, while the 876.4 MiB row
takes 2.936 s.
That is 19x the data for 12x the time, so the growth is real but not yet quadratic in practice.
At a thousand packs it will be. A sweep needs to checkpoint `index.cix` once per condemned pack
and must rescan the candidate set per pack, and neither is batched.
This is the first thing to fix before a sweep over a real store, and it is not fixed here.

## Tests

Run everything with `cargo test -p cowfs-gc`. The heavy ones are listed after.

- `src/lib.rs`, `src/state.rs`: 12 unit tests. A hole is the zero id and not the hash of nothing,
  a barrier is `None` when a caller offers none, hints survive a reload and a torn last record is
  dropped, the cap drops hints and counts them, a corrupt or truncated state file is an empty set
  and never an error, a full set is dropped rather than written huge, and a real `NodeId` round
  trips through the file.
- `tests/mark.rs`: 11 tests. Shared subtrees walked once across six forks, an unchanged root
  skipped whole with its blocks still live, a changed root walked in full, holes never demanded of
  the store, pinned blocks never swept and reclaimable once unpinned, the active pack never a
  candidate, state kept out of the store directory.
- `tests/model.rs`: one property test, 40 cases of up to 60 random steps over write, fork, unlink,
  snapshot remove, collect and reopen, against an in-memory model of the live set.
  The invariant is checked after every step, not just at the end: every block the model says is
  live reads back with exactly the bytes that were put.
- `tests/crash.rs`: 4 tests. A crash image at each of six steps of a compaction, each reopened,
  `fsck`ed and re-collected. A negative control builds the ordering bug the protocol exists to
  prevent, the pack holding the live blocks unlinked with no copy, and the same assertions catch
  it: 1 of 2 live blocks gone. A cancelled collect leaves the store consistent and the next cycle
  does the work.
- `tests/race.rs`: 5 tests. Writers, snapshot creates, snapshot removes and collects at once for
  1.5 s, with a barrier that behaves like `cowfs-core`'s flusher lock, checking after every collect
  that every referenced block reads. A collector with no barrier frees and copies nothing. Two
  collectors on one store. The stall a barrier costs a writer.
- `tests/control.rs`: 9 tests. A dry run leaves the store directory byte-identical and still
  reports, progress is streamed and monotonic, a cancel stops the cycle and is reported, removing
  a snapshot reclaims exactly the bytes the report said, a read never writes and hints flush in
  append-only batches, the I/O budget bounds a cycle, a store with known data loss is refused.
- `tests/corrupt.rs`: 4 tests. A pack with damage to durable bytes is reported, refused by
  `begin_compaction`, never unlinked, and makes the collector refuse the store. A gap is counted
  and never copied away silently. A torn sidecar is reported and kept. An acknowledged store is
  collectable again after a restart.
- `tests/kill9.rs`: 1 test, ignored. A child writes, syncs and collects in a loop while the
  parent SIGKILLs it at a random moment. Run:
  `COWFS_KILL_ROUNDS=120 cargo test -p cowfs-gc --release --test kill9 -- --ignored --nocapture --test-threads 1`.
  The measured run: 120 rounds, 309 live block reads verified, no corruption and `fsck` clean every
  time.

The benchmarks are an example, not a test: `crates/cowfs-gc/examples/gc_bench.rs`.
