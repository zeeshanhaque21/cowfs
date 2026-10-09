# #40 follower-durability: deterministic correction: evidence

Lane: one of the five recorded #40 mutation controls (`follower acked before leader fsync`).
Subject: PR #146 `test(meta): port the follower-durable-ack harness for #40`.
Prior head `73f8bb2`; PR base `1580e69b`; local main `9874afa`.
This doc records the correction after the wbuddy review (`docs/reviews/pr146-follower-durability-port-wbuddy-review.md`).

## Actual fault (from CI logs, not theory)

Bounded read of the two failing CI runs (`--log-failed`) showed the ported test never compiled:
`cargo clippy --workspace --all-targets -- -D warnings` failed on
`crates/cowfs-meta/tests/follower_durability.rs`
- line 27: unused import `AtomicBool`;
- line 37: `field 0 is never read` on `Ev::S(usize)`.
So the two "fail" jobs were lint failures, not follower-test failures; the follower path never ran in CI.

## Deterministic boundary (replaces the racy discriminator)

The review's core objection was that `per-call applied = snap.root()` vs `durable_snapshots` is a narrow race, not an ordering proof, and can false-fail when a later writer advances the root.
Correction: the ordering kill now lives in `crates/cowfs-meta/src/db.rs` `#[cfg(test)] mod tests`, test `follower_wait_does_not_ack_before_the_leader_publishes_durable_seq`.
It exists there because the follower branch reads private `Inner` state (`gc: Mutex<bool>`, `gc_cv: Condvar`, `durable_seq: AtomicU64`), unreachable from the separate integration-test crate.

Construction (no new production API, no timing sleep as the pass condition):
- A real leader thread is held inside its public `before_sync` hook, so `*led == true` naturally and `durable_seq < target`.
- Entry is confirmed by channel, then the real private `inner.wait_durable(target)` runs on a second thread.
- Bounded `recv_timeout(150ms)` detects an early return (mutant acks now) vs blocking (real code waits for the leader's `notify_all`).
- The leader is released; both threads join; assert `durable_seq >= target`.
- Reopen from the file and require the acked create to survive, which is genuine durability, not a root compare.

The integration test's racy exact-root comparison was replaced with a monotonic presence check (`durable_has`: the acked create's name is present in the durable tree at return), which cannot false-fail from a later writer advancing the root.

## Expected mutant red: NOT EXECUTED

Canonical mutant recipe: at `wait_durable`, replace the `if *led {` arm body with `return Ok(());` and guard the original `gc_cv.wait_timeout` under `if false {`.
Effect: a follower acks before the leader publishes `durable_seq`.
Recipe for the auditor: apply that exact diff, then `cargo test -p cowfs-meta --lib follower_wait_does_not_ack_before_the_leader_publishes_durable_seq` (expect FAIL); the unmutated base must be green.
Mutation status: NOT EXECUTED here. READY5 is over the 8 GiB artifact cap (20 GiB floor), so no local cargo/build/test/mutation was run. No executed-kill claim is made.

## Current CI and remaining controls

Before this fix: 3 checks, 1 passed (`linux-fuse`), 2 failed (the two clippy jobs). New head CI is pending at push time.
Remaining #40 controls still open: reap-step-not-durable, magic-check-removed, snapshot-limit-removed, inode-limit-removed.
