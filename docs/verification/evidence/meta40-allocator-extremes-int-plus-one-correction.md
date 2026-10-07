# Issue #40 M5: clippy `int_plus_one` correction

Lane: READY5, branch `test/meta-allocation-option-extremes-40`, draft PR #144.
New head: `2d9e263fdef4acb7be8c8678063bfda51d05ed99`. Prior head: `fbdd104...`.

## Actual CI failure
Run `37531555084`, `check` (Ubuntu, macOS), `cargo clippy --workspace --all-targets -- -D warnings`:
```
error: unnecessary `>= y + 1` or `x - 1 >=`
  --> crates/cowfs-meta/tests/allocation_option_extremes.rs:85:9
85 |         floor >= created.0 + 1,
   |         help: change it to: `floor > created.0`
```

## Fix
`floor >= created.0 + 1` -> `floor > created.0`. Same condition, assertion retained.
No other assertion changed, no production change. No local test run (8 GiB cap; no waiver).
Standalone `rustfmt --edition 2021 --check` clean.

## Status
CI on `2d9e263`: PENDING. Not green until the named tests pass in a completed run.
Runnable command: `cargo test -p cowfs-meta --test allocation_option_extremes`.
Old receipts `486b16f6...`, `c418d15c...`, `ec04ce04...` and review `0f730979...` unchanged.
