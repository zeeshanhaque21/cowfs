# Crash injection seam for gate g6 (issue 173)

Status: slice 1 (crate-level process-exit evidence) merged in PR 183.
Slice 2 (daemon-level reach) is on branch `feat/crash-injection-173-daemon`.
Refs: issue 173, issue 88, `docs/reviews/issue88-status-20261008.md`, `docs/verification/daemon-crash-acceptance.md`.

## Problem

Gate g6 asks for zero data loss and no torn tree under crash injection.
The public SIGKILL harness (`scripts/verify-daemon-crash.py`) reaches only windows a client can see.
Three gaps stay open.
Gap 1: the kill in the `gc_crash` case lands after the gc ack, and that case frees 0 bytes, so no sealed pack is ever rewritten or unlinked under a crash.
Gap 2: the internal orderings (pack fsync, then watermark, then metadata commit) have no public boundary.
Gap 3: power loss, which SIGKILL cannot test because the page cache survives the kill.
Issue 88 forbids fault APIs in the public control surface.

## What already exists (reused, not reinvented)

`cowfs-store` already has a cargo feature `fault-injection` (off by default, never named by a release profile).
It is switched on for `cargo test` by a self dev-dependency in `crates/cowfs-store/Cargo.toml`.
Under that feature `crates/cowfs-store/src/fsio.rs` counts every durability boundary (write, sync, dir sync, rename, truncate) and exits the process with status 77 when `C7D_EXIT_BOUNDARY_N`, `C7D_EXIT_SYNC_N` or `C7D_EXIT_FILE` plus `C7D_EXIT_LEN` match.
Tests in `crates/cowfs-store/tests/compact.rs`, `round4.rs` and `round5.rs` drive it by re-executing the test binary as a child.
`oplog_start`, `oplog_marker` and `oplog_take` record writes and fsyncs per thread.
The oplog (the thread-local `LOG` and those three functions) is itself compiled only with `fault-injection`; a normal build keeps a no-op `log_data` and the `LogOp` enum type it names, but no log, no recording entry points and no per-write allocation (ops are built lazily).
`tests/crash.rs` uses it and `tests/crash.rs` rebuilds a disk image that drops unsynced writes.
`crates/cowfs-gc/tests/kill9.rs` shows the child-process pattern for the collector, and `crates/cowfs-gc/tests/crash.rs` builds a crash image at each compaction step by driving the store API by hand.
None of these crashes the real `Gc::collect` cycle with real reclamation.

## Mechanism (revised: no new production code)

The first draft added a named `crash_point` function.
The advisor review found that the existing boundary hook already exits inside real compaction and discard, so named points would be a second seam reaching no extra state.
Slice 1 therefore adds no production code.
A process crash changes disk state only at the store's `Io` boundaries.
The unlink is a boundary since slice 3 (`Io::remove_file`), but a process exit keeps the page cache, so "unlinked, dir not yet fsynced" is the same disk state as "dir fsynced".
Sweeping `C7D_EXIT_BOUNDARY_N` over every n from 1 upward covers every store `Io` boundary of the cycle, which hand-placed names would not.
It does not crash between the metadata sync at the freeze or the collector's own `gcstate` writes, because neither goes through `Io`.
Named points would be justified only for an edge with no `Io` boundary of its own; within the store none was found, and the metadata and gc-state edges are outside this seam.
The metadata-commit edge of gap 2 is likewise outside it, because `cowfs-meta` does not go through `Io`.

The only change is `cowfs-gc`'s `[dev-dependencies]`, which enables the store's existing `fault-injection` feature for that crate's tests, the same way the store enables it on itself.
No release profile or `[features] default` enables it.

Constraint from issue 88:

- Nothing is added to the control protocol, daemon, CLI or any public type.
- Cost when disabled is zero, because the seam, the oplog (`LOG`, `oplog_start`, `oplog_take`, `oplog_marker`) and the power-loss image builder (`crashmodel`) are compiled out by `cfg(feature)`.
  Before PR 261 (issue 247) this was not true: `Io::write_at` copied every pack write into a `LogOp` before checking whether a log was active.
  Ops are now built lazily inside `log_data`, which is a no-op without the feature, and `write_at_allocates_nothing_without_a_log` pins it with a counting allocator.
  The `LogOp` enum is not gated, but it is only a type: nothing constructs it when the feature is off.
- `scripts/check-fault-seam-absent.sh`, run as a CI step, builds the library crates in a separate target dir without the feature.
  It fails if `cowfs-daemon`'s feature graph enables the feature, or if the rlib contains the seam's env key `C7D_EXIT_BOUNDARY_N`.
  It then rebuilds with the feature as a positive control and fails if the key is not found, so the check cannot be blind.
  The release `cowfs-daemon` is also searched for `oplog_start` and `crashmodel`; those two have no positive control of their own, because an unused item is dropped from a release binary.
  It is a separate cargo invocation because `cargo test --workspace` turns the feature on through the dev-dependency edges.
- Known pre-existing caveat, unchanged: `cargo build --workspace --all-targets` unifies the feature onto the lib (see `docs/verification/gc-daemon-e2e.md`).
  The CI workflow has no release build step, so the library graph above is the artifact checked.

## Slice 1 test

`crates/cowfs-gc/tests/crash_inject.rs`, test `a_process_exit_at_every_boundary_of_a_reclaiming_collect_loses_nothing`.
The test re-executes its own binary as a child, the pattern of `kill9.rs`, and passes the environment only through `Command::env`.
Parent: seed a store whose sealed packs each mix dead and referenced records, so a collect must copy before it unlinks; record every live block id and its bytes as the receipt; sync; persist as a template.
Control: an uncrashed child must exit 0, free more than 0 bytes, unlink at least 2 packs, and leave every receipt intact and `fsck` clean.
Sweep: for n from 1 to a cap, copy the template and run the child with `C7D_EXIT_BOUNDARY_N=n`.
Require exit status 77 (any other status fails), then in the parent reopen the store and check: no corruption, every receipt reads back with exactly its bytes, `fsck` clean, and a second collect finishes with no errors and the receipts still intact.
The sweep stops at the first n whose child exits 0, then checks the count is stable (n-1 still dies, n+1 still finishes).
The raw pack directory is read before reopening, because an open ignores a file below the watermark floor.
At least one crash image must be missing a seed pack, which proves a crash landed after an unlink with every receipt intact.

Failing-first evidence: with the dev-dependency line absent, `cargo test -p cowfs-gc --test crash_inject` fails with "the sweep crashed only 0 times", because the child never exits 77.
The line is what makes the crate-scoped run work.
Under `cargo test --workspace` (what CI runs) the store's self dev-dependency already enables the feature, so the workspace run alone would not show this failure.
Negative control by mutation: making the collector unlink the new pack instead of the source (`discard(rw.to, ...)`) fails the test with "live block lost ... pack file missing".
The same mutation was not caught while the fixture held only fully dead packs, which is why the fixture mixes live and dead records.
A mutation that moves the unlink ahead of the durable watermark raise is NOT caught.
The reason: a boundary fires after its operation, and an unsynced write is visible after a process exit, so the first boundary after an early unlink already sees the watermark written.
This test therefore could not catch a regression that moves the unlink earlier.
Slice 3 added the `fault_boundary("unlink")` this note asked for, and `power_discard.rs` covers the ordering under power loss.

## Slice 2: the daemon dies during a real gc sweep

Closes gap 1 at daemon level.
`cowfs-daemon` gets a cargo feature `fault-injection = ["cowfs-store/fault-injection"]`.
It adds no code, no control-protocol field and no CLI flag, and no default or release build enables it.
`scripts/verify-daemon-crash.py --fault-boundary N` (or `A-B`) builds that daemon into a private target dir (`target/fault-daemon`).
It refuses to run if that binary has no `C7D_EXIT_BOUNDARY_N` string or if the normal daemon has one.
Only the daemon that gets killed (d1) is the fault build, and the env var is set only on it.
The fixture builder and every restart run the normal `target/debug/cowfs-daemon`, so recovery is proven by the shipped code path.
Any inherited `C7D_*` key is dropped from every daemon's environment.
The matrix `gc_crash` case is unchanged and still frees 0 bytes, because everything lives in the open pack and a 288 MiB fixture per matrix rep is too costly.
The mid-gc reach is a separate stage with its own fixture.
The fault stage uses a new fixture, built once and cloned (`cp -cR`) per point.

- Four 64 KiB live files, each fsynced (durable receipts).
- Then 288 MiB of random garbage in a second snapshot that is fsynced, removed, and committed by a clean `cowfs shutdown`.
- The garbage passes the 256 MiB pack limit, so the first pack (`pack-00000000.cpk`) is sealed and holds the live records beside dead ones.
- A collect must therefore copy before it unlinks.

Per point, the harness starts the fault daemon on a clone with `C7D_EXIT_BOUNDARY_N=n` and runs `cowfs gc`.
It requires the daemon to exit 77 and the request to fail.
It then unmounts the dead mount and records the raw pack directory (names and sizes) before any reopen.
A fresh normal daemon then starts on the same store, and the harness requires all of these.

- The store opens.
- Every durable receipt matches its readback (sha256 and size), and the removed name is absent.
- `fsck` is clean.
- A second `gc` succeeds, and the receipts and `fsck` still hold after it.

The control point (no env var) must succeed and report `freed_bytes > 0`, then passes the same verification.
The stage fails closed: it fails if no point crashed inside the gc.
A sweep that reaches the completed gc also fails if no crash image still had the source pack, or if none was missing it (so the sweep must cross the unlink).

Evidence, this Mac, 2026-10-08, run id `f173-c` under `bench/out/crash88/` (gitignored), on the final code.
Earlier runs `f173-a` and `f173-b` gave the same points but restarted on the fault build, before the restart was moved to the normal daemon.

- Control: `freed_bytes` 268347942, 3588 freed blocks of 4093 candidates, 262845 bytes rewritten.
- Sample `--fault-boundary 1` ran on the final code before the sweep (run `f173-d`): exit 77 inside the gc, request failed, restart and every check passed.
- Sweep n=1..19: n=1..18 each die with status 77 during the gc and pass every check, and n=19 finishes the gc, so the cycle has exactly 18 store `Io` boundaries here.
- Startup contributes none, because n=1 already lands in the gc.
- Raw pack directory at death: the source pack is still present for n=1..13 and missing for n=14..18 (and 19, the completed run).
- So the sweep crashes the daemon both before and after the source pack is unlinked, and a restart is clean from each image.
- Failing-first: with `FAULT_GARBAGE_BYTES` at the old 12 MiB the control point fails `control_gc_freed_bytes` (nothing is freed) and the stage exits 1 with all three fail-closed messages.
- Failing-first: with the daemon feature line absent, `cargo build` refuses with "the package 'cowfs-daemon' does not contain this feature".

Absence checks, `scripts/check-fault-seam-absent.sh` (CI step, same name):

- A RELEASE-profile `cowfs-daemon` built without the feature has 0 `C7D_EXIT` hits.
- Rebuilt with `--features fault-injection` it has hits, so the grep is not blind.
- `cargo tree --workspace -e normal,build,features -i cowfs-store` shows no `fault-injection`.
- The same command with `dev` edges does show it, as the positive control.
- `cargo tree -p cowfs-daemon --features fault-injection` shows the daemon feature forwarding the store feature.

Cost: about 5 minutes of wall time for the full sweep on a busy host (load average 40), one daemon and mount at a time.
The template is built once per invocation, in roughly 1 minute.

## Remaining

- Power loss (slice 3): process exit keeps the page cache, so a missing fsync is invisible here.
- The metadata-commit edge and the freeze `meta.sync` window: `cowfs-meta` does not go through `Io`, so no boundary exists there.
- The write-path ordering (pack fsync, watermark, metadata commit) during client writes is not swept; only the gc cycle is.
  Sweeping a writing daemon would need the fixture build to run under the env var, and the counter starts at process start.
- Mutation 2 (the unlink moved ahead of the durable watermark raise) was not caught by slice 2; slice 3 closes it, see "Slice 3" below.
- The sweep is a manual stage (`--fault-boundary 1-60`), not a CI job, because it needs a real NFS mount and a 300 MiB fixture.
- No stability check (n-1 still dies, n+1 still completes) beyond the sweep stopping at n=19.
- These counts predate slice 3.
  Slice 3 added the unlink boundary and routed the compaction copy through `Io`, so a store cycle now has more boundaries (the crate-level sweep `crash_inject` went from 73 to 88).
  The daemon sweep needs an NFS mount and was not re-run in slice 3, so n=18, n=14 and "startup contributes none" above are stale until it is.

## Slice 3: power loss over compaction and discard

Mechanism: `cowfs_store::crashmodel` (feature `fault-injection` only) rebuilds the disk a power cut at any op of a recorded run could leave.
Fsynced data survives, unsynced writes survive in any subset and may tear at 512 B or 4 KiB, a created or unlinked file keeps its directory entry change only if that directory was fsynced after it, and a whole-file write is atomic.
`Io::remove_file` logs `LogOp::Unlink`, and `LogOp::DirSync` now says which directory.
The compaction copy had been writing the new pack straight to the file, outside `Io`, so the model never saw it; it goes through `Io::write_at` now, and `the_log_replays_to_the_real_disk` fails if any write bypasses the log again.
`tests/power_discard.rs` sweeps every op index and 16 seeds (`C173_SEEDS=n`) over a real compaction plus discard, and over the discard alone.
Each image is reopened with the shipped open path and must keep every live block, report no loss, pass `fsck` and not hold a whole-pack acceptance of a pack that is still on disk.
It fails closed: at least one image must lose the source pack and one must keep it.

Mutants (`crates/cowfs-store/tests/mutate.py`, run with `MUT_ARGS="--test crash --test power_discard"` so only the power-loss tests can kill them):

- W1, watermark written before the pack fsync: killed by `crash_loops`. Its pattern was refreshed to current source.
- M2, unlink before the watermark raise (mutation 2): killed by the discard sweep.
- N1, no new-pack fsync in `finish_compaction`: killed by the compaction plus discard sweep.
- D1, no packs directory fsync after the unlink: killed by the discard sweep.

M2 is also killed by the process-crash test `a_crash_at_every_step_of_a_discard_leaves_the_store_clean` now that the unlink is a boundary.
Not done: `LogOp::Rename`, because the only renames in the store are inside `write_whole`, which the model already treats as one atomic `Whole` write.

## Out of scope and next slices

- Slice 3: power loss (store part done, see above).
  Chosen mechanism: a store-boundary write-ordering simulation, extending the existing `oplog_*` crash model (which already drops unsynced writes) to a recorded `Gc::collect`.
  It proves the store's fsync ordering under modelled filesystem rules, not that a real kernel, NFS or disk honours fsync.
  Real power loss needs the VM power-cut harness (`qa4-vm.sh` and `qa5-vm.sh` show the shape); out of scope.
- Process-exit evidence cannot show a missing fsync, because the page cache survives; no slice so far makes a power-loss claim.
- No change to `progress/plan.json`, the control protocol, GC behaviour, or `docs/verification/daemon-crash-acceptance.md`.
