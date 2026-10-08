# PR #134 recovery review: the block is resolved, and the daemon branch is now driven end to end

Reviewer lane: `.treehouse-build-train/.treehouse/cowfs-7c1bf8/6/cowfs`, held slot 6.
Lease verified before any work: branch `review/gc-root-mark-retention-82`, HEAD `b4b55ab`, working tree showing only the six pre-existing untracked `docs/reviews/*.md` files from other lanes, no process of mine running. Matched expectation, so the lane was used.

Corrected head reviewed: `8264ca73238eb5fb7d567946f8699fd188b77a60`.
Base reviewed: `93cfef94457a989d031cb6b0a475ac4edbdb85ef`, which is current `origin/main` and an ancestor of it.
Blocked head this supersedes: `4646f513a02bb9b3256920d138a9f01551e35c34`.
Blocking report this answers: `docs/reviews/pr134-swap-provenance124-final.md`, sha256 `803ea5c5e049225369ab72b50829a9f6483b2ac96ff1d06a9b16405d58c17e7e`, preserved immutable and committed byte for byte on the new head.
Review date: 2026-10-05.
Artifacts: `bench/out/swap-provenance124-recovery-final-critic/**` inside the lease.
This document: canonical PRIMARY copy, `docs/reviews/pr134-swap-provenance124-recovery-final.md`.

## Verdict

SCOPED PASS, for issue #124 only.

The defect I proved is gone, and the fix is now the safety floor rather than a guess about which failure happened.
The false invariant that caused the block is deleted from the code and retracted in the receipt.

I closed the one gap the author disclosed as unproven.
The daemon's uncertain-error branch is now driven end to end through the real production `CoreSnapshots::swap`, with a real fault, and it is shown to leave the record unknown.
The same harness on the blocked head fails with exactly the state #124 was filed for, so the discrimination is real and not merely the absence of a restore call.

No stale-commit path remains that I can find, on either backend, in the code or at runtime.

One behaviour change is a finding, not a block, and it is disclosed below.

## The block is resolved, on the source

`crates/cowfs-daemon/src/backend.rs` at the corrected head:

- `restore_base_record` does not exist. `rg -c restore_base_record` returns `0`.
- The clause "An error out of the staged swap means it rolled back" does not exist. `rg -c 'means it rolled back'` returns `0`. `rg -c 'rolled back'` over the whole file returns `0`.
- `invalidate_base_record` now returns `io::Result<()>` at `backend.rs:337`, and drops the `Option<BaseMeta>` capture entirely, so there is no value left anywhere that could be written back.
- `CoreSnapshots::swap` at `backend.rs:711` does the preflight first, at lines 728 to 743, inside the `bases.exclusive` section opened at line 720. Then `invalidate_base_record` at line 760, then `promote_base` at line 761. No `map_err`, no restore.
- `PathSnapshots::swap` at `backend.rs:1003` does the same shape: preflight at 1004 to 1017, invalidate at 1024, then the filesystem work, with no restore on any branch.
- `rollback_base` survives at `backend.rs:355` and is called from exactly two places, `backend.rs:782` and `backend.rs:1080`, both inside `rename`. It is not reachable from either swap. That is correct: `rename` moves a record that still describes a tree that is still there, so putting it back is honest.

The comment at lines 748 to 754 now states the correct thing, naming the file and line that refute the old claim and saying the error cannot distinguish the two cases.

## The daemon branch, driven end to end

The author disclosed this as unproven: no public seam reaches `CoreSnapshots::swap` because `CoreBackend` exposes no meta-options hook.
That disclosure was accurate about the public surface and wrong about the crate.

Inside the crate the two private fields of `CoreSnapshots` at `backend.rs:313` are reachable, and `Core::open_with_meta` is public.
So a `Core` opened through `open_with_meta` with a failing `before_sync` can be placed into the exact production struct and the exact production `swap` called.
No new API, no new framework, no production hook, no shipped source change.

The harness lives only in my private probe archive, `bench/out/swap-provenance124-recovery-final-critic/probe-daemon`, which is a copy of the corrected head plus one `#[cfg(test)] mod critic_uncertain_err_probe` appended to `backend.rs` and one dev-dependency line in that copy's manifest.
The `head/` tree under test was verified byte-identical to `8264ca7` with `diff -rq` after the harness was moved out.

Result at the corrected head, one foreground `mac-heavy.lock` hold, isolated `CARGO_TARGET_DIR` and `TMPDIR`, exit `0`:

```text
critic: fail_at=1 swap returned Err immediate_response_commit=None
critic: fail_at=2 swap returned Err immediate_response_commit=None
critic: fail_at=3 swap returned Err immediate_response_commit=None
critic: fail_at=3 PENDING INTENT, commit=None tree="BBBB-from-srcB"
critic: fail_at=4..8 swap returned Ok immediate_response_commit=None
test backend::critic_uncertain_err_probe::critic_daemon_swap_err_never_restores_a_stale_commit ... ok
```

`fail_at=3` is the third durable commit of the swap, which is the fork of the staging snapshot into the target name.
The swap returned `Err`, the intent file survived it, so the failure landed past the point of no return.
The next open, through the production `CoreBackend::open`, ran `swap::recover` and installed `BBBB-from-srcB`, the new tree.
The record reads `commit=None`, which is the honest answer.

The same harness, byte-identical, grafted into the blocked head `4646f51` in a separate probe tree whose only delta is `backend.rs`, exit `101`:

```text
critic: fail_at=3 PENDING INTENT, commit=Some("commit-AAA") tree="BBBB-from-srcB"
assertion `left == right` failed: critic: the record still names a commit that did not produce this tree
  left: Some("commit-AAA")
 right: None
```

That is the OLD unsafe state and the NEW safe state, from the same harness, the same fault, the same reopen path.
The harness itself was verified identical across both trees, sha256 of the appended module `7967d77cecb46542dd12f9bdc3725d9a` in each.

The receipt's causal probe and the author's own `probe:` output lines are consistent with this and were not re-derived as the primary proof, since the daemon-level result is strictly stronger.

## Path backend: the retired tree is no longer deleted

My secondary finding from the blocked round is fixed, and the fix is source-correct.

`backend.rs:1028` and `1029` clear `staging` and `retired` before the work starts, which is a change from the blocked head and matches what `main` already did at its line 961.
Then at 1030 to 1035 the copy and the two renames.
On `Ok` at 1037, `force_remove_dir_all(&retired)` runs only after a successful replacement, so the cleanup is scoped to the success path.
On `Err` at 1038, `force_remove_dir_all(&staging)` runs, and then at 1044 `if retired.is_dir()`, the old tree is renamed back to where it was at 1045.
If that rename fails, the error at 1047 to 1053 names the retired path and says it was left rather than deleted.

Nothing on the failure path deletes the only remaining copy.

Checked the deletion sites individually, as asked:

- `.cowfs-swap-{name}` and `.cowfs-retired-{name}` both begin with a dot, and `cowfs_snapname::validate_snapshot_name` refuses a leading dot at `crates/cowfs-snapname/src/lib.rs:70`, returning `LeadingDot`.
- `snapshot_dirs` at `backend.rs:916` filters on `validate_snapshot_name`, so neither staging nor retired is ever listed as a snapshot.
- Therefore neither cleanup can reach a real user snapshot, the source tree, a base record, or anything an intruder could name as a snapshot.
- The base record lives under `.cowfs-base-meta`, not in the snapshot namespace, and no branch of the path swap touches it after invalidation.

This part remains source-level.
No `std::fs::rename` fault seam exists in this repository, so I did not claim and do not claim a runtime counterexample for it.
I searched `cowfs-vfs-path` and `cowfs-daemon` for `set_fault`, `arm`, `fault_boundary` and `Fault::` and found none, which confirms the author's statement and mine.

## Independent old-fails-new-passes, third time, on the exact base

The nine-test file is byte-identical at all three heads, sha256 `db58ba7658b2f4759df0805a9ca3b7a27b44b81a697516937bce4762d3e367c2`, blob `bfc2336fd2f97831dbd4e47d4b5e1b69eb69e23b`, and `git diff` between `4646f51` and `8264ca7` for that path is empty.

Against `main`'s production `backend.rs` at sha256 `53892a6ecd60528527e96ba8df9b05e1da5757ea71198be836a63b0a5064a976`, same test file, exit `101`:

```text
test result: FAILED. 4 passed; 5 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.60s
```

At the corrected head, twice, exit `0` both times, `9 passed; 0 failed`, 1.69s and 1.71s.

## Scoped results at the corrected head

One foreground `mac-heavy.lock` hold for the batch, isolated `CARGO_TARGET_DIR`, project-local `TMPDIR`, receipts captured directly and never through a pipeline.
Exit codes in the right-hand column are the exit of the command itself, not of a `tee`.

| Check | Result | Exit |
| --- | --- | --- |
| `--test swap_provenance_124` repeat 1 | 9 passed, 0 failed, 1.69s | 0 |
| `--test swap_provenance_124` repeat 2 | 9 passed, 0 failed, 1.71s | 0 |
| `cargo test -p cowfs-daemon --locked --lib` | 100 passed, 0 failed, 9.97s | 0 |
| `--test snapname_drift` | 4 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --locked --test swap` | 3 passed, 0 failed | 0 |
| `--test namespace_durability` | 0 passed, **3 ignored**, 0 failed | 0 |
| `cargo fmt -p cowfs-daemon -- --check` | clean | 0 |
| `cargo clippy -p cowfs-daemon --all-targets --locked -- -D warnings` | zero warning or error lines | 0 |
| critic harness, corrected head, daemon `swap` | 1 passed, `commit=None` at pending intent | 0 |
| critic harness, blocked head `4646f51`, same harness | 1 failed, `commit=Some("commit-AAA")` | 101 |
| nine tests against `main`'s `backend.rs` | 4 passed, 5 failed | 101 |

The three `namespace_durability` tests are reported as ignored, not as passing.
They mount a filesystem and kill a daemon, and were not run.

## Finding: the same-name refusal now precedes existence on the core backend

Not a block, and not a provenance issue, but a real behaviour change I found while checking the preflight ordering.

`backend.rs:728` checks `name == from` before the existence loop at 734.
`Core::promote_base` at `crates/cowfs-core/src/lib.rs:359` looks the source up first, so before this change a swap of a nonexistent name with itself reported `NotFound`.

Measured on both trees, one locked hold each:

```text
corrected head: core swap(nosuch,nosuch) kind=InvalidInput msg=cannot swap a snapshot with itself
                path swap(nosuch,nosuch) kind=NotFound       msg=snapshot "nosuch" does not exist
blocked head:   core swap(nosuch,nosuch) kind=NotFound       msg=snapshot "nosuch" does not exist
                path swap(nosuch,nosuch) kind=NotFound       msg=snapshot "nosuch" does not exist
```

The two backends now disagree for that one input, and the core backend's answer changed from `NotFound` to `InvalidInput`.
Both answers are refusals, both preserve the old tree and the old record, and `control_io` at `backend.rs:472` maps `InvalidName` to `InvalidInput`, so the wire code stays in the documented family.
The nine tests do not cover it: `the_existing_swap_refusals_keep_their_record` uses an existing `base` for the same-name case.

Worth a line in the receipt and a decision on which backend is authoritative.
Not worth a block on a #124 stale-provenance review, and it does not weaken the contract.

## Contract assessment, not weakened

The new contract is: an admitted swap invalidates the record and never restores it; refusals decided before anything is touched keep the full old tree and the full old record.

I checked that against the code rather than taking it on trust, and it holds.

- No restore path exists on either backend, so a stale commit cannot be written back by any branch. This is stronger than the old design, which had a restore path guarded by a precondition that was false.
- The record is never left holding a commit that did not produce the bytes under its name. That is the property #124 exists to protect, and it now holds on every path I can find.
- Refusals keep the full old state. `backend.rs:728` to 743 is inside the `bases.exclusive` section opened at 720, and every namespace mutation in the daemon takes that same section: `create` at 706, `remove` at 731, `rename` at 774, `promote` at 802. `import.rs` reaches the namespace only through the `Snapshots` trait, so it inherits the section. The refusal cannot race a concurrent namespace change into a stale record.
- A record publication failure is still strict. `invalidate_base_record` runs before any tree operation, so the record-write failure at line 760 leaves both the old tree and the old record intact. That is test 6, unchanged, still passing, and it is genuinely a before-mutation case.
- Source provenance is still deliberately not adopted, `backend.rs:337` to 351 and the test at `swap_provenance_124.rs:183`. Option 1 is preserved.
- A never-promoted target still acquires no record, `backend.rs:342` to 344 and the test at line 231.

The honest cost the author discloses is real and I accept it: a swap that truly rolled back loses still-true provenance and reports stale.
That is the correct floor for code that cannot distinguish rollback from a failed roll-forward, and it is strictly better than a base claiming a commit that did not produce its bytes.

## Source binding

Verified by `git cat-file -t` on both commit ids, `git archive` into fresh directories, and `diff -rq`.

| Path | State | git blob at `8264ca7` | sha256 under test |
| --- | --- | --- | --- |
| `crates/cowfs-daemon/src/backend.rs` | modified | `73d37d0fca08f6de7a74d0c5c4c38728d3adcf3a` | `bc98830db838a38c1855fb7fac2c2296dc4e1d3513d5fb43cf2644960be25372` |
| `crates/cowfs-daemon/src/base_meta.rs` | unchanged | `6c020ae303b7ac3213efe87a0c205629acac12d5` | `4a8eac20294e6d5785a0fb34776731cfab077cdb40685e6bd80255813c1b4b92` |
| `crates/cowfs-daemon/tests/swap_provenance_124.rs` | unchanged | `bfc2336fd2f97831dbd4e47d4b5e1b69eb69e23b` | `db58ba7658b2f4759df0805a9ca3b7a27b44b81a697516937bce4762d3e367c2` |
| `crates/cowfs-core/src/swap.rs` | unchanged | `4cbc3f289b68e8d626321a481525ced5f5562af0` | `5a3e630494c8ca14d3d98dbd6c68283141b4b780e57ccbd07ca3b7bc8b3996ff` |
| `crates/cowfs-core/src/lib.rs` | unchanged | `efc502cae4b04a811e4e6d4260115f8d748da0d4` | not re-hashed, blob-matched |

`base_meta.rs` and `swap.rs` carry the same blobs at `93cfef9` and `8264ca7`, so the metadata format and the core swap are untouched.

Document receipts, verified against the canonical PRIMARY checkout and against the blobs inside the new head:

| Path | sha256 | expected |
| --- | --- | --- |
| `docs/reviews/pr134-swap-provenance124-final.md` | `803ea5c5e049225369ab72b50829a9f6483b2ac96ff1d06a9b16405d58c17e7e` | matches, preserved immutable |
| `docs/verification/evidence/swap-provenance124-repair.md` | `0dcb6a695a8b933528a348209c46c13c216d5a933a396daa7a33ff187e4f80f9` | matches, superseded, not rewritten |
| `docs/verification/evidence/swap-provenance124-recovery-repair.md` | `cfeffa7b35644feeef07296b223224f9b7e20db782ae4795f94b37a048e3bf2a` | matches |
| `docs/verification/evidence/swap-provenance124-reproduction.md` | `3fde9b054fea530288e1fe3d4577bdbe97a626fad2f6f26cf92611b8d3a029a1` | matches, untouched |

The receipt that carried the false claim is still in the tree with its original hash.
The retraction lives beside it and in the PR body rather than by rewriting history, which is the right call.

History on the branch, oldest first: `b942276` the first fix, `cbac228` the reproduction, `4646f51` the first receipt, `264eec1` the correction, `8264ca7` this receipt.
The PR body supersedes my blocked contract, quotes `swap.rs:194-197` against itself the way I did, and carries the negated paragraphs: the honest cost of losing provenance, the refusal kinds that are checked early, and the limitations.

## CI, one snapshot, on the new head

One `statusCheckRollup` read for the last commit of PR #134, exact head `8264ca7`.
Nothing was dispatched, rerun or reconfigured.

| Check | Status | Conclusion | Started | Completed |
| --- | --- | --- | --- | --- |
| `check (ubuntu-latest)` | IN_PROGRESS | null | 2026-10-05T20:01:04Z | null |
| `check (macos-latest)` | IN_PROGRESS | null | 2026-10-05T20:00:52Z | null |
| `linux-fuse` | COMPLETED | SUCCESS | 2026-10-05T19:56:16Z | 2026-10-05T20:01:16Z |

CI on this head is **not green**: two of three still running, one passed.
The three green checks reported for `4646f51` in the earlier receipt say nothing about this head, which is exactly what that receipt claimed.

Other PR state, same snapshot: `headRefOid` `8264ca73238eb5fb7d567946f8699fd188b77a60`, `baseRefOid` `93cfef94457a989d031cb6b0a475ac4edbdb85ef`, `state` OPEN, `isDraft` true, `mergeable` MERGEABLE, `reviewDecision` null.
`closingIssuesReferences` is empty and issue #124 is open.

## Limitations and uncertainty, stated plainly

- The path-backend retired-tree fix is source-level only. No `std::fs::rename` fault seam exists in this repository, so no runtime counterexample is claimed for it. The deletion-site analysis above is what backs it.
- The daemon harness is a `#[cfg(test)]` module inside a private copy, not a shipped test. It is the only way I found to reach the daemon branch with a real fault, and it needs no production change, but it does not ship. The author may reasonably want a permanent regression here; that is their call, and this review does not waive it.
- `before_sync` is one real failure class. It proves the daemon's branch is safe for a failure after the point of no return with a pending intent. It does not claim to cover every such failure.
- The harness replaces the store-durability hook. It is measuring which commit fails, not durability ordering, and it makes no durability claim.
- The refusal-kind finding is measured on both trees and is exact, but I did not check whether any caller depends on the old `NotFound`, and I did not check the CLI or treehouse presentation of that kind.
- No live socket round trip, no mid-GC, power-loss or full-filesystem acceptance, no runtime frequency or core blame claim. Browser unverified.
- `no-mistakes` is uninitialized in this lane and was not initialized.

## Scope discipline

Reviewed only issue #124, the changed backend, and the seam it depends on.
No new issue, no new feature, no audit matrix, no new task.
Issues #125, #127 and #128 are parked and were not touched.
No author file was modified: the harness lives in two private probe copies, and `head/` was verified byte-identical to `8264ca7` with `diff -rq` after the harness was moved out.
The prior critic report at `803ea5c5` and the prior artifact tree `bench/out/swap-provenance124-final-critic/` were not touched; my new tree did not exist before this review.
No checkout, branch change or commit in the lease.
No lease acquired, returned, reset, stashed, pruned or destroyed; all leases left as found.
No signal, restart, sudo, install, unmount or store operation.
The shared daemon PID 15263 with start time `Sat Oct 3 20:44:29 2026`, store `/Users/zeeshanhaque/.cowfs/store`, mount `/Users/zeeshanhaque/.cowfs/mnt` and socket `/Users/zeeshanhaque/.cowfs/sock/daemon.sock` were never contacted, and no mount was traversed.
Concurrent lanes untouched: the #136 critic on metadata and `Core` inner clock, the #135 author on documentation errata, the #134 author at READY5.
One disk reading before the batch: 313 GiB free against the 20 GiB floor; artifacts total 4.9 GiB against the 8 GiB cap.

## What remains before merge

1. CI green on `8264ca7`. Two checks were still running when this review was written.
2. A decision on the same-name refusal ordering, so the two backends answer the same input the same way. Not a #124 blocker.
3. Optionally, a permanent regression for the daemon's uncertain-error branch, so the next change to `swap` cannot reintroduce a restore without a test catching it.

Nothing was fixed, merged, marked ready or closed.
Issue #124 stays open, PR #134 stays a draft, and no new task was created.