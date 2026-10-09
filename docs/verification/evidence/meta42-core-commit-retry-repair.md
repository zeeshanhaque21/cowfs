# #42 request 4 (Core consumer): commit-error retry repair and dead-code removal

Status: source complete on draft PR #142, CI running on the pushing head.

This is the repair receipt for the commit-error retry gap in the reserved-inode consumer.
It is separate from the immutable implementation report:
`docs/verification/evidence/meta42-core-reserved-inode-consumer-implementation.md`.

## What was wrong

A metadata batch running under `Ack::Durable` removed each selected create's inode number from the
session's outstanding-reservation set at the moment the closure returned.
The removal happened before the commit that actually makes the edit durable.
When that commit failed, the caller got an error and retried, but the numbers were already gone from
the session, so the retry was refused with
`reserved number was not issued by this store's open session`.
The reservation that the caller legitimately held had been lost by a failure on the commit path.

Core itself opens Meta with `Ack::Applied`, so `wait_for` is `None` and this path was not exercised by
any existing test.
The bug is reachable only under `Ack::Durable`.

## The fix

The spent numbers now stay owned by the session until the create is durable.
On a `wait_durable` failure the spent numbers are re-inserted into the session's reserved set and the
error is returned to the caller.

The tree is deliberately not rolled back.
`commit()` on error is atomic (redb two-phase commit persists nothing), but the in-flight edit lives in
the snapshot tree, which concurrent `mutate` calls can interleave with while the writer lock is dropped
inside `wait_durable`.
A blind tree rollback could clobber a concurrent edit, so the pending create is left in the tree.
A retry at the same number is then stopped by the inode-exists check, not silently duplicated.

This satisfies both halves of the contract:

- retry ownership is retained until the commit succeeds, and
- an error after a real persist does not produce a duplicate, because the pending create still exists
  in the tree and the retry is refused as `Exists`.

## Regression proof

Two focused unit tests in `crates/cowfs-meta/src/db.rs` drive both outcomes through a private
`#[cfg(test)]` commit fault seam (`1` fails just before the redb commit, `2` just after):

- `a_failed_durable_commit_does_not_strand_the_reserved_number`
- `a_durable_commit_that_persisted_then_failed_does_not_duplicate`

A public-API end-to-end test in `crates/cowfs-core/tests/reserved_inode_identity.rs` exercises the
consumer reachable path: create, write, a flush that fails past the internal retry budget, then a retry
that succeeds, then a reopen, asserting the same inode number, the same durable meta identity, and the
same bytes:

- `a_create_that_failed_its_first_flush_keeps_its_number_and_bytes`

The fault seam is test-only and adds no production fault API.

## Dead-code removal on the same head

The same head failed `cargo clippy --workspace --all-targets -- -D warnings` with six dead-code errors
in `cowfs-core`, because the selected-ID path removed the lib-internal callers that had kept the
virtual allocator alive.

Removed: `alloc_virt`, `reserve_virt`, and the `Inner` fields `next_virt`, `virt_reserved`, `virt_lock`,
plus `VIRT_BLOCK`.

Kept under `#[cfg(test)]`: `virt`, `write_virt_mark`, `newest_copy`, and the `std::io::Write` import
that only `write_virt_mark` uses.
The read side (`read_virt_mark`) and the shape classification (`classify`, `snap_of`, `Aliases`) are
still live for stores written before meta owned the reservation, and the tests need a way to lay a mark
down.

A first push fixed the six dead-code items but left the `std::io::Write` import unconditional, so the
lib target had an unused import and clippy failed again on both Linux and macOS.
That was repaired in `890cd27` by gating the import with its only user.

## Compile fixes found by CI

CI did reach the pushed heads and found two more problems, both in my owned files.

`d7646eb` failed clippy on both platforms with `unused import: std::io::Write as _` at `ino.rs:4`: the
only writer of that trait, `write_virt_mark`, is behind `cfg(test)`, so the lib target had no user.
`890cd27` gates the import with its user.

CI on `890cd27` then failed to compile the `cowfs-meta` lib-test target with two errors in the new
tests:

- `E0369`: T12 compared a `Result<Attr, Error>` against `Err(Error::NotFound)`, but Meta's `Error` has
  no `PartialEq`. Replaced with `matches!`, which needs no equality.
- unused variable `want` in T14. The number is now asserted against the persisted inode's `.ino`,
  which is the point of the test: after a commit that persisted then reported an error, the inode
  exists once, at the reserved number.

`976aaa4` carries those fixes, and `89d3a9d` re-triggers CI on the same tree.

## Runtime and gates

Local `cargo build`/`cargo test` is blocked by a full-worktree capacity block, so the only local
verification performed is `rustfmt --edition 2021 --check` (exit 0) on every edited file.
No production behavior was executed locally.

The actual gate is CI on the pushed head, which builds and tests the same commit on Linux and macOS.

## CI infrastructure gap (open)

CI ran for `01c8a2c`, `d7646eb`, and `890cd27`, and produced real compile/lint failures that are now
fixed. It did not run for the two subsequent pushes.

After pushing `976aaa4` (the compile fixes) and a follow-up empty re-trigger commit `89d3a9d`, GitHub
created no `pull_request` run for either head SHA, and nothing was queued repo-wide at the time.
Verified as infrastructure, not configuration:

- `repos/zeeshanhaque21/cowfs/actions/runs?head_sha=976aaa4b6` and `...89d3a9db6` both return
  `total_count: 0`.
- The PR head on GitHub is the pushed SHA (`89d3a9db6`), so GitHub received the pushes.
- The workflow is `active`, `pull_request:` has no `paths` or `types` filter, and there is no
  `concurrency` key that would dedupe the run.
- Both SHA checks and pushes were repeated, with two bounded polls over roughly 22 minutes total, and
  no run appeared.

The branch tip under test is `89d3a9d` (source content identical to `976aaa4`). No local build was run
in place of CI, and no test was weakened to work around the missing run.

## Commits

- `01c8a2c` (prior worker): reserved-ID create consumer with store- and session-bound tickets.
- `d7646eb`: the commit-error retry repair plus the dead-code removal.
- `890cd27`: gate the mark writer's trait import with its only user.
- `976aaa4`: fix the reservation-retry tests to compile (`matches!` for T12, assert the number in T14).
- `89d3a9d`: empty re-trigger of the pull_request workflow.
