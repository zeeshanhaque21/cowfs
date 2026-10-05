# PR #137 runtime review: the exact-head run is green on both platforms, the old red is a pass, and the merge into current main is clean

Reviewer lane: `.treehouse-build-train/.treehouse/cowfs-7c1bf8/6/cowfs`, held slot 6.
Lease verified before any write: branch `review/gc-root-mark-retention-82`, HEAD `b4b55ab`, working tree showing only the six pre-existing untracked `docs/reviews/*.md` files from other lanes, no process of mine running.

Head reviewed: `c2afa0e12c09d7a46d0a604c5ef7493b81e30adb`.
PR base as recorded by the API: `93cfef94457a989d031cb6b0a475ac4edbdb85ef`.
Current remote main, fetched into the local object store: `b486d4541bc47b273a5bbd222b95c24fed05c36d`, the merge commit of PR #136.
Prior critic report this continues: `docs/reviews/pr137-meta42-snapshot-rename-root-final.md`, sha256 `9341cc4f8e5724846bae85a131d13bf3bb661d8755570fbe4836d226f28063d2`, preserved immutable.
The blocked report it descends from: `docs/reviews/pr137-meta42-snapshot-rename-final.md`, sha256 `471a07310e793016763a1f77e4e195bfeca89dcf1613d84fe50b238a6355b0ec`, preserved immutable.

Review date: 2026-10-05.
Artifacts: `bench/out/meta42-snapshot-rename-runtime-final-critic/logs/**` inside the lease, 1.74 MiB, four log files.
This document: canonical PRIMARY copy, `docs/reviews/pr137-meta42-snapshot-rename-runtime-final.md`.

## Verdict

SCOPED PASS for issue #42 request 1, the metadata rename API.

The runtime gate I left open in `9341cc4f` is now closed, and closed by evidence rather than by assertion.
I read the actual job logs for the exact head and confirmed the bound numbers myself: `snapshot_rename` is 10 passed, 0 failed, 0 ignored on both Ubuntu and macOS, every one of the ten named tests ending `ok`, with the test that was red at `snapshot_rename.rs:372:5` now passing.
Clippy ran and checked fifteen crates including `cowfs-meta` with zero error and zero warning lines on both platforms, and `cargo fmt --all --check` produced no diff.

The old red, the fix, and the merge position all hold up under fresh checks.
My earlier source verdict is not withdrawn and is not restated: this document adds the runtime half and the integration half.

Two limits stay load-bearing.
I executed nothing; every runtime number here is the author's CI run on the exact head, read by me, which is not the same as my own execution and I do not present it as such.
And this is a PASS for one request of one issue: the API still has no consumer, `cowfs-core` still stages its own rename, and #42 is not complete.

## Budget: still blocked, and nothing was run by me

The lane's `bench/out` measured 35.881 GiB against the 8 GiB cap when I started and 35.882 GiB when I finished, the difference being the 1.74 MiB of logs in my own directory.
So there was no cargo invocation, no build, no archive, no target directory, no private probe, no heavy job, and no deletion, pruning, move or offload.
No cap was waived, and I did not treat free space, which was 282.2 GiB, as authorization.

Everything below that is a result is CI's, on the exact head, read from logs I retrieved.

## CI, one snapshot, exact head

Run `37387037235`, read through the API: `head_sha` `c2afa0e12c09d7a46d0a604c5ef7493b81e30adb`, `head_branch` `fix/meta-snapshot-rename-42`, `event` `pull_request`, `run_attempt` 1, created `2026-10-05T23:11:22Z`, completed `23:27:09Z`, conclusion `success`.
The head sha also appears literally inside both job logs, so the logs are bound to the head and not only to the run record.

| Job | Id | Started | Completed | Conclusion |
| --- | --- | --- | --- | --- |
| `linux-fuse` | `112022716010` | `23:11:24Z` | `23:15:06Z` | success |
| `check (ubuntu-latest)` | `112022716217` | `23:11:24Z` | `23:21:17Z` | success |
| `check (macos-latest)` | `112022716236` | `23:11:29Z` | `23:27:09Z` | success |

The PR's own `mergeStateStatus` moved from `UNSTABLE` to `CLEAN`, `isDraft` is true, `state` OPEN, `closingIssuesReferences` is empty.
Nothing was polled, rerun, dispatched or reconfigured, and no workflow, runner or job setting was touched.

## I read the full logs, not a tail

I retrieved each failing-then-passing job log twice, by two different routes, and read both in full.

Route one, the raw jobs API, which streams the archive with its timestamps and escape sequences intact:
`bench/out/meta42-snapshot-rename-runtime-final-critic/logs/ci-37387037235-ubuntu-112022716217.log` at 363,878 bytes and `ci-37387037235-macos-112022716236.log` at 352,424 bytes.

Route two, the coordinator's route, `gh-axi run view --job N --log`, which prints a `full_log:` path:
`37387037235-job-112022716217-log.log` at 560,980 bytes, sha256 `1fe886677cc925ea232c88cc17759cabff9af576fda4005be011a23e716f8e34`, and `37387037235-job-112022716236-log.log` at 541,001 bytes, sha256 `7c219f0df5ebd47ecd1b2014e7809342a77f75d3898d825bf588bbf5bdac810a`.
Both digests match the values I was given, byte for byte, so the copies I read are the copies that were hashed.

One assembly caveat worth recording, because it will mislead anyone who slices the wrong copy.
The `gh-axi` assembly emits a step's stdout *after* that step's `##[endgroup]` marker, so a slice bounded by the group markers shows the clippy step as nine lines with no output and looks like a no-op.
The raw API copy interleaves correctly and shows the real thing.
I read both, and the numbers below come from the raw copy for step output and from the `gh-axi` copy for per-test lines, which are present and identical in both.

## The bound numbers

`check (ubuntu-latest)`, job `112022716217`:

```
Running tests/snapshot_rename.rs (target/debug/deps/snapshot_rename-9d5ff8fd5fa17170)
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
```

`check (macos-latest)`, job `112022716236`:

```
Running tests/snapshot_rename.rs (target/debug/deps/snapshot_rename-e8fea655cdbc51c8)
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.40s
```

All ten tests, named individually, ending `ok`, on both platforms:

```
a_rename_keeps_the_id_the_root_the_numbers_and_an_open_handle ... ok
a_rename_does_not_move_the_next_id_or_the_inode_floor ... ok
renaming_to_the_current_name_is_a_no_op ... ok
a_name_held_by_another_snapshot_is_refused ... ok
a_missing_id_and_an_invalid_name_are_refused ... ok
a_rename_commits_a_pending_tree_change_and_leaves_the_row_readable ... ok
a_before_sync_failure_at_the_rename_commit_changes_nothing ... ok
a_rename_frees_the_old_name_and_keeps_the_new_one_busy ... ok
a_rename_keeps_real_file_bytes_readable_after_a_drop_and_reopen ... ok
a_rename_of_a_dirty_snapshot_keeps_a_forked_old_root_live_and_the_renamed_one_fresh ... ok
```

Neither log contains any `test result: FAILED`, any line starting `error` or `warning`, any `##[error]`, or any non-zero `Process completed with exit code`.

The three results that answer my earlier findings directly:

- `a_rename_commits_a_pending_tree_change_and_leaves_the_row_readable` is the exact test that was red on `9d1e5ef` at `snapshot_rename.rs:372:5` with `7 passed; 1 failed`. It is `ok` on both platforms at `c2afa0e`, on the byte-identical test body I verified last round. That is the pinned old red turned into a pass.
- Because that test continues past the root assertion to `again.snapshot("renamed")`, `lookup(b"pending")`, the three original files, and `again.check()`, the consequences I had to leave as predictions in `9341cc4f` are now executed: the reopened row resolves to a readable tree, the pending write is present with the inode number it was handed, and the integrity check passes. The missing-node path and the `check()` failure were never observed, and now the run gets past the assertion that used to stop before them.
- `a_rename_keeps_real_file_bytes_readable_after_a_drop_and_reopen` and `a_rename_of_a_dirty_snapshot_keeps_a_forked_old_root_live_and_the_renamed_one_fresh` are both `ok`, so the real-store byte readback and the shared-root stale-content variant are executed rather than owed.

The test binary hashes are the same strings the red run's logs named, `9d5ff8fd5fa17170` and `e8fea655cdbc51c8`.
That is consistent and not a problem: cargo names an integration test binary from crate metadata and target name, not from source content, so the same target keeps its path across a source change. The content difference is what the ten results above show.

## Clippy and fmt, on both platforms

`cargo clippy --workspace --all-targets -- -D warnings`, the exact lint that produced the earlier red at `924c16b`:

| Platform | Crates checked | `Finished` reached | `error`/`warning` lines | `cowfs-meta` checked |
| --- | --- | --- | --- | --- |
| Ubuntu `112022716217` | 15 | yes | 0 | yes |
| macOS `112022716236` | 15 | yes | 0 | yes |

So the lint did real work on this run, not a cache no-op, and it emitted nothing under `-D warnings`.
That closes the gap I could only bound statically last round, where I scanned for unused bindings by hand and declined to claim clippy passes.

`cargo fmt --all --check` produced one line, the command echo, and no diff, on both platforms.
That agrees with the standalone `rustfmt 1.10.0-stable --check` I ran myself on both owned files in the previous round, and it removes the toolchain-skew caveat I recorded there for the formatter step, since CI's own `cargo fmt` is now green.

## Integration against current main

`origin/main` was fetched into the tracked remote-tracking ref rather than left in `FETCH_HEAD`, so the object persists independently of any transient ref.
It resolved to `b486d4541bc47b273a5bbd222b95c24fed05c36d`, the object is present in the local store as a commit, and the remote ref read through the API agrees.
`HEAD` and the checked-out branch were unchanged by the fetch: branch `main`, HEAD `93cfef94457a989d031cb6b0a475ac4edbdb85ef`, so the local `main` still lags the remote and I did not move it.

`git merge-tree --write-tree b486d454 c2afa0e1` exits 0 and returns one tree with no conflict list:

```
d1741ce805ed741d8faf17fd9ba51ca85a230d0b
```

I then read that merged tree rather than trusting the clean exit:

| Property of the merged tree | Result |
| --- | --- |
| `new_roots` mentions in `crates/cowfs-meta/src/db.rs` | 7, so the root fix is present |
| `#[test]` count in `crates/cowfs-meta/tests/snapshot_rename.rs` | 10 |
| `db.rs` identical to the PR head's | yes, so #136 did not touch it |
| `crates/cowfs-meta/src/tx.rs` identical to main's | yes, so #136's content is carried |
| `crates/cowfs-core/src/inner.rs` identical to main's | yes, so #136's core content is carried |
| `Cargo.lock` identical to the PR head's | yes, so #136 introduced no dependency drift against this branch |

File-level overlap is nil, which is why the merge is clean: this branch changes `cowfs-meta/src/db.rs`, `cowfs-meta/tests/snapshot_rename.rs` and five documents, while the #136 merge brought `cowfs-meta/src/tx.rs`, `cowfs-meta/tests/operation_time.rs`, `crates/cowfs-core/src/inner.rs`, `crates/cowfs-core/tests/operation_time.rs`, `docs/v1-core.md` and its own documents.

No checkout, no reset, no stash and no lease action was used to get there.

## What this does not claim

- **No local execution by me.** The ten passes, the clippy pass and the fmt pass are the author's CI run on the exact head, read by me from the logs. That is strong evidence and it is not my own build.
- The budget gate is still blocking, so nothing heavier can be added by me without a coordinator decision.
- This is a PASS for request 1 of #42 only. The API has no consumer, `cowfs-core` still stages its own rename through `src/swap.rs`, and nothing outside `cowfs-meta` changes behaviour. #42 is `open` with `state_reason` `reopened`, so it is not complete and this does not complete it.
- Not a merge recommendation. The merge into current main is verified clean, the CI is green on the exact head, and the draft flag, the review state and the budget decision are the coordinator's to settle.
- The two variants I derived by source rather than execution last round are now covered by executing tests, which is an upgrade, not a reason to restate the reasoning as if it had been measured earlier.
- `no-mistakes` is uninitialized in this lane and was not initialized. Browser unverified.
- Misakanet is local-only here and was not consulted; no local memory store was reachable in this lane.

## Closing-keyword hazard, retained and not rewritten

Unchanged from `9341cc4f`, and still the only hazard.
The PR body and title are clean, the title ends `(#42)` with no keyword in front of it, the body's only reference form is `Refs #42, request 1.`, and `closingIssuesReferences` is empty.
The hazard sits in two commit subjects of this branch:

- `9d1e5ef` `test(meta): fix the #42 rename regression fixture and the clippy lint (#42)`, where `fix` and `#42` share a line.
- `c66befa` `fix(meta): resolve a renamed snapshot's root through new_roots (#42)`, the same shape.

Neither is a merge commit, and GitHub derives closing references from commit messages only on the default branch, so nothing closes today.
The exposure is whoever squash-merges and pastes a subject into the merge message.
Rewriting published history is the wrong repair and I am not asking for it; reporting it is the deliverable.

## Source carry, unchanged and re-verified

The delta from the PR base to the head is seven paths: `crates/cowfs-meta/src/db.rs` at 93 insertions with no deletions, `crates/cowfs-meta/tests/snapshot_rename.rs` as a new file, and five documents.
No `Cargo.toml` and no `Cargo.lock` line moves in the branch, and the merged tree's lock is identical to the head's, so no dependency version moved.
No `cowfs-core`, `cowfs-store`, `cowfs-vfs` or `cowfs-meta/src/tx.rs` change in this branch.

Every document is byte-exact between the MAIN primary checkout and the blob inside the head, and all are unchanged by this review:

| Path | sha256 |
| --- | --- |
| `docs/reviews/pr137-meta42-snapshot-rename-final.md` | `471a07310e793016763a1f77e4e195bfeca89dcf1613d84fe50b238a6355b0ec` |
| `docs/reviews/pr137-meta42-snapshot-rename-root-final.md` | `9341cc4f8e5724846bae85a131d13bf3bb661d8755570fbe4836d226f28063d2` |
| `docs/verification/evidence/meta42-snapshot-rename.md` | `93c4b1317b7d9445ced3f7d1316e159b3cca472cbee6172c0a81e2b9a8883c0b` |
| `docs/verification/evidence/meta42-snapshot-rename-regression-correction.md` | `a390aaf4b84bcd6a36117b854faa54710543e4c32c20e868185ff08a390be811` |
| `docs/verification/evidence/meta42-snapshot-rename-root-repair.md` | `91aeec0e68d2c2b85daef8a1e61c70afb8de6642a9a12570a65ac6e45848f3da` |
| `docs/verification/evidence/meta42-snapshot-rename-format-correction.md` | `9a8fb4efdb9acc1c97356a1a50fb4a214da21d7e7d1e54da03d477c58b756f7a` |

The earlier #134 critic reports `803ea5c5`, `ccc5eafc` and `4c3450f5` are also unchanged.

## Scope discipline

Issue #42 request 1 only.
No new issue, no new feature, no audit matrix, no new task.
No production edit, no fixture edit, no commit, no push, no rerun.
No workflow, runner or job setting touched.
No checkout, no reset, no stash, no branch change.
The one mutation I made to repository state is the explicit fetch of `origin/main` into `refs/remotes/origin/main`, which was authorized and which moved no branch and no `HEAD`.
No lease acquired, returned, reset, stashed, pruned or destroyed.
No signal, restart, sudo, install, unmount or store operation, and no deletion of any kind.
The shared daemon PID 15263 with start time `Sat Oct 3 20:44:29 2026`, the Linux host `9879298996041209860`, and every store, mount and job were never contacted, and no mount was traversed.
Concurrent lanes untouched: READY1's PR #139 cache review, READY6's PR #138 hole-flag final at `6bb`, READY5 idle at `c2afa0e`.
Files I own for this review: this document and the four logs under `bench/out/meta42-snapshot-rename-runtime-final-critic/logs/` in the lease, which `.gitignore` excludes.

## What remains

1. The coordinator's decisions: the draft flag, the review state, and the `bench/out` budget, which is still over cap and still blocks every lane that wants to execute.
2. Whoever merges must not paste `9d1e5ef`'s or `c66befa`'s subject into the merge commit message.

Nothing was fixed, merged, marked ready or closed.
PR #137 stays a draft, issue #42 stays open, and no new task was created.