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
