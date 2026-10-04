# Garbage collection over the real core and daemon

Issue: #10.
Source contracts: `docs/design.md` ("Garbage collection"), `docs/v1-gc.md` (the collector and its step-5 protocol), `docs/v1-core.md` (locks).
This note settles one question the collector left open: what is the reference barrier of the real `cowfs-core`, who owns what, and in which order.
It does not change the collector's algorithm and it does not remove hash-on-read.

## Status: not production-ready, one critical race caught by review and corrected

An independent adversarial review of the first wiring found a data-loss race in the collector's mark phase.
It is old `cowfs-gc` logic (PR 39), but nothing over core had ever freed a pack, so this change is what made it reachable: with a real barrier, a pack could be unlinked while a live fork still needed it.
It is corrected here, with a deterministic regression, and the head is still a draft pending re-review.

The race: step 1 lists `(root, id)` pairs from the durable snapshots.
Step 2 walks each id through `snapshot_by_id(id).live_blocks(...)`, which reads the snapshot's *current* root, not the listed one.
A writer that commits to a listed snapshot between the listing and its walk moves it to a new root.
The walk then yields the new root's blocks but the old code recorded them under the *listed* root key, and a fork still on the listed root was skipped in step 2 and again in step 5 as already covered.
Blocks only the listed root referenced sat in no live set, became candidates, and their pack was unlinked.

The correction is narrow and keeps the algorithm:

- `cowfs-meta` walks the root it actually read: `Snapshot::live_blocks_with_root` holds the session read lock while it reads the root and opens the node table from the same read transaction, so the root returns and the nodes walked are one epoch.
- The collector records the root the walk returned (`walked_roots`, the persisted `mark.bin`, and the marker bookkeeping), never the listed key.
  When a listed key is not the walked root it is left unwalked, so the entries that still resolve to it are walked themselves.
- A root already walked this cycle is skipped by the walked root, and a persisted mark is honoured by the walked root.
  In-cycle reuse is safe: a mark this cycle records names the root it actually walked.
  A mark left on disk by a collector *before* this correction is not safe to reuse, because that collector could record a listed root key next to a different, newly committed root's blocks.
  The marks file therefore carries a format version (`MAGIC_MARKS`), and this correction bumped it from `COWMARK1` to `COWMARK2`.
  A file in the old format is treated as empty and every root is walked in full, which is always correct because the cache is derived data; no user block or snapshot root is deleted.

### The marks cache records which blocks belong to which root

`COWMARK2` stored one flat block list under the whole root list, so the loader credited every recorded block to every recorded root.
Inside one cycle that is the safe direction.
Across cycles it is not: a removed snapshot's blocks stay in the list and the surviving roots' records keep naming them, so a per-request collector that trusts the file finds nothing dead and reclaims nothing (issue 82).

`COWMARK3` stores one block list per walked root, and only a walk that started with an empty marker is recorded, because a walk sharing `cowfs-meta`'s node marker with an earlier root yields a delta rather than that root's complete reachable set.
A root reached only as a delta is walked again next cycle.
`COWMARK1` and `COWMARK2` are both rejected, since neither carries a per-root association and neither can be reconstructed into one.

`docs/gc-root-mark-retention.md` is the format, the two recording rules, what makes a record unusable, and the regressions over the real core.

Proof (private stores, this worktree):

- Deterministic regression `a_commit_between_the_freeze_listing_and_a_walks_the_listed_root` in `crates/cowfs-gc/tests/core_reclaim.rs`.
  A test seam on the collector (`Gc::set_between_list_and_walk`) runs one commit in the exact listing-to-walk window, so the test does not depend on timing.
  It asserts the fork's block survives a reopen *and* that the cycle still unlinked at least one real dead pack, so a "never free anything" fix cannot pass it.
- With the old key-recording behaviour restored the deterministic test fails: the fork's block is gone after the cycle (`... named by a file is missing`).
  With the correction it passes 10/10, and the review's timing-based fixture no longer loses data (10/10).

### A snapshot removed between the lookup and the walk

Step 2 looks each listed id up with `snapshot_by_id(id)`, then walks it with
`live_blocks_with_root`, in a separate call.
A removal that commits in the gap between those two calls made the walk report `NoSuchSnapshot`, and
the collector returned that as a cycle error, so a collect running while a snapshot was removed failed
outright (issue 83).

`NoSuchSnapshot` there is a snapshot that came and went, not a failure:

- The id names no root in the durable table any more, so nothing this cycle keeps is reached through
  it.
- A fork made **before** the freeze listing recorded its own root before the removal committed, so it
  is walked through its own listed id. A fork made **after** the listing is not in the step-2 list at
  all; it is covered by the step-5 re-list of durable roots under the barrier, so the vanished
  snapshot's blocks are still kept if that late fork is their only holder.
  Reviewer evidence reproduced both rows, including the case where the late fork is the only holder
  (the packs holding the victim's blocks were skipped `BecameLive` in step 5).

The collector therefore skips **that one id** and continues.
It does not record the failed root as walked, does not add a block or root to the persisted mark, and
writes no partial marker: the skip happens before any of that.
Every other error (storage, corruption, a failing hook) still stops the cycle with
`Err(e) => return Err(e.into())`, the same conversion the old `?` used, so a real failure is never
treated as a vanished snapshot.

### The lookup/walk seam runs in step 5 too, with the barrier held

`Gc::set_between_lookup_and_walk` is a test-only seam whose callback runs for **every** walk the
collector does, not only the step-2 mark pass. In step 5 the collector re-walks the durable roots
under a taken barrier, so a callback that fires there runs with the barrier held. A callback that
does a gated core write on that thread would self-deadlock at the gate. The shipped tests guard on the
snapshot id and fire once, so none of them hit it; a new test that uses this seam must do the same.
The production code never sets it.

Proof (private stores, this worktree):

- `crates/cowfs-gc/tests/core_reclaim.rs::a_snapshot_removed_between_the_lookup_and_the_walk_does_not_fail_the_cycle`
  uses the `Gc::set_between_lookup_and_walk` seam to remove the snapshot in the exact window, so the
  test needs no timing. On the pre-fix source it fails with `Meta(NoSuchSnapshot)`; after the fix the
  cycle is `Ok`, unlinks at least one real dead pack, and a fork of the removed victim reads every
  byte of every shared file after a reopen, with `fsck` clean.
- `crates/cowfs-gc/tests/regressions.rs::a_non_nosuchsnapshot_error_in_the_walk_window_fails_the_cycle_and_frees_nothing`
  arms a metadata `before_sync` fault in the same window so the walk fails with a non-`NoSuchSnapshot`
  error. The cycle must fail and free nothing. A fail-open mutation (`Err(_) => continue`) makes the
  test fail, so the propagation arm is load-bearing.

Both seams, `Gc::set_between_list_and_walk` and `Gc::set_between_lookup_and_walk`, are
`#[doc(hidden)]` and production never sets them.

## Problem

`cowfs-gc` needs two things from the reference side.
It needs `ExtraRoots::pinned_blocks`, which core already answers exactly or with `Busy`.
It needs `ExtraRoots::reference_barrier`, which core did not have, so every cycle over core reported candidates and freed nothing.
It also needs `Arc<Store>` and `Arc<Meta>`, and core exposed `&Store` and `&Meta`.

The barrier is the hard part.
Step 5 of the collector reads the durable roots and the pinned set, then unlinks a pack.
A writer that deduplicates onto a block of that pack after the read and before the unlink takes a reference to a block that is about to disappear.
Nothing the collector can read closes that window.
The reference side has to stop acquiring references from the moment the collector starts looking until the unlink is done.

## What can acquire a reference to an existing block

A reference to an existing block is acquired by exactly two operations.
Everything else (fork, rename, swap, snapshot publication, import staging, unlink, remove) only re-points at, or drops, references that a snapshot root already holds.

| Operation | Where | How it is covered |
|---|---|---|
| `Store::put` that deduplicates onto a garbage record | `Blocks::put`, reached only from `FileData::flush` and `FileData::truncate` | `Blocks::put` takes a `&Entry`, so a put without the gate does not compile. |
| `Snapshot::batch` with `set_content` (a chunk list becomes part of a snapshot root) | `Inner::commit`, reached from `flush_snapshot` and `barrier` | Both enter the gate before they drain the queue. |

The second row matters because a chunk list moves from "pinned in a node" to "in a snapshot root" at commit time.
If a commit could land between the collector's read of the roots and its read of the pins, the blocks would be in neither answer.
Holding commits still while the barrier is held removes the ordering question: the roots and the pins describe one frozen state.

The following were checked and need no gate:

- `fork`, `promote_base`, `rename`, `finish_swap`, import publication: each copies a root that is already in `Meta::snapshots`.
  The collector syncs meta under the barrier, so every root that exists is in `durable_snapshots`, and a fork made while the barrier is held has the root of its source.
  A fork flushes its source first, and that flush is gated.
- Unlink, rename over a file, and snapshot removal while a handle is open: the node keeps its chunk list in memory before the namespace change is queued (`preserve_orphan`), so the blocks move from a root to a pin and never to neither.
  `remove_snapshot` is refused while a handle is open.
- `Blocks` cache hits: a cache hit is a read of a block something already references.

## The gate

`cowfs-core` owns one `Gate` per mount (`src/gate.rs`).

- A **reader** is a thread that may acquire a reference: it holds an `Entry` while it chunks and stores a file, and while it drains the queue and commits a batch.
  Any number of readers run at once.
- The **barrier** is the collector.
  `take` moves the gate `Open -> Draining`, waits until no reader is inside, then moves it to `Held`.
  The returned `Hold` moves it back to `Open` on drop.
- While the gate is `Draining` or `Held`, a new blocking `enter` waits and `try_enter` returns `None`.
- `take` gives up after a fixed patience (2 s, the same bound `pinned_blocks` uses for a node lock), and also when another barrier is held or the calling thread is itself inside the gate.
  It returns `None`, which the collector reports as `RootsUnavailable` and treats as no barrier: that pack is not unlinked.
  So a reader that is stuck for a long time costs the collector a skipped pack, not a hang and not a wrong free.
- `Hold` owns an `Arc<Gate>` and a plain state flag under a mutex.
  It is `Send + Sync`, which is what `cowfs_gc::Held` requires, and it uses no std guard and no unsafe code.
  This is why the gate is a counter and a condvar and not a `RwLock`: a `RwLockWriteGuard` is not `Send`.

`Entry` is not `Send`.
A thread that already holds an `Entry` on a gate and enters it again is admitted without waiting, so a helper that enters defensively cannot deadlock against a pending barrier.

## Lock order

The gate is the **outermost** lock: it is taken before `SnapCtx::ns`, `SnapCtx::flush`, any node lock and the meta lock.

1. A blocking `enter` is only called with no node lock, no `ns` lock and no `flush` lock held.
   `flush_snapshot` and `barrier` enter before they take `sc.flush`.
   `op_setattr` enters before it takes the node write lock when it will truncate.
2. A caller that holds a node write lock and might need to store a chunk uses `try_enter` and does not block.
   `op_write` does this for its threshold flush: if the gate is not open it leaves the bytes dirty, and the background flusher or the next write stores them.
   Deferring is harmless because those bytes are not a reference to anything yet.
   `relieve` does the same.
3. Nothing waits on the gate while another thread waits on a node lock the waiter holds.
   `pinned_blocks`, which the collector calls under the barrier, takes node read locks.
   Because no thread blocks on the gate while holding a node lock, `pinned_blocks` is never stalled by a thread parked at the gate.
4. The collector, while it holds the barrier, calls `Meta::sync`, `durable_snapshots`, the snapshot walk, `pinned_blocks` and `Store::discard_pack`.
   None of them enters the gate.
   The meta sync hook (`Store::sync`) runs with no reader inside, so it cannot wait on a reader.

If an audit is ever wrong and a cycle forms, the cost is bounded: `take` gives up after its patience, clears `Draining`, and every parked reader proceeds.
That is a skipped pack and a short stall, never a hang.

## Ownership

```text
Core ---- Arc<Guard> ---- Inner ---- Blocks ---- Arc<Store>
  |                          \------ Meta (cheap clone, shared database)
  \-- Collector = { Gc (Arc<Store>, Arc<Meta>), CoreRoots { Core clone } }
```

- `Core::collector(opts)` builds a `Collector` over core's own store and metadata, with its state under `<root>/gc`.
  The `Gc` holds the `Arc<Store>` that core's `Blocks` holds, so no second open of the store is attempted.
- `CoreRoots` holds a `Core` clone.
  `Core::close` refuses (`Error::Stale`) while any clone is alive, before it flushes or closes anything, so a live collector cannot be left with a closed meta.
- Dropping the `Collector` drops the `Gc` (its `Arc`s) and the `Core` clone.
  After that, `Core::close` succeeds and takes the store.
- A cycle runs on the thread that calls `collect`.
  `Gc::cancel` is the only cross-thread control.

## Daemon

`Backend` gains `collect_garbage(dry_run, progress, cancelled) -> CtlResult<Option<GcOutcome>>`.
The default is `None`, which the handler reports as the existing `unsupported` error, so a passthrough or in-memory backend keeps its answer.
`CoreBackend` registers the run, then clones the `Core` out of its slot (it must not hold the slot lock for the length of a sweep) and builds a `Collector` with `Core::collector`.
It runs the cycle on a scoped thread.
The calling thread forwards the collector's progress to the client and polls the client's cancel every 50 ms, so a cancel does not wait for the next event.
One collection runs at a time: a second request is `busy`.
`CoreBackend::close` marks the backend closing, cancels a running collection and waits for it to drop its `Core` clone before it takes the core, so shutdown never races a sweep and `Core::close` never sees `Stale` from a collector.
The registration happens before the `Core` clone is taken, so a `close` that does not see a run cannot take the core out from under one that is about to start.

The protocol is unchanged (`GcParams`, `GcReport`, progress events, `cancelled`).
The fields of `GcReport` map as follows.

| Protocol field | Meaning here |
|---|---|
| `candidate_blocks` | blocks the store holds that no snapshot root and no pin reaches (`store_blocks - live_blocks`) |
| `candidate_bytes` | record bytes in the candidate packs that no root reaches (`GcReport::candidate_dead_bytes`): the most a cycle can free from them |
| `freed_blocks` | the drop in `Store::stats().blocks` across the cycle |
| `freed_bytes` | **gross** bytes on disk freed by unlinked packs, under the legacy name |
| `gross_removed_bytes` | the same gross figure under its explicit name, equal to `freed_bytes` |
| `rewrite_bytes` | bytes written into the packs this cycle created, file headers included: committed rewrites plus any abandoned partial copy |
| `net_reclaimed_bytes` | `gross_removed_bytes - rewrite_bytes`, signed, so a cycle that cost more than it removed reads negative instead of a saturated zero |

Candidate figures are an upper bound: a pack below the dead-ratio threshold is not rewritten, so its dead blocks are counted in `candidate_blocks` and not in `candidate_bytes`.
The gross and net figures are cycle-owned: they come from the packs this cycle actually unlinked and the packs it actually wrote, not from a process-wide before/after of the store size, which a concurrent writer would corrupt.
See `docs/gc-space-accounting.md` for the full accounting rule.
A live request that freed nothing because the reference side would not hold still answers `busy`, and one that freed nothing because of a cycle error answers `io_error`, so a quiet success is never a silent failure.
That `io_error` message carries the cycle's actual gross, rewrite and signed net, because a cycle that errored after writing a new pack but before unlinking anything still spent those bytes and a bare "freed nothing" would hide the cost.
A pack whose unlink could not be made durable is counted in those figures, since the file is really gone, and the request still answers `io_error` rather than a quiet success: the removal is real but unconfirmed, so the caller is told.
A cancelled request answers `cancelled`; what the collector had already copied is finished and unlinked (the collector's rule), so the store is consistent and the next request does the rest.

## Failure behaviour

Every failure keeps blocks.

| Situation | Result |
|---|---|
| No barrier offered, `take` returns `None`, or `reference_barrier` errors | the cycle reports candidates and frees nothing |
| `pinned_blocks` is `Busy` or unavailable | the cycle frees nothing |
| a root or the snapshot walk errors under the barrier | that pack is left in place |
| dry run | no pack copied or unlinked, no `mark.bin`, no `atime.bin` written |
| cancel | work already copied is finished and unlinked (existing collector rule), nothing new starts |
| collector dropped before `Core::close` | `close` succeeds |
| collector alive at `Core::close` | `close` returns `Stale` and has changed nothing |

## Acceptance (all runnable)

```text
cargo test -p cowfs-core
cargo test -p cowfs-gc
cargo test -p cowfs-daemon
cargo test -p cowfs-gc --test core_reclaim -- --nocapture
```

`crates/cowfs-core/src/gate.rs` holds the gate's own tests: the hold is `Send + Sync`, a reader inside makes `take` give up and leaves the gate open, an entrant parks while the barrier is held and runs when it drops, a nested enter is admitted while a barrier drains, a second barrier does not wait for the first, and the fault seam takes without closing.

`crates/cowfs-gc/tests/core_reclaim.rs` is the real-core deliverable.
Its first test makes several packs through core, keeps one snapshot and removes another, runs a cycle, and checks that dead packs are gone from `store/packs`, that bytes and blocks shrank, and that every survivor reads back after `Core::close` and `Core::open`.

Adversarial cases, all in that file:

1. A committed durable snapshot arrives while the cycle runs, and it dedups onto blocks of dead packs (`a_writer_that_dedups_inside_the_barrier_window_waits_for_the_unlink`: the writer starts inside the window, after the last read of the reference side and before the unlink).
2. Writing a dead block again revives it before the cycle.
3. A dedup that is only queued, with the chunk list uncommitted, keeps the block alive, and a dirty file below the flush threshold stays readable.
4. An unlinked file with an open handle survives, and its pack is reclaimed after the handle is released and forgotten.
5. No roots, no barrier, `Busy` pins, a barrier error and a barrier whose `take` fails each keep every block and every old pack. When nothing can be freed nothing is copied either.
6. A dry run leaves the store directory, the collector state and the snapshot roots byte-identical.
7. A cancel before the cycle copies nothing and `resume` finishes the job. A cancel mid-cycle leaves unstarted candidates `NotReached`, finishes what was copied, and leaves the store fsck-clean.
8. A collector dropped before `Core::close` lets it succeed. A live collector makes `close` return `Stale` with nothing changed.
9. Three writers (with overwrites, unlinks and repeated content), a fork and remove loop and a collection loop run together for six seconds. A watchdog exits the process if no operation completes for 60 s. Every file reads back, before and after a reopen.

Negative control: `Core::set_gate_fault(1)` makes `take` return a hold that does not close the gate.
`gc_barrier_window::negative_control_a_barrier_that_does_not_close_the_gate_loses_data` (a `cowfs-core` unit test) runs the window test with it and requires that data is lost (the writer's blocks are gone after the reopen).
The same scenario with the real barrier loses nothing and shows the writer parked at the gate while the barrier is held.
So the barrier test is sensitive to the thing it tests, and removing the barrier makes a deterministic test fail.

The mark-phase regressions are `a_commit_between_the_freeze_listing_and_a_walks_the_listed_root` and `a_snapshot_removed_between_the_lookup_and_the_walk_does_not_fail_the_cycle`, described under Status above.
Their seams, `Gc::set_between_list_and_walk` and `Gc::set_between_lookup_and_walk`, are `#[doc(hidden)]` and production never sets them.
The barrier-removing core seam is different: `Core::set_gate_fault` is a fail-open switch because it makes `take` succeed without closing the gate.
It (and `gate_waiters`, and the gate's own `set_fault`/`waiting`) is now compiled only under `#[cfg(test)]`, so it is visible only to `cowfs-core`'s own unit tests.
The barrier window test therefore lives in `crates/cowfs-core/src/gc_barrier_window.rs`, not in `cowfs-gc`'s integration tests.
A Cargo feature cannot be used for this: `cowfs-core` is a normal dependency of `cowfs-daemon` and a dev-dependency of `cowfs-gc`, and Cargo unifies a package's features across a resolve, so any feature `cowfs-gc`'s dev-dependency enabled would also land in the `cowfs-core` the daemon links in the same resolve.
Keeping the seam at `#[cfg(test)]` and the test inside the owning crate removes the leak entirely.
A production build has neither the method nor the field, so the switch cannot be reached at all: `cargo build -p cowfs-core` and `cargo build -p cowfs-daemon` have no `set_gate_fault`.
The negative control still runs under `cargo test --workspace`, so no test coverage is lost.
`Core::store()` and `Core::meta()` still hand out handles a caller could use to write without the gate; the daemon uses neither, and this note records it rather than widening scope here.

`crates/cowfs-daemon/src/handler.rs` tests: a dry run over a real core changes nothing and reports candidates, a live run frees blocks and bytes, emits `mark` and `sweep` progress and leaves survivors readable and fsck-clean, a cancelled request is `cancelled` and the next one finishes the job, closing the backend after a collection releases the store lock, and the passthrough backend still answers `unsupported`.

## Non-goals

- Changing the mark, the candidate choice, the copy, or the per-pack step-5 rule of the collector.
- Removing or weakening hash-on-read.
- A background or scheduled collector, and any policy for when to run one.
- Changing the control protocol types, the NFS adapter, the FUSE adapter, or the benchmark crate.
- Making the sweep faster (the quadratic per-pack checkpoint is a `cowfs-store` concern, `docs/v1-gc.md` decision 6).
- Collecting a store opened by another process: the store lock still refuses that.
