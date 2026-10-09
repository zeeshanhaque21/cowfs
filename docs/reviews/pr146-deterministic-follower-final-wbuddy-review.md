# PR146 deterministic follower-ordering final review (wbuddy)

Readonly review. e915330 is the corrected head; it fixes the lint failure that killed the previous head 73f8bb2.
CI on the corrected head is IN PROGRESS, so runtime is PENDING, and the mutant has still NOT been executed on this head. Runtime and mutation are separate gates; neither is closed.

## Exact pins (verified, not assumed) and source verdict

- PR146 head (remote, live): e91533077baca6f8963d93594f94dbcb87a8a184 (refs/pull/146/head). Earlier review's pin 73f8bb2 is now the parent, not head.
- merge-base with remote main: 1580e69b9d987f63c07b2430f8c0b4547ecd8622. Remote main head 01fa855fc3521c519e8a93fe0b867dc56ebfdc5d is NOT an ancestor of the PR head.
- Ancestry for the tested tree: main only added PR144 tests/receipts since the base. merge-tree(base, e915330, 01fa855) is clean, no conflict markers, so the PR tip is not guaranteed to include main's PR144 additions and must be merged, not assumed tested as-is.
- Head touches only crates/cowfs-meta/src/db.rs (+119, cfg(test) mod tests) and crates/cowfs-meta/tests/follower_durability.rs. No production code path changed: the addition is test-only, so production semantics are unchanged.

Source verdict: the deterministic construction is genuinely better than the racy root-equality discriminator and is REAL, not source-predicted-only in design. Confirmed at head:

- Production follower branch exists exactly as the mutant targets it. db.rs:994 `wait_durable`; :1000 `if *led { let _ = self.gc_cv.wait_timeout(led, 50ms); continue; }`; :1004 `*led = true`. Privacy is real: griffe fields `durable_seq: AtomicU64`, `gc: Mutex<bool>`, `gc_cv: Condvar` are private to Inner (:272-274), unreachable from the separate integration-test crate. So the branch must be tested in-crate, as claimed.
- The test FORCES the follower branch before leader release. Leader thread calls `snapshot("s").create(...)`; the `before_sync` hook sends `entered_tx` and blocks on a bounded `recv_timeout(30s)` (:2582-2590, :2608-2610). Only after entry confirmation is `target = durable_seq + 1` read, then the follower asserts `durable_seq < target` (:2622) and runs the real private `inner.wait_durable(target)`. Leader holds the only wake; `gc_cv.notify_all` only fires at publish (:737). So entry into the `*led == true` arm is genuinely observed, not incidental.
- Negative is failure-aware and not a mere spawn. `done_rx.recv_timeout(150ms)` fires only on an EARLY return; a spurious early return panics with the observed seq (:2641-2646). A real follower cannot return while the leader is held. No sleep is the pass condition.
- Bounded cleanup on both waits (30s entry, 30s release hook, 150ms early-return probe), so a failure cannot hang the suite.
- The exact mutant recipe is present and coherent. Mutant = replace the `if *led` arm body with `return Ok(());` (guard the original `gc_cv.wait_timeout` under `if false`). Applied, the follower returns instantly, `recv_timeout` fires, and the assert FAILS. This is the right red control for this test at this boundary.

## Scheduled follower / integration root compare

- Integration test (crates/cowfs-meta/tests/follower_durability.rs) replaced the racy exact-root compare with `durable_has` presence (:162-170, :214): the acked create's own snapshot NAME must be present in the durable tree at return. Names are disjoint per thread (`s{t}`, file `t{t}-{i}`), so a later writer advancing the root cannot false-fail it. This removes the false-fail/false-green shape the earlier review flagged.
- Named tests to run (must come back green on head, unmutated): `follower_wait_does_not_ack_before_the_leader_publishes_durable_seq` (db.rs:2572, the authoritative kill) and the integration `follower_durability` (concurrent durable callers + crash image).
- `shared-commit gate` is NOT follower evidence, correctly retained only as a liveness check, not as the proof. The integration asserts `calls < threads*per` (:238-243); that can hold whenever any coalescing occurs, including with zero true followers, so it proves "commits were shared", not "a caller saw a running leader". Do not present it as the follower-path proof. Crate-internal test is the ordering proof; integration is the end-to-end port.
- Crash image valid: `Be::from_image(be.synced_image())` reopens and `total == threads*per` (:245-263) requires every acked create to survive. So the integration half does carry a real durability check.
- Leader half `single_durable_ack_follows_the_hook` (:269) pins the non-shared case.

## Mutation status

- NOT EXECUTED on this port or on this head. The docstring proves design intent and the recipe is exact, but no FAIL-from-mutant or green-from-unmutated run has been recorded on e915330. Absent that, no executed-mutation claim is made here.
- The earlier "SURVIVED" record (`mutate.log`) is for the critic's lane, not this port. Porting is not executing.

## CI (bounded read, no dispatch/poll/rerun)

- Corrected head e915330 -> run 37540375872: IN PROGRESS (3 jobs: macos check, ubuntu check, linux-fuse). No conclusion yet.
- Prior head 73f8bb2 runs 37538589292 and 37538452455: FAILURE, clippy `-D warnings`: `unused import AtomicBool` (tests/follower_durability.rs:27) and `field S(usize) is never read` (:37). The e915330 commit message and diff remove exactly those two, so the corrected head is intended to clear that failure; runtime confirmation still PENDING.

## Return

- Failures first: no run of the named tests on e915330 (CI in progress); mutant NOT EXECUTED on this head; tested-merge with the new main is a clean merge-tree, not a verified checkout tree, so the PR tip does not contain PR144's additions.
- Sources pin exactly, corrected head e915330, merge-base 1580e69b, remote main 01fa855, PR146 head not containing main.
- HOLD 146 until BOTH (a) named-test green on e915330, and (b) executed mutant red on e915330 with unmutated green. Source acceptance, runtime, and mutation are separate.

Remaining four #40 controls: not addressed here, still open. This review fixes scope to the follower-durability ordering kill only; no whole-#40 claim.
Bounded at 60 lines.
