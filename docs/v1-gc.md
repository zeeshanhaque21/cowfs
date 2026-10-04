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
    fn pinned_blocks(&self) -> Result<Vec<BlockId>, RootsError>;
    fn reference_barrier(&self) -> Result<Option<Box<dyn Barrier>>, RootsError> {
        Ok(None)
    }
}

pub enum RootsError {
    /// A writer holds a lock the answer needs. Try again later.
    Busy,
    /// The answer cannot be produced at all.
    Unavailable,
}
```

`pinned_blocks` returns the chunk ids of open orphans and of files whose chunk list is not
committed yet, which is what `Core::pinned_blocks` already provides.

`reference_barrier` returns a *way* to keep the reference side still, not a guard that already
does.
Constructing it must cost nothing and stall nobody; acquiring is `Barrier::take`, and that is where
a writer waits.
The collector calls `take` once, in step 5.
`cowfs-core` returns its flusher lock.
`Ok(None)` means the caller offers no ordering, and the collector then reports candidates and
reclaims nothing (see "The barrier is required").

### The contract core must meet

**Both methods are fallible, and that is the whole point of them.**

The reason is measured, not theoretical. `Core::pinned_blocks` returns an empty vector whenever
any writer holds a node lock: 38,372,183 of 38,374,217 polls against a live writer returned zero of
three chunks. A collector cannot tell that answer from "nothing is pinned", so a garbage collector
wired to that implementation frees whatever those writers have in flight. Those three chunks were
live data. This is the single worst failure mode in the crate, because it is silent and it deletes
the thing the whole design exists to protect.

So the rules are:

1. **An answer is exact or it is an error.** There is no partial answer. If you cannot enumerate
   every block you hold right now, return `RootsError::Busy`. Do not return what you managed to
   read, and above all do not return an empty vector because a lock was contended.
2. **`Busy` and `Unavailable` mean the same thing to the collector: nothing is freed.** The
   collector marks, reports the failure in `GcReport::roots_error`, skips every candidate with
   `SkipReason::RootsUnavailable`, and frees nothing and copies nothing. It does not distinguish
   them, because both mean "I cannot tell you what is pinned".
3. **A barrier that cannot be promised is no barrier.** `reference_barrier` returning `Err` stops
   the cycle exactly as returning `Ok(None)` does.
4. **Answering twice is safe; answering once is not required to be stable.** The collector polls
   more than once per cycle: at the freeze, after the mark, before the copy, and once per pack
   before each unlink. It unions every answer, so a block reported on any poll survives. An
   implementation that is lazy on one poll and correct on another loses nothing, which means a
   reference side under load can afford to answer `Busy` rather than a guess.
5. **Reporting a block that is not pinned is safe and costs only space.** Unioning never removes a
   block from the protected set, so over-reporting is the cheap direction.

The collector cannot defend against rule 1 being broken. If `pinned_blocks` returns an empty vector
while blocks really are pinned, and does so on every poll, the collector will free them. The
re-poll makes a *transient* lie harmless; it cannot make a *persistent* one safe. That is why the
methods return `Result`.

### What the collector does with the store

The store refuses to be collected over when it is not in a state the collector understands, and the
refusal happens before anything is copied or freed:

- **A lock it does not hold.** `Store::open` returns `Error::Locked` with the holder's pid, so a
  collector can never be built over a store another process holds. `Store::close` consumes the
  store, and the collector holds an `Arc<Store>`, so there is no window in which a close has
  happened and the collector then writes: the type system refuses it, not a runtime check.
- **An unacknowledged loss.** `RecoveryReport::has_corruption` is true for unaccepted damage,
  for a missing synced pack, and for a missing watermark with a discarded tail or a gap. A cycle
  over any of those returns `Error::CorruptStore` before the mark, so it copies nothing and frees
  nothing. Collecting over known data loss would replace a reported loss with a silent one.
- **A pack it must not rewrite.** A pack with damage to durable bytes is refused by
  `begin_compaction` and skipped with `SkipReason::Corrupt`, because rewriting it would copy only
  the valid records and quietly drop the damaged region.
- **A pack id it did not reserve.** Every collector pack comes from `alloc_id`, so the id is
  reserved durably before the file exists and is never handed out again, including after the
  collector discards that pack.

The one ordering the collector owns end to end is copy, then fsync, then unlink:
`finish_compaction` fsyncs the new pack and the index before `discard_pack` removes the source,
and `discard_pack` raises the watermark floor before the unlink and writes the whole-pack acceptance
after it. `docs/v1-store.md` spells that out and `crates/cowfs-store/tests/compact.rs` holds it
with a crash at every step of the discard.

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
   A root listed in step 1 can have moved by the time it is walked: a writer may have committed to
   that snapshot in between.
   The walk therefore reports the root it actually read (`snapshot_by_id`'s current root, read in the
   same read transaction as the nodes it walks), and that walked root is what is recorded in
   `walked`, in the persisted `mark.bin`, and in the `Marker`.
   Recording the listed key instead would claim a root the walk never descended, and a fork still
   on the listed root would be skipped as already covered while its blocks sat in no live set.
   When the walked root differs from the listed key, the listed key is left unwalked so the entries
   that still resolve to it are walked themselves.
   The persisted `mark.bin` carries a format version (`MAGIC_MARKS`).
   The current version is `COWMARK3`, which records one block list per walked root; `COWMARK2` and
   `COWMARK1` stored a flat block list, which cannot say whose blocks a removed snapshot had, and
   `COWMARK1` could also record a listed root key beside a different, newly committed root's blocks.
   Neither older file is trusted: it loads as empty and every root is walked in full.
   `docs/gc-root-mark-retention.md` has the format and the recording rules.
   The cache is derived data, so discarding it costs one walk and deletes no user block or snapshot.
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
5. **`W` Verify and unlink, one candidate at a time, each under its own barrier.**
   For each candidate, in turn:
   - `Barrier::take` the barrier. A `None` means the reference side would not hold still after all,
     so this pack is skipped as `RootsUnavailable` and the rest are too.
     The guard has to be alive from here through this pack's unlink: bound inside the block that
     acquired it, it dies before either, and the check below then races the discard it exists to
     gate.
   - `Meta::sync`, then re-read `durable_snapshots`. **Not** the list from step 1: a snapshot
     committed while the copies ran is in no list the earlier passes consult again, so its blocks
     would look dead and be unlinked. This re-read is the load-bearing one.
   - Re-walk every root not already in `walked`, sharing the one `Marker`, so only the paths that
     changed during the copy are descended. Union into the live set, and union `pinned_blocks`
     again.
   - If any condemned id is in the live set, leave the old pack alone and report it as skipped.
     Its records are still indexed, so every live block stays readable.
   - Otherwise unlink the old pack, fsync `packs/`, drop the index entries of the condemned ids,
     and lower the watermark `base` to the lowest pack that still exists.
   - Drop the barrier.

   One barrier per pack, not one per sweep. Measured, the barrier is taken exactly once per
   discarded pack: 63 takes for 63 candidates at 64 packs, 252 for 252 at 253. That is the bound on
   what a writer waits for.

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
This is not a free choice: holding it across the whole cycle was measured at 94 of 94 progress
callbacks with the barrier live and 47 packs rewritten under it, which is a full-store write stall
for the length of a sweep.
It is not free in the other direction either: one guard for the whole sweep, moved to step 5 alone,
cost a 684 ms worst single write at 253 packs, because the window scaled with the sweep.
`the_barrier_is_not_held_while_packs_are_copied` in `tests/regressions.rs` holds that line, and
`a_writer_that_dedups_onto_a_rewritten_pack_keeps_the_block` holds the correctness that depends on
it, which a writer blocked for the whole cycle would have hidden.
The benchmark in this document measures the write stall that step 5 costs.

### The barrier is required

Without `reference_barrier` there is no sound way to free anything, and pretending otherwise
would trade criterion 3 for reclaimed bytes.
So a collector with no barrier marks, reports, and reclaims nothing, and says so in
`GcReport::skipped`.
`cowfs-core` now provides one: the reference gate of `docs/gc-core-integration.md`.
The flusher lock of this note's first draft could not serve, because there is one flush lock per
snapshot and the NFS and FUSE writers reach the store without it.

### The wiring

`cowfs-core` implements both halves, and `docs/gc-core-integration.md` holds the design, the lock order and the tests.

- `Core::collector(Options)` builds a `Collector`: a `Gc` over the `Arc<Store>` that core's own block layer holds, and `CoreRoots` as its `ExtraRoots`.
- `CoreRoots::pinned_blocks` is `Core::pinned_blocks`, exact or `Busy`.
- `CoreRoots::reference_barrier` returns a barrier over the reference gate. `take` closes the gate: no thread can store a chunk or commit a chunk list until the returned hold is dropped, and it gives up (`None`) after two seconds rather than wait for a stuck reader.
- `GcReport::candidate_dead_bytes` is new: the record bytes of the candidate packs that no root reaches.

`tests/core_end_to_end.rs` keeps its own copy of `ExtraRoots` that offers no barrier, which is the contract check for `pinned_blocks`.
`tests/core_reclaim.rs` is the collection over the real core.

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
  unlinked, and `Gc::cancel` is polled before each pack inside the copy loop, so a client can
  cancel from its own progress handler and the store is left consistent: a half-copied new pack is
  a pack with no index entries, and open indexes it as ordinary data.
- A cancel bounds how much work a cycle **starts**, not what it **finishes**. Every pack in the
  unlink loop has already been copied and indexed, so those are unlinked even though the cancel is
  set. Leaving them would leave the source and its copy both on disk, and the next cycle redoes
  the copy to reclaim the same bytes. This was a real bug: a cancelled cycle reported
  `packs_rewritten: 1, packs_unlinked: 0` and freed nothing, and the cycle after it redid the same
  copy instead of finishing the job.
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
   The persisted set attributes blocks to the root that yielded them, so a root that leaves drops
   its blocks with it.
   A flat set cannot: it cannot say whose blocks they were, so nothing is ever pruned and the store
   never shrinks after the first cycle.
   The file format does not record the attribution, so a loaded set credits every block to every
   root it knows; that only costs a walk later, where under-crediting would free a live block.
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
6. **The sweep is quadratic in pack count, and that is measured, not guessed.**
   Two causes, both in `crates/cowfs-store`: `finish_compaction` checkpoints `index.cix` per pack,
   which is O(entries) per pack, and the candidate scan re-reads each pack's `index.cix` once per
   cycle. Measured per-pack cost grows from 102 ms at 16 packs to 156 ms at 253 packs.
   The default roll size is 256 MiB, so 1000 packs is about 256 GiB of store and this is a
   first-order cost there, not a footnote.
   Not fixed in this change: both are store-wide concerns and a fix belongs in `cowfs-store`, not
   in the collector.
   Dropping the checkpoint to one per cycle, and doing one candidate scan per cycle instead of per
   pack, is the fix. Both are localised and neither touches the durability ordering.
   **All measurements in this document were taken at load 1 on a machine with 32 to 60 cores
   available.** Nothing here is claimed at load above 30.

### The step-5 write stall, measured

`examples/writer_stall.rs` runs a writer that ingests, commits and syncs a snapshot while a collect
runs, and reports its worst single write. Release build, 64 KiB packs, garbage interleaved with
live data so every pack is a candidate, load 0.00:

| packs | candidates | barrier takes | cycle ms | per pack us | writer worst us |
|---|---|---|---|---|---|
| 32 | 31 | 31 | 3637 | 117322 | 79886 |
| 64 | 63 | 63 | 7842 | 124476 | 105999 |
| 127 | 126 | 126 | 21102 | 167476 | 371799 |
| 253 | 252 | 252 | 33480 | 132857 | 2962616 |

Per-pack cost is flat from 32 to 253 packs and the barrier takes equal the candidate count exactly,
so the window is one pack's check and discard.

**The 253-pack figure is not a window measurement.** In the same run, rounds with zero candidates
and therefore zero barrier takes still cost the writer 66 ms to 193 ms per write, because the
writer does a full `Meta::sync` per write and that dominates. One 253-pack sweep reported 2.96 s
against a 66 to 193 ms no-barrier baseline in the same run: scheduling noise on a worst-of-run
sample. Re-measure with core's real barrier before quoting a number.
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
- `tests/core_reclaim.rs`: 12 tests over the real `cowfs-core`: a reclaiming cycle with survivors read after a reopen, a revived block, a queued dedup, an open orphan, no barrier or no answer, a dry run, a cancel before and during a cycle, a collector dropped before and alive at `Core::close`, writers and forks beside collections, and the barrier window with its negative control.
- `tests/kill9.rs`: 1 test, ignored. A child writes, syncs and collects in a loop while the
  parent SIGKILLs it at a random moment. Run:
  `COWFS_KILL_ROUNDS=120 cargo test -p cowfs-gc --release --test kill9 -- --ignored --nocapture --test-threads 1`.
  The measured run: 120 rounds, 309 live block reads verified, no corruption and `fsck` clean every
  time.

The benchmarks are an example, not a test: `crates/cowfs-gc/examples/gc_bench.rs`.
