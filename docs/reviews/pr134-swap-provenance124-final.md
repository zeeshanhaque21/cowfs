# PR #134 final independent review: swap provenance #124

Reviewer lane: `.treehouse-build-train/.treehouse/cowfs-7c1bf8/6/cowfs`, held slot 6, branch `review/gc-root-mark-retention-82`, clean and idle at start.
PR head reviewed: `4646f513a02bb9b3256920d138a9f01551e35c34`.
Base reviewed: `93cfef94457a989d031cb6b0a475ac4edbdb85ef`.
Review date: 2026-10-05.
Artifacts: `bench/out/swap-provenance124-final-critic/**` inside the lease.
This document: canonical PRIMARY copy, `docs/reviews/pr134-swap-provenance124-final.md`.

## Verdict

BLOCK.

The happy path is correct, proven, and the nine tests do discriminate old from new.
The ordering argument the repair rests on is wrong in one specific and reachable case, and the code comment states it as an unconditional truth.

A returned `Err` from `Core::promote_base` does not mean the old tree is still there.
`CoreSnapshots::swap` treats every `Err` that way, so it restores the old record onto a name whose tree the next `Core::open` is about to replace.
The result is exactly the defect #124 was filed for: a base that reports `commit-AAA` over a tree built from `srcB`.

This is a real defect, not a missing test for a hypothetical.
It is runtime-proven below, with an existing production seam and no production edit.

## The contradiction, traced before judging

The author's two claims cannot both hold, and the author identified the seam that breaks them without noticing.

Claim A, `docs/verification/evidence/swap-provenance124-repair.md:46-47`:

> That is safe for the specific reason the core rolls forward.
> A returned `Err` from `promote_base` means the swap rolled back and the old tree is still there, which is exactly the state the restored record describes.

Claim B, same file line 39, quoting `crates/cowfs-core/src/swap.rs`:

> The only `Err` after that point is a failure of the roll-forward itself, which leaves the intent file for the next `Core::open`.

B is correct and A denies it.
`crates/cowfs-core/src/swap.rs:194-205` is explicit:

```rust
// Past this point an error cannot be reported as "nothing happened", so the swap is rolled
// forward instead and the call succeeds. The only exception is a failure of the roll
// forward itself (an I/O error), which returns `Err` with the intent file on disk: the next
// `Core::open` completes it.
```

`done` at `swap.rs:205` is the `Result` from `finish_swap`, returned verbatim.
`finish_swap` at `swap.rs:238-257` propagates `?` from `snap_by_name_raw`, `flush_snapshot`, `sc.snap.fork(target)` and `register`.
Any of those failing after the victim was unregistered yields `Err` with the old tree already gone and the intent file still on disk.

The module doc at `swap.rs:9-12` says the same in prose:

> An operation that returns `Err` leaves the mount exactly as it was, unless the old target was already removed, in which case the swap is rolled forward and the call returns `Ok`. There is no third state.

"There is no third state" is the claim the daemon now depends on, and it is false for the roll-forward failure the file itself documents four lines later.

The author's code comment repeats the false claim as an invariant, at `crates/cowfs-daemon/src/backend.rs:769-771`:

```rust
// An error out of the staged swap means it rolled back, so the old tree is still there
// and the record it was published with is put back, the same way `rename` puts a moved
// record back.
.map_err(|e| restore_base_record(self.bases.root(), records, name, was, e))
```

The "An error out of the staged swap means it rolled back" clause is the whole bug.

## Precise call path

Production, unchanged by this PR:

1. `crates/cowfs-daemon/src/backend.rs:737` `CoreSnapshots::swap`.
2. `backend.rs:755` `invalidate_base_record` writes `Record::promoted_unknown()` and keeps `Option<BaseMeta>` as `was`.
3. `backend.rs:767` `c.promote_base(from, name)`.
4. `crates/cowfs-core/src/lib.rs:359` `Core::promote_base` calls `swap_snapshot(src, None, base)`.
5. `crates/cowfs-core/src/swap.rs:186-192` the victim is unregistered. The old tree is gone from the live namespace at this point.
6. `swap.rs:201` `let done = self.finish_swap(&staged, new);`
7. `swap.rs:238-248` `finish_swap` fails inside `sc.snap.fork(target)` (or `flush_snapshot`, or `register`). `swap.rs:253` never runs, so `swap-<target>` stays on disk.
8. `swap.rs:205` returns `Err`.
9. `backend.rs:772` `restore_base_record` writes `Record::from(&was)`, which is `commit-AAA`, `/repoA`, `refs/heads/main`, and returns the original error.
10. The caller sees `Err` and believes nothing happened.
11. The next `Core::open` runs `swap::recover` at `crates/cowfs-core/src/lib.rs:310`, which calls `finish_swap` at `swap.rs:130`, which succeeds this time and forks the staged tree into `base`.
12. `base` now holds `srcB`'s tree. `.cowfs-base-meta/base/base.json` still says `commit-AAA`.

`restore_base_record` at `backend.rs:359-377` is otherwise a faithful copy of the existing `rollback_base` at `backend.rs:381-395`: same `io::Error::new(cause.kind(), ...)` composition, same "report both failures" convention, same `write_locked` primitive.
The helper is not the problem.
Its precondition is stated wrongly and is false, and `swap` supplies an error that violates it.

## Counterexample, runtime-proven

Injection uses only existing public API.
`Core::open_with_meta` (`crates/cowfs-core/src/lib.rs:198`) is the one entry point that hands the caller the `cowfs_meta::Options` it opens with.
`cowfs_meta::Options::before_sync` (`crates/cowfs-meta/src/db.rs:89`) is an existing documented hook: when it returns an error the commit does not happen (`db.rs:419-427`).
No production file is edited, no fault framework is added.

The probe is a standalone crate in the reviewer's own artifact tree, `bench/out/swap-provenance124-final-critic/probe-crate`, with its own manifest and its own `CARGO_TARGET_DIR`, so no workspace manifest and no dependency list of the PR is touched.
The PR head archive under `head/` was verified byte-identical to `4646f51` with `diff -rq` after the probe was moved out.

Result, `probe-crate/src/lib.rs`, one foreground `mac-heavy.lock` hold, exit `0`:

```text
critic: the commit that must fail is k=3
critic: promote_base returned Err: i/o error: critic: injected meta sync failure
critic: after reopen commit=Some("commit-AAA") repo=Some("/repoA") tree="BBBB-from-srcB"
critic: on-disk record = {   "repo": "/repoA",   "git_ref": "refs/heads/main",   "commit": "commit-AAA",   "promoted": true }
```

`k=3` is the third durable commit of the swap, which is the fork of the staging snapshot into `base`.
The record on disk is the real `BaseMetaStore` output, written through the daemon's real public setter, which is byte-for-byte what `restore_base_record` writes.
The tree is read through `Backend::snapshot`.

A second probe, `crates/cowfs-core/tests/critic124_probe.rs`, swept every commit index and recorded the state at each.
It confirms the window is narrow but real, and it is the one window the ordering argument cares about:

```text
critic: seed commits=3 swap commits=4 total=7
critic: k=1 returned=Err(...) intent_at_err=false live=Some("OLD-TREE") after_reopen=Some("OLD-TREE") src_intact=true
critic: k=2 returned=Err(...) intent_at_err=false live=None             after_reopen=Some("OLD-TREE") src_intact=true
critic: k=3 returned=Err(...) intent_at_err=true  live=None             after_reopen=Some("NEW-TREE") src_intact=true
critic: k=4..=10 returned=Ok                       intent_at_err=false live=Some("NEW-TREE") after_reopen=Some("NEW-TREE") src_intact=true
```

`k=3` is the only index where `Err` leaves a pending intent and a rolled-forward tree.
That is the case the comment at `backend.rs:769` rules out.

The author's own `cowfs-core` test table at `crates/cowfs-core/tests/swap.rs:7-14` already predicts this row, but only for an injected fault at step 4, which the core swallows into `last_error` and rolls forward.
The real-I/O variant of the same row, which returns `Err`, is not in that table and not in any test in the PR.

## Why the nine tests do not reach it

`crates/cowfs-daemon/tests/swap_provenance_124.rs:264` `a_swap_that_cannot_publish_the_record_fails_and_keeps_the_old_base` makes the record directory read-only.
`invalidate_base_record` is the first statement inside the critical section at `backend.rs:755`, before `self.with(...)` at `backend.rs:756`.
The fault therefore fires before any tree operation, and the old tree is trivially still present.
The test proves the record-publication refusal only.
It is a genuine real-permissions fault with an active-injection probe, and it is not a mock, but it exercises the before-mutation branch exclusively.

No test in the PR drives `promote_base` to `Err` after the point of no return.
`set_swap_fault` cannot do it either: `swap.rs:198-204` shows that faults 4 and 5 are recorded into `last_error` and swallowed, never returned.
So the seam that would expose this is `before_sync`, and the PR does not use it.

The contract the fix is supposed to hold is "record failures preserve old tree, full record, source and fresh open".
For the refusal that is tested, it holds.
For a roll-forward failure it does not, and the code comment says it cannot happen.

## Secondary finding: the path backend restores a record for a tree it just deleted

`crates/cowfs-daemon/src/backend.rs:1039-1047`:

```rust
let result = (|| {
    copy_tree(&self.dir(from), &staging)?;
    std::fs::rename(self.dir(name), &retired)?;
    std::fs::rename(&staging, self.dir(name))?;
    Ok::<(), io::Error>(())
})();
cowfs_vfs_path::force_remove_dir_all(&staging);
cowfs_vfs_path::force_remove_dir_all(&retired);
result.map_err(|e| restore_base_record(self.bases.root(), records, name, was, e))
```

If the third statement fails, the old tree is at `retired`, not at `name`.
The unconditional `force_remove_dir_all(&retired)` on line 1046 then deletes it.
`restore_base_record` on line 1047 writes `commit-AAA` for a name that has no tree at all.
`restore_base_record`'s own doc at `backend.rs:357-358` claims "a record that is left cleared names a base whose provenance nobody knows, when the tree it described is still there and was still described", which is false on this path.

Not a regression: the pre-change code had the same unconditional `force_remove_dir_all(&retired)` and left the record at `commit-AAA`, so the observable state is the same as before this PR.
Reported because the PR adds a helper whose stated precondition this path violates, and because a second rename failure is exactly as unreachable-by-test as the core case while being just as reachable by a full filesystem.

## What is correct and stays correct

The happy path is right, and the discrimination is real.

Independent old-fails-new-passes, exact `93cfef9` production blob, same test file, one foreground `mac-heavy.lock` hold per tree, isolated `CARGO_TARGET_DIR` each run, exit codes captured directly and never through a pipeline:

| Check | head `4646f51` | old code, `backend.rs` at `53892a6e…` |
| --- | --- | --- |
| `--test swap_provenance_124` | 9 passed, 0 failed, exit 0 | 4 passed, 5 failed, exit 101 |
| `--lib` | 100 passed, 0 failed, exit 0 | 100 passed, 0 failed, exit 0 |
| `--test namespace_durability` | 0 passed, 3 ignored, exit 0 | 0 passed, 3 ignored, exit 0 |
| `--test snapname_drift` | 4 passed, 0 failed, exit 0 | 4 passed, 0 failed, exit 0 |
| `cargo fmt -p cowfs-daemon -- --check` | exit 0 | exit 0 |
| `cargo clippy -p cowfs-daemon --all-targets --locked -- -D warnings` | exit 0 | exit 0 |

The three `namespace_durability` tests are reported as ignored, not as passing.
They mount a filesystem and kill a daemon, and were not run.

The old-code failures name the defect rather than a symptom, reproduced independently:

```text
assertion `left == right` failed: core, immediate response: a stale commit survived:
SnapshotInfo { name: "base", parent: None,
  base: Some(BaseMeta { repo: Some("/repoA"), git_ref: Some("refs/heads/main"),
                        commit: Some("commit-AAA") }), .. }

core: a swap that cannot publish its record must not report success
```

Also confirmed independently and unchanged by this PR:

- `base_meta.rs` is byte-identical at sha256 `4a8eac20294e6d5785a0fb34776731cfab077cdb40685e6bd80255813c1b4b92`, git blob `6c020ae303b7ac3213efe87a0c205629acac12d5` at both commits.
- The owned diff is exactly `backend.rs` at 104 insertions and 21 deletions plus the new 425-line test file.
- Lock ordering is not newly established. `bases.exclusive` outside the core slot lock is what `create` (`backend.rs:706`), `remove` (`backend.rs:731`) and `rename` (`backend.rs:785`) already do. No deadlock is claimed and none is claimed here.
- Source provenance is deliberately not adopted, `backend.rs:338-354` and the test at line 183. Option 1 is preserved.
- A never-promoted target acquires no record, `backend.rs:343-345` and the test at line 231.
- `swap` calls `self.info(name)` at `backend.rs:774`, after the critical section closes, so it does not re-enter the record mutex.

## Source binding

Verified by `git cat-file -t` on both commit ids and by `git archive` into fresh directories.
`diff -rq` between a fresh archive of `4646f51` and the `head/` tree under test reported nothing.

| Path | State | git blob at `4646f51` | sha256 under test |
| --- | --- | --- | --- |
| `crates/cowfs-daemon/src/backend.rs` | modified | `5392b6be31d15ba3c2197429bbde117a7356274a` | `5e8d640b3891731e57c0fd13f90837a5a8614d396ecff31ecfbaacb3af37116a` |
| `crates/cowfs-daemon/src/base_meta.rs` | unchanged | `6c020ae303b7ac3213efe87a0c205629acac12d5` | `4a8eac20294e6d5785a0fb34776731cfab077cdb40685e6bd80255813c1b4b92` |
| `crates/cowfs-core/src/swap.rs` | unchanged | `4cbc3f289b68e8d626321a481525ced5f5562af0` | unchanged |
| `crates/cowfs-daemon/tests/swap_provenance_124.rs` | added | present | `db58ba7658b2f4759df0805a9ca3b7a27b44b81a697516937bce4762d3e367c2` |

Documentation receipts, verified against the canonical PRIMARY checkout and against the blobs inside the PR head:

| Path | sha256 | matches the assignment |
| --- | --- | --- |
| `docs/verification/evidence/swap-provenance124-reproduction.md` | `3fde9b054fea530288e1fe3d4577bdbe97a626fad2f6f26cf92611b8d3a029a1` | yes |
| `docs/verification/evidence/swap-provenance124-repair.md` | `0dcb6a695a8b933528a348209c46c13c216d5a933a396daa7a33ff187e4f80f9` | yes |

History and closure state, one GraphQL snapshot:

- `headRefOid` `4646f513a02bb9b3256920d138a9f01551e35c34`, `baseRefOid` `93cfef94457a989d031cb6b0a475ac4edbdb85ef`, `state` OPEN, `isDraft` true.
- `closingIssuesReferences` is empty. Issue #124 stays open.
- `origin/main` is `93cfef94457a989d031cb6b0a475ac4edbdb85ef`, and `93cfef9` is an ancestor of it, so the base is current main.
- Body history is intact and in order: `b942276` code, `cbac228` reproduction, `4646f51` repair receipt. The negative-claim keywords are all present in the body, including the deliberate non-adoption of source provenance and the reported CI-pending caveat.

## CI, read once, not rerun

One snapshot of `statusCheckRollup` for the last commit of PR #134, exact head `4646f51`.
No workflow was dispatched, no runner was changed, nothing was rerun.

| Check | Status | Conclusion | Started | Completed |
| --- | --- | --- | --- | --- |
| `check (ubuntu-latest)` | COMPLETED | SUCCESS | 2026-10-05T18:52:34Z | 2026-10-05T19:02:29Z |
| `check (macos-latest)` | COMPLETED | SUCCESS | 2026-10-05T18:52:42Z | 2026-10-05T19:09:00Z |
| `linux-fuse` | COMPLETED | SUCCESS | 2026-10-05T18:52:34Z | 2026-10-05T18:57:29Z |

Three checks, all green, all on the exact head under review.
That corrects the assignment's expectation of queued or in-progress runs: at the time of this review all three had completed successfully.

## Scope discipline

Reviewed only what the PR changes and the seam it depends on.
No new issue, no new feature, no audit matrix.
No author file was modified: the probe lives in a separate crate with its own manifest, and `head/` was verified byte-identical to `4646f51` afterwards.
The lease working tree still shows only the pre-existing untracked `docs/reviews/*.md` files from other lanes.
Previous critic artifacts under `bench/out/` were not touched; `bench/out/swap-provenance124-final-critic/` did not exist before this review and was created here.
No lease was acquired, returned, reset, stashed, pruned or destroyed.
No signal, restart, sudo, install, unmount or store operation.
The shared daemon PID 15263 with start time `Sat Oct 3 20:44:29 2026`, store `/Users/zeeshanhaque/.cowfs/store`, mount `/Users/zeeshanhaque/.cowfs/mnt` and socket `/Users/zeeshanhaque/.cowfs/sock/daemon.sock` were never contacted; no mount was traversed.
No live socket round trip is claimed, no runtime frequency or core blame is claimed, and no mid-GC, power-loss or full-filesystem acceptance is claimed.

## Uncertainty, stated plainly

- `before_sync` fails a metadata commit in the store the test builds.
  The probe proves the ordering defect for that failure class, which is a real I/O failure class the core itself documents.
  It does not prove every possible post-victim-removal failure, and no claim is made that it does.
- The path-backend finding is source-level.
  No seam exists in this repository for a `std::fs::rename` failure, so it is reported as a precise call path rather than a runtime counterexample.
- `restore_base_record` failing as well is handled: it composes both messages and keeps the original error kind, matching `rollback_base`.
  That composition was read, not executed, because it needs a second simultaneous fault.
- CI green on the exact head is a snapshot at 2026-10-05T19:09Z and does not speak to any later push.

## What has to happen before this merges

One of these, and the second is the smaller change:

1. Make the record handling honest about the roll-forward failure.
   When `promote_base` returns `Err`, the daemon cannot know from the error alone whether the swap rolled back or its roll-forward failed.
   The safe floor is to leave the record invalidated (`promoted_unknown`) rather than restore a record that may end up over a tree the pending intent will replace.
   Restoring on a rollback is correct, and restoring on a roll-forward failure is the stale-provenance defect, so one of the two must give.
   The core's `Err` cannot distinguish them today, so either the core reports which happened, or the daemon stops restoring and accepts the lost provenance on a rolled-back swap.
2. Fix the comment at `backend.rs:769-771`.
   As written it states a false invariant, and it is the sentence a future reader will trust.
   `crates/cowfs-core/src/swap.rs:194-197`, three hundred lines away in another crate, says the opposite in the same repository.
3. Add a regression that drives `promote_base` to `Err` past the point of no return and asserts the record and the tree agree after the reopen.
   The `before_sync` hook used above is the existing seam and needs no production change.

The nine existing tests, the helper shape, the error composition, the lock ordering and the deliberate option-1 provenance discard are all fine as they stand.
The block is on one wrong precondition in one comment and one `map_err`, and on the claim in the receipt that no such window exists.

Nothing was fixed, merged, marked ready, or closed.
Issue #124 stays open and no new task was created; that is the coordinator's call.