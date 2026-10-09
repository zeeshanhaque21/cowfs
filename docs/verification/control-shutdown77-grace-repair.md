# #77 control shutdown: one absolute grace, full delivery window

Owner: builder, treehouse lease 15 (mounted worktree).
Branch: `fix/control-progress-shutdown`. PR #79, draft.
Reviewed head this supersedes: `0f4cca97f3499e7aeaca02e4ccb59fb9607210e8` (front-half split, BLOCKed).

## What the review found, and what changed

The one-grace budget fix survived review. The front-half split did not.

`crates/cowfs-ctl/src/server.rs` had three waits after the shutdown deadline: the abandon workers'
completion signals, joining those workers, and waiting for every straggler connection to be released.
Three graces at `2de8697`, because each wait computed its own `Instant::now() + drain_deadline`.

`0f4cca9` replaced those with one `grace_end`, which is the correct shape and is kept. It then spent
the back half of that grace on the close, with `kill` at the halfway mark. Measured consequence: a
client resuming at 340 or 400 ms in `a4`'s own 200/250 ms geometry got `NoFrame` where the three-grace
code gave a whole `shutting_down` frame.

`release_start` is now `grace_end`. The whole `shutdown_deadline + drain_deadline` is the delivery
window. A client that resumes anywhere inside it still gets its frame, because after the resume the
parked write unwinds, the handler returns and the terminal frame is written within that same window.

## The two withdrawn claims

Recorded because they were stated as measured facts and neither survived.

1. **"Without the split the close never ran and a parked connection was still open at the return
   (`connection_closed_at_return=false`)."** Not reproduced. Across mixed and parked-only shapes, three
   grace values and two deadlines, read EOF was observed at the return in every case on the unsplit
   source. Reported as an original observation that did not reproduce, therefore withdrawn as a
   justification. It remains an unproven claim about a shape nobody constructed.

2. **"The reserve is for the close."** Wrong even where the close is real. A reserve buys letting a
   killed parked handler unwind before the return. That is weaker than the contract:
   `docs/v1-control-api.md` detaches handler threads and lets them die with the process. The reserve
   is removed and that case is handed back to the contract instead of being paid for out of the
   delivery window.

## Two internal assertions corrected, with the reason

`abandoned_blocked_connection_is_released_before_wait_returns` and
`shutdown_budget_with_only_a_parked_writer` asserted `!handler_alive_at_return`. That is stricter than
the Shutdown contract and, with the grace spent on delivery, is not something the server can promise:
`kill` half-closes the socket and the parked `write_all` unwinds on its own schedule afterwards.

They now gate what the contract requires, that the connection is closed at a `wait()` inside the
budget, and assert separately that the detached handler does end. That second assertion is this test
loop's own thread budget, not a promise to any caller: without it the loop would accumulate a live
thread per case. `handler_alive_at_return` is still reported in every run as the detached-handler
observation it is, and is `true` on the parked shapes.

User requirements that outrank the old assertion, and are all still asserted: a whole terminal frame
inside the full window, and the connection closed before `wait()` returns.

## Test-only CI repair on top of this

A hosted run on `da9870f` was red on the new regression itself, in the fixture rather than the claim.
`crates/cowfs-ctl/src/server.rs` did not change: sha256
`5eb3f6bcd092ec2a9fd69a3898b06060f5bd4fc8296bb4b0608afc3aa4a71664`, byte-identical.

Linux: `assert_parked` failed with the step counter advancing 37 to 50, so the fixture never parked
and the assertion about the delivery window was never reached. The fixture emitted 400 events of 2000
bytes, 781 KiB, which is not reliably more than an AF_UNIX send buffer plus the peer's receive buffer.
That the buffer absorbed it is inference from the counter and the defaults, not a measurement. Now one
mebibyte per event, the pattern every other parking fixture here uses, and two events so it still parks
if a runner absorbs the first.

Two events and not eight. Eight left up to 7 MiB for the client to drain after it resumes, charged
against the 110 ms the 450 ms geometry allows, and failed locally at `elapsed_ms=456` with no whole
terminal frame. That was found by running the suite, not by argument.

macOS: one rep in six missed `whole_terminal` at `elapsed_ms=547` against a 450 ms window. The
200/250 geometry resuming at 400 ms left 50 ms and is no longer a committed point. 340 ms in that
geometry still sits past the 325 ms cut the half-split head used, with more than twice the room.
400 to 449 ms there stays a hand probe, reported in the review evidence, not a hosted gate.

Case labels now carry the window-closing time and the resume, both from the start of shutdown, because
900 ms is 90 percent of a 1000 ms grace but 75 percent of the 1200 ms window.

A third `handler_alive_at_return` gate, in the mixed test, was corrected as well. Two were corrected in
the previous round and this one was missed. It is the same assertion in the same class: with the whole
grace spent on delivery, `parked_handler_alive_at_return=true` is the normal state, so it asserted a
scheduler rather than a contract. The scope of the correction is therefore three assertions, not the
two previously authorised. `assert_parked` is unchanged.

### An unrelated flake, reported without a verdict

One full-suite run on this head failed `an_unblocked_client_still_receives_a_terminal_frame_at_shutdown`
with `terminal_present=false` at 332 ms. That test was not modified in this round and the production
blob is identical in both heads. It passes 8 of 8 in isolation on this head and 8 of 8 on the previous
head `da9870f`. Two of three full-suite runs on this head and one of one on the previous head passed.
The cause is not isolated: the test's margin depends on a client draining 1 MiB frames faster than the
flood fills the socket, which is load-sensitive. It is reported as an open flake, not as fixed.

## Committed regression for the contract block

`a_client_resuming_late_inside_the_full_grace_gets_a_whole_frame`. A finite handler, so the terminal
frame is the only thing left to deliver and a late arrival cannot be a progress frame.

| geometry | resume | result |
|---|---|---|
| 200/250 ms, window closes 450 | 340 ms | whole frame, 5/5, 110 ms room |
| 200/1000 ms, window closes 1200 | 900 ms | whole frame, 5/5, 300 ms room |
| 200/1000 ms, window closes 1200 | 950 ms | whole frame, 5/5, 250 ms room |

15 of 15 whole frames on this source, every case also confirming the peer closed. Parked-ness is the
same two consecutive frozen windows the other tests use, and no socket is read before `wait()`
returns, because a read drains the buffer and unparks the write under test.

Same test file, same binary, against the split head `0f4cca9` (`server.rs` sha256
`25f41e561e2e7750d763c6c90a6f7e5e478864930f8fabfbab0209c9d14c1c2c`, byte verified): the first geometry
fails outright with `whole_terminal=false` at 340 ms.

`a4` is restored to the 250 ms default grace. Widening it to 500 ms is what hid this.

## Budget, mixed shape, old against new

`shutdown_budget_is_the_deadline_plus_one_grace_with_a_parked_writer_and_a_cpu_handler`, deadline 300
ms, drain 500 ms, budget 800 ms plus 250 ms of scheduling allowance.

| shape | three graces `2de8697` | this head | budget |
|---|---|---|---|
| 1 parked writer + 1 CPU handler | 1312 ms, FAIL | 810 ms, PASS | 1050 ms |
| 1 parked writer only | 882 ms | 810 ms | 1050 ms |
| 1 CPU handler only | 813 ms | 810 ms | 1050 ms |

Only the mixed shape discriminates; both single-connection controls pass even against the inflated
budget, which is why only the mixed case is the regression. `2de7697` was extracted with `git archive`
and its `server.rs` sha256 verified against `git show`.

## Detached handler: actual states and backend lifetime

The parked flood handler and the CPU-bound handler both outlive `wait()` on this head:
`handler_alive_at_return=true`, `parked_handler_alive_at_return=false` only when the mixed case gets
lucky enough for the parked write to unwind inside the grace. That is the contract's detached-thread
behaviour, recorded rather than asserted away.

Safety of that is checked, not assumed. In the mixed case the detached worker is still running after
`wait()` returns, still holding the handler, which holds an `Arc` the server also owns. Its counter is
sampled twice after the return and must keep advancing, so the return cannot have freed or borrowed
past anything the worker needs. Measured `cpu_ticks 9142 -> 9177` while detached. Its peer is
confirmed closed at the same time, which is why the handler is detached rather than joined.

No claim is made that every handler is joined or deallocated, and no universal daemon-shutdown figure
is claimed. The 120 s `GC_STOP_PATIENCE` in the daemon backend belongs to the daemon's own close path.

## Half-close and the past-bound boundary

- Within the grace: `a4` within-grace `ending=Response`, a whole frame.
- Past the grace: `ending=NoFrame`, `wait_ms` bounded, no fabricated or duplicated terminal. A peer
  still unreadable past the bound may see a truncated frame then EOF, and handlers may detach.
- `m1_half_close_means_no_more_requests_not_cancel` and
  `a5_a_blocked_terminal_write_does_not_stall_admission`: 20 of 20 rounds each.
- `terminal_frame_survives_process_exit_after_wait`: 100 of 100 frames after `wait()`, so `cowfs
  serve` exiting at once still cannot drop a readable client's frame.

## Spawn-failure seam

Read, not independently reproduced, and no production hook was added for it. It is the pre-existing
`Err` arm at the worker spawn; with the fix it calls `best_effort_abandon(left(now, release_start))`
then `kill()`, so it inherits the single budget and is bounded. The existing narrow tests for it were
kept as they are. Scope is honestly one representative injected-failure run by me in the previous
round; no independent reviewer has reproduced a spawn failure.

## Verification

109 `cowfs-ctl` tests, suite exit 0 (10 unit, 15 admission, 1 flood, 13 progress_shutdown, 19 regress,
31 server, 20 wire). Admission 15 of 15. Child-exit frames 100 of 100. `cargo fmt -p cowfs-ctl --
--check` exit 0 and `cargo clippy -p cowfs-ctl --all-targets -- -D warnings` exit 0, each read from the
command's own exit status rather than a pipeline's.

Timings are single-machine macOS measurements on a loaded host, not performance results. No local Linux
run; hosted CI covers Linux and macOS. Hosted CI result for this head is reported in the PR, not
asserted here.

Independent re-review is required before merge. This PR is a draft and is not merged.

## Canonical documents

- `docs/control-progress-shutdown.md` in the main repository's primary checkout is the
  user-visible source of truth for the evidence and semantics.
- The leased worktree carries the same file for the branch commit; the two are byte-identical.
- The reviewer's report, `docs/reviews/control-shutdown77-one-grace-final.md`, was read only.