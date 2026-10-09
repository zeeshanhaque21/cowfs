# Issue 88 and gate g6 status, 2026-10-08

Scope: issue #88 (real daemon crash recovery with durable receipts) and gate g6 (crash-injection: zero data loss, no torn tree).
Method: read the issue body, `progress/plan.json` items 88 and g6, `docs/verification/daemon-crash-acceptance.md`, `docs/reviews/crash88-*.md`, PR 96 and issue 90, then ran the harness on current main.
Nothing in `progress/plan.json` was edited.

## Verdict

Harness acceptance bullets: MET on current main, as scoped process-crash evidence.
Gate g6 "zero data loss in crash-injection": MET for the sampled process-crash windows on this Mac.
Gate g6 is NOT MET for power loss, mid-GC crash and the internal fsync-to-watermark orderings, because none of those has a public boundary to drive.
Issue #88 is still OPEN and the tracker still says blocked.
That tracker text is stale on one point and correct on another, detailed below.

## Stale-issue trap

The tracker says the gate is blocked by "rename lost after successful directory/read-only fsync".
That defect was issue #90.
Issue #90 is CLOSED.
It was repaired by PR #96 (merge 951045f, an ancestor of current main): `Adapter::durable` calls `Vfs::sync_namespace` before every name or attribute change is acknowledged.
Evidence: `docs/verification/namespace-durability90.md` (3 of 3 reps kept the rename for dir fsync, read-only fd fsync and no sync at all) and `docs/verification/evidence/namespace90/`.
`docs/verification/daemon-crash-acceptance.md` is stale: it still reports 6 of 29 failing and "the source fix is not in this branch", measured at 90c9a8f before #96.
The 29-case matrix was never re-run on post-#96 main until this report.

## Status against the issue's acceptance bullets

| bullet | status | evidence |
|---|---|---|
| committed runnable harness | MET | `scripts/verify-daemon-crash.py`, merged via PR 91 (03bbec8); `bench/test_daemon_crash.py` 118 tests OK (skipped=1) at 04bbdd0 |
| complete validated small sample | MET | run `s88a` below: 2 of 2 pass, receipts compared with readback |
| explicit durability receipts | MET | 52 durable, 18 applied, 2 removed receipts in the matrix; receipts are append-only and never demoted |
| fresh-reopen source-hash verification | MET | every durable receipt sha256 compared with the file read through a fresh daemon on the same store: 52 of 52 match |
| native control results | MET, weak by construction | 1 native case passes; a process kill on APFS cannot expose a missing fsync, so it validates the recipe and readback only |
| independent review | MET for the harness | `docs/reviews/crash88-mount-parser-final.md`, `docs/reviews/crash88-doc-corrections-final.md`: source and doc PASS, CI green; reviews predate #96 |
| pid, command, store, socket verified before every signal | MET | `kill.verified_target` before each SIGKILL; `kill.refused_foreign_pid` selftest passes |
| distinguish unacked loss, acked loss, structural corruption | MET | `applied` may be absent, `durable` must match, `fsck` per case |
| ordinary writes, snapshot fork, rename, fsync, bounded GC | MET as sampled | matrix cases below |
| report which windows sampled and untested | MET | the acceptance doc lists them; restated below |
| no production fault APIs or semantics changes | MET | harness-only change; #96 is a separate reviewed fix |

## Re-measured on current main

Worktree: `.treehouse-ci/.treehouse/cowfs-7c1bf8/6/cowfs`, branch verify/crash88-sample, HEAD 04bbdd0ec430eecd606dc55acd64d98f7d3a4611, which contains PR 96.
Build: `CARGO_BUILD_JOBS=3 cargo build -p cowfs-daemon -p cowfs-cli`, dev profile, 1m40s.
Harness sha256 prefix 1c400c5edd34208e, the same digest the PR 91 reviews pinned.
Build artifacts, not reproducible: cowfs-daemon prefix 7ea65e2eabd608fd, cowfs prefix 46aa94a1522f06c3.

### Sample

Command: `python3 scripts/verify-daemon-crash.py --stage sample --reps 1 --run-id s88a`
Result: executed_all_passed, 2 executed, 0 reused, accounting balanced, 2 passed, 0 failed, 15.7 s.
Cases: `write_fsync` and `rename_posix_durability`.
`write_fsync`: 4 durable receipts (4096 B each, `live/d00.bin` to `d03.bin`), SIGKILL, fresh daemon on the same store, 4 of 4 sha256 equal, snapshot name present, fsck clean.
`rename_posix_durability`: receipt `live/orig.bin` sha256 3abf7c6a575263490f316e87db451d30c53cee112e8afa6765943e24fd785832, repathed to `live/moved.bin` after `os.rename` plus `fsync(parent dir)`, SIGKILL, reopen: `live/moved.bin` got equal to want (same hash, 4096 B), fsck 0 problems.
That second case is the exact boundary that failed 3 of 3 before #96.

### Matrix

Command: `python3 scripts/verify-daemon-crash.py --stage all --reps 2 --run-id m88a`
Result: executed_all_passed, 29 executed (28 cowfs and 1 native), 0 reused, balanced, 29 passed, 0 failed, 3m05s wall.
Receipts: 52 durable (52 matched, 0 missing), 18 applied, 2 removed.
fsck: 28 of 28 clean.
Per case pass counts: write_fsync 4, rename_posix_durability 4, rename_posix_durability_ro 2, rename_committed 2, write_nofsync 2, write_race 2, mixed 2, mmap 2, snapshot_fork 2, snapshot_remove 2, gc_crash 2, kill_control 2, native 1.
Before #96 the same matrix was 23 passed and 6 failed, all six at the rename plus fsync boundary.
Evidence is gitignored: `bench/out/crash88/s88a` and `bench/out/crash88/m88a` in the worktree above.
This is a finite matrix of 2 reps, scoped evidence and not a proof over all crash windows.

## Remaining gaps, exact

1. Power loss: NOT MET and not testable here. SIGKILL does not drop the host page cache, so un-fsynced pack bytes always survive and fsync is never really exercised. Needs a VM or fault-injecting block device.
2. Mid-GC crash: NOT MET. The kill lands after the gc ack. The GC case frees 0 bytes (`freed_bytes` 0, 164 and 167 candidate blocks) because a small fixture lives in the open pack. Real reclamation with sealed packs is only covered by `docs/verification/gc-daemon-e2e.md`, not crash-injected. Owned by builder 164, not touched.
3. Internal orderings (pack fsync to watermark advance, watermark to metadata commit): NOT MET, no public edge exists and fault APIs are forbidden by the issue.
4. Concurrent writers and shutdown as a crash boundary: not sampled.
5. Stale docs: `docs/verification/daemon-crash-acceptance.md` verdict table and "What is still needed" still say BLOCKED and must be updated with the post-#96 matrix. `progress/plan.json` item 88 and g6 text is stale for the same reason. Both are for the coordinator, since the plan is off limits for me.
6. Crash-injection harness reviews predate #96, so no independent reviewer has seen the 29 of 29 result.
7. This run is a local macOS result (Darwin 25.6.0, real NFSv3 loopback), not a CI result.

## Recommendation

Update the acceptance doc and plan with the 29 of 29 post-#96 result (needs a doc PR and a short independent review), keep g6 open with gaps 1 to 3 named, and close #88 only if the coordinator accepts process-crash scope as the issue's own text allows ("passing a finite matrix is scoped evidence").

## Isolation

Private store and mount under the worktree's `bench/out/crash88/`, own socket dir under `/private/tmp`, removed by the harness.
Every signal went to a harness-spawned pid whose command line carried its own store and socket.
After the runs: no mount path containing `crash88` remains, no daemon from the `/6/cowfs` worktree remains, and the shared daemon pid 15263 was not touched.
Other cowfs mounts and daemons seen belong to other agents (the treehouse tests under `/5/cowfs` and tmp fixtures) and were left alone.
g1 and g2 performance gates were not run.
