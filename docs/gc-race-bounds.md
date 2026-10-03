# Bounded GC race fixtures (issue 83)

`crates/cowfs-gc/tests/race.rs` runs writers, snapshot create/remove, and collects at once.
Its concurrent fixtures are now bounded, for the reason in issue 83: on a runner where a collect
stalled, the writer loops kept appending until the disk filled (`StorageFull`, "No space left on
device"), and the workspace step ran for about five hours.

## What changed

- Every concurrent writer loop has a fixed byte budget (`WRITE_BYTE_BUDGET`, 24 MiB) and a fixed
  iteration cap (`WRITE_ITER_CAP`).
- The phase has a finite wall-clock cap (`RUN_CAP`, 4 s) and a no-progress timeout
  (`NO_PROGRESS`, 60 s) enforced by an outermost watchdog thread that sets the shared stop flag.
- Any worker or collector error sets the stop flag and records the failure; the phase then asserts
  on that failure instead of panicking inside a spawned thread.
- Each run prints the bytes, writes and collects it actually reached, and asserts they are
  non-zero, so a budget that silently starved the workload fails loudly rather than passing empty.

The bound is on the rate as well as the total: a writer stops as soon as the byte budget is spent,
so a descheduled runner stops early instead of growing the store without limit.

## What did not change

- The adversarial structure is preserved: a shared `base` snapshot, forked per writer, with a
  concurrent create/remove/reap thread and a collect loop, and every collect's live set re-read
  from the store.
- The stall test's assertion is still the relative one (`held_us < collect_us`): the barrier's own
  held time is a fraction of the collect window, so it is handed off per pack. That assertion is a
  functional concurrency check, **not** a performance gate: wall-clock under a descheduled runner is
  variance. The loose `worst_us < 30_000_000` line is only a backstop for a barrier that is
  pathological for some other reason.
- No timeout was loosened and no failure is ignored.

## The `NoSuchSnapshot` failure is not the fixture's

The CI `NoSuchSnapshot` at `writers_and_collects_at_once_lose_nothing` was a **production defect in
the collector**, reproduced deterministically and reported in issue 83; it was not this fixture doing
an invalid operation. The fixture removes snapshots while a collect runs, which the collector's own
documentation says it supports.

The defect: the mark pass looked a listed snapshot up and then, in a separate call, read its root and
walked it. A removal that landed in the gap between those two calls made the second call report
`NoSuchSnapshot`, and the collector returned that as a cycle error, so a collect running while a
snapshot was removed failed outright.

The fix is in `crates/cowfs-gc/src/lib.rs`, in the mark pass:

- `NoSuchSnapshot` from `live_blocks_with_root` is the snapshot removed in that gap. It names no root
  in the durable table any more, and a fork of it recorded its own root before the removal
  committed, so no snapshot the cycle keeps is reached through it. The cycle skips **that one id**
  and continues.
- Every other error still stops the cycle (`Err(e) => return Err(e.into())`, the same conversion the
  old `?` used). It is never treated as a vanished snapshot.

The fix is exactly that match. It does not mark the failed root walked, does not cache a block or a
root for it, and persists no partial mark: the `continue` is taken before any of that.

Regression tests:

- `crates/cowfs-gc/tests/core_reclaim.rs::a_snapshot_removed_between_the_lookup_and_the_walk_does_not_fail_the_cycle`
  drives the removal into the exact window with a test seam, over a real store and real core. On the
  pre-fix source it fails with `Meta(NoSuchSnapshot)`; after the fix the cycle is `Ok`, reclaims real
  dead packs, and a fork of the removed victim still reads every byte of every shared file after a
  reopen, with `fsck` clean.
- `crates/cowfs-gc/tests/regressions.rs::a_non_nosuchsnapshot_error_in_the_walk_window_fails_the_cycle_and_frees_nothing`
  arms a metadata `before_sync` fault in the same window so the walk fails with a non-`NoSuchSnapshot`
  error. The cycle must fail and free nothing. A fail-open mutation (`Err(_) => continue`) makes this
  test fail, so the propagation arm is load-bearing.

The bounded fixture still surfaces any such defect (it records the collector error and fails), so it
does not hide it. This file only bounds resources; it does not change what the test asserts about
correctness.
