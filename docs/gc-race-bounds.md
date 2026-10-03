# Bounded GC race fixtures (issue 83)

`crates/cowfs-gc/tests/race.rs` runs writers, snapshot create/remove, and collects at once.
Its concurrent fixtures are now bounded, for the reason in issue 83: on a runner where a collect
stalled, the writer loops kept appending until the disk filled (`StorageFull`, "No space left on
device"), and the workspace step ran for about five hours.

## The two bounds, stated exactly

There are two different bounds, and only one of them is hard.

**Cooperative in-process bound** (`race.rs`):

- `WRITE_BYTE_BUDGET` is 24 MiB **per test, shared across all of that test's writers** (`clone_state`
  shares one counter), not per writer. It counts input bytes only (20000 or 8000 per write), not the
  stored bytes, which also include compaction copies and the metadata file. Overshoot is at most one
  write per writer.
- Two tests carry a budget, so the in-job input bound is about 48 MiB plus overshoot. This is an
  input bound, not a claim about physical bytes on disk.
- `WRITE_ITER_CAP` (100000) is never the binding limit at the default write size, so it adds nothing
  beyond a backstop.
- The phase is asked to stop at `RUN_CAP + 5 s` (9 s); the doc previously said 4 s, which is only the
  half of the expression. The writers test has no other time stop; the stall test also stops when its
  4 cycles finish.
- Any worker or collector error sets the stop flag and records the failure; the phase then asserts on
  that failure instead of panicking inside a spawned thread.
- Each run prints the bytes, writes, cycles and barriers it actually reached, and asserts they are
  non-zero, so a budget that silently starved the workload fails loudly rather than passing empty.

This whole bound is **cooperative**: it sets a flag every loop checks. It bounds what the writers do,
but it cannot end a test whose collector parks forever inside a blocking call, because the collector
runs inside `std::thread::scope` and the scope joins it. Reproduced: with the collector parked in a
seam, the test ran until an external 40 s alarm killed it (`rc=142`), despite the 9 s cooperative cap
and the 60 s no-progress timeout.

**Hard process-level bound** (`tests/resource_watchdog.rs`), for the resource-sensitive fixture:

- The fixture runs in a child of the same test binary. The parent holds a fixed deadline of its own
  and polls `try_wait`, so it detects a prompt non-zero exit and otherwise ends at the deadline.
  The parent's deadline does not depend on the child's own joins, cancels or progress flags.
- At the deadline the parent kills **only the child it spawned**, after re-checking the child's pid
  and command; it never signals a process group and never touches any other process.
- The child's stdout and stderr are piped and drained on threads, and the child appends and fsyncs a
  phase log, so a hard kill still leaves the phase it reached.
- A recursion guard makes the spawned process run the fixture body instead of spawning again, and the
  parent requires the child's evidence line, so a filter typo that makes the child run nothing cannot
  pass.
- Three controls run: a happy child that reclaims real packs and reads a survivor back after a
  reopen (the parent accepts it); a child that parks its collector forever (the parent kills it at
  the deadline and the parent FAILS, never PASS or SKIP); and a child whose writer fails (it exits
  non-zero promptly and the parent fails with its log). The parked case is the exact situation the
  cooperative bound cannot end.

The `24 MiB` and `9 s` figures are the cooperative in-process knobs, not the hard runtime bound. The
hard bound for the wrapped fixture is the parent's declared deadline.

## What did not change

- The adversarial structure is preserved: a shared `base` snapshot, forked per writer, with a
  concurrent create/remove/reap thread and a collect loop, and every collect's live set re-read from
  the store.
- No timeout was loosened and no failure is ignored. The parent deadline fails (never skips) on a
  hang.
- Coverage, observed on the committed fixture (reviewer evidence, issue 83): the writers test did 8
  to 26 writes and 1 to 2 collects per run, and the stall test 2 of 4 cycles, because the collector
  loop starves the writers and each cycle is slow. The per-collect "every live block reads" check
  therefore ran once or twice. The floors are `writes > 0`, `cycles > 0` and
  `barriers > 0`; that is a low-work floor, not a throughput claim.

## The barrier-stall assertion

The stall fixture is a **functional** concurrency check: the barrier is taken at least once, the
writers and the collector both make progress, and a collect causes no loss (every live block reads
back). It is **not** a latency or fairness gate.

An earlier version asserted `held_us < collect_us`, on the theory that a collector holding one
barrier to the end of the sweep could not pass it. That was reproduced false: a mutant that takes the
barrier once and holds it for the whole sweep (199 writes, 4 cycles), and a mutant that takes it once
and holds it for the whole cycle (8 writes, 4 cycles), both pass it, and both pass the 30 s backstop.
The ratio does not discriminate the hand-off property, so both the ratio and the backstop are gone,
and no hand-off or latency claim is made from this fixture. The hand-off itself is covered by the gate
unit test in `cowfs-core`, not by this wall-clock test.

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
