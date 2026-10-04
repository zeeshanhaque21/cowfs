# Root-specific mark retention

Issue 82.
The marks cache (`<root>/gc/mark.bin`) exists so a cycle can skip the walk of a root an earlier cycle already walked, while still knowing the blocks that walk found.
It is derived data: discarding it costs a walk and nothing else.
This note is about what the file records and why the format changed twice.

## The bug

`COWMARK2` stored one flat block list under the whole root list:

```
COWMARK2 | u64 n_roots | root[32] * n_roots | block[32] * n_blocks
```

The loader could not tell whose blocks were whose, so it credited every recorded block to every recorded root.
That errs safe inside one cycle, but it makes the file useless across cycles.
A removed snapshot's blocks stay in the list, and the surviving roots' records keep naming them, so no later cycle finds them dead and no cycle can reclaim them.

A per-request collector reaches this immediately, because a fresh collector reloads the file and trusts it:

```
GcReport { marked: 0, marked_skipped_roots: 1, live_blocks: 34, store_blocks: 34,
           candidates: 0, packs_unlinked: 0, freed_bytes: 0, ... }
```

Every block in the store was "live" because the one surviving recorded root's record named all 34, including the removed snapshot's.
That is safe over-retention, not the separate mark-cache upgrade data-loss blocker.

## The format

`COWMARK3` stores one group per root:

```
COWMARK3 | u64 n_roots | ( root[32] | u64 n_blocks | block[32] * n_blocks ) * n_roots
```

A block two roots share is written under both, so dropping one root keeps it live for the other.
The file is still written whole through `mark.tmp`, fsynced and renamed, so a crash leaves the previous file or the new one and never a half-written mix.

`COWMARK1` and `COWMARK2` are both rejected.
Neither carries a per-root association, so neither can be reconstructed into one, and a loader that guessed would be guessing at which blocks belong to which root.
A rejected file loads as an empty set: every root is walked in full, which is always correct.

## Two rules that make a recorded set complete

A root is only recorded from a walk that started with an empty marker.

`cowfs-meta`'s `Marker` is keyed by node, so a walk that shares a marker with an earlier root descends only into nodes that walk did not reach and yields a delta.
That delta is not the root's complete reachable set.
Recording it means a later cycle trusts the root, skips its walk, and frees whatever the shared subtree carried, which is a data-loss bug rather than over-retention.

A root that is reached only as a delta is not recorded at all, and is walked again next cycle.
The cost is bounded: a root that is trusted is skipped rather than walked, so it contributes nothing to the marker, so the next untrusted root in the listing still gets a complete walk.
That gives a fixed point after at most N cycles over N roots, and after it no cycle walks anything.
It is an argument from the walk order, not a guarantee, and it is not measured beyond the three-root case in the evidence below.

Sharing is real but narrow.
A fork that adds a file shares its *blocks* with its parent and no *nodes*, because the leaf holding a file's chunk list is rebuilt when another entry joins it, so the marker gives no reuse and the walk is complete anyway.
Two roots share a *node* when a directory with identical content appears in both, which is what `a_root_whose_walk_shared_a_subtree_is_not_recorded_and_not_trusted` builds with `mkdir`.

## What makes a record unusable

`Marks::has_root` is false and the walk runs again when:

- the file holds no format this build understands, or is short, corrupt, torn, or has a count that runs past its end
- the file has trailing bytes the root count does not account for
- the same root appears twice
- a recorded root has no blocks, which no writer emits
- more pairs or more roots than `max_persisted_blocks` allows
- the roots this cycle's listing does not name, which is a mismatch between the file and the freeze rather than a property of the file

Nothing is committed to the loaded set until the whole file parses.
A partial read trimmed into a smaller valid-looking set would leave a root credited with only some of its blocks, and trusting that root would free the rest.

A root with no recorded blocks is never trusted, in memory or from a file.
Trusting one would skip its walk and seed live from nothing.

## Bound

`max_persisted_blocks` bounds recorded (root, block) pairs, which is what the file size is made of.
The default of 4 Mi pairs is a 128 MiB file.
The bound is on pairs rather than distinct blocks because a shared block is written once per root that references it, and it is the file size that has to stay bounded.
Past the bound the set is dropped and the next cycle walks in full, which is the same safe direction as a rejected file.

## Evidence

Private temporary store, one `cowfs-core`, `gc_opts()` from `crates/cowfs-gc/tests/core_reclaim.rs`, no shared daemon.
Reproduce with:

```
cargo test -p cowfs-gc --test core_reclaim
cargo test -p cowfs-gc --lib state
```

- `a_removed_base_is_reclaimed_by_the_next_fresh_collector` fails at `7566a76` and passes with the change.
  Two snapshots share no content, cycles run until the marks trust both roots, the base is removed, and the next fresh collector must unlink a real eligible pack.
  It also asserts the pack count and byte total fall, that every surviving file reads back byte-identical after `Core::close` and a reopen, and that `fsck` is clean, so a cycle that reports a reclaim it did not perform, or one that got the reclaim by freeing live data, fails.
- `a_fork_keeps_the_subtree_it_inherited_from_a_deleted_parent` runs one cycle, removes the parent, and requires the next cycle to free nothing at all, then converges and requires a real reclaim of the garbage snapshot.
- `a_root_whose_walk_shared_a_subtree_is_not_recorded_and_not_trusted` builds two roots sharing a directory node, reads the file to find which root was recorded, removes that one, and requires the survivor's shared file to still read and the cycle to free nothing.
  That is the discriminating case: with the recording rule loosened to record every walk, the survivor is trusted on a delta that is missing exactly the shared subtree, and the test fails.
  Which assertion reports it is not fixed.
  Across 21 runs of the mutant it failed on the recorded-count claim 20 times and on the later data-loss claim once, because the removed root's identity depends on the walk order the durable table happens to return, so the precondition does not always fail first.
  The safety property is what the test holds either way: it never reaches a state where a shared file is freed and the run passes.
- `a_torn_marks_file_is_walked_in_full_and_the_cycle_still_reclaims` keeps one whole group and cuts the next short, then requires `marked > 0`, which is the observable for the file not being trusted.
  On its own this is the weak half of the torn-file coverage, because it exercises one truncation shape.
  The real guard across all the malformed shapes is the loader unit test `a_malformed_file_is_never_a_smaller_valid_set`, which states each shape as bytes and checks that none yields a partial set.

## Not proved here

- No crash-injection run against a `mark.tmp` left behind by a kill mid-write.
  The rename makes a torn file impossible to observe, and the loader rejects one anyway, but no test kills a writer between the fsync and the rename.
- The convergence bound is an argument from the walk order, not a guarantee.
  The only measurement is a three-root store reaching its fixed point after four collector requests, with the reclaim landing on the later request against zero on the baseline.
- No multi-process daemon test.
  The mark file is per state directory and each cycle holds the collector's own mutex, so two daemons on one state directory is out of scope here as it was before.

## Independent review

Reviewed at `b097a3b` by an agent with no stake in the change, on its own worktree.
Its report is not on this branch yet, so it is cited here rather than linked, and it is the reviewer's and was left as found.
It ran 41 real steps over a live daemon after a three-root warm-up, which is the per-request collector this issue is about, and recorded:

- net reclaimed 15,477,073 bytes, gross removed 16,772,668, rewrite 1,295,595, against a baseline of zero reclaimed
- survivor files re-read byte-identical, with hashes recorded at those points, and a clean `fsck` on a fresh reopen

Those figures are the reviewer's, scoped to the private store it built, and they describe the head it reviewed rather than any later one.
