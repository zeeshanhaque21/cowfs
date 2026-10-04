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
  and `admission.rs::a4_a_terminal_frame_is_delivered_to_a_client_that_resumes_within_the_grace`.
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
3. **Close without draining when the connection was already killed.** `drain_and_close` read away
   what the peer sent, for up to `drain_deadline`. Past the grace there is nobody left to be polite
   to, and that read is what held the socket open after `wait()` returned.

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

The one grace is split rather than given to whichever wait asks first. Delivery takes the front half
and the close the back half. The close is what the contract requires and a frame is only promised as
best effort, so it cannot be starved. With the split removed, the delivery wait consumed the whole
grace, the close never ran, and a parked connection was still open when `wait()` returned
(`connection_closed_at_return=false`).

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

The 250 ms default `drain_deadline` cannot serve both phases: the close needs about 150 ms, leaving
under 100 ms for delivery. Two tests therefore declare a `drain_deadline` they can actually be served
inside, rather than implying delivery works at any budget:
`a4_a_terminal_frame_is_delivered_to_a_client_that_resumes_within_the_grace`, whose client resumes
400 ms after shutdown, and `abandoned_blocked_connection_is_released_before_wait_returns`, which needs
room for the close. The source deadline is unchanged.
