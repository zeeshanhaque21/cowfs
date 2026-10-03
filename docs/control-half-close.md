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
Retain the inflight lock through the terminal write so completion is observable only after the write finishes.
Split frame writing from failure-triggered cancellation so a failed terminal write releases the inflight lock before `kill` reacquires it.
The write timeout remains the existing bound on a client that does not read.

## Deterministic regression and negative control

`terminal_write_remains_inflight_until_sent` holds the write lock while a finisher reaches it.
The old implementation exposes an empty inflight map in that window and deterministically fails the test.
The changed implementation holds the inflight lock until the terminal write completes, after which the test checks the exact encoded frame before EOF.
`terminal_write_failure_does_not_deadlock_cancellation` checks that a failed terminal write completes within two seconds and cancels another pending request.
Both tests passed on the changed implementation.
Temporarily restoring only the old `finish` body made the first test fail with `teardown can observe completion before the terminal write`.
The changed body was restored afterwards.

```sh
cargo test --locked -p cowfs-ctl --lib terminal_write
```

Full crate tests, clippy, repeated changed socket regression, and independent review remain pending at this WIP checkpoint.
No shared daemon, mount, store, active lease, or runner was restarted or modified by these tests.
