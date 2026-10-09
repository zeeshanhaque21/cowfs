# Review: PR 92 issue 115 at 38111b49, against c3bafb7b

Reviewer: native critic 14, worktree `.treehouse/cowfs-7c1bf8/14/cowfs`, branch `review/linux-namespaces-17`, lease `0f3d1e5092d0bbec0544cd33ba163168`.

Subject: `38111b4929bff46189f26e85f76e3e3c5229c44f` (`docs(verification): issue 115, reproduced deterministically and repaired`), code commit `8602040` (`fix(daemon): a create cannot destroy a base another thread just published`).
PR 92's head is exactly this SHA, state OPEN, `closingIssuesReferences` empty.

This is the canonical copy, in the main repository's primary checkout, with a byte-identical mirror in the leased worktree.

**No checkout, no reset, no merge.** My worktree HEAD is still `c3bafb7b86358e45aa744f53082ff32e6fe26008` and it has no tracked edits.
The head was fetched into `FETCH_HEAD` and everything below comes from `git show FETCH_HEAD:…` and a `git archive` of that commit into my own scratch.
Runtime artifacts are only under the lease at `bench/out/provenance115-critic/`, which is gitignored.

Preserved, hashes checked on arrival:
`linux-namespaces17-create-repair-final.md` `d740a0e7ff2b61777db388f458d1d6c800151e6d164d85964965855c1007b1fa`, `linux-namespaces17-metadata-repair-final.md` `043f1171385db9bfdbe7fccad4e7c236e6cf74136ee8c17d8dcede4d47a168c8`, `linux-namespaces17-publication-final.md` `2aeeca9ff657db950853e57c7e608716c0d96fdd55b49b0b36a4db9ddb12de8b`, `linux-namespaces17-refresh-repair.md` `ce3b2e1178ae57c143447cb2ff0eed50b638bf54760aa1b682cc5f8d177eac7b`, `linux-namespaces17-integration-final.md` `2a20e526978e74a905e9d6ddffddb80b5bfdc153b5923c6be01cd274b754d9e3`, `linux-namespaces17-helper-final.md` `513d9c3f3d7d0db6f1ed2fa35b9450fddbc41fdae29792d827aa4ff032a5b430`.

## Verdicts

**The issue-115 atomicity repair: PASS, verified deterministically.**

The interleaving is reproduced on demand rather than waited for, the fix closes it, and the tests that prove it survive a mutant that removes the gate from `create` alone.
Every figure below was measured on this machine, in process, with no daemon, no mount, no SSH and no FUSE, on the same toolchain the author used.

**Lock order: PASS, no reverse ordering exists.**

Record mutex before the core's own lock, on every route, with the reverse case absent rather than merely unlocked.
The one nested case I could construct does not exist.

**API compatibility after deleting the public record methods: PASS.**

`BaseMetaStore::promote`, `::remove` and `::rename` are gone and have zero remaining call sites anywhere in the workspace.
`import.rs`, `handler.rs` and `exports.rs` are byte-identical to `c3bafb7b`, so no other lane's call site changed.

**One finding, low, documentation: the stated reason for leaving `swap` unwrapped is wrong, and a second sentence about it is wrong too.**

The conclusion is safe, the reasoning is not, and both errors are in a document that is otherwise careful.

**One precision point about the evidence, not a defect: the "old create order" column is one step older than my source review point.**

At the true `c3bafb7b` order the failure count is 4, not 5.
The 5th failure requires additionally discarding the deletion error, which is an older shape.
The prose does say the error was discarded, so this is disclosed; the table label is what invites the wrong reading.

**ETXTBSY: still a separate merge BLOCK, carried.**

Not re-run, not retried, not ignored, not serialised, no new spike, no kernel cause claimed.
No stress loop, no 40-iteration probe, and no second hold on a shared lane this round.

**My own worst error this round, stated before the evidence: I briefly believed the head failed its own test suite, and that was my bug, not the product's.**

I shared one `CARGO_TARGET_DIR` across four source trees, and `cp -a` preserves mtimes, so cargo's fingerprint matched and the runs I made against the pristine tree were served a binary compiled from a **mutant** source.
The reading "the head fails 4 of its own tests" was void and I retract it.
Every number below comes from a re-run with one target directory per tree, which makes that class of mistake impossible.
The void evidence is kept under its own name.

## Binding

`git archive` of `FETCH_HEAD` into `bench/out/provenance115-critic/new/`, five blobs matched the git objects exactly:

| File | sha256 |
|---|---|
| `crates/cowfs-daemon/src/backend.rs` | `53892a6ecd60528527e96ba8df9b05e1da5757ea71198be836a63b0a5064a976` |
| `crates/cowfs-daemon/src/base_meta.rs` | `4a8eac20294e6d5785a0fb34776731cfab077cdb40685e6bd80255813c1b4b92` |
| `crates/cowfs-daemon/src/import.rs` | `914b7732407bdaca3931593ab27e62fd6372e941591d20c5da50a7b49078eec5` |
| `crates/cowfs-daemon/src/handler.rs` | `935946477abcd9a0…` |
| `crates/cowfs-daemon/src/exports.rs` | `8841fc4af7de4471…` |
| `docs/verification/evidence/base-provenance115-interleaving.md` | `d0279bb046ce18045712a5ee35a2dff1913bac0fe099858ef77f0b5adbe59e07` |

The two documents the brief named hash exactly as specified, and both are byte-identical to their blobs at this head:
`base-provenance115-interleaving.md` `d0279bb046ce1804…`, `base-provenance98.md` `067d4fdea5488c35ae55c24c640edf8012a0a705a33ddd554c2fea2231cebe54`.

Machine: this Mac, `rustc 1.99.0 (b940084d7 2026-09-28)`, `clippy 0.1.99`, which is the author's toolchain, so the counts are directly comparable to the macOS figures rather than a cross-platform guess.
In-process only, as the brief preferred, so nothing was started, mounted, signalled or locked on the Linux host this round.

## Part 1: the repair

One critical section on the record store's own mutex, through one new entry point:

```rust
pub(crate) fn exclusive<T>(&self, f: impl FnOnce(&mut BTreeMap<String, Record>) -> T) -> T {
    let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
    f(&mut records)
}
```

`create`, `promote`, `rename` and `remove` on **both** backends each take it, and each one contains the namespace decision together with the record change, in that order and inside the same closure.

The pre-check, the record clear and the namespace call are now one step, so a name another thread takes in between cannot have the record it just published deleted by a create that is then refused.

`BaseMetaStore::promote`, `::remove` and `::rename` were deleted rather than kept as wrappers, and that is the right call: each was a way to take the record lock for the record change separately from the namespace decision, which is the shape of this defect.
The name validation that lived on the removed `remove` moved into `remove_locked`, so traversal refusal is unchanged, and `no_symlinked_dir` still runs before `remove_dir_all`.
I verified the check moved rather than disappeared: `check_private_name(name)?` is now the first statement of `remove_locked`.

### Lock order, and the deadlock question

Every namespace mutation takes the record mutex first and the core's own lock second, on both backends and in all four methods.
I checked for the reverse with a brace-matching pass over every braced `self.with(|c| { … })` body in the file: **no record access appears inside any of them.**
My first attempt at this check produced three false positives because several `self.with(|c| Ok(…)?)` closures have no braces and my matcher ran past them into the enclosing block; the corrected pass finds nothing.

`set_base_meta` is deliberately **not** wrapped, and that is safe for a reason worth naming: it re-validates before it writes.
It calls `self.info(name)` and returns `missing(name)` if the snapshot is gone, then `self.bases.set`. So if a concurrent `remove` takes the tree between a `promote` and the `set_base_meta` that follows it in `base_refresh`, the write is refused rather than attaching a record to a snapshot that no longer exists.
It is two critical sections rather than one, and the re-validation is what closes the gap.

Mutex poisoning is handled with `unwrap_or_else(PoisonError::into_inner)` on every acquisition, including inside `exclusive`, so a panicking holder does not wedge the daemon permanently.
Disk I/O, including `write_locked`'s fsync and directory fsync, happens inside the critical section, which is a latency cost rather than a correctness one, and the path backend's `copy_tree` does the same inside `create`.
Rename rollback is inside the section too, so the rollback cannot race another operation on either name.

### The cross-process claim is substantiated, not assumed

The evidence document says two daemons cannot serve one store and therefore makes no cross-process claim.
That is backed by an actual lock: `cowfs-store/src/store.rs` flocks the store and records the owner's pid, with a comment that the flock rather than the text is the authority.
So "no cross-process claim" is a statement about a real exclusion mechanism.
What is true, and disclosed, is that two `BaseMetaStore` **instances** over one path in one process would hold independent mutexes.
The daemon constructs one per store, so within the product the single-instance case is the whole of it.
I am not extending this to a multi-host or multi-daemon threat model, which is out of scope.

### API compatibility

Zero call sites remain for the three deleted methods, anywhere in the workspace.
`import.rs`, `handler.rs` and `exports.rs` are byte-identical to `c3bafb7b`, so no other lane's code needed touching.
Issue 40's metadata work is in `cowfs-meta`, a different crate that this delta does not touch at all, and I did not assume any relationship between the two.

## Part 2: the deterministic proof, and the mutant that matters most

The hook is `#[cfg(test)]` only, so there is no production hook and no production path reaches it.
It is installed by the thread it parks and filters on `std::thread::current().id()`, so it can only ever park its own thread.
It removes itself through a guard when that thread's closure returns, so a panicking test cannot leave it armed to park an unrelated test.
Synchronisation is two channels and one bounded wait per arm, five seconds for the release and two for the second thread, with no sleeps, no iterations and no shared daemon, and the parked thread panics rather than hanging if it is not released.

Measured on this machine, one target directory per tree, source identity re-verified immediately before each run:

| Tree | four interleaving arms | stale-record arm | arms reporting `ran while parked=true` |
|---|---|---|---|
| **head `38111b49`** | **4 passed, 0 failed**, exit 0 | 1 passed, exit 0 | **0** |
| mutant A, true `c3bafb7b` create order | 4 failed, exit 101 | **1 passed**, exit 0 | 4 |
| mutant B, deletion error discarded | 4 failed, exit 101 | **1 failed**, exit 101 | 4 |
| mutant C, gate dropped from `create` only | **4 failed**, exit 101 | not run | 4 |

The head's four arms each report `accepted=true, ran while parked=false`, which is the fix observable from the inside: the second thread could not get in at all while the first was parked, and the two-second bounded wait is the entire visible cost.

**Mutant C is the result the brief asked for specifically.**
Dropping the gate from `create` alone, while `promote`, `rename` and `remove` keep it, still fails **all four** arms, including both `a_concurrent_rename_onto_a_name_keeps_the_base_on_{core,path}`.
So the rename arms are not merely detecting that `create` is serialised; they detect the rename-versus-create interleaving that a lock inside `create` alone would miss, which is the whole argument for putting the section around all four methods.

### The false-oracle risk is handled, and I checked that it was noticed

The brief warned about comparing core trees by Merkle root, since two empty core snapshots have different roots because each has its own inode numbers, which would make a correct implementation fail.
The code compares entry names read through the backend's own `Vfs` for core, and the directory listing for path, against a control snapshot created the same way with no parent.
The evidence document records that the first version compared roots and failed on a correct implementation, and that the correction was the author's.
That is the right way to record it.

### No false PASS from a hook that never ran

Structurally impossible, and I checked the mechanism rather than taking the claim.
If the hook never fired, `reached_rx.recv_timeout(BOUND).expect("the first create reached the point after its name check")` would panic, so the test fails rather than passes.
If the harness never released the parked thread, the hook panics with "the parked create was not released".
So a broken hook is a failure in both directions.
My mutants corroborate it from the other side: all three mutants reach the hook and record `parked=true`, so the hook is demonstrably armed and firing.

## Part 3: the "old order" is one step older than my review point

My source review point is `c3bafb7b`, where `create` propagated the deletion error with `?`.
Mutant A reproduces exactly that order and yields **4** failures: the four interleaving arms, with the stale-record arm still passing.
Mutant B additionally discards the deletion error, which is the `f1529cc`-era shape, and yields **5** failures.

The evidence document's prose describes B accurately, including "with the deletion error discarded" and the note that the fifth failure is a bonus discriminator because the old order reported success instead of refusing.
So nothing is misstated in the prose.
What I would correct is the column heading "old create order", because that is naturally read as the immediately previous head, and at `c3bafb7b` the count is 4.
The interleaving defect itself is genuinely present at `c3bafb7b`, which mutant A proves, so the repair addresses a real defect in the head I reviewed and not an invented one.

## Part 4: `swap` is left out, and the reason given is wrong

The document says `swap` "is deliberately not wrapped, because it reaches `create` internally and the mutex is not re-entrant; it never reads or writes a record for the name it swaps."

Neither clause survives contact with the code.

Neither `swap` reaches `Snapshots::create`.
Core's `swap` calls `c.promote_base(from, name)`, the core's own method, inside `self.with(|c| …)`, and then calls `self.info(name)` **after** that block closes.
Path's `swap` does its staging with `force_remove_dir_all`, `copy_tree` and two `std::fs::rename` calls, then `self.info(…)`.
A scan of both bodies finds zero occurrences of `self.create(` or `bases.exclusive`.

And `swap` **does** read a record: `self.info(name)` consults the store for `name`, which is a `bases.get`.

So the stated deadlock reason is not the reason, and the "never reads a record" clause is false.
The conclusion is still safe, and it is safe for a different reason than the one given: `swap` acquires the core lock and the record lock in sequence and never nests them, so wrapping it would not deadlock either.
Wrapping it would additionally serialise it against the four methods, which is a coherence improvement rather than a hazard.

What is genuinely left open by not wrapping it: `swap` mutates the same namespace unserialised, so its tree renames can interleave with a concurrent `create`, `remove` or `rename` on that name.
I traced the record consequences and did not find a way to lose one: every record mutation is inside `exclusive` and paired with its own decision, and a concurrent `create(name)` against a name that exists is refused at its own pre-check, so it never reaches the clear.
The exposure is therefore tree-level, not provenance-level, and it is pre-existing rather than introduced here.
One thing this delta makes visible rather than causes: `swap` replaces a snapshot's tree without touching its record, so a base that is swapped keeps a record describing a commit the new tree was not built from.
That is a coherence question about `swap` itself, outside issue 115, and I am flagging it rather than resolving it.

Severity: low, and documentation-only in effect.
Two inaccurate sentences in an otherwise scrupulous document are worth fixing because that document is what a later reader will trust.

## Part 5: preserved behaviour, and one of my own checks that was vacuous

Ten named regressions, run individually on the head:

| Test | Result |
|---|---|
| `a_refused_duplicate_create_keeps_a_core_snapshot_and_its_whole_record` | pass |
| `a_refused_duplicate_create_keeps_a_path_snapshot_and_its_whole_record` | pass |
| `a_recreated_snapshot_is_never_a_base_even_when_a_record_outlived_it` | pass |
| `a_create_that_cannot_clear_a_stale_record_creates_no_snapshot` | pass |
| `a_removal_whose_record_cannot_be_deleted_is_reported_and_loses_no_commit` | pass |
| `a_rename_whose_record_cannot_be_moved_reports_failure_and_changes_nothing` | pass |
| `a_reused_name_does_not_inherit_a_lost_commit` | **0 passed, 0 failed: no such test** |
| `a_rename_that_cannot_write_the_destination_keeps_the_source_byte_identical` | pass |
| `a_rename_never_overwrites_a_different_destination_record` | pass |
| `a_removal_that_lost_the_record_does_not_keep_claiming_it` | pass |

I guessed that tenth name from memory and it does not exist, so that row is a vacuous pass and I am not claiming it as evidence.
The property the brief asked about, an orphan record being cleared by the next create rather than adopted, is covered by `a_recreated_snapshot_is_never_a_base_even_when_a_record_outlived_it`, which passes on both backends and is the real evidence.

The `base_meta` module whole, three iterations: 15 passed each time, exit 0.
Both removal reconciliation branches, the destination-collision refusal, the byte-identical source on an unwritable destination, and the removal-not-crash-durable wording are all still present and passing.

## Part 6: counts, toolchain, CI

Mac, `rustc 1.99.0`, `clippy 0.1.99`. Each exit captured on its own line with no pipeline in a status position.

| Suite | Result | Exit |
|---|---|---|
| `cargo test -p cowfs-daemon --lib`, default threads | `running 89 tests`, 89 passed, 0 failed, 1 ignored | 0 |
| `cargo test -p cowfs-daemon`, all targets | 89 lib, 0 main, 5 ignored in `end_to_end`, 0 doc | 0 |
| `cargo test -p cowfs-daemon --lib base_meta::`, 3 iterations | 15 passed each | 0 |
| `cargo test -p cowfs-treehouse --test canonical`, macOS | `running 10 tests`, 10 passed | 0 |
| `python3 -m unittest discover -s bench -p test_namespaces.py`, macOS | `Ran 17 tests`, `OK (skipped=10)` | 0 |
| `cargo fmt --all --check` | clean | 0 |
| `cargo clippy -p cowfs-daemon --all-targets -- -D warnings` | clean | 0 |
| `cargo clippy -p cowfs-treehouse --all-targets -- -D warnings` | clean | 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean | 0 |

By module, from the executed names: `backend` 25, `base_meta` 15, `import` 14, `handler` 14, `exports` 13, `mounts` 3, `holders` 3, `daemon` 2, totalling 89.
The author reports the same 89 with `backend` 24 "plus one store probe"; the total agrees and the attribution differs by where that one test is counted.
Neither figure is adjusted to the other.
These are macOS figures; the Linux count is different and I did not measure it this round, because the brief scoped this to in-process work and the Linux host was not needed.

The clippy version discrepancy remains **unresolved and version-dependent, and is not waived**.
On 1.99 here the workspace gate is green; on 1.95 in earlier rounds it was red with `collapsible_match` at `crates/cowfs-meta/src/tx.rs:314`.
The shape is still present in the source at `tx.rs:313-316`, and `cowfs-meta` is in **zero** files of this delta, so no one in this branch touched it.
It belongs to the core-metadata lane and I did not edit it.

CI, one read of the exact head, no polling, no dispatch, no rerun:

```
38111b49 check-runs: linux-fuse success, check (macos-latest) success, check (ubuntu-latest) success
total: 3
```

The brief noted these were pending for the author; by the time I read them once they were complete and green.
Green proves nothing about ETXTBSY, and I infer no private-Linux isolation from it: this round's Linux-adjacent evidence is source inspection only.

PR 92's head is `38111b49`, the SHA I reviewed, OPEN, `closingIssuesReferences` empty.
The body opens `Refs #17, Refs #98, Refs #115.` — the neutral `Refs` form, no `Closes`/`Fixes`/`Resolves` keyword attached to any issue.
Issue 115 is OPEN, "Core snapshot create can remove live base provenance after a concurrent promote", and issue 17 is OPEN.
Issue 98's Core-ingest scope is unchanged: `base_refresh` still refuses directory ingest on Core, so no auto-close is implied for it either.

Delta overlap with other lanes: the change touches `backend.rs`, `base_meta.rs` and two documents, and **zero** of `import.rs`, `handler.rs`, `exports.rs`, `holders.rs`, `cowfs-meta`, `cowfs-core`, `cowfs-store` or `cowfs-nfs`.
No merge, cherry-pick or rebase was performed by me.

## What is not proven at this head

- No cross-process or multi-host concurrency, and none claimed; the store's flock is what makes the single-daemon case the whole of it.
- No runtime frequency measurement for the pre-fix interleaving, and none is needed now that it is reproduced on demand rather than waited for.
- No Linux test run this round. Every executed figure here is macOS on 1.99; the Linux count for this head is unmeasured by me.
- No Path warm-base publication acceptance re-run, and no Core publication at all; `base_refresh` still refuses Core directory ingest.
- No mode (b) acceptance, and not the 15/16 gate.
- Atomicity per critical section, not transactional. A crash mid-section leaves what the filesystem already had, and the removal path's crash behaviour is unchanged and documented as not durable.
- The lock is process-wide, not per name, so two unrelated names serialise against each other.
- `swap` coherence under concurrency: I traced the record consequences and found no way to lose one, but I did not force a tree-level interleaving to observe the outcome.
- No `fsck`, no deduplication, no build-time benefit, no btrfs, XFS or other kernels.
- No browser or rendering validation of any kind. Every document claim I checked was checked structurally against the source and the blob hashes.
- I did not review anything after `38111b49`.

## Minimal defects, no fixes applied

No source or test edits in the repository, no commit, no push, no merge, no rebase, no force, no lease return.
My mutants live only in scratch copies under my owned gitignored path.

1. **The `swap` sentences in `base-provenance115-interleaving.md`.** Correct both: neither swap reaches `create`, and both read a record through `self.info`. If the reason for leaving it unwrapped is to be documented, the real reason is that it takes the core lock and the record lock in sequence without nesting, so wrapping it would serialise rather than deadlock.
2. **The "old create order" column heading**, same document. At the immediately previous head the failure count is 4; 5 requires the older discarded-error shape. The prose is right, the label invites the wrong reading.
3. **`swap` coherence, as a separate question.** It replaces a snapshot's tree without touching its record, so a swapped base keeps a record describing a commit its new tree was not built from. Out of scope for issue 115 and worth its own issue.
4. **`crates/cowfs-meta/src/tx.rs:313`**, red on 1.95 and green on 1.99, byte-identical either way, in no file of this delta. Owned by the core-metadata lane. One clippy version over one commit settles it.
5. **ETXTBSY**, not fixed here by instruction, still a merge BLOCK on its own.
6. **Keep issues 17, 98 and 115 open.** This change is a real, verified repair of a real interleaving, and it is not the mode (b) warm-base acceptance.
7. Nothing in either canonical document is contradicted by my measurements. Both refuse the claims that would have been easy to make: no transaction, no per-name locking, no cross-process scope, no per-name frequency, no `posix_spawn` in `mode_b.rs`, and the root-versus-entries correction.

## Reproduction

Mac, leased worktree, no checkout and no reset:

```sh
cd /Users/zeeshanhaque/Projects/cowfs/.treehouse-build-train/.treehouse/cowfs-7c1bf8/14/cowfs
git fetch --no-tags origin 38111b4929bff46189f26e85f76e3e3c5229c44f
git rev-parse HEAD                    # still c3bafb7b, untouched
git log --oneline c3bafb7b..FETCH_HEAD
D=bench/out/provenance115-critic; mkdir -p "$D/new"
git archive --format=tar FETCH_HEAD | tar x -C "$D/new"
cd "$D/new"
CARGO_TARGET_DIR="$D/t-new" cargo test -p cowfs-daemon --lib -- --nocapture a_concurrent
```

Expected, and what I measured: 4 passed, 0 failed, exit 0, every arm reporting `accepted=true, ran while parked=false`; and `cargo test -p cowfs-daemon --lib` giving 89 passed, 0 failed.

The mutant that matters, drop-gate-create-only: revert `Core::create` and `Path::create` to the pre-fix order, leave `promote`, `rename` and `remove` wrapped, and the two `..._rename_onto_...` arms must still fail.
Give each tree its own `CARGO_TARGET_DIR`.
If you share one across trees copied with `cp -a`, cargo will serve you a binary compiled from a different tree, and you will spend an hour reporting a defect that is not there. That is not hypothetical; I did it.

My probes and evidence, all under `bench/out/provenance115-critic/`, gitignored:

```
new/ mutantA/ mutantB/ mutantC/     four trees, each differing only where the report says
ab-take2.sh                          the trustworthy A/B: one target directory per tree
final-take2.sh                       whole library, module counts, regressions, fmt, clippy, swap facts
ab-inprocess.sh                      take one, kept because its flaw is the evidence for the retraction
why-interleave.sh                    take one's follow-up, which is what exposed the shared-target bug
evidence-ab-take2.txt                head 89/0, mutants A/B/C, and the parked=true counts
evidence-final-take2.txt             89 by module, ten named regressions, fmt 0, clippy 0 on 1.99
evidence-ab-inprocess.txt            take one: the first, genuine head pass
evidence-final-inprocess.txt         take one: the VOID runs, served a mutant binary
evidence-why-interleave.txt          take one: the isolation that found the shared-target bug
t2-*.log x-R*.log *.build.log       the per-tree and per-run logs behind all of the above
```

Cleanup: nothing of mine was ever started this round, so no process and no mount to reap; I verified both, and verified the shared `mac-heavy.lock` is free rather than held by me.
The four per-tree build caches were removed after showing their contents, each case-guarded to my owned path, reclaiming 3.7 GiB and leaving 26 MiB of sources, mutants and logs.
The lease worktree has no tracked edits and its HEAD never moved.
The Mac had 365 GiB free afterwards, and my whole owned footprint is 26 MiB against an 8 GiB allowance.

Two mistakes of my own this round, both reported above rather than buried.
The shared `CARGO_TARGET_DIR` across four source trees, which made me briefly report that the head failed four of its own tests when it passes all 89.
And a regex error in my own swap-check helper, which crashed; I redid it by inspection instead, and the inspection is what the `swap` finding above rests on.

No source or test edits in the repository, no commit, no push, no merge, no cherry-pick, no rebase, no lease return.