# cowfs-ctl issues status, 2026-10-09

Scope: #121, #127, #128, #166, all open, all referenced by PR 79 (merged as 150179e).
Sources: issue bodies and comments, docs/reviews/79-critic-20261009.md, docs/reviews/pr79-spike-20261008.md, server.rs and tests on origin/main ce403b8.
Machine: this Mac, idle-ish, one cargo process, -j2, debug build.

## Status table

| Issue | Status | Evidence | Gap |
|---|---|---|---|
| #166 a4 margin | Fixed in code, owner decision on the structure | 7b52f97 and c97ea82. The 250 ms case is always asserted (200 ms of slack), the 400 ms case only when measured early. | The issue asked to derive the resume from the server's observed close. PR 79 measures the actual resume time and discards late attempts instead. Hosted CI runs without --nocapture, so it is not visible whether the 400 ms case asserts there. |
| #128 wait vs bound | Likely fixed, one sample missing | Production: 2257796 (one absolute grace_end). Test: dfee8b4 (client reads raw bytes in the window, parses after). Budget tests: shutdown_budget_*, staggered_stalled_writers_stay_within_the_deadline_plus_grace. | The 2322 ms vs 1800 ms case B was measured on server blob 5eb3f6bc, before 2257796. The slow-but-reading client geometry (64 KiB per 100 ms, 400 ms park) was not re-run on main. Existing budget tests use parked-writer and CPU-handler geometry, which the issue says cannot refute it. |
| #121 terminal frame missed | Open, not reproduced, mechanism fixed by PR 194 | 30 of 30 pass on main. Deterministic unit failure of the kill-vs-finishing race (below). | The original log is gone. The race is a plausible fit for bare EOF at 332 ms after a 300 ms deadline, but it is not proven to be that event. |
| #127 kill racing a terminal write | Fixed and closed by PR 194 (c3c6fbd) | Unit test failed on main: teardown killed the connection with one terminal write in flight. | None. The test `teardown_does_not_cut_a_terminal_write_that_is_in_flight` is the regression guard. |

## Loops

#121: `cargo test -p cowfs-ctl --test progress_shutdown -- --exact an_unblocked_client_still_receives_a_terminal_frame_at_shutdown`, N=30, 30 pass, 0 fail.
This is the test as reworked by dfee8b4 (fast raw drain), so it is weak evidence about the original failure at 4b210de, which used the slow parsing client.
The 73 reviewer repetitions and the spike case runs quoted in the issue also never reproduced the original event.
#166: `cargo test -p cowfs-ctl --test admission -- --exact a4_a_terminal_frame_is_delivered_to_a_client_that_resumes_within_the_grace`, N=30, 30 pass, 0 fail.
In a further 10 runs with --nocapture the 250 ms case resumed at 254 to 257 ms and the 400 ms case at 400 to 410 ms, all 20 asserted and all returned Response.
#127: no loop. The failure needs a stalled terminal write, so the evidence is the deterministic test below, not repetitions.
Harness note: running progress_shutdown with `--test` alone fails terminal_frame_survives_process_exit_after_wait because the example child is not built.
Run `cargo build -p cowfs-ctl --examples` first or run the whole package.
That failure is a harness artifact, not a finding.

## #127 and #121: the race

`Conn::finish` removes the id from `inflight` and bumps `finishing` before it writes the terminal frame.
`abandon_inflight` then finds nothing for that request.
Both shutdown teardown paths call `kill()` right after it: the abandon worker in the accept loop, and the connection thread when `stopping()`.
`kill()` does not consult `finishing`, so `shutdown(Write)` fails a terminal write that is still queued.
The peer sees bare EOF and no terminal frame.
This is the hypothesis in #127, now shown deterministically.
A handler cancelled at shutdown start finishes within milliseconds, while its terminal write can sit behind a 1 MiB progress write the client is still draining.
The connection thread (stopping) or the 300 ms abandon worker then kills the connection.
That matches #121: EOF a little after the deadline, no terminal frame, and a masking effect from the faster client in dfee8b4.
It is an inference from the signature, not a reproduction.
PR 194 is merged and the coordinator is keeping #121 open.
The unit test is the regression guard and the 30 of 30 loop is the non-reproduction on main.
The link between the race and the original 332 ms event stays an inference.

## PRs

PR 194 (merged, c3c6fbd), fix/ctl-kill-finishing-127: `abandon_inflight_until(grace_end)` waits for `finishing` to reach zero before `kill`, bounded by the one absolute grace end, so no grace is added.
Test `teardown_does_not_cut_a_terminal_write_that_is_in_flight` failed first.
PR 193 (open, head 8413701, CI pending at the time of writing), fix/ctl-ordering-127: `send_progress` checks `inflight` under the write lock, so no progress frame follows its request's terminal frame.
The critic's ordering race is real in principle, but narrow: `accept_loop` calls `cancel_all` at shutdown start and `OpContext::progress` checks the token first, so only a handler past `check()` and not yet cancelled can hit it.
The test `no_progress_frame_follows_the_terminal_frame_of_its_request` asserts the post-condition on the `Conn` seam, not the cross-thread interleaving.
This is a different root cause from #121 and #127's kill race, so it is its own PR.
PR 196 (merged, 954755d), fix/ctl-drain-scope: `drain_and_close` skipped the read-away loop whenever `dead` was set, and its only caller kills right before it, so it skipped on every teardown.
It had been described only in docs/control-progress-shutdown.md, while docs/v1-control-api.md says every close reads away what the peer sent.
Decision: fix the code, not the doc. The skip is needed only after the shutdown deadline. A doc edit would have made the weaker contract permanent for protocol-error closes.
Test `a_normal_close_reads_away_what_the_peer_sent` failed first.
The doc now names the post-deadline skip.
All three passed the critic (docs/reviews/ctl-prs-193-194-196-critic-20261009.md); local runs after merging main: lib 14, admission 15, progress_shutdown 13, clippy clean.
Each is a code PR, so full CI applies; grep the linux-fuse log for `... FAILED`.
Worktree leases 3 (PR 193), 10 and 11 (merged) are still held under .treehouse-ci.

## Follow-ups, text only

Re-run the #128 case B geometry (slow reading client) against main once, with the wait timer around `wait()` only.
Log in a4 whether the 400 ms case asserted or was skipped, unconditionally, so hosted runs show it.
`best_effort_abandon` (spawn-failure path) has the same kill-without-finishing shape as #127 and was left alone because it runs on the accept thread and must not block.
