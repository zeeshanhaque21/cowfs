# #42 request 4 (Core consumer): main integration and conflict resolution

Status: merge complete and pushed on draft PR #142. CI run exists for the merge head, result not yet known.

This receipt covers only the integration of published `origin/main` into the branch.
It does not restate the retry repair, which is in
`docs/verification/evidence/meta42-core-commit-retry-repair.md`.
The original implementation report
`docs/verification/evidence/meta42-core-reserved-inode-consumer-implementation.md` is unchanged.

## Correction: the empty re-trigger commit

The previous session pushed `89d3a9d`, an empty commit titled
`ci: re-trigger the pull_request workflow for this head`, to force a CI run after two
pushes produced no `pull_request` run.
The independent audit
`docs/reviews/pr142-core-commit-retry-wbuddy-review.md` found that `89d3a9d` has tree
`f823bde1`, byte-identical to `976aaa4`, and that the prompt forbade an artificial trigger commit.

That was a rule violation, stated plainly here and not excused.
The commit added no content and changed no source, but it was an artificial trigger and should not
have been made.
No reset, amend, or force was used to remove it; the history object `89d3a9d` is preserved.
No further empty, cosmetic, or re-trigger commit was made in this session.
The earlier summary that "only CI remains" is not repeated as a finding here.

## Why the PR was dirty, and the real cause

Verified with read-only GitHub reads before any git mutation:

- PR #142 `base.ref` is `main` (expected, not an unexpected base), `base.sha` was
  `89353e17e5085000711dc428e834f9cc41840a1f`.
- Feature head was `89d3a9db6df3e99d893168b0f867089e0255fabc`.
- Remote `main` was `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e`, so the PR base was behind main.
- `mergeable: false`, `mergeable_state: dirty`.

`89353e17` (the PR base) was already an ancestor of the head, so nothing about the dirty state was a
history rewrite. Main was not an ancestor of the head. The merge base of head and main was
`573b02f5e069f1e52bc32a11f2da4ce4ec8083c4`, the PR 140 main-line chain tip.

This is genuine integration work: the branch had to absorb main's changes.

## The conflict, and its resolution

`git merge-tree --write-tree HEAD origin/main` reported exactly one conflicted path:
`crates/cowfs-meta/src/db.rs`.

The conflict is a pure append-after-the-same-test conflict at the end of `mod tests`:

- `origin/main` appended its INO_LIMIT ceiling suite (`the_inode_ceiling_admits_the_last_range_and_refuses_the_overflow`,
  `a_range_ending_on_the_limit_is_the_largest_legal_one`, `open_recover_keeps_a_reservation_bound_across_a_real_rollback`,
  `a_large_reservation_is_measured_against_a_single_one`) plus the `seed_floor` helper.
- The branch appended its ticket and commit-retry suite (T9 to T14, `a_ticket_creates_at_its_number_and_is_spent_once`
  through `a_durable_commit_that_persisted_then_failed_does_not_duplicate`).

Both blocks sit after the identical preceding test and share no function name.
The resolution keeps both blocks with every assertion intact.
Neither side was discarded, and no assertion was weakened.

No production code conflicted: git auto-merged every production hunk. Main's production change to this
file was limited to tests; the branch's production additions (`reserve_tickets`, `ReservedIno`,
`create_at`, the commit fault seam) are untouched.

Main's other changes to the tree were in `crates/cowfs-core/src/lib.rs` (atomic rename),
`queue.rs` (`SnapCtx`), `io.rs` and `swap.rs` (already present on the branch via the shared
dependency chain), and test/doc additions. Each is present in the merge alongside the branch's own
changes; verified by diffing the merged tree against both parents.

## Merge commit

- Merge commit `1a49279253c0c9f31677793b779973a137fb143e`.
- Parents: `89d3a9db6df3e99d893168b0f867089e0255fabc` (branch) and
  `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` (origin/main).
- Pushed over HTTPS as a normal fast-forward that creates the merge, no force.

Post-merge checks:

- No conflict markers anywhere in the tree.
- `rustfmt --edition 2021 --check` exit 0 on every edited source and test file.
- `e488a17` (main) is now an ancestor of the head.
- The identity and retry regression assertions in
  `crates/cowfs-core/tests/reserved_inode_identity.rs` are present and unchanged.
- PR #142 now reports `mergeable: true`, `base.sha e488a17`.

## Runtime and gates

Local `cargo build`/`cargo test` is blocked by a full-worktree capacity block: READY3 is at 7.6 Gi
with no measured headroom, so no local build, test, archive, or probe was run.

The only gate is CI.

CI actually ran on the merge head `1a49279` (`37527891397`).
Both `check` jobs completed with failure on the same single error, a real one:

```
error: unused variable: `want`
  --> crates/cowfs-meta/src/db.rs:2473:13
  = note: `-D unused-variables` implied by `-D warnings`
error: could not compile `cowfs-meta` (lib test) due to 1 previous error
```

T13 (`a_failed_durable_commit_does_not_strand_the_reserved_number`) declared `let want` but asserted
only on the retry error text, so the lib test target failed under `-D warnings`. `67fd0ac` fixes it by
asserting that the pre-persist failure left nothing at the number
(`matches!(s.getattr(want), Err(Error::NotFound))`), which is the claim the test exists for and uses
`want`. `rustfmt --edition 2021 --check` exit 0 after the edit.

The `linux-fuse` job of this run was still in progress at the last bounded read, so the full
three-job result of `1a49279` is not claimed.

A run exists for the fix head `67fd0ac` (`37528269132`, three jobs in progress at the last bounded
read). Its conclusion is not yet known, so the runtime result of the fix is UNVERIFIED.

Earlier runs on `01c8a2c`, `d7646eb`, and `890cd27` reported compile and lint failures, all of which
`976aaa4` addresses; the `1a49279` run then reached real test compilation and exposed this one
remaining `want` error, now fixed in `67fd0ac`.

## Commits on the branch

- `01c8a2c` (prior worker): reserved-ID create consumer with store- and session-bound tickets.
- `d7646eb`: the commit-error retry repair plus the dead-code removal.
- `890cd27`: gate the mark writer's trait import with its only user.
- `976aaa4`: fix the reservation-retry tests to compile (`matches!` for T12, assert the number in T14).
- `89d3a9d`: empty re-trigger commit, kept in history. Not repeated.
- `1a49279`: merge of `origin/main` (`e488a17`), resolving the single db.rs test conflict.
- `67fd0ac`: use the reserved number in the failed-commit retry test, fixing the real `unused variable: want` the `1a49279` CI run reported.
