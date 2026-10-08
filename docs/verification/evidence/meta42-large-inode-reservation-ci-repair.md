# meta42-large-inode-reservation-ci-repair: the request-4 branch now compiles, runs, and is green

Lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, READY5, held throughout.
Branch: `fix/meta-inode-reservation-42`.
Entry head: `355b5fcaee9c87a1da1f527071be816f0b66dd57`, verified clean and equal to the remote before any edit.
New head: `24340e488d3bb1c9851d9a549e4c792222e2ce06`, pushed, remote equal to local.
Base of the branch: `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`.
This receipt supersedes nothing.
`docs/verification/evidence/meta42-large-inode-reservation.md` and every older receipt are untouched.

## What was wrong on entry

The prior receipt, at `355b5fca`, said plainly that none of its eight tests had ever compiled or run.
That was accurate.
CI run `37414435594` on that head was a FAILURE on both `check` jobs.
`linux-fuse` passed.

Both `check` jobs failed at the same step and for the same three reasons:

- `error: unused doc comment` at `crates/cowfs-meta/src/db.rs:154`, the `RESERVE_FAULT` `thread_local!`.
- `error: unused doc comment` at `crates/cowfs-meta/src/db.rs:164`, the `RESERVE_COMMITS` `thread_local!`.
- `error: function put_meta is never used` at `crates/cowfs-meta/src/db.rs:2005`.

`cargo fmt --all --check` had passed: the failure was `cargo clippy --workspace --all-targets -- -D warnings`, which builds the lib test target and promoted these to errors.
The two doc comments sat on macro invocations, which rustdoc cannot attach, so `-D unused-doc-comments` rejected them.
`put_meta` was a test helper the tests never called: they write the bound through `reserve_intent` directly.
All three are in the lib test build only, so the shipping library already compiled; nothing reached runtime.

## The repair, in two commits

### `07eccf0` - clear the three clippy errors

- The two `///` blocks became `//` comments, same text.
- `put_meta` removed.

No production behaviour changed.
Standalone `rustfmt --edition 2021 --check` on the owned file exited 0 with no diff.

### `24340e4` - fix three real test failures the compile had been hiding

Once clippy passed, the eight tests ran for the first time.
Five passed.
Three failed, all test bugs, not production bugs:

- `a_range_the_cached_floor_covers_commits_nothing` asserted `0` commits and saw `2`.
  Its premise was wrong: `reserve_inodes` consumes `next` up to the range end, so two consecutive reservations always extend the floor and the covered branch is unreachable through them.
  The covered branch needs `next < reserved`, which only an ordinary `Tx::alloc` inside a block produces.
  The test now creates two files so `next` trails the cached floor, then reserves inside it, and asserts `0` commits, that the range starts at `next`, and that it stays under the cached floor.

- `a_failure_after_the_floor_persisted_leaves_the_floor_ahead_and_never_reissues` panicked with `Storage("injected after the durable commit")` at the reopen.
  The injected fault was still set: `Meta::open` does not clear the test seam, only the module-local `open()` helper does, and the reopen calls `Meta::open` directly.
  `reset_reserve_probe()` now runs before the reopen.
  The production claim is unchanged and the test now proves it: after the injected failure the durable floor really is ahead and a reopen does not reissue.

- `ordinary_creation_still_starts_above_a_large_reserved_range` panicked with `NoSuchSnapshot`.
  It called `snapshot("s")`, which opens a snapshot that does not exist yet.
  It now calls `new_snapshot("s")`, which creates one.

No production line changed in this commit either.
`rustfmt --edition 2021 --check` exits 0 on the file after both commits.

## The proof that actually ran

CI run `37508429898`, branch `fix/meta-inode-reservation-42`, head `24340e4`.
All three jobs completed, all three SUCCESS:

| job | id | conclusion |
|---|---|---|
| `check (ubuntu-latest)` | 112422951709 | success |
| `check (macos-latest)` | 112422951361 | success |
| `linux-fuse` | 112422951080 | success |

The `check` jobs run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`.
Clippy is clean, so the three entry errors are gone.

The eight reservation unit tests ran and passed on **both** `check` platforms, from the run log:

- `db::tests::a_large_reservation_costs_two_durable_commits_however_big` ... ok
- `db::tests::a_range_the_cached_floor_covers_commits_nothing` ... ok
- `db::tests::a_bound_left_behind_is_exactly_what_recovery_skips_to` ... ok
- `db::tests::a_file_without_a_bound_still_recovers_by_one_block` ... ok
- `db::tests::a_failure_before_persisting_exposes_no_number_and_consumes_nothing` ... ok
- `db::tests::a_failure_after_the_floor_persisted_leaves_the_floor_ahead_and_never_reissues` ... ok
- `db::tests::a_failure_before_the_bound_commits_exposes_nothing` ... ok
- `db::tests::ordinary_creation_still_starts_above_a_large_reserved_range` ... ok

The `cowfs-meta` lib test binary reports `24 passed; 0 failed` on both platforms.
The `tests/inode_reservation.rs` integration binary reports `11 tests`, all ok, on both platforms.

This is the first green runtime this API has ever had.
The claim "two durable commits however big" is now asserted by a passing test and a commit counter, not by a design argument.

## What this does and does not establish

Established, by a real CI run on the head:

- The branch compiles under `-D warnings` on Ubuntu and macOS.
- The eight unit tests pass, including the two-commit count and the four fault-injection cases (fail before the floor commit, after it persisted, before the bound commit, and the covered-range zero-commit case).
- The eleven integration tests pass.
- A reopen after a reservation at both the API level and after an injected post-persist failure does not reissue.

Still owed, and **not** claimed here:

1. **The real `open_recover` redb repair is still not driven for the reservation.**
   `record_recovery` is exercised directly.
   Its arithmetic and the `check()` invariant after it are proved, but no test forces redb itself to discard the newest commit on a genuine damaged file and then reads the bound back.
   The production helper and redb's repair are not the same code path, and this receipt does not say they are.
   Driving it needs either the page-damage fixture `tests/health.rs` uses (redb-layout dependent, PAGE-sized, and a new failure matrix in its own right) or a production fault API the prior receipt deliberately refused.
   Neither fits the bounded scope here, and neither can be iterated locally, so it was not added.
   It remains owed.
2. **No latency measurement.**
   The O(1) property is a commit count, not a wall-clock figure.
3. **`n` at the limit** is still checked by arithmetic, not exercised with the allocator near `INO_LIMIT`.

## Interleavings scrutinised, no production change needed

The task asked for a close look at retry after a before/after persistence error, a pending bounded intent being replaced, ordinary `Tx::alloc` interleaving, and the legacy no-bound fallback.
Reading the seams:

- A retry after a failure that returned `Err` recomputes `target` from the unchanged in-memory `next`, so it can only ask for the same or a higher floor.
  The floor only ever moves forward.
- A leftover intent from a `reserve_intent` whose floor commit never ran can be overwritten by a later, larger target, or spent by a later `Tx::alloc`'s `reserve_durable`, which writes its own `ino_reserved` and removes the key.
  Either is safe: the caller of a failed reservation was handed nothing, so no live number can be lost.
  A range is only safety-relevant once it has been returned, and a returned range always had its floor durably committed first.
- `Tx::alloc` at `crates/cowfs-meta/src/tx.rs:66-79` still advances one block at a time and writes no bound, which is correct: it moves the floor by at most one block, already inside the one-block rule.
- The legacy branch in `record_recovery` is taken only when no bound is present, and a fresh file with no bound still recovers by one block, proved by a passing test.

No production code changed to reach these conclusions; they are a reading, and the tests that pin the fault cases now pass.

## Files touched

One, owned by this lane:

| File | Change |
|---|---|
| `crates/cowfs-meta/src/db.rs` | three clippy errors cleared in `07eccf0`; three unit tests corrected in `24340e4` |

No production function changed in either commit.
The reservation mechanism, `check.rs`, `Cargo.toml`, `Cargo.lock`, the schema, and every other crate are untouched.

## Boundaries held

- No local cargo, build, test, clippy, archive, target directory, probe or fault run.
  `bench/out` was not measured, cleaned, pruned, moved or offloaded, and `ready-40` is intact.
- No cap waiver, no new target directory, no cleanup.
- No change to `cowfs-core`, `cowfs-ctl`, `cowfs-daemon`, `cowfs-store`, `cowfs-fuse`, `cowfs-nfs`, GC, CI, the runner, or any provider.
- No checkout, reset, stash, rebase or branch change.
  The lane stayed on `fix/meta-inode-reservation-42`.
- No merge, no close, no new issue, no completion form.
  PR 140 stays draft and references #42 without closing it.
- All leases, the shared daemon, the Linux hosts, every store, mount and job untouched and unsignalled.
- Local and remote are both `24340e48`; nothing is held back.

Issue #42 stays open.
This is still request 4's metadata half, and the Core consumer is still not built here.
