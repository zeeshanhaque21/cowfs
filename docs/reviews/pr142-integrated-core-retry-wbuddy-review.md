# PR 142 integrated Core commit-retry review (head `67fd0ace`)

Reviewer: wbuddy (independent, read-only).
Scope: exact-head audit of PR 142 head `67fd0aceb400fd84010527b0ed355ebfb1540f43` against issue #42 request 4 (Core reserved-inode consumer) and issue #40.
Method: GitHub API reads (integer-only), `gh run view --log` plaintext, immutable `git show` on fetched objects.
No source was edited, no checkout, no cargo, no build, no test, no archive, no probe, no cap waiver, no lease, no signal, no SSH, no shared-resource change, no commit, no push, no PR-body edit, no issue action, no CI rerun or dispatch.
Local execution was forbidden for this audit; every runtime statement below is from a completed GitHub-hosted CI run or is labelled UNEXECUTED.

## 1. Exact pins

| Object | SHA |
|---|---|
| Head | `67fd0aceb400fd84010527b0ed355ebfb1540f43` |
| Head tree | `5715fa6b76524718add359b9814675f59ded55c7` |
| Head parent | `1a49279253c0c9f31677793b779973a137fb143e` (merge) |
| Merge `1a492792` parents | `89d3a9db6df3e99d893168b0f867089e0255fabc` + `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` |
| Merge `1a492792` tree | `4a8ed17be771f3ba6945971ccff8c8fbe98bbef1` |
| Merge base of the two parents | `573b02f5e069f1e52bc32a11f2da4ce4ec8083c4` |
| Remote `main` | `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` |
| Remote PR branch `fix/core-reserved-inode-consumer-42` | `67fd0aceb400fd84010527b0ed355ebfb1540f43` |
| PR 142 `base.ref` | `main` |
| PR 142 `base.sha` | `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` |
| PR 142 `mergeable` / `mergeable_state` | `true` / `unstable` |
| PR 142 `state` / `draft` / `merged` | `open` / `true` / `false` |
| PR 142 `changed_files` / `+` / `-` | 12 / 846 / 52 |

`git ls-remote` confirms remote `main` is exactly `e488a17b` and the PR branch tip is exactly `67fd0ace`.

`gh api pulls/142` reports `base.sha = e488a17b`, i.e. the PR base advanced to current main. This matches the integration receipt, which recorded the base as `89353e17` **before** main moved to `e488a17b`. The base advancing is not a rewrite; no head SHA was rewritten anywhere in this lineage.

## 2. Lineage and integration integrity

Linear ancestry, no rewritten SHAs:
`01c8a2c -> d7646eb -> 890cd27 -> 976aaa4 -> 89d3a9d -> 1a492792 (merge) -> 67fd0ace`.

- `1a492792` is a genuine merge (two parents: branch `89d3a9d`, main `e488a17b`).
- `git diff-tree --cc --name-only 1a492792` lists exactly one conflict-resolved path: `crates/cowfs-meta/src/db.rs`.
- `67fd0ace` differs from `1a492792` by exactly one file: `crates/cowfs-meta/src/db.rs`, `+8 -0` (the T13 assertion).
- Every production hunk in `db.rs` before `mod tests` (line 2078) is the branch's own approved #42-request-4 integration (`NEXT_STORE_ID`, `reserved` set, the commit-fault seam, retry logic). Hunks from line 184 onward are all inside `mod tests`.

### 2.1 Conflict resolution preserved both test blocks

Test-function set diff, branch `89d3a9d` db.rs vs head db.rs:

- Only in `89d3a9d`: none (nothing from the branch was lost).
- Only in head: `a_large_reservation_is_measured_against_a_single_one`, `a_range_ending_on_the_limit_is_the_largest_legal_one`, `open_recover_keeps_a_reservation_bound_across_a_real_rollback`, `the_inode_ceiling_admits_the_last_range_and_refuses_the_overflow` (exactly main's four-test INO_LIMIT/`seed_floor` suite).

`seed_floor` references: main `e488` = 3, branch `89d3a9d` = 0, head = 3. So main's suite is retained.
T13 presence: main `e488` = 0, branch `89d3a9d` = 1, head = 1. So the branch's suite is retained.
Head db.rs holds 18 `#[test]` functions, including the 11 named reservation/retry tests in T9-T14. Both blocks are intact and no assertion was weakened by the merge itself.

Verdict on integration: **clean and faithful**. The merge introduced no production change beyond the approved integration. The empty `89d3a9d` re-trigger commit is acknowledged, preserved in history, and not repeated; the author receipt states this plainly as a rule violation. No further artificial commit was made.

## 3. The `67fd0ace` fix and its T13 semantics

### 3.1 What CI forced

Run `37527891397` on the merge head `1a492792` failed `cargo clippy --workspace --all-targets -- -D warnings` at the `cowfs-meta` lib-test target:

```
error: unused variable: `want`
  --> crates/cowfs-meta/src/db.rs:2473:13
  = note: `-D unused-variables` implied by `-D warnings`
```

T13 declared `let want = tickets[0].ino();` but its only assertion was on the retry error text, so `want` was unused. `67fd0ace` "uses" `want` by adding:

```rust
// Nothing was persisted, so no inode exists yet at the number.
assert!(
    matches!(s.getattr(want), Err(Error::NotFound)),
    "the failed durable commit left an inode behind"
);
```

### 3.2 The assertion contradicts the retained-pending-tree design

The T13 test's own header comment (db.rs, head) states the intended semantics:

> The batch returns an error, so the caller retries. With the removal of the number from the session's outstanding set placed before the commit, the retry was refused as if the ticket had never been minted. Here the number stays owned by the session, so **the retry is stopped only by the create that is still pending in the tree**, never by a lost reservation.

And the retry's error check only forbids the "was not issued" message, i.e. it **expects** the retry to be refused by the visible pending inode (`Error::Exists`), not by a missing reservation.

Source trace at head `67fd0ace`:

1. `set_commit_fault(1)` makes `commit()` return `Err` at `db.rs:711`, **before** `wtx.commit()`.
2. On that `Err` path `commit()` returns early at `db.rs:723-727` and **never** runs `e.tree.reset(root)` (the only `Ok`-path reset is at `db.rs:731`).
3. `wait_durable` (`db.rs:1086`) calls `commit()`, gets `Err`, does `r?`, and never advances `durable_seq` (stored only in the commit Ok tail at `db.rs:782`).
4. `mutate` (`db.rs:961`) rolls back the tree only on closure `Err`/panic (`e.tree = saved` at `db.rs:1002`/`1006`). The `wait_durable`-failure branch (`db.rs:1066+`) re-inserts `want` into `s.reserved` and returns `Err`, but does **not** reset `e.tree`.
5. The pending create's inode record therefore remains in the in-memory session tree (a `Kid::Mem` overlay).
6. `Meta::getattr` (`db.rs:1985`) reads through `read()` (`db.rs:1962`), which reads `e.tree` plus the store overlay (`MemTree::get`, `ptree.rs:446`). It resolves the name against that tree and returns the pending inode record.
7. So `s.getattr(want)` returns `Ok(Attr)`, and `matches!(s.getattr(want), Err(Error::NotFound))` is **false**.

The retry then hits `read::inode`/`read::entry` in `new_child_at` and is refused `Error::Exists` because the pending inode is present, exactly as the T13 comment describes. The newly added assertion is therefore **semantically wrong**: it asserts the opposite of the test's own stated design.

Ownership note: `want` is `tickets[0].ino()`, a plain `u64` copy of the ticket's number, not a move of the `ReservedIno`. Reading the number here is safe and does not violate the move-only authority contract. The problem is purely the asserted value, not the use of `want`.

The main integration receipt repeats the same error in its justification: it calls `Err(NotFound)` "the claim the test exists for." That claim is incorrect for the same reason.

## 4. Actual runtime evidence (completed CI)

Run `37528269132` on head `67fd0ace` completed failure. I did not poll, wait, rerun, or dispatch; this is the terminal state observed on read.

Job steps (ubuntu):

| # | Step | Conclusion |
|---|---|---|
| 5 | `cargo fmt --all --check` | success |
| 6 | `cargo clippy --workspace --all-targets -- -D warnings` | success |
| 7 | `cargo test --workspace` | **failure** |
| 8 | Bench harness unit tests | skipped |

macOS `check` also completed failure on the same tests. The `linux-fuse` job completed success (FUSE config conformance) - this is **not** Core/Meta evidence.

### 4.1 The actual failure is a regression in `crates/cowfs-core/tests/alias.rs`

Both architectures fail the same two unchanged tests:

```
test a_create_past_the_alias_ceiling_is_refused ... FAILED
thread ... panicked at crates/cowfs-core/tests/alias.rs:202:5:
  left: 0
 right: 4

test a_session_alias_costs_a_bounded_number_of_bytes_per_inode ... FAILED
thread ... panicked at crates/cowfs-core/tests/alias.rs:69:5:
  assertion `left == right` failed: the alias count must be one per live inode:
    Stats { ..., aliases: 0, ... }
  left: 0
 right: 20001

test result: FAILED. 1 passed; 2 failed; 1 ignored
error: test failed, to rerun pass `-p cowfs-core --test alias`
```

`alias.rs` blob is **identical** on base `e488` and head `67fd0ace`: `38c37627b57de3400f76fd7db05a4a0c771e0b4d`. The PR did not touch this file. On main `e488` run `37521445526`, `cargo test --workspace` passed, and the same `alias.rs` blob was present. Therefore these are **not stale tests**: the PR's production change broke a green main test suite. This is a real regression.

### 4.2 Responsible seam: `inner.rs` reserved-create alias skip

The production change is in `crates/cowfs-core/src/inner.rs`. In `commit_batch`, the head inserts a session alias only for virtual children:

```rust
// Only a virtual child needs the bridge to its meta number. A create at a
// reserved number already holds the packed meta number, so aliasing it would be
// a self-entry that inflates the table and counts against the alias ceiling.
for (v, m) in &created {
    if matches!(classify(*v), Id::Virt { .. }) {
        al.insert(*v, sc.id, *m);
    }
}
```

The same diff removes the old `alloc_virt` alias-ceiling refusal (`inner.rs` around line 249) and repoints creates to `take_reserved` / `reserve_tickets`.

The unchanged `alias.rs` contract, however, is exactly `s.aliases == n + 1` ("the alias count must be one per live inode") and `s.aliases == 4` for the ceiling-refusal path. The head produces `aliases: 0`, because reserved creates no longer record an alias. The excluded virtual-only insertion satisfies the new design intent but violates the published, still-live Core contract and its tests.

Minimal responsible seam: the reserved-create alias policy in `inner.rs` `commit_batch` (head lines ~787-793) and the removed ceiling enforcement in `inner.rs`. The defect is a production design tension introduced by the integration, **not** a test that needs silencing.

One of the two failing tests is load-bearing for the design under audit: `a_create_past_the_alias_ceiling_is_refused` is precisely the "session inode limit / bounded alias table" gate (F7) that the prompt names as a remaining acceptance gate. It now fails because the ceiling it tests is no longer enforced by the reserved path.

## 5. What actually executed vs what did not

`cargo test --workspace` stops at the first failing test binary; the workflow's `fail-fast: false` is on the job **matrix**, not on cargo, so it does not continue past a failing binary.

Unexecuted on this head because `cowfs-core` sorts before `cowfs-meta` and its `--test alias` binary failed:

- The entire `cowfs-meta` lib-test target, including T12 (`a_closure_error_leaves_the_ticket_usable_for_retry`), T13 (`a_failed_durable_commit_does_not_strand_the_reserved_number`), and T14 (`a_durable_commit_that_persisted_then_failed_does_not_duplicate`).
- The main-side db.rs suite: `the_inode_ceiling_admits_the_last_range_and_refuses_the_overflow`, `a_range_ending_on_the_limit_is_the_largest_legal_one`, `open_recover_keeps_a_reservation_bound_across_a_real_rollback`, `a_large_reservation_is_measured_against_a_single_one`, and `seed_floor` tests.
- `crates/cowfs-meta/tests/inode_reservation.rs` (11 `#[test]` functions) - never ran.
- `crates/cowfs-core/tests/reserved_inode_identity.rs` - the four real Core identity tests (`a_created_file_keeps_one_durable_identity_across_a_flush_and_reopen`, `a_created_number_is_never_the_virtual_alias_shape_or_the_root`, `a_virtual_alias_number_is_stale_after_a_reopen`, `a_create_that_failed_its_first_flush_keeps_its_number_and_bytes`) - never ran, because the `alias` binary in the same crate failed first (or would have).

So on head `67fd0ace`:

- **T13 runtime result: UNEXECUTED.** My source finding (the assertion is wrong and would fail) is a source conclusion, not a runtime observation. The runtime failure that did occur is the `alias.rs` regression.
- **Core reservation identity/runtime: UNEXECUTED.**
- **Meta reservation/retry runtime: UNEXECUTED.**

The current sub-100% runtime state is worse than "T13 pending": the run is `failure`, and the failing suite is an unchanged Core contract test. `67fd0ace` fixed the clippy `unused variable` symptom while introducing/omitting a resolution for a genuine behavioral regression and shipping an assertion that contradicts its own test's design.

## 6. Still-remaining #42 request-4 acceptance

Source-covered, runtime-unmet:

- T13 and the whole Meta reservation/retry suite have no green run on any PR-142 head.
- The four Core identity/retry tests have no green run (in `reserved_inode_identity.rs` nor elsewhere).
- The F7 "bounded alias table / session inode limit" gate is now failing the `alias.rs` contract and must be reconciled between the reserved-create policy in `inner.rs` and the published Core stats contract.
- M5 allocator runtime green (from the PR-144 lane) remains pending.
- Whole-#42 consumer/snapshot acceptance is owned by other lanes and is out of this review's scope.

## 7. Merge verdict

**NOT MERGE_READY.** Two independent blockers:

1. **RUNTIME FAILURE (regression).** Head `67fd0ace` CI `37528269132` is `failure`. `crates/cowfs-core/tests/alias.rs` (blob `38c37627`, identical to main, green on main) fails two tests on both ubuntu and macos. Responsible seam: the reserved-create alias policy in `inner.rs` `commit_batch` and the removed ceiling enforcement. No test was changed; the production change is the cause.
2. **SOURCE DEFECT in the fix.** The added T13 assertion `matches!(s.getattr(want), Err(Error::NotFound))` contradicts the retained-pending-tree design stated in T13's own comment, because the failed `commit()` does not reset the session tree, so `getattr(want)` resolves the pending inode. It would fail if executed (UNEXECUTED here only because the `alias` binary fails first).

Neither the `alias.rs` contract nor the T13 assertion may be silenced or weakened; both need a source resolution. This review makes no source edits.

## 8. What I did not and cannot claim

- No local build/test/cargo/fmt/clippy was run (forbidden). Every runtime line above is a GitHub-hosted CI result or an UNEXECUTED label.
- T13 did not run; its failure is a source conclusion.
- The `linux-fuse` green job is not Core/Meta proof.
- No mutant proof was run; no mutant is claimed.
- The four Core identity tests and the Meta reservation suite are UNEXECUTED on this head.
- Whole-#42 completion is not claimed. A source pass on one lane would not complete it; here even that is absent because of the `alias` regression.
- This file is the only artifact I wrote. All prior reviews and receipts are immutable and unchanged by me.

## 9. Raw evidence pins

- PR 142: `gh api repos/zeeshanhaque21/cowfs/pulls/142` -> head `67fd0ace`, base `e488a17b`, base.ref `main`, mergeable `true`, state `unstable`, draft `true`, merged `false`, 12 files, +846/-52.
- Head run `37528269132`: `status completed`, `conclusion failure`; `check (ubuntu-latest)` and `check (macos-latest)` failure, `linux-fuse` success.
- Merge-head run `37527891397`: `completed failure`, clippy `unused variable: want` at `db.rs:2473:13`.
- Main run `37521445526` (`e488a17b`): `completed success`, `cargo test --workspace` success.
- `alias.rs` blob: base `38c37627b57de3400f76fd7db05a4a0c771e0b4d` == head `38c37627b57de3400f76fd7db05a4a0c771e0b4d`.
- Log source: `gh run view 37528269132 --log` (plaintext), pinned at `/tmp/pr142_67fd.log` during the read; ANSI-stripped copy used for counting.

No sandbox run occurred; the `context-mode` MCP tools were unavailable this session, so large log outputs were derived with plain in-line filters (grep/sed) rather than an indexed sandbox.
