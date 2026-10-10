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
- Cost when disabled is zero, because the seam, the oplog (`LOG`, `oplog_start`, `oplog_take`, `oplog_marker`), the power-loss image builder (`crashmodel`) and, since slice 5, core's root-op recorder (`fsops::rootlog`) are compiled out by `cfg(feature)`.
  Before PR 261 (issue 247) this was not true: `Io::write_at` copied every pack write into a `LogOp` before checking whether a log was active.
  Ops are now built lazily inside `log_data`, which is a no-op without the feature, and `write_at_allocates_nothing_without_a_log` pins it with a counting allocator.
  The `LogOp` enum is not gated, but it is only a type: nothing constructs it when the feature is off.
- `scripts/check-fault-seam-absent.sh`, run as a CI step, builds the library crates in a separate target dir without the feature.
  It fails if `cowfs-daemon`'s feature graph enables the feature, or if the rlib contains the seam's env key `C7D_EXIT_BOUNDARY_N`.
  It then rebuilds with the feature as a positive control and fails if the key is not found, so the check cannot be blind.
  The release `cowfs-daemon` is also searched for `C7D_EXIT` and `oplog_start`; the feature-on hit is the positive control for the pair, and `oplog_start` has none of its own because an unused item is dropped from a release binary.
  `crashmodel` is not searched: the daemon never links it even with the feature on (0 hits), so a grep would pass whether or not the module were gated.
  The compiler is that gate: `crashmodel` imports `LogOp`, whose re-export is `cfg(feature)`, so removing the cfg from `pub mod crashmodel` fails a feature-off build with E0432 (issue 276, tried on the box).
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

Issue 276 follow-ups, killed by the same harness (`MUT_ARGS="--test crash --test power_discard"`):

- E7, `discard` skips its leading `sync()`: killed by `discard_makes_earlier_puts_durable`.
  A cut inside the discard cannot show it, because the base is already durable; the test puts blocks after the last sync and checks the end state (`crash_image` takes `k == ops.len()` for "after the last op returned").
- E8, `acknowledge_corruption` skips the directory fsync after dropping `index.cix`: killed by `dropping_the_stale_checkpoint_is_followed_by_a_directory_fsync`, a static ordering check.
  The sweep `power_loss_at_every_op_of_acknowledge_corruption_keeps_live_blocks` now reaches that path, but it cannot kill E8: `open` re-validates a resurrected checkpoint against the packs and ignores one that names a missing pack, so both outcomes of the removal are safe.
  The removal is defence in depth, and its fsync is pinned by order, not by a loss.

M2 is also killed by the process-crash test `a_crash_at_every_step_of_a_discard_leaves_the_store_clean` now that the unlink is a boundary.
Not done: `LogOp::Rename`, because the only renames in the store are inside `write_whole`, which the model already treats as one atomic `Whole` write.

## Slice 4: power loss over a whole `Gc::collect`

`crates/cowfs-gc/tests/power_collect.rs`, test-only, behind the dev-dependency `fault-injection` edge; no production code, public API or on-disk format changes.
One thread-local op log carries three timelines in one order:

- the store, rebuilt by `crashmodel::crash_image` at every op index of a real reclaiming collect;
- the metadata database, on a recording redb `StorageBackend` passed to `Meta::open_with_backend`; each backend event stamps a marker (`1 << 40 | n`) into the store log, and the image at a cut is the base plus every event up to the last completed `sync_data` (all unsynced redb writes lost);
- the collector's state directory (`atime.bin`, `mark.bin`), as any mix of old file, new file, torn prefix and no file.

The meta database runs with the mount's `before_sync` hook (store sync before metadata commit), and the recorded cycle contains two commits on purpose: a `late` snapshot whose file is committed by the freeze, and a `mid` snapshot created after the freeze listing and committed by the sweep's fresh listing; a final `meta.sync()` after the collect plays the mount's background commit.
Both snapshots name blocks that are garbage in the store until they do, so a lost commit-before-unlink ordering shows as a missing block.
Each image is reopened (store, meta, collector) and must: report no store loss, hold no whole-pack acceptance of a pack still on disk, pass `meta.check`, read back byte for byte every block of every durable snapshot, pass `fsck`, and survive a second collect with no error and the same receipts.
Fail-closed asserts: some image lost a source pack and some kept all; the `late` and `mid` files are durable in some images and missing in others.
`C173_SEEDS=n` sets the seeds (default 8; about 1100 images, 15 s on the box).

Collector state tolerates the states a power cut leaves (old, torn, missing), so it is not routed through the store's fsync model.
`gc_state_is_advisory` is the evidence: 64 seeds of every old/new/torn/missing mix over the final disk never lose a block or fail a collect, and a snapshot untouched since phase A (`frozen`) makes the next cycle read the persisted mark cache.
It is advisory against same-length corruption too: `mark.bin` ends in a BLAKE3 hash of everything before it (`COWMARK4`), and a mismatch discards the cache so every root is walked in full (issue 288).
`mark_bin_bit_rot_is_rebuilt` zeroes a 64-byte span of `mark.bin` over 64 seeds and checks no block is lost; it was the ignored `mark_bin_bit_rot_loses_live_blocks_KNOWN_BUG`, which lost live blocks on 58 of 64 seeds.
`atime.bin` needs no hash: it only orders work and never decides what is freed.
The shipped fsyncs make power loss safe; the hash covers bit rot and partial overwrites.
`GA1` (no file fsync) and `GA2` (no directory fsync) survive because the test already explores every torn, old or missing state those fsyncs could change, so they are equivalent here, not because the state is harmless.

Mutants (`MUT_PKG=cowfs-gc MUT_ARGS="--test power_collect" python3 crates/cowfs-store/tests/mutate.py ...`):

- GM1, `collect` discards the new pack instead of the source: killed.
- M2, unlink before the watermark raise: killed.
- N1, no new-pack fsync: killed.
- D1, no packs directory fsync after the unlink: killed (through the whole-pack acceptance check).
- Survivors, with the reason: W1 and E7 (watermark before data fsync, no leading sync) need a writer with unsynced puts, which a collect does not have; the store sweeps kill them.
  GF1 and GF2 (no metadata sync at the freeze, none at the fresh listing) are equivalent mutants: `live_blocks_with_root` runs `inner.sync()` itself (db.rs:2141), so the walk commits regardless.
  Only the combination GF1+GF2+walk-sync is observable, and only with the test's `mid` hook removed (8 of 992 images, per the critic); that defence in depth is pinned by no test.

Not covered: metadata unsynced writes surviving a cut (only the durable prefix is modelled; redb's own recovery is `cowfs-meta/tests/crash.rs`), the core-level timeline (slice 5, below).

## Slice 5: power loss over a whole `Core` workload

`crates/cowfs-core/tests/power_core.rs`.
New test file; `crash.rs`, `durability.rs`, `ns_durability*.rs`, `kill9.rs` and `swap.rs` are unchanged and stay.
One thread-local op log carries four timelines in one order:

- the store, rebuilt by `crashmodel::crash_image` at every op index;
- the metadata database, on the recording redb backend of `Meta::open_with_backend`, stamped into the log as in slice 4; at a cut the image is the base plus the events up to the last completed `sync_data` (seed % 3 = 0), or every event that happened (1), or the synced part plus a random subset whose writes may tear (2);
- the mount root's durable writes, which is the swap intent file: create, write, fsync, rename, unlink and root directory fsync, recorded by `cowfs_core::fsops::rootlog` and stamped in the same way;
- the acknowledgements (`Core::sync`, `fsync`), stamped, so a cut knows which snapshots an acknowledgement had already made durable.

Root model: file data is durable once its file was fsynced, else any prefix of it (including none); a directory entry change (create, rename, unlink) is durable once the root was fsynced after it, and each of the rest reaches the disk or not on its own (a rename of a missing source does nothing).
That is the same subset rule as the store model, and it is more pessimistic than a journalling filesystem, which writes directory changes in order.
The op stamped at the cut may or may not have run.

Workload, one thread, `background: false` for Core and for meta:

- scripted: create, overwrite, rename and unlink files and fsync one; fork, remove and rename snapshots; a replacing `promote_base` (victim acknowledged, source with unsynced files) and a non-replacing one; a reclaiming `Collector::collect`; syncs;
- seeded, in the spirit of `crash.rs`'s `step` (`COWFS_POWER_WORKLOADS` workloads, default 2): create, overwrite at an offset, truncate, rename over an existing name, unlink, fsync, fork and remove a fork, flush, on files of whichever snapshot is not under a swap check.

Size: 280 to 410 store ops, about 190 metadata events, 14 root ops and 5 or 6 acknowledgements per workload, 5 seeds per cut, so about 3600 to 4200 images.
Timings of the sweep test (4 threads, 5 seeds per cut, 2 workloads), measured in the CI run of PR 310: 198 s on the macOS runner (negative control 89 s) and 90 s on ubuntu (negative control 41 s on the other shard); the cachyos box takes about 70 s.
PR 299 saw 74 s on ubuntu and 212 s on macOS, so ubuntu runners vary by about 20 percent between runs.
On a slow runner turn the knobs down: `COWFS_POWER_SEEDS` (images per cut, default 5), `COWFS_POWER_WORKLOADS` (default 2) and `COWFS_POWER_THREADS` (default 4).
The fail-closed assert needs more than 1000 images, and a workload gives about (ops + 1) x seeds of them with 280 to 410 ops, so keep at least 1 workload with 5 seeds or 2 workloads with 3 seeds; fewer can fail the assert without any bug.
Only workload seeds 0 and 1 run in CI.
The op count has two modes, which differ from the very first op: Core iterates a `HashMap` of snapshots with a per-process random seed, so the order in which it flushes two snapshots changes.
A failing tag (`w=<workload> k=... seed=...`) is therefore not reproducible by number.
What a failure keeps instead: the first four failing images, under `target/tmp/power_core` (`CARGO_TARGET_TMPDIR`), their paths in the `FAIL` lines, and `trace-w<workload>-<pid>.txt` next to them, the whole recorded timeline (every store op by the `k` of the tag, the root ops with their position, the acknowledgements), which is also printed to stderr after the `FAIL` lines so the CI log carries it (nothing uploads the directory).
An image opens with `Core::open`; the `k` of a tag names a line of the trace.
The fail-closed asserts hold in both modes: 14 consecutive runs at 2 seeds per cut all passed, with 355 to 412 ops.

Each image is reopened with the shipped `Core::open` and must:

- open, report no store loss, pass `Core::check` and `fsck`;
- hold no staging snapshot and no intent file after recovery;
- show every acknowledged snapshot with every unchanged file byte for byte, and no file that an acknowledged unlink or rename removed (the check runs both ways), and no snapshot the workload could not have created;
- show a swap or rename target as exactly its old tree (less what was changed in it since) or the new one, and a renamed snapshot under at least one name;
- accept a second operation, a new snapshot with a file and then a `promote_base` of it, that survives a further reopen.

Fail-closed asserts: the timeline is long enough, the gc cycle unlinked a pack, the swaps left the intent ops, some images hold an intent file, and swap targets come back both as the old and as the new tree.
`the_recorded_log_replays_to_the_real_disk` cuts after the last op of a workload that ends in a sync and checks that the store log replays to the real packs, the metadata log to the real file and the root log to the real directory, so an op that went round the log (for instance on another thread, which a thread-local log cannot see) fails there.

Negative control: `the_power_test_notices_a_missing_store_sync_before_metadata_commits` runs the same machinery with Core's store-sync hook removed and asserts that some image fails with `MissingLiveBlock`; about half of the images do.
The memo's second falsifier does not apply: the order needs no lock, because the log is one thread's.

Production code touched, which the memo's "route the intent writes" requires:

- `cowfs-core/Cargo.toml`: feature `fault-injection` (forwards the store's) and the self dev-dependency that enables it for the crate's tests, as the store does;
- `src/fsops.rs`: `create_with`, `rename` and `remove_file`, thin wrappers over the `std::fs` calls `swap.rs` made, which log only under the feature, and the feature-gated `rootlog` module;
- `src/swap.rs`: its intent-file calls use those wrappers.

Behaviour, on-disk format and public API of a normal build are unchanged.
`scripts/check-fault-seam-absent.sh` now also builds the `cowfs-core` rlib with and without the feature and searches for the recorder's panic message (`cowfs-rootlog`): 0 hits without it, 1 with it; and it checks that no normal or build edge enables the core feature.
The needle was tried on the box against a scratch copy where the module and its call sites are ungated: it finds the message (1 hit) in the feature-off rlib, so the check can fail for the right reason.
It has no release-binary check of its own, because the daemon's feature forwards only the store's.
"Cost when disabled is zero" above holds for the core recorder too.

Mutants (`python3 crates/cowfs-core/tests/mutate_power.py`; the sweep test alone may kill them, `MUT_FILTER`):

| tag | mutation | result |
|---|---|---|
| E1 | Core's `store_sync_hook` does nothing | killed by the sweep (and the log replay) |
| E2 | meta runs the store-sync hook after the transaction commits, not before | killed: `MissingLiveBlock` |
| E3 | the snapshot-add commit (`fork_snapshot` and the swap's staging fork) skips the hook | killed: `MissingLiveBlock`, also inside the swap alone |
| E4 | the snapshot-rename commit (`rename_snapshot`, the swap's rename) skips the hook | killed: `MissingLiveBlock`, by `rename_snapshot` only (see notes) |
| E5 | the snapshot-remove commit (`remove_snapshot`, the swap's victim removal) skips the hook | killed: `MissingLiveBlock`, by `remove_snapshot` only (see notes) |
| I1 | intent file: no root directory fsync after its rename | killed: `durable snapshot "base" is missing` |
| I2 | intent file: no fsync of its data before the rename | SURVIVED, equivalent by design (see notes): acceptance item not met |
| I3 | `finish_swap` removes the intent file (and syncs the removal) before the staging rename | killed: `durable snapshot "base" is missing` |
| I4 | no root directory fsync after the intent removal | SURVIVED, equivalent: a resurrected intent is finished again, also after user operations on the target (see notes) |
| W1 | the store raises its watermark before the pack fsync (`sync_capture`) | killed at Core level: store lost durable data on reopen |

Notes on the results:

- E1 and E2 are the memo's "reorder the store sync before the metadata commit": Core has no separate flush ordering of its own, the order is the `before_sync` hook Core installs and the commit that calls it, so the two mutants are the hook doing nothing and the hook running after the commit.
- "Drop the store sync before the swap rename" is an equivalent mutant for the swap path.
  E4 and E5 are killed only by the standalone `rename_snapshot` and `remove_snapshot` steps of the workload: with those two steps removed from a scratch copy, E4 and E5 survive, and E3 (the staging fork, the commit before them) is still killed inside the swap, at its first intent op.
  The staging fork's commit has just synced the store, so the victim removal and the rename commits have nothing left to sync.
- I2 (no fsync of the intent file's data) is an ACCEPTANCE ITEM NOT MET: the memo expected it to fail, and it survives.
  It is equivalent by design.
  `recover_intent` handles an intent file that is cut short, empty or missing its newline, by taking the staging name from the file name and rolling forward when the target is gone.
  The evidence that the path runs: with I2 applied, 223 of 1440 images held a torn intent file, and every one recovered.
  The fsync is defence in depth against a record that is wrong but looks whole, which needs media corruption, as the comment in `swap.rs` says.
- I4 (no directory fsync after the intent removal) survives, and is equivalent: a resurrected intent is finished again, idempotently.
  The workload uses a swap target after its swap (a write and a rename in `base`; a write, an unlink and then the removal of `fresh`, all before anything syncs the root directory again), so an intent that comes back meets those user operations.
  With I4 applied, 513 of 2400 and 386 of 1860 images (workloads 0 and 1) opened with an intent file whose unlink had completed before the cut (the test counts these as `resurrected`; it only prints the count, and the structural guard `post_swap_guard` is what fails if the post-swap operations are removed from the workload), against 5 and 6 without the mutant, and every one recovered: the target exists, the staging snapshot does not, so `finish_swap` only drops the intent; where the user removed the target, neither tree exists and recovery reports it and drops the intent.
- A mutant that raises the store's watermark before the pack fsync (W1) is killed at Core level; before this slice only the store crate saw it.

Not modelled, and what covers it instead:

- A background thread.
  The thread-local log cannot see `cowfs-flusher` or `cowfs-meta-bg`, whose `before_sync` calls `Store::sync`, so the test runs both off and checks the replay.
  The memo's remedy is a per-store log (the `Trace` that `Io` already carries), which sees every thread; it was not built because the single-thread timeline already orders every durable write of these flows, and a second log would need its own replay check.
  Cutting a workload with the flusher running is what a per-store log, or the daemon sweep, would add.
- Unsynced writes surviving a cut.
  The store model lets unsynced writes survive in any subset and tear.
  The metadata model keeps the synced part, or everything, or the synced part plus a random subset that may tear.
  The root model lets each unsynced directory change reach the disk or not, and an unsynced file keeps any prefix.
  None of them is checked against a real filesystem, APFS or a real device, so no claim is made for one.
- The collector's state directory (`<root>/gc`): it is in no image, which is the legal "missing" state; old and torn states are slice 4's `gc_state_is_advisory`.
- The daemon: no `--fault-boundary` sweep was re-run for this slice, and `base_meta.rs` is out of scope as in the memo.
- `virt.ino`: nothing writes it in production (only a test helper), so it is not recorded.
- Concurrent operations: the workload is serial.
- `Vfs::sync_namespace` (names durable, dirty data not): the acknowledgement model has no "names only" ack, so it is not driven here.

What the older Core tests still own, because the new test does not replace them:

- `crash.rs`: a longer random workload over directories and hard links, with the store cut by truncating the last pack past the watermark (no unlink, rename or directory-fsync model) and a recording meta backend that still runs its background thread.
  It is the only test of hard links and directory trees under a cut.
  The new test has no directories and no hard links.
- `durability.rs`: the order and error handling of the intent and store syncs, by the `fsops` trace and injected sync failures.
  It is not a power cut, and it stays as the cheap per-call check of the same order.
- `ns_durability*.rs`: namespace durability and its cost, with the elided unlink; no cut at all.
- `kill9.rs`: SIGKILL of a child with the flusher running; the page cache survives, so it cannot see a missing fsync, but it is the only test that kills with Core's flusher thread running.

## Out of scope and next slices

- Slice 3, 4 and 5: power loss (store, gc and core parts done, see above).
  Chosen mechanism: a store-boundary write-ordering simulation, extending the existing `oplog_*` crash model (which already drops unsynced writes) to a recorded `Gc::collect`.
  It proves the store's fsync ordering under modelled filesystem rules, not that a real kernel, NFS or disk honours fsync.
  Real power loss needs the VM power-cut harness (`qa4-vm.sh` and `qa5-vm.sh` show the shape); out of scope.
- Process-exit evidence cannot show a missing fsync, because the page cache survives; no slice so far makes a power-loss claim.
- No change to `progress/plan.json`, the control protocol, GC behaviour, or `docs/verification/daemon-crash-acceptance.md`.
