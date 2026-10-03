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
It hands the stragglers to one detached `cowfs-ctl-abandon` thread and breaks the wait loop, so
`wait()` returns at the deadline.
The abandoned thread still calls `abandon_inflight` then `kill` for every straggler, so:

- a client that is (or becomes) reading still receives its `shutting_down` terminal frame, and
- a client that never reads is closed by the same kill, bounded by `write_timeout`.

The change is confined to the deadline branch.
The graceful path (before the deadline) and the connection thread's own `abandon_inflight` are
unchanged, which is why `a4_a_terminal_frame_being_written_is_not_cut_by_shutdown` still holds.

## Resource bounds

The detached thread is created at most once per shutdown and iterates the connections still in the
registry, which is capped by `max_connections`.
Each connection is killed by the detached path, so a client that never reads is not left holding a
connection slot indefinitely; the connection thread wakes on the `dead` flag and exits.
The thread itself lives at most `write_timeout` and then dies with the process.

## Evidence

Timings are one machine (Apple M3 Max, macOS), one load shape, one run per cell, not a general
performance estimate.
`write_timeout = 4 s`, `shutdown_deadline = 300 ms`.

| test | baseline `103a65e` | changed | bound |
|---|---|---|---|
| blocked progress shutdown | 7601 ms | 300 ms | < 3000 ms |
| two blocked progress writers | 7718 ms | 308 ms | < 3000 ms |
| admission with blocked progress | 1 ms | 0 ms | < 3000 ms |
| unblocked reader terminal frame | present, 308 ms | present, 305 ms | present |
| parallel pending ids | both present, 60 ms | both present, 52 ms | present |

Full suite on the changed variant: 93 tests pass
(10 unit, 14 admission, 1 flood, 5 progress_shutdown, 19 regress, 31 server, 13 wire).
`cargo clippy --locked -p cowfs-ctl --all-targets -- -D warnings` and
`cargo fmt -p cowfs-ctl -- --check` are clean.

CI run `37104454670` on this branch passed all three jobs
(`check (ubuntu-latest)`, `check (macos-latest)`, `linux-fuse`), so the workspace suite including
`progress_shutdown` passes on Linux as well as macOS.

PR #73 behaviour preserved on the changed variant:
`a5_a_blocked_terminal_write_does_not_stall_admission` first frame 0 ms and
`a5_a_blocked_terminal_write_does_not_stall_shutdown` 309 ms against a 300 ms deadline.

## Not done

- No dedicated Linux host was used for the local reproduction, and no Linux-specific socket
  shutdown semantics were exercised beyond what the build needs.
  CI's `check (ubuntu-latest)` job runs `cargo test --workspace`, which includes
  `progress_shutdown`, and it passed, so the fix and the new tests run on Linux too.
- The fix is not a throughput claim.
  It bounds `wait()` at the deadline; it does not change how fast a blocked client is finally closed.
