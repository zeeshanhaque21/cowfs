# Issue 115: a create cannot destroy a base another thread published

Companion to `docs/verification/base-provenance98.md`, for the interleaving the review of `c3bafb7b`
disclosed and issue 115 now tracks. Everything here was measured; nothing is projected.

Branch: `feat/linux-namespaces-17`. Code under test: `8602040`.
Platform: macOS, `rustc 1.99.0 (b940084d7 2026-09-28)`, `clippy 0.1.99`. In-process, no daemon, no mount,
no SSH, no FUSE. Each exit captured directly.

## The defect, and its reach

`CoreSnapshots::create` and `PathSnapshots::create` each checked that a name was free, and then, as a
separate step, cleared any record under that name before creating. A second thread could take the name
in between:

1. Thread A's name check passes, because no snapshot has the name yet.
2. Thread B creates the name and promotes it, publishing a live, complete record.
3. Thread A's record clear runs and deletes B's live record.
4. Thread A's creation is refused, and A returns an error.

The end state is a live snapshot that has silently stopped being a base, produced by a call the API
correctly refused. No tree is affected: the snapshot keeps its contents and its root. What is lost is the
provenance, and the record is gone from the store as well as the map, so it does not come back on a
reopen.

Two honest notes on scope. **The frequency was never measured**: this was a source-derived hypothesis,
and the previous review's only attempt to observe it, a 40-iteration probe, wedged on iteration 1 and
held the shared lane for 36 minutes. It is now reproduced deterministically, so the frequency question
is moot for this defect: it does not need luck to occur. **Single-user does not exclude it**: `docs/design.md`
defines single-user as mode bits and ownership, and the control surface is a Unix socket, so two agents
in one tree are two callers. What is lost is a live base's provenance, which is the annotation that says
which commit a warm base was built from.

The rename variant is worse and is why a lock inside `create` alone would not have been the fix. A rename
moves the record before the tree, so a create that clears the record in between takes a base's provenance
with nothing left for the rename's own rollback to put back: the record ends up on **neither** name.

## The deterministic proof

`backend.rs` carries a hook compiled only into the test binary (`#[cfg(test)]`), at the exact source
position between a create's name check and its record clear. It is installed by the thread it parks and
matches on `std::thread::current().id()`, so it can only park its own thread. The code under test is
therefore the production order, not a reimplementation of it.

Synchronisation is two channels and one bounded wait per arm. No sleeps, no iterations, no stress loop, no
shared daemon. Every wait is bounded: the parked thread fails the test if the harness does not release it
within five seconds, and the wait for the second thread is two seconds. A mistake fails the test instead of
wedging it, which is the failure mode the previous probe had. The hook removes itself through a guard when
its thread returns, so a panic cannot leave it to park an unrelated test.

The choreography: park a create of the name after its check, let a second thread mutate the namespace for
that same name, then let the create continue. Whether the second thread got in is *recorded* and printed;
it is not what the assertion rests on. The assertion is the end state, on both backends, live and after a
fresh reopen.

### Old fail, new pass, same tests

`cargo test -p cowfs-daemon --lib -- --nocapture`, whole library, default test threads.

| | old create order | this head |
|---|---|---|
| result | **84 passed, 5 failed**, exit 101 | **89 passed, 0 failed**, exit 0 |
| core promote arm | `accepted=false, publisher ran while parked=true`, base gone: `base: None` | `accepted=true, publisher ran while parked=false` |
| path promote arm | same loss | same |
| core rename arm | record on **neither** `warm` nor `target`: `[]` | `accepted=true, renamer ran while parked=false` |
| path rename arm | same | same |

The old run is the same tree with the two `create` bodies reverted to the pre-fix order: the name check,
then the record clear outside any critical section, with the deletion error discarded. Its failures:

```
a_concurrent_published_base_survives_another_threads_create_on_core   FAILED
a_concurrent_published_base_survives_another_threads_create_on_path   FAILED
a_concurrent_rename_onto_a_name_keeps_the_base_on_core                FAILED
a_concurrent_rename_onto_a_name_keeps_the_base_on_path                FAILED
a_create_that_cannot_clear_a_stale_record_creates_no_snapshot         FAILED
```

The fifth is a bonus discriminator: the old order discarded the deletion error, so a create that could not
clear a stale record reported success instead of refusing.

Raw logs, gitignored: `bench/out/provenance115/AB-old-create-order.log` and `AB-new-create-order.log`.

### What each arm asserts

`assert_warm_is_the_published_base`, both backends, live and reopened:

- exactly one snapshot named `warm` exists;
- it is still a base and still carries the commit the other thread published;
- its contents are those of an untouched empty tree. For the path backend that is the directory listing.
  For the core backend it is the tree's entry list read through the backend's own `Vfs`, because two empty
  core snapshots have **different** Merkle roots — each snapshot has its own inode numbers — so the root
  cannot serve as the identity there. That correction was mine: the first version of the assertion
  compared roots and failed on a correct implementation.

`assert_the_base_survives_under_one_name`, both backends, live and reopened:

- exactly one of `warm` and `target` carries the commit;
- the name carrying it is a snapshot that exists.

That is the contract in both outcomes. If the rename goes first, `target` is the base; if the create goes
first, the rename is refused and `warm` is. What must never happen is the record going nowhere, which is
exactly what the old order produced.

### Disclosure about the precondition

A and B both go through the public namespace surface: `create`, `promote`, `rename`,
`set_base_meta`, `create_meta`, `list`. `set_base_meta` is the same trait method `base_refresh` uses to
publish a warm base's provenance; there is no CLI verb that publishes a commit, so the commit itself is a
fixture. No tree is built in either arm, so the fixture is a provenance record and nothing else.

## The repair

One critical section, on the record store's own mutex, through one new entry point:

```rust
self.bases.exclusive(|records| { /* name check, record clear, namespace call */ })
```

`create`, `promote`, `rename` and `remove` on both backends each take it. Every namespace mutation of a name
goes through one of those four, so nothing can be published for a name between another call's decision
about that name and its record change.

`BaseMetaStore::promote`, `::remove` and `::rename` were **deleted**, not kept as wrappers. Each was a way
to take the record lock for the record change separately from the namespace decision, which is the shape of
this defect; leaving them would leave the trap in place. The name validation that lived on the removed
`remove` moved into `remove_locked`, so traversal and symlink refusals are unchanged, and `rename_locked`
keeps its own validation.

Lock ordering: the record mutex is taken before the core's own lock, never the reverse, and no existing
path nested them. `swap` is deliberately *not* wrapped, because it reaches `create` internally and the
mutex is not re-entrant; it never reads or writes a record for the name it swaps.

## Preserved, and still passing

- A genuine orphan record is cleared by the next create of that name and never adopted, both backends.
- A refused duplicate create changes nothing: record bytes, snapshot bytes or Merkle-rooted contents, and
  the listing, live and after a reopen.
- A create that cannot clear a stale record creates no snapshot, both backends.
- Both removal reconciliation branches, rename destination collision refusal, rename byte-identical
  source on an unwritable destination, rename rollback, unique per-call temporary publication under
  concurrency, and symlinked record directory refusal.
- Removal is still **not** crash-durable and still says so: `write_locked` fsyncs the record and its
  directory, the removal path fsyncs nothing after the unlink. No fsync was added and no durability scope
  was taken on.

## Limits, stated rather than implied

- **Atomicity here is per critical section, not transactional.** It is a mutex; a process crash mid-section
  leaves whatever the filesystem already had, and the removal path's crash behaviour is unchanged and
  documented as not durable.
- **The lock is process-wide, not per name.** Two unrelated names serialise against each other. For a
  single-user daemon issuing control operations this is not a throughput concern, and it is simpler and
  safer than a per-name lock map. It is a global critical section, not a transaction framework, and no
  such framework was introduced.
- **No cross-process claim.** Two daemons cannot serve one store, so this is the whole of the concurrency
  the daemon has.
- **No runtime frequency measurement**, and none is now needed for this defect: it is reproduced on demand
  rather than waited for.
- **Not** Core warm-base publication: `base_refresh` refuses directory ingest on the core by design.
- **Not** mode (b) acceptance, and the Path publication acceptance is not re-run by this change: it
  exercises blobs this does not touch.

## Counts at `8602040`, measured

`cargo fmt --all --check` 0. `cargo clippy -p cowfs-daemon --all-targets -- -D warnings` 0.
`cargo clippy -p cowfs-treehouse --all-targets -- -D warnings` 0.
`cargo test -p cowfs-daemon --lib`: `running 89 tests`, 89 passed, 0 failed, exit 0. By module: `backend`
24, `base_meta` 15, `import` 14, `handler` 14, `exports` 13, `mounts` 3, `holders` 3, `daemon` 2, plus one
store probe. `cargo test -p cowfs-daemon` also 0, with 0 main tests, 5 ignored in `end_to_end` and 0 doc
tests. `cargo test -p cowfs-treehouse --test canonical` 10 passed on macOS, exit 0.
`python3 -m unittest discover -s bench -p test_namespaces.py`: `Ran 17 tests`, `OK (skipped=10)`, exit 0.

89 is the macOS count. Platform-gated tests make the Linux count different, and no single figure describes
both, so neither is adjusted to match the other.

## Separate blocks, unchanged by this repair

- **ETXTBSY** in `crates/cowfs-treehouse/tests/canonical.rs`. Still a merge BLOCK on its own: 1 in 12 on the
  review's bounded sample under concurrency, 0 of 6 at one thread, root cause unconfirmed. Not retried,
  not ignored, not serialised, and no new spike: a theory without a discriminating old-fail and new-pass is
  what the five rejected hypotheses already were. There is no explicit `posix_spawn` call in
  `crates/cowfs-treehouse/src/mode_b.rs`; it builds the helper process with `std::process::Command`, which
  is a statement about the standard library's Linux behaviour, not about this source. No kernel bug is
  claimed.
- **`crates/cowfs-meta/src/tx.rs:312`**, red on Rust 1.95 with a `collapsible_match` and green on 1.99.0,
  byte-identical either way. Not this branch's file. Not edited and not waived here.

Issue 17 stays open, and this change does not reference it as closed.

## Evidence

Runtime and test logs, gitignored, inside this lease:

```
bench/out/provenance115/AB-old-create-order.log    84 passed, 5 failed, with the traces
bench/out/provenance115/AB-new-create-order.log    89 passed, 0 failed, with the traces
bench/out/provenance115/backend.rs.final           the exact source the A/B was taken against
```

No remote artifact was produced for this repair: it is an in-process proof and needs no daemon, no mount
and no shared resource lane. That is deliberate, given the previous round's 36-minute hold on one.