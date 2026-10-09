# #42 request 4: alias-contract regression and pending-retry correction

Status: source fix pushed on draft PR #142, head `b588a0e94bd1165c22edcb4d3062765e1eb723e3`. CI run queued, result PENDING.

This receipt covers the two blockers the independent review
`docs/reviews/pr142-integrated-core-retry-wbuddy-review.md` found on head `67fd0ace`.
It does not restate the merge or the retry repair, which are in
`docs/verification/evidence/meta42-core-main-integration-conflict-resolution.md` and
`docs/verification/evidence/meta42-core-commit-retry-repair.md`.
Those receipts and the review are unchanged.

## What the review found, and it was right

Two defects on `67fd0ace`, both real:

1. A runtime regression in `crates/cowfs-core/tests/alias.rs`, an unchanged test file. CI run
   `37528269132` completed `failure`, with both `check` jobs failing on the same two tests:
   `a_create_past_the_alias_ceiling_is_refused` (`alias.rs:202`) and
   `a_session_alias_costs_a_bounded_number_of_bytes_per_inode` (`alias.rs:69`), the second reporting
   `Stats { ..., aliases: 0, ... }` where the contract requires `n + 1` (`right: 20001`).
   The same `alias.rs` blob (`38c37627b57de3400f76fd7db05a4a0c771e0b4d`) is on main `e488a17`, whose
   run `37521445526` passed `cargo test --workspace`. So the test is not stale: the PR's production
   change broke a green suite.
2. A source defect in the T13 assertion I added in `67fd0ac`:
   `matches!(s.getattr(want), Err(Error::NotFound))` contradicts T13's own design comment, because a
   failed durable commit does not reset the session tree, so the pending create's inode still
   resolves. The review correctly labelled this a source conclusion, not a runtime observation: the
   `alias` binary failed before `cowfs-meta` ran.

I had earlier reported "only CI remains". That was false, and the review is correct to say so.
Both recorded blockers are fixed here.

## Root cause of the alias regression

Source-level, with a same-input two-ways comparator:

- Main `e488a17` and feature `67fd0ace` carry the identical `alias.rs` blob. Main is green, feature
  fails. That is a controlled experiment: the only variable is the PR's production change.
- The seam is `commit_batch` in `crates/cowfs-core/src/inner.rs`. Before reservations, every create
  used a virtual number and `commit_batch` inserted one alias per committed create:
  `for (v, m) in &created { al.insert(*v, sc.id, *m); }`.
- Head `67fd0ace` guarded that insert with `if matches!(classify(*v), Id::Virt { .. })`. A reserved
  create's child is the packed meta number (`Id::Meta`), so the guard skipped it and the alias table
  stayed empty. That is exactly the `aliases: 0` the test reported.
- The guard's comment claimed a reserved alias would be a "self-entry". It is not: the alias key is
  the packed visible number and the value is the bare meta number, so `meta_of` and `canon` both
  resolve through it correctly.
- The old ceiling enforcement lived in `alloc_virt` (`inner.rs`), which the reservation change
  removed. No replacement was added, so the session inode limit was never enforced either.

This is the third change in this area after earlier fixes, so the seam was identified by comparison
rather than a further theory: base loop versus head loop, main run versus feature run.

## The fix

`crates/cowfs-core/src/inner.rs`, `commit_batch`: alias every committed create again, restoring the
base loop. One alias per live inode, so the table tracks the live count and `canon`/`meta_of` resolve
a reserved number to itself.

`crates/cowfs-core/src/ns.rs`, `make`: refuse at the session ceiling before reserving, with the same
message the contract checks (`"session inode limit reached: {live} inodes are live, the ceiling is {}"`),
and store it in `last_error` so `last_flush_error` reports it. `make` is the only create path
(`create`/`mkdir`/`symlink` all route through it), so the check covers every create.

`crates/cowfs-meta/src/db.rs`, T13: corrected to the true observable. A failed durable commit leaves
the create pending in the session tree, so T13 now asserts the pending inode exists at the reserved
number with the created kind, that a different-name retry is refused `Exists` by that inode, and that
a fresh reopen of the store holds no inode at the number because the failure was before redb
persistence. The correct contrast is T14, where the failure was after persistence and the reopened
store still holds the inode.

No test was weakened, no assertion silenced, and `want` is used, not underscored.

## What is preserved

- Strong identity: a created file still holds a real meta-backed inode number, not a virtual alias.
  The alias table is used as the identity bridge, exactly as `canon`/`meta_of` expect, not to restore
  virtual IDs.
- No production rollback of the pending tree to satisfy a test.
- The hidden authority and retry contract is untouched: store- and session-bound tickets, one-use
  retries, the `Op` queue drain/restore of the ticket capability, no clone/copy of the range, no raw
  inode authorization, no per-ID durable commit, no small request cap.

## Verification done and not done

Done:

- Source-level root cause with a main-versus-feature comparator on the identical `alias.rs` blob.
- `rustfmt --edition 2021 --check` exit 0 on `inner.rs`, `ns.rs`, and `db.rs`.
- Brace balance on all three edited files.
- Confirmed `make` is the single create path; confirmed `maybe_evict_node` keeps the alias while
  `try_reclaim` removes it on the last unlink, which is the free-then-reallocate the ceiling test
  exercises.

Not done:

- No local `cargo build` or `cargo test`: READY3 is at 7.6 Gi with no measured headroom under the
  8 GiB cap / 20 GiB floor, so a local heavy run is forbidden.
- CI result for the fix head is PENDING. Run `37532881115` is the latest; the earlier run on
  `b588a0e` (`37530284116`) completed `failure` on a compile error, see below.
- No runtime observation of T13: it is a source correction, and the whole `cowfs-meta` suite plus
  `reserved_inode_identity.rs` have no green run on this head.

## CI on the first fix head b588a0e

Run `37530284116` completed `failure`: both `check` jobs failed to compile `cowfs-meta` (lib test)
with one error:

```
error[E0599]: no method named `getattr` found for struct `db::Meta` in the current scope
  --> crates/cowfs-meta/src/db.rs:2512:28
```

`Meta` has no `getattr`; the method is on `Snapshot`. T13's reopen check called it on the reopened
`Meta`, so the meta test target never compiled and masked the rest of the run, including whether the
alias fix works. `6a0515e` reads through the reopened snapshot by id
(`again.snapshot_by_id(SnapshotId(1))`), which is how the other durable reopen tests read a store.

## Remaining gates

- CI on `b588a0e`: the whole-workspace `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` (`alias.rs`, the `cowfs-meta` reservation/retry suite, and the Core
  `reserved_inode_identity.rs` identity tests), on ubuntu and macos, and the `linux-fuse` job.
- The F7 bounded alias table and session inode limit gate, which this fix is intended to restore, is
  verified only when `alias.rs` passes on CI.
- Whole-#42 acceptance is not claimed by this receipt.

## Head

- `6a0515e6460b9211d8cbf51164b8a1b8ea6960bc`, branch `fix/core-reserved-inode-consumer-42`,
  pushed over HTTPS.
- Parents: `b588a0e`.
- Changed paths in the two commits: `crates/cowfs-core/src/inner.rs`, `crates/cowfs-core/src/ns.rs`,
  `crates/cowfs-meta/src/db.rs`.
