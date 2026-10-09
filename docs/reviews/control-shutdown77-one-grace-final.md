# PR #79 head `0f4cca9`: the one-grace budget is real, the front-half split is not free

Reviewer: independent critic, treehouse lease 4.
Head under review: `0f4cca97f3499e7aeaca02e4ccb59fb9607210e8`.
Base: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`, verified an ancestor of the head, not the PR-base label.
Previous head reviewed: `2de869761d70f0bea7206b5a437baa87584c3349` (three graces).
CI run read: `37244238927`, `head_sha` `0f4cca97...`, `completed` / `success`, all three jobs `success`
with per-job `head_sha` matching the head. One read-only API call, no dispatch, no rerun, no polling.

## Verdict

**BLOCK**, narrow and specific.

Two separable claims are in this PR and only one of them survives measurement.

1. The budget fix, one absolute `grace_end` instead of three per-wait graces: **PASS**.
   Independently reproduced old FAIL / new PASS with a real mutation gate, not a docs claim.
2. The front-half / back-half split of that one grace: **contract BLOCK**.
   It silently removes a guarantee that `docs/control-progress-shutdown.md:182` still promises in
   this same commit, and it forced a retune of `a4` rather than fixing anything.

Merge waits on #2. #1 alone is a clean, shippable change and does not need to be held back for it.

## Environment and discipline

Dev machine, macOS 26, load average 12.7 to 14.3 during the runs with sixteen other leases active.
Timings below are observed on a busy host and are not a quiet-machine performance claim; every
comparison that matters is a same-session A/B against the same build of the same probe.

Leased worktree `4` under `.treehouse-build-train/.treehouse/cowfs-7c1bf8/4/cowfs`, fast-forwarded
with `git merge --ff-only` to the exact head. No reset, no stash, no source or test edits, no commit,
no push, no merge, no lease return, no CI dispatch. Shared PID 15263, the shared store, mounts,
sockets, runners, devices and all other leases untouched.

Probe artefacts live in the ignored `bench/out/shutdown77-one-grace-critic/**`. Probe and scratch
build trees were removed at the end after verifying no stray processes remained.

Parked-ness is proven the way the committed tests do it: the flood step counter must be still across
two consecutive windows, which can only happen while a `write_all` has not returned. Nothing reads a
socket before `wait()` returns in any shape where a parked write is the thing under test, because a
read drains the buffer and unparks the write. Peer closure is measured by reading to EOF, never by
`set_read_timeout` succeeding. Buffered data is drained before judging a close state, since a closed
socket with queued bytes still returns those bytes rather than EOF.

## What the budget fix does, confirmed

`crates/cowfs-ctl/src/server.rs` now computes `grace_end` once at the deadline branch (line 278) and
reuses it for the worker-signal wait, the join and the release wait. One `Instant::now()`, one budget.
The branch is polled every 10 ms, so the instant is at most one poll interval after the real deadline.

Mutation gate, the strongest evidence in this review:

- Scratch archive of the exact head, only `server.rs` replaced with the `2de8697` version, byte
  verified `sha256 0556aaf8...`, everything else head, tests unmodified.
- `cargo test -p cowfs-ctl --test progress_shutdown -- shutdown_budget_is_the_deadline_plus_one_grace`
  **FAILS**: `wait() took 1.320351542s, past the deadline plus one grace (1.05s)`.
- Same test on the head source: **passes**.
- Independent probe, same shape and options, deadline 300 / drain 500:

| shape | old `2de8697` | head `0f4cca9` | budget |
|---|---|---|---|
| 1 parked writer + 1 CPU handler | 1310, 1312 ms | 805, 806 ms | 800 ms |
| 6 parked + 1 CPU | 1312, 1318 ms | 802, 805 ms | 800 ms |
| 1 parked writer only (control) | 896 ms | 692 ms | 800 ms |
| 1 CPU handler only (control) | 811 ms | 805 ms | 800 ms |

The mixed shape discriminates and the two controls do not, exactly as the committed test's comment
claims. Excess over budget on the head is 2 to 12 ms across every shape and every grace tried
(250, 500, 1000 ms). Peers see EOF after the return in every case on both heads. **This part is real
and it is correctly gated.**

Green on the head, all run locally:

- `cargo test -p cowfs-ctl --test progress_shutdown`: 12 passed, run 3 times.
- `cargo test -p cowfs-ctl --test admission`: 15 passed, run 4 times.
- `terminal_frame_survives_process_exit_after_wait`: `child_exit_frames=100/100`, run 3 times,
  72 s per run.
- `cargo fmt --all --check`: clean, zero diff lines. `cargo clippy -p cowfs-ctl --all-targets`: no
  warnings, no errors, real subprocess exit status.

One caveat on my own process, stated because it nearly became a false finding. Two scratch archives
reported `terminal_frame_survives_process_exit_after_wait` FAILED at line 516, which is
`assert!(child.exists(), "child fixture not built")`. The scratch archive had no
`target/debug/examples/progress_exit_child`. After `cargo build -p cowfs-ctl --example
progress_exit_child` in the same scratch tree it passed 100/100. That was a scratch artefact, not a
flake in the suite and not a finding against the PR.

## The contract block: the split removes a documented guarantee

The coordinator's question was whether a client resuming just past the new front-half split but
before the full grace end still gets a whole terminal frame. It does not. Measured, same probe, old
source against head.

The split is `let release_start = grace_end - opts.drain_deadline / 2;` at line 284, and
`kill()` runs at `release_start`. So the delivery window is `[deadline, release_start]`, that is
`deadline + 2/3 of drain_deadline`, while the old window was the whole `deadline + drain_deadline`.
The loss is not only at the cut, it starts earlier, because after the resume the parked write still
has to unwind, the handler has to return, and the worker then has to write the terminal frame. Every
millisecond of reserve taken from delivery is taken from that unwind-plus-write tail.

Finite parked handler, no ongoing flood, so the terminal frame is the only thing left to deliver,
deadline 300 / drain 500, `grace_end` 800, head cut 550, five reps each:

| resume after shutdown | old `2de8697` | head `0f4cca9` |
|---|---|---|
| 250 ms | `ShuttingDown` 5/5, frame at ~453 ms | `ShuttingDown` 5/5, frame at ~470 ms |
| 400 ms | `ShuttingDown` 5/5, frame at ~613 ms | `Partial` 5/5 |
| 500 ms | `ShuttingDown` 5/5, frame at ~698 ms | `Partial` 5/5 |
| 600 ms | `ShuttingDown` 5/5, frame at ~800 ms | `Partial` 5/5 |
| 700 ms | `Partial` 5/5 | `Partial` 5/5 |
| 780 ms | `Partial` 5/5 | `Partial` 5/5 |

At drain 1000 the same shape, `grace_end` 1300: old delivers whole at 700, 800, 900 ms and mostly at
1000 ms; the head is already `Partial` at 700 ms.

`a4`'s own geometry, deadline 200 / drain 250, so the head cut is 325 ms and the old window closed at
450 ms, five reps each:

| resume after shutdown | old `2de8697` | head `0f4cca9` |
|---|---|---|
| 280 ms | `ShuttingDown` 5/5 | `ShuttingDown` 5/5 |
| 300 ms | `ShuttingDown` 5/5 | `ShuttingDown` 5/5 |
| 320 ms | `ShuttingDown` 5/5 | `ShuttingDown` 5/5 |
| 340 ms | `ShuttingDown` 5/5 | `NoFrame` 5/5 |
| 360 ms | `ShuttingDown` 5/5 | `NoFrame` 5/5 |
| 400 ms | `ShuttingDown` 5/5 | `NoFrame` 5/5 |
| 440 ms | `ShuttingDown` 5/5 | `NoFrame` 5/5 |

Every one of those resume points is inside the old approved window. The head gives a client that
resumes at 340 ms nothing at all where the old code gave it a whole `shutting_down` frame.

### Why CI is green anyway

No committed test resumes past the cut and demands a whole frame. `a4` was retuned from
`drain_deadline` 250 to 500 in this commit, with the comment "With the 250 ms default the delivery
half would end before this client resumes, so the test declares a grace it can actually be served
inside". The table above is that exact retune, measured: at the original 250 ms geometry the resume
at 400 ms is past the new 325 ms cut, and the head returns `NoFrame` where the old source returned a
whole frame three times out of three. The retune is a consequence of the split, not the repair of a
pre-existing flake. `admission.rs:384-397` still describes the 250 ms grace in prose while the code
below it now uses 500 ms.

So the test change is what hides the contract change. That is the finding.

### The docs contradict themselves inside this commit

`docs/control-progress-shutdown.md:182-184` still says, in the head:

> **Inside the delivery grace** (`shutdown_deadline` plus `drain_deadline`), a client that reads
> gets its complete terminal frame, and `wait()` does not return before that frame is on the wire.

That is false on the head. The delivery window is `deadline + 2/3 of drain_deadline`. The correction
section added at lines 277-283 explains the split, and never revisits line 182. A reader of the
document is told the full grace delivers, and the code delivers two thirds of it.

`docs/v1-control-api.md:403-407` is untouched by this PR and is the actual Shutdown contract:
abandoned handlers "get `shutting_down`, their connections are closed and the server returns", and
"The handler threads are detached and die with the process." Nothing there promises a whole frame to
a client that resumes late, so this PR is not violating the v1 contract. It is violating its own
`control-progress-shutdown.md`, which is the document a maintainer will read.

### The split's stated justification did not reproduce

The PR body and the correction section both justify the split with: "Without the split the delivery
wait consumed the whole grace, the close never ran, and a parked connection was still open when
`wait()` returned (`connection_closed_at_return=false`)."

I could not reproduce a connection open at the return on the old source. Measuring the close state at
the instant `wait()` returned, using a non-blocking read that drains queued bytes first:

| shape | old `2de8697` | head `0f4cca9` |
|---|---|---|
| mixed, drain 250 / 500 / 1000, 6 reps | EOF in all 18 + 12 | EOF in all 18 + 12 |
| parked only, drain 250 / 500 / 1000, deadline 300 and 600, 6 reps | EOF in all 24 | EOF in all 24 |

And the `release_start = grace_end` mutant below closes at the return too, EOF in all 20 of its
reps. So the justification as written is not supported by what I measured. The close happens at the
return on the unsplit source; what the split actually protects is the parked handler being
unwound, which is a different claim and a weaker one, since the v1 contract detaches handler threads.
I report this as not reproduced rather than as false, because a shape I did not construct may still
exist. But it is the stated reason the delivery window was cut, and it did not survive a direct
attempt to reproduce it.

## Minimal fix

One line. `crates/cowfs-ctl/src/server.rs:284`:

```rust
-            let release_start = grace_end - opts.drain_deadline / 2;
+            let release_start = grace_end - opts.drain_deadline / 3;
```

Evidence for `/ 3` specifically, not a guess:

- The entire committed suite passes on it: `progress_shutdown` 12 passed and `admission` 15 passed,
  run three times green end to end, plus `child_exit_frames=100/100`.
- One-grace budget holds: mixed 804 ms against 800 ms, and 1 and 6 parked clients both close at the
  return with EOF.
- Delivery is restored well past the head's cut: whole `shutting_down` at 340, 360, 400 ms in the
  `a4` geometry where the head gives `NoFrame`, and at 400 and 500 ms in the finite geometry where the
  head gives `Partial`.
- Resume at 90 percent of a 1000 ms grace now yields a whole frame, 3 of 3, where the head yields
  `NoFrame` 3 of 3.

I checked the neighbours so the recommendation is not knife-edge. `/ 4`, `/ 5` and `/ 6` all fail
`shutdown_budget_with_only_a_parked_writer` on the handler-alive assertion, so the floor for that
assertion sits between one quarter and one third. `/ 3` is the smallest fraction that keeps the whole
committed suite green.

Full delivery, `release_start = grace_end`, is the other option and it is worse than it looks. It
restores delivery completely, whole frames out to 600 ms in the finite geometry and out to 1000 ms at
drain 1000, and keeps the budget and the close at the return, and passes
`child_exit_frames=100/100`. It fails two committed tests, `shutdown_budget_with_only_a_parked_writer`
and `abandoned_blocked_connection_is_released_before_wait_returns`, and it fails them on exactly one
assertion each: `assert!(!handler_alive_at_return)`, "the server still owns the abandoned handler
when wait() returns". That assertion is stricter than `docs/v1-control-api.md:405-407`, which detaches
handler threads and lets them die with the process. I am not recommending that direction, because
relaxing a committed assertion to widen a guarantee is the wrong trade to make unilaterally. It is
recorded here so the coordinator can see that the tension is real and where it sits.

Two doc lines must move with whichever fix lands:

- `docs/control-progress-shutdown.md:182-184`, to state the delivery window as the actual slice of
  the one grace and not `shutdown_deadline` plus `drain_deadline`.
- `docs/control-progress-shutdown.md:277-283` and the PR body's split paragraph, which currently sell
  the split as necessary for a close-at-return property I could not reproduce in the old source.
  Replace with what the reserve is actually for: letting a killed parked handler unwind before the
  return.

`admission.rs:384-397` should also stop describing the 250 ms grace in prose while the test uses 500.

## Test-quality notes, no action required

- Both `yield_now` spin loops are bounded by `grace_end`, so there is no unbounded spin. Measured CPU
  for a two-case probe run: 1.31 s user, 0.19 s sys over 2.24 s wall. The change from a 1 ms sleep to a
  yield trades CPU for latency inside a bounded window and does not loosen any assertion.
- Parked-ness in the committed tests is proven by two consecutive frozen windows and the assertions
  check real end states, `peer_closed` and handler-dropped, not that a flag was set.
- The process-wide fd gate was replaced by per-connection release proof and the counts are now
  reported rather than gated. That is a loosening and it is honest: the removed gate read 18 on the
  hosted runners against 6 locally and ended a cycle above its own baseline, so it was measuring the
  harness. The replacement assertion, peer read returning EOF or an error plus the handler being
  dropped, cannot be satisfied by an unrelated descriptor.
- No production hooks were added to make any of this testable. The spawn-failure seam at line 313 is
  the pre-existing `Err` arm; I read it and did not independently reproduce a spawn failure. It calls
  `best_effort_abandon(left(Instant::now(), release_start))` then `kill()`, so with the fix it
  inherits the single budget and is bounded. Read-only scope, not a finding.
- Half-close is real at the return: the `AT_RETURN` probe confirms EOF at the return in every shape and
  at every grace on the head, which is what `kill()`'s `shutdown(Shutdown::Write)` should produce.
- A CPU-bound handler with no socket still outlives `wait()` on the head, and that is correct and
  documented. The committed test asserts it is alive at the return so the case stays non-vacuous.
  Nothing here supports any universal daemon-shutdown figure, and the 120 s `GC_STOP_PATIENCE` in
  `crates/cowfs-daemon/src/backend.rs:180` belongs to the daemon's own close path, which I read only.

## Scope I did not cover

I did not run a real daemon against a live Core. The daemon and Core shutdown ordering was read, not
executed: `Daemon::run` waits then shuts down, `CoreBackend::close` raises cancel and waits
`GC_STOP_PATIENCE` for a running gc before taking the `Core`. No claim here depends on it.

## Artifacts

- `bench/out/shutdown77-one-grace-critic/probe/src/main.rs`: modes `mixed`, `controls`, `scale`,
  `a4params`, `a4retune`, `finite`, `finite1k`, `resume`, `atreturn`, `atreturn_parked`, `healthy`.
- `bench/out/shutdown77-one-grace-critic/old-ctl/`: `2de8697` source, `server.rs` byte verified.
- `bench/out/shutdown77-one-grace-critic/fixd3/`: the recommended `/ 3` mutant.
- `bench/out/shutdown77-one-grace-critic/fixb/`: the full-delivery mutant, recorded for the
  coordinator's decision.
- Seven earlier reports in the lease-4 worktree preserved untouched:
  `gc-space-accounting-final.md`, `gc-space-accounting-repair-final.md`,
  `gc-space-accounting-wire-final.md`, `gc-post-unlink-accounting.md`,
  `gc-space-unlink-final.md`, `control-shutdown77-bounded-final.md`,
  `control-shutdown77-budget-ordering.md`.