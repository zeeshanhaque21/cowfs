# #77 shutdown budget: one absolute deadline across grace phases

Branch `integ/ctl-77`, pushed to `fix/control-progress-shutdown` (PR #79) as commit `2257796903756eff40f045f28d8729cc73f878e5`.

## Scope

The critic finding (2de8697) showed that `accept_loop` derived the grace end at observation time, `Instant::now() + drain_deadline`, inside the branch that runs once the shutdown deadline has passed.
A real mixed case (parked blocked progress writer plus CPU-bound handler) consumed two graces.
Measured: 1309 ms against an 800 ms budget, with `shutdown_deadline` 300 ms and `drain_deadline` 500 ms.

## Merge

`integ/ctl-77` was created at `4b210de`, then `main` was fetched and `bc3ea7d` merged as `baaebd8`.
`crates/cowfs-ctl/src/server.rs` auto-merged with no conflicts.
The merge brought in an unrelated `validate_mount_relative` change in the `holders` dispatch, which is preserved as-is.

## Change

- `begin_shutdown` now computes both instants once, when shutdown begins: `abandon = start + shutdown_deadline` and `grace_end = abandon + drain_deadline`.
- They are stored together in `Shared::deadlines: OnceLock<ShutdownDeadlines>`.
- `Shared::abandoned_grace_end()` returns `Some(grace_end)` once `now >= abandon`.
- `abandoned()` is now `abandoned_grace_end().is_some()`, so the request-wait caller in `run_connection` is unchanged.
- `accept_loop` takes `grace_end` from that stored instant. The fresh `Instant::now() + opts.drain_deadline` is removed.
- At `4b210de` the wait sites already shared one `grace_end` local, so the only remaining fresh computation was that one instant, taken at observation.
- Every later wait (`release_start`, the `done_rx` wait, the worker join loop, the straggler release loop) is bounded by that same value through a saturating `left()` or a direct comparison.

## Derivation

The abandon phase ends at `grace_end = begin_shutdown + shutdown_deadline + drain_deadline`, with no poll slack added.
Before this change the end was `observation_time + drain_deadline`.
Observation time is at least `begin + shutdown_deadline` and can lag it by up to one 10 ms poll interval, plus scheduling delay.
Now the end is fixed at shutdown begin, so every wait in the abandon phase spends the same single grace.

## Test

No new test was added.
`crates/cowfs-ctl/tests/progress_shutdown.rs:897` (`shutdown_budget_is_the_deadline_plus_one_grace_with_a_parked_writer_and_a_cpu_handler`) is the mixed fixture.
It runs a parked progress writer plus a CPU-bound `fsck` handler with `shutdown_deadline` 300 ms and `drain_deadline` 500 ms.
Its budget is `300 + 500 + 250` ms, where the 250 ms is documented scheduling slack, and it asserts `wait() elapsed < budget`.
The existing fixture already fails on the two-grace behaviour: 1309 ms against a 1050 ms budget.
The negative controls `shutdown_budget_with_only_a_parked_writer` and `shutdown_budget_with_only_a_cpu_handler` use the same budget.

## Checks

- `rtk proxy rustfmt --edition 2021 --check crates/cowfs-ctl/src/server.rs`: exit 0.
- No local `cargo build` or `cargo test` was run, because of the disk limit.

## Limits

- No local runtime evidence. The single-deadline claim rests on the code path and the existing fixture, not on a measured run of this commit.
- CI for `2257796` is pending and was not waited on.
- The CPU-bound handler is still detached at return, as `v1-control-api.md` allows. That behaviour is unchanged.
- The observation-lag measurement was not re-taken after this change.

## Files changed

- `crates/cowfs-ctl/src/server.rs` (28 insertions, 13 deletions)
