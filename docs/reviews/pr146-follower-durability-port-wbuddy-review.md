# PR146 follower-durability port review (wbuddy)

PR #146 `test(meta): port the follower-durable-ack harness for #40`. Head `73f8bb2dc0caf92fef84dc84c9d139dce3361e95`, PR base `1580e69b`, actual local HEAD `9874afa`.
Source-only: `crates/cowfs-meta/tests/follower_durability.rs` (283 lines, blob `bb484d17`). No local cargo/build/test/mutation (READY5 cap). CI is the only execution evidence.

## Source failures first

1. **No executed proof of the mutant kill.** Body asserts the port kills `follower-acked-early`; nothing ran it. `mutate.log:5` records SURVIVED-builder; kill was lane-local only (`report.md:26`, `fol.log` B=407/D=602). Porting is not executing. Gate unproven until the mutant is applied to this test and observed FAIL. Reject source-predicted kill.
2. **Concurrency test may not kill the mutant even if it compiles.** Discriminator compares per-call `applied = snap.root()` vs `durable_snapshots`. To fire on a follower, the follower's read must land after its `create` applies but before the leader's commit publishes `SNAPSHOTS`. Narrow race, not deterministic. With `GROUP_WAIT` coalescing the read likely sees the committed root and passes.
3. **Shared-commit gate (`calls < threads*per`) is not follower-path evidence.** Fewer hook calls happens whenever any coalescing occurs, including zero followers (each call its own leader). Never proves a caller saw a leader already running.
4. **Follower branch never forced; admitted in the docstring.** Test relies on incidental timing. With (3), "not exercised" is indistinguishable from passing.

## Root assertion / discriminator / topology+t timing

- **No ordering detection.** `snapshots()` (`db.rs:1435`) includes applied-not-yet-durable; `durable_snapshots` (`:1443`) is last durable commit. Root equality is a durability-and-race check, not a sequence proof; an earlier step acked early can still yield equal roots at read time.
- **Same-root-different-transition invisible.** A later concurrent commit can also advance `durable_snapshots` past the caller's applied root, giving a **false fail on correct code**. False green and false red are both reachable.
- **Leader half weak.** `single_durable_ack_follows_the_hook` only pins the leader side; it cannot reach the follower branch.
- Physical backend `Be` correctly models "only post-fsync writes survive" (synthetic `synced_image`), but the yield lives in the meta hook, not the store, so applied-vs-durable byte ordering is only as faithful as the model. Crash drain `total == threads*per` is a whole-run reopen invariant, not per-call.
- Waits are bounded: `wait_timeout(50ms)` + `GROUP_WAIT` spin cap; no sleeps-based negative pass; no deadlock observed.

## Mutation gate (the actual runtime, separate from the source claims)

- **REQUIRED: apply `follower-acked-early` (diff: `if *led { return Ok(()); }` replacing the `gc_cv.wait_timeout` follower wait) to this ported test and record FAIL.** Also record base unmutated green. Until then the named mutant is still only lane-local-killed and this PR closes nothing.
- No new global lock or timing threshold demanded; keep the per-call assertion if it can be made deterministic, otherwise the red control is the evidence.

## Whole-#40 remaining four controls

- follower acked before leader fsync: this PR, not closed (no executed kill).
- reap step not durable: MISSING, no mutant run vs `critic.rs:357`.
- magic check removed: MISSING, no old-fail/new-pass log vs `review.rs:741`.
- snapshot limit removed: MISSING, no `SNAPSHOT_LIMIT` -> `LimitExceeded` test or mutant.
- inode limit removed: MISSING, no `Tx::alloc` -> `INO_LIMIT` test or mutant. All five controls open.

## CI (bounded, no dispatch/poll/rerun/commit)

`gh-axi pr checks 146`: `check (ubuntu-latest)` fail, `check (macos-latest)` fail, `linux-fuse` pending (0 passed, 2 failed, 3 total).
The two completed named jobs FAILED; logs unavailable (run in_progress, `--log-failed` refused). No named follower-test log exists. Verdict: **FAILED as named, specific follower tests PENDING, no executed pass.** Retrieve a failing compile/test log before any green claim.

## Provenance verification

- `crit2.rs` PRESENT; `b_durable_group_commit_4_threads` / `d_durable_background_8_threads` PRESENT.
- Mutant PRESENT; exact diff is `if *led { return Ok(()); } if false {` guarding the original `wait_timeout`, matching the body. `report.md:26/105`, `mutate.log:5`, `mut/fol.log` confirm SURVIVED-builder / KILLED-lane-local. **Claims TRUE.**
- Ported file at head byte-identical to PR diff blob (283 lines), verified in leased slot `cowfs-7c1bf8/5` @ `73f8bb2`.

## Return

- **Failures first:** no executed mutant kill; root compare is race/durability not ordering; shared-commit gate not follower evidence; follower branch never forced.
- **Exact pins:** head `73f8bb2`, base `1580e69b`, local main `9874afa`; test blob `bb484d17`; mutant `mut/fol/crates/cowfs-meta/src/db.rs`.
- **Actual test proof:** PENDING (no logs; named CI FAILED).
- **Mutation gate:** NOT satisfied (source-predicted only).
- Whole #40: all five controls open.
