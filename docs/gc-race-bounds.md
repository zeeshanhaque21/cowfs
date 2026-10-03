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

The CI `NoSuchSnapshot` at `writers_and_collects_at_once_lose_nothing` is a **production defect in
the collector**, reproduced deterministically and reported in issue 83; it is not this fixture doing
an invalid operation. The fixture removes snapshots while a collect runs, which the collector's own
documentation says it supports. See issue 83 for the minimal counterexample.

The bounded fixture still surfaces that defect (it records the collector error and fails), so it does
not hide it. This file only bounds resources; it does not change what the test asserts about
correctness.
