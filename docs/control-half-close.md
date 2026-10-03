# Control half-close regression (#58)

## Reproduction

Unchanged main `ab99868` passed the initial single macOS run.
A bounded repetition of the real Unix-socket regression reproduced the CI failure on run 55: `unexpected EOF` at `tests/common/mod.rs:136`.
The preceding 54 runs passed.
Evidence is append-and-fsync JSONL in `bench/out/half-close/baseline.jsonl` in the verification worktree.
This is a reproduced intermittent failure, not an estimate of its frequency under other loads.

```sh
cargo test --locked -p cowfs-ctl --test regress m1_half_close_means_no_more_requests_not_cancel -- --exact
python3 scripts/reproduce-half-close.py --runs 200 --seconds 180 --output bench/out/half-close/new-baseline.jsonl
```

## Cause and change

`Conn::finish` removed the final request from `inflight` before writing its terminal frame.
The connection reader could observe an empty map after client write-half-close, return, and shut down the server write side before the request worker sent its response.
The first fix retained the inflight map lock through the terminal write so completion is observable only after the write finishes.
Independent review found that this held the shared admission lock for the whole write timeout: `admit` (eviction at `max_connections`) and shutdown (`abandon_inflight`) both take `inflight`, so a client that stopped reading stalled both for `write_timeout`.

The corrected change tracks completion with a separate `Conn::finishing: AtomicU64` counter instead of the map lock.
`finish` removes the id and increments `finishing` while holding `inflight`, releases the map lock, writes the terminal frame, then decrements `finishing`.
`inflight_empty` returns `inflight.is_empty() && finishing == 0`, so half-close drain, idle eviction and admission eviction still wait for the terminal write to complete, but no path holds the map lock across the write.
The increment happens under the map lock, so any observer that sees the id gone also sees a nonzero counter until the write finishes; the decrement is after `write_frame` returns, so a failed write decrements before `kill` reacquires `inflight` through `cancel_all`.
`write_frame` remains separate from failure-triggered cancellation, so a failed terminal write cannot deadlock, and no duplicate terminal frame is possible: the `remove` returns `None` for an id already finished.

## Deterministic regression and negative control

`terminal_write_remains_inflight_until_sent` holds `write_lock` while a finisher reaches the terminal write.
It waits for `finishing` to become nonzero, asserts `!inflight_empty()` in that window, then releases the write lock and checks the exact encoded frame before EOF.
The pre-fix implementation never bumps `finishing`, so the finisher is not observed and the test fails deterministically.
`terminal_write_failure_does_not_deadlock_cancellation` checks that a failed terminal write completes within two seconds and cancels another pending request.
Both tests passed on the changed implementation.
Temporarily restoring only the old `finish` body and single-field `inflight_empty` made the first test fail with `finisher did not reach the write`; the changed body was restored afterwards.

The two admission repros below fail on the PREVIOUS fix (map lock held across the write) and pass on the corrected one:

- `a5_a_blocked_terminal_write_does_not_stall_shutdown`: a handler returns a 4 MiB `snapshot_list`, the client never reads, `write_timeout` is 4s and `shutdown_deadline` is 300ms. `Server::wait()` must return near 300ms.
- `a5_a_blocked_terminal_write_does_not_stall_admission`: same blocked writer with `max_connections = 1`; a second connection must be refused promptly instead of waiting on the map lock.

Measured on the corrected source: admission first frame 1ms, shutdown 306ms.
Negative control with the previous fix's lifecycle in place, isolated target dir, verified source identity (`finishing` absent): admission 7655ms, shutdown 7614ms, both FAIL.

```sh
cargo test --locked -p cowfs-ctl --lib terminal_write
cargo test --locked -p cowfs-ctl --test admission a5_a_blocked_terminal
```

Full `cargo test --locked -p cowfs-ctl` passed 88 tests across six test binaries (86 pre-existing plus the two new A5 repros).
`cargo clippy --locked -p cowfs-ctl --all-targets -- -D warnings` passed.
`cargo fmt --all -- --check` passed.
The changed real socket regression passed 120 consecutive runs through the compiled regression binary.
The direct-binary repetition excludes Cargo startup overhead but exercises the identical test and server/client path.
Independent review of this corrected change is delegated to a fresh reviewer at the pushed SHA.
No shared daemon, mount, store, active lease, or runner was restarted or modified by these tests.
