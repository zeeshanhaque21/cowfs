# Issue #40 M5: original-snapshot reopen correction

Lane: READY5, branch `test/meta-allocation-option-extremes-40`, draft PR #144.
New head: `825bd38e0b6aa62b06b4de25ec3579d572cc753f`. Prior head: `ef9b40a...`.

## Actual CI failure
Run `37533053984`, `check` (Ubuntu, macOS): 3 of 4 tests passed; MAX failed:
`a_u64_max_block_is_clamped_in_the_ordinary_allocator ... FAILED` at
`allocation_option_extremes.rs:91`, `lookup(ROOT_INO, b"f") -> Error::NotFound`.

## Root cause
Fixture bug, not production data loss. `f` was created in snapshot `s0`; after a
clean close and reopen the test made a fresh EMPTY `s1` and looked up `f` in it.
`f` was in `s0` all along. The zero/default cases never look up the old file
after reopen, so they are correct.

## Fix
Capture `s.id()` when `f` is created; after reopen open the same snapshot with
`Meta::snapshot_by_id` (public API), assert `f` identity (same inode, kind,
mode), and create `g` there. Absence semantics not weakened. Missing-clamp
discriminator and live-floor-limit assertions unchanged. No production change.

## Status
CI on `825bd38`: PENDING until the named four tests pass in a completed run. Runnable command: `cargo test -p cowfs-meta --test allocation_option_extremes`.
Old receipts `486b16f6...`, `c418d15c...`, `ec04ce04...`, `15801061...` and review `0f730979...` unchanged.
