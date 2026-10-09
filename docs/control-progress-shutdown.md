# Blocked progress writes and the control-server shutdown deadline (#77)

## Symptom

A client that stops reading while a request streams progress kept `Server::wait()` running for
about twice the write timeout instead of the `shutdown_deadline`.
On macOS, with `write_timeout = 4 s` and `shutdown_deadline = 300 ms`, shutdown took about 7600 ms.

This is a pre-existing bug, reproduced independently of PR #73.
It concerns blocked **progress** writes, not terminal writes (the terminal case is what PR #73 fixed).

## Reproduction

A private fixture starts a real control server on a private Unix socket.
The handler streams 1 MiB progress events in a loop.
The client completes the handshake, sends one `gc` request, and then never reads.
After the first progress write fills the send buffer, the connection thread parks inside `write_all`.
The test then calls `shutdown()` and measures `wait()`.

The regression lives in `crates/cowfs-ctl/tests/progress_shutdown.rs`:

- `blocked_progress_write_does_not_hold_shutdown_past_the_deadline` (the core issue).
- `two_blocked_progress_writers_are_both_abandoned_at_the_deadline` (two stragglers, one wait).
- `blocked_progress_write_does_not_stall_admission` (the admission path is not the regression).
- `an_unblocked_client_still_receives_a_terminal_frame_at_shutdown` (normal reader keeps its frame).
- `pending_requests_are_abandoned_together_when_one_progress_write_is_blocked` (parallel pending ids).
- `terminal_frame_survives_process_exit_after_wait` (the process-exit race, child fixture).
- `healthy_reader_behind_a_stalled_writer_is_not_delayed` (a healthy reader is not queued behind one).
- `staggered_stalled_writers_stay_within_the_deadline_plus_grace` (stalls do not stack).

Baseline source was extracted with `git archive 103a65e` into `bench/out/progress-shutdown/base/` and
its `server.rs` sha256 was checked against `git show 103a65e:crates/cowfs-ctl/src/server.rs`:
`e61942c15f2ff9d7f963e1e543c92de9e030ba61660cb95707d677aba82ebf62`.
The baseline and changed variants used separate `CARGO_TARGET_DIR`s (`target-base`, `target-changed`).

## Mechanism

Shutdown teardown ran on the accept thread.
When `abandoned()` became true, `accept_loop` called `conn.abandon_inflight()`, which calls
`finish(id)` -> `write_frame` -> `lock(self.write_lock)`.
That lock is held by the connection thread for the whole duration of a progress `write_all` when the
client is not reading.
So the accept thread blocked on the same per-connection write lock it was supposed to abandon.

A timestamped spike on the baseline confirmed it: the accept thread entered `abandon_inflight` and
sat in `finish(1): removed, acquiring write lock` until the blocked progress write finally failed at
about 7.3 s.
Two stragglers were abandoned serially, giving about 7.7 s.

The lock attribution is now reproduced, not a code-read hypothesis.

## Fix

In `accept_loop`, the deadline branch no longer performs the terminal writes inline.
For every straggler it spawns a per-connection `cowfs-ctl-abandon` worker that runs
`abandon_inflight` (which writes the `shutting_down` frame) and then `kill`, and it waits a bounded
`drain_deadline` for those workers before breaking the wait loop.
The bounded wait is what makes the process-exit path safe: a readable client's frame is on the wire
before `wait()` returns, so `cowfs serve` can exit right after it without dropping the frame.
A worker for a client that stopped reading blocks on the per-connection write lock held by a
progress write; the grace expires and `wait()` returns anyway, and that worker is left detached
(documented for handler threads: they die with the process, and `drain_and_close` closes the reader
side once the write unwinds).

If a worker cannot be spawned, the deadline path delivers the frame best-effort with a non-blocking
write (`best_effort_abandon`, which skips a held write lock) and then `kill`s the connection, so a
readable peer still gets `shutting_down` and no connection is left open behind a spawn failure.

The change is confined to the deadline branch.
The graceful path (before the deadline) and the connection thread's own `abandon_inflight` are
unchanged, which is why `a4_a_terminal_frame_being_written_is_not_cut_by_shutdown` still holds.

## Why the frame is not cut

The first version of this fix returned at the deadline and detached one serial worker.
`cowfs serve` exits the moment `wait()` returns, so the worker lost the race against process exit
and the client saw a bare EOF. The private child fixture (`examples/progress_exit_child.rs`) runs a
stuck handler that ignores cancellation, one reading client, and `process::exit(0)` right after
`wait()`. On the serial-detached version, **66 of 100** clients got the frame and **34 of 100** saw a
bare EOF; the baseline (PR #73) got 100 of 100.
The bounded grace above writes the frame before `wait()` returns, so the changed variant gets
**100 of 100**, and the committed regression
`terminal_frame_survives_process_exit_after_wait` fails on the serial version (45 of 100) and passes
on this one.

## Resource bounds

At most one worker per straggler is created, so the worker count is capped by `max_connections`.
Each worker finishes in microseconds when its peer is readable; one that is parked behind a blocked
write is left detached and held only until that write unwinds (bounded by `write_timeout`), exactly
like any other detached handler thread.
No worker is joined on the deadline path, so a blocked write cannot stretch `wait()` past the grace.
Across 6 start/shutdown cycles with a forced client close, file descriptors returned to the baseline
(4 before, 4 after), and a new server accepted connections on the same path immediately after
`wait()` returned.

## Evidence

Timings are one machine (Apple M3 Max, macOS), one load shape, and a small number of runs per cell,
not a general performance estimate.
`write_timeout = 4 s`, `shutdown_deadline = 300 ms`, `drain_deadline = 250 ms`.

| test | baseline `103a65e` | old fix `ff6c595` | corr. | bound |
|---|---|---|---|---|
| blocked progress shutdown | 7601 ms | 304 ms | 556 ms | < 1200 ms |
| two blocked progress writers | 7718 ms | 308 ms | 554 ms | < 1200 ms |
| six stalled writers (probe) | serial | 304 ms | 562 ms | < 1200 ms |
| staggered stalls n=4 (probe) | serial | - | 556 ms | < 1200 ms |
| admission with blocked progress | 1 ms | 0 ms | 0 ms | < 1200 ms |
| unblocked reader terminal frame | present | present | present, 350 ms | present |
| healthy reader behind stalled (probe) | 2/8 near | 2/8 near | 8/8, worst 31 ms | frame < 1200 ms |
| parallel pending ids | both | both | both, 73 ms | both present |
| child process exit, stuck handler | 100/100 | 66/100 | 100/100 | `== reps` |
| restarted server on same path | ok | ok | ok, 0 ms | ok |
| fds after cycles | 4 | 4 | 4 | <= baseline + 4 |

`wait()` on this fix is `shutdown_deadline` plus the bounded grace, not the write timeout.
The earlier serial fix returned at 300 ms but dropped frames on process exit; the correct fix trades
about one `drain_deadline` of `wait()` time for keeping the frame, still far under the bound.

Full suite on the corrected variant: 96 tests pass
(10 unit, 14 admission, 1 flood, 8 progress_shutdown, 19 regress, 31 server, 13 wire).
`cargo clippy -p cowfs-ctl --all-targets -- -D warnings` and
`cargo fmt -p cowfs-ctl -- --check` are clean.

The injected spawn-failure variant (a private patch that makes the worker spawn always fail) delivers
`shutting_down` and closes in 30 of 30 runs, bounded at 307 ms p50; the naive `shutdown(Both)`
fallback before a best-effort write delivered 0 of 30, which is why the fallback writes first.

PR #73 behaviour preserved on the changed variant:
`a5_a_blocked_terminal_write_does_not_stall_admission` first frame 0 ms and
`a5_a_blocked_terminal_write_does_not_stall_shutdown` 309 ms against a 300 ms deadline.

## Not done

- No dedicated Linux host was used for the local reproduction, and no Linux-specific socket
  shutdown semantics were exercised beyond what the build needs.
  The hosted CI job runs `cargo test --workspace`, which includes `progress_shutdown`, so the fix and
  the new tests are exercised on Linux there; a local Linux run was not performed.
- The fix is not a throughput claim.
  It bounds `wait()` at the deadline plus the delivery grace; it does not change how fast a blocked
  client is finally closed.
- A client that only becomes readable after `wait()` returns is not guaranteed its frame on the
  process-exit path, because the process is gone; the frame is guaranteed for a client readable at
  the deadline, which is the contract's case.

## `a4` vs the Shutdown contract: mutually exclusive cleanup (spike)

`reconcile_spike.rs` (real `Server` API, isolated sockets, `write_timeout=5s`,
`shutdown_deadline=200ms`, flood handler holding the write lock in a blocked progress write) ran two
client behaviours under the same code: resume reading at 900ms (`admission.rs::a4`) and never read
(#77). Three cleanup variants:

| variant | client | `wait()` ms | whole terminal |
|---|---|---|---|
| baseline `89c271e` (no kill) | resume 900ms | 463 | true |
| baseline `89c271e` (no kill) | never reads | 459 | true |
| kill after grace (reviewer `alt`) | resume 900ms | 464 | false |
| kill after grace (reviewer `alt`) | never reads | 463 | false |
| `best_effort_abandon` + kill, no lock wait | resume 900ms | 200 | false |
| `best_effort_abandon` + kill, no lock wait | never reads | 206 | false |

On the kill variants the client that resumes at 900ms sees a truncated tail and then EOF
(`tail_len=0` after the partial frame is consumed): the close lands while a progress frame is
mid-write, so the terminal frame never gets a turn. `a4` asserts the terminal "must be whole" for a
client the server has already given up on, which requires waiting out the flood's blocked write (up
to `write_timeout`, 5s). #77 requires the connection closed by `deadline` plus a bounded grace. The
two are the same socket in the same state; no bounded cleanup satisfies both.

`a4` was added by `9aec638` (#13), the same commit that introduced the Shutdown contract text. Its
comment states the intent ("the terminal frame write is still in flight when the server gives up on
the connection"). The contract's "connections are closed and the server returns" is the binding
behaviour; `a4`'s stronger "must be whole" is the pre-#77 behaviour that #77 removes.

## Contract change: what is guaranteed, and what is not

The Shutdown contract (`docs/v1-control-api.md:397-399`) says abandoned handlers have "their
requests get `shutting_down`, their connections are closed and the server returns". #77 makes the
close real. The guarantee is now explicitly two-sided, and the second side is a relaxation of what
`a4` previously asserted:

- **Inside the delivery grace** (`shutdown_deadline` plus `drain_deadline`), a client that reads
  gets its complete terminal frame, and `wait()` does not return before that frame is on the wire.
  Asserted by `an_unblocked_client_still_receives_a_terminal_frame_at_shutdown`,
  `healthy_reader_behind_a_stalled_writer_is_not_delayed` (frame at 307 ms, code `shutting_down`)
  `admission.rs::a4_a_terminal_frame_is_delivered_to_a_client_that_resumes_within_the_grace` and
  `a_client_resuming_late_inside_the_full_grace_gets_a_whole_frame`, the latter resuming at 340 and
  400 ms in `a4`'s 200/250 ms geometry and at 90 and 95 percent of a 1000 ms grace.
- **Past the delivery grace**, the connection is closed before `wait()` returns. A peer that is
  still not reading then gets no promise for a frame: it may see a partial frame followed by EOF.
  A complete terminal frame cannot be promised to a peer that resumes reading only after the close.
  Asserted by `abandoned_blocked_connection_is_released_before_wait_returns` and
  `admission.rs::a4_a_terminal_frame_is_not_guaranteed_past_the_grace`.

This is the whole of the relaxation. A peer never receives a fabricated or duplicated terminal:
`classify` and the frame readers only accept a terminal parsed out of a complete line, and
`a4_a_terminal_frame_is_not_guaranteed_past_the_grace` rejects both `NoHello` and `TwoTerminals`.

## Counterexample on the pre-fix baseline, then on the fix

`counterexample.rs`, real private Unix socket, `write_timeout` 3 s, `shutdown_deadline` 300 ms.
Sampled at the instant `wait()` returned, before waiting for anything.

| source | `wait()` ms | handler alive at return | connection open at return | handler dropped | peer saw close |
|---|---|---|---|---|---|
| `89c271e` (sha256 `7ec25ae6…`) n=1 | 557 | **true** | **true** | never, within 6 s | 6562 ms |
| `89c271e` n=6 | 552 | **true** | **true** | never, within 6 s | 6555 ms |
| fix n=1 | 597 | false | false | 597 ms | 597 ms |
| fix n=6 | 603 | false | false | 603 ms | 603 ms |

The baseline identity was checked with `shasum -a 256` against `git show 89c271e:…/server.rs`
(`7ec25ae6fe72ad9bde2cdd38476246fd4504addac0ca42339a3f05b22ac9b4ba`) on the extracted copy, so the
counterexample was measured on that source and not on the working tree.

## Why three changes, not one

1. **Kill every straggler after the shared grace.** Half-closing the write side aborts the parked
   `write_all`. Without it nothing interrupts that write, so the connection, socket and handler
   outlive `wait()` by up to `write_timeout`, which is the reported defect.
2. **Join the abandon workers, and wait for each straggler to be released, before returning.** The
   workers own the only writes this server still makes to a control client, and a request worker owns
   the handler, so both must be finished at the return for nothing of an abandoned connection is
   retained. `Conn::workers` counts the request workers: a connection thread dropping out of `conns`
   does not mean its handler is gone, which the spawn-failure variant demonstrated
   (`handler_alive_at_return=true`, `connection_closed_at_return=false`, `fds_at_return` 10 of 12).
   `Conn::released` is the predicate the accept loop waits on, bounded by the same grace.
3. **Close without draining once the shutdown deadline has passed.** `drain_and_close` reads away
   what the peer sent, for up to `drain_deadline`. Past the deadline there is nobody left to be
   polite to, and that read is what held the socket open after `wait()` returned. Every earlier
   close still drains, as `v1-control-api.md` says. An earlier version keyed the skip on the
   connection being killed, which `run_connection`'s caller does before every close, so it skipped
   the drain on every teardown.

## Spawn-failure and full-buffer fallback

The abandon-worker spawn can fail under thread exhaustion. That path runs on the accept thread, so
its write is bounded by the delivery grace: a peer whose receive queue is completely full would
otherwise park that `write_all` for a whole `write_timeout`, which is the deadline this path exists
to protect.

Verified with a private patch that forces the spawn to fail
(`bench/out/progress-shutdown/inject_spawnfail.py`, not shipped source), full buffer, same tests:

| case | `wait()` ms | handler alive at return | connection closed at return | fds after cycle |
|---|---|---|---|---|
| n=1 | 378 | false | true | 4 of 4 |
| n=6 | 396 | false | true | 4 of 4 |

All 9 `progress_shutdown` tests pass on that variant, `admission` 14 of 15, `regress` 19 of 19, no
panic, no unbounded write, no descriptor leak across cycles.

**Known limitation of the degraded path.** When the abandon worker cannot be spawned, the terminal
write is best effort and is skipped while a progress write holds the write lock. A client that
resumes reading inside the grace therefore gets no frame on that path:
`a4_a_terminal_frame_is_delivered_to_a_client_that_resumes_within_the_grace` fails under the injected
patch (`ending=NoFrame`) and passes on the shipped source. This is not fixable inside the constraint:
the lock is held until `kill` aborts the write, and `kill` half-closes the write side, so no write
can follow it. Delivering the frame would mean waiting on the accept thread for the write timeout,
which is the defect this issue is about. The within-grace delivery guarantee therefore holds for the
normal path, and the close-within-the-bound guarantee holds for both.

## Not done

- No dedicated Linux host was used for the local reproduction. The hosted CI job runs
  `cargo test --workspace`, which includes `progress_shutdown`, so the fix and the new tests are
  exercised on Linux there; a local Linux run was not performed.
- The fix is not a throughput claim. It bounds `wait()` at the deadline plus the delivery grace.
- A detached handler that ignores cancellation and never touches its socket is still allowed to
  outlive `wait()`; the contract already says handler threads are detached and die with the
  process. What is guaranteed here is that no handler blocked in socket IO, no control-client buffer
  writer, and no connection survive the return.

## Correction: the budget is one absolute end instant, not three graces

An earlier revision of this note said the three waits after the shutdown deadline were bounded by
"the same `drain_deadline`". That was true of the duration and false of the end instant, which is
what a budget is made of. Each wait computed its own `Instant::now() + opts.drain_deadline`, so a
straggler that finished nothing stretched `wait()` to `shutdown_deadline` plus three graces.

There is now one `grace_end`, computed once where the deadline branch is entered and reused by the
worker-signal wait, the worker join and the release wait. The branch is polled every 10 ms, so that
instant is at most one poll interval after the real deadline, which is the scheduling slack.

The whole grace is the delivery window. `kill` runs at `grace_end`, so a client that resumes reading
at any point inside `shutdown_deadline + drain_deadline` still gets its frame: after the resume the
parked write unwinds, the handler returns, and the terminal frame is written inside that window.

An earlier revision reserved the back half of the grace for the close. That was withdrawn, not
justified:

- **It silently shortened a documented guarantee.** `kill` ran at the halfway mark, so a client
  resuming at 340 or 400 ms in `a4`'s own 200/250 ms geometry got `NoFrame` or a truncated frame
  where the three-grace code gave a whole one. The fix for that had been to widen `a4` to a 500 ms
  grace, which made the cut invisible rather than removing it.
- **Its stated reason did not reproduce.** The justification was that without the split the close
  did not happen at the return. Measured across mixed, parked-only, three grace values and two
  deadlines, read EOF was observed at the return in every shape on the unsplit source.

What a reserve would actually buy is letting a killed parked handler unwind before the return.
That is a weaker claim than the contract makes: `v1-control-api.md` detaches handler threads and lets
them die with the process. So the reserve is removed and that case is handed back to the contract
rather than paid for out of the delivery window.

Two internal test assertions that required the handler to be unwound at the return were stricter than
the contract and are corrected to gate the close and the bounded `wait()` instead, with the detached
handler's eventual exit asserted separately as this test loop's own thread budget.
`handler_alive_at_return` is reported, not asserted away.

Measured with `shutdown_deadline` 300 ms and `drain_deadline` 500 ms, so the budget is 800 ms and the
test allows 250 ms more for scheduling:

| case | three graces | one grace |
|---|---|---|
| parked writer and CPU handler | 1303 ms (fail) | 805 ms (pass) |
| parked writer only | 888 ms (passes either way) | 679 ms |
| CPU handler only | 807 ms (passes either way) | 803 ms |

The mixed case is the one that discriminates, which is why it is the committed test: a parked writer
alone returns inside even the inflated budget, and a CPU-bound handler alone does too. Only the pair
holds the release wait open for the whole grace.

Parked-ness is proven, not assumed: the flood handler's step counter must go still in two consecutive
250 ms windows, which can only happen while a `write_all` has not returned. Neither socket is read
before `wait()` returns, because a read would drain the buffer and unpark the write under test.

### What the fix does not promise

A handler stuck on the CPU rather than in a socket write cannot be reached by half-closing. It is
detached and dies with the process, which is what the contract already says of handler threads. It no
longer costs a second grace. No claim is made here that every handler is joined or deallocated.

## Tests that declare their own budget

`a4_a_terminal_frame_is_delivered_to_a_client_that_resumes_within_the_grace` is back on the 250 ms
default grace, resuming at 400 ms inside a window that closes at 450 ms. That was its original
geometry and it is left alone deliberately: widening it to 500 ms is what hid the delivery cut.

`a_client_resuming_late_inside_the_full_grace_gets_a_whole_frame` is the committed regression. A
finite handler, so the terminal frame is the only thing left to deliver and a late arrival cannot be
a progress frame. Three geometries, five reps each, all past the cut the half-split head used and all
with real room left for the unwind, the handler return and the terminal write. Resume is measured from
the start of shutdown, so the window closes at `deadline + drain_deadline`:

| case | window closes | resume | room left | half-split cut was |
|---|---|---|---|---|
| 200/250 | 450 ms | 340 ms | 110 ms | 325 ms |
| 200/1000 | 1200 ms | 900 ms | 300 ms | 700 ms |
| 200/1000 | 1200 ms | 950 ms | 250 ms | 700 ms |

15 of 15 whole frames with the peer closed, and on the half-split head the first geometry fails
outright with `whole_terminal=false` at 340 ms.

The 200/250 geometry resuming at 400 ms was a committed point and is no longer one. It left 50 ms, and
one hosted macOS rep in six missed it at `elapsed_ms=547` against a 450 ms window. A hard `whole`
assertion that close to the bound fails intermittently on a loaded runner and gets blamed on something
else. Resuming at 340 ms in that geometry still sits past the 325 ms cut, which is the property under
test, with more than twice the room. Resuming at 400 to 449 ms there remains worth probing by hand and
appears in the review evidence; it is not a hosted correctness gate, because no scheduler promises
50 ms.

The fixture emits one mebibyte per event, which parks on the first write on every runner where the
sibling 1 MiB fixtures here park, and two events, so it still parks if a runner absorbs the first.
Deliberately not more: every extra event is backlog the client must drain after it resumes, and that
drain is charged against the room left in the window. An earlier 400 events of 2000 bytes, 781 KiB in
total, did not park on the ubuntu runner at all, so `assert_parked` failed before any assertion about
frames ran; that the socket buffer absorbed it is inference from the step counter advancing 37 to 50,
not a measurement. Eight events of 1 MiB then left up to 7 MiB to drain in the 110 ms the 450 ms
geometry allows, and failed locally at `elapsed_ms=456`. `assert_parked` is unchanged throughout.

Measured with `shutdown_deadline` 300 ms and `drain_deadline` 500 ms, so the budget is 800 ms and the
