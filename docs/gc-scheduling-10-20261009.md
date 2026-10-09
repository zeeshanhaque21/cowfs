# Issue #10: scheduled GC and last-accessed hints, status and decision memo

Date: 2026-10-09.
Basis: origin/main at 1ea38b1.
Scope: design memo only.
No code is changed by this memo.
Labels: (V) means read in the source at the cited line.
Labels: (U) means unverified inference.

## Issue text

Issue #10 has one sentence and no comments.
"Mark-and-sweep from snapshot roots, incremental marking over unchanged subtrees, batched last-accessed times as a sweep-candidate hint only. See docs/design.md."
The contract is `docs/design.md:39-46`, which adds "on demand and on a schedule" and "Each block records a last-accessed time, kept in memory and flushed in batches so reads do not become writes".

## Acceptance rows

| # | Row | Status | Evidence |
|---|-----|--------|----------|
| 1 | Mark-and-sweep from snapshot roots | DONE | `crates/cowfs-gc/src/lib.rs` (`Gc::collect` at :306); tests `crates/cowfs-gc/tests/core_end_to_end.rs`, `core_reclaim.rs`, `race.rs`; daemon `handler.rs:836` |
| 2 | Incremental marking over unchanged subtrees | DONE | `lib.rs:6-9` shared `Marker`, persistent per-root marks `COWMARK3` (`docs/gc-root-mark-retention.md`, issue #82 closed); tests assert `marked_skipped_roots == 1` at `crates/cowfs-gc/tests/mark.rs:210`, `:324` and `control.rs:112` |
| 3 | Net-space reporting (gross, rewrite, net) | DONE | `docs/gc-space-accounting.md`, `crates/cowfs-gc/src/report.rs`, daemon `handler.rs:415`; test `a_mixed_pack_reports_gross_removed_rewrite_and_signed_net` at `crates/cowfs-gc/tests/core_reclaim.rs:1073` |
| 4 | Last-access hint store (in memory, batched flush, never a reason to free) | DONE in the crate | `lib.rs:221` `note_access`, `:232` `flush_hints`, `state.rs:133` `coldness`, `lib.rs:446` coldest-first sort; test `crates/cowfs-gc/tests/control.rs:358` |
| 5 | On demand GC | DONE | `cowfs gc` in `crates/cowfs-cli/src/cli.rs:63`, handler `crates/cowfs-daemon/src/handler.rs:352`, backend `backend.rs:645-677` (single run, cancel, close waits); test `gc_over_the_core_reclaims_dead_packs_and_survivors_still_read` at `handler.rs:836` |
| 6 | GC "on a schedule" (`design.md:40`) | OPEN | No timer, interval flag or periodic task in `crates/cowfs-daemon/src` (`main.rs` has only `--store --backend --mount --socket --export-root`) (V by absence) |
| 7 | Hints are fed by real reads | OPEN | `note_access` has no caller outside `crates/cowfs-gc/tests/control.rs`. Searches `note_access`, `atime`, `atime.bin`, `Hints` outside `crates/cowfs-gc` find only POSIX attribute atime in `cowfs-core` (`inner.rs`, `io.rs`, `ns.rs`), which is unrelated. The `v1-gc.md:496` passage is the gc crate's own `control.rs` test (V) |
| 8 | Hints survive across cycles in production | OPEN, follows from 7 | The daemon builds a fresh `Collector` per request (`backend.rs:265`, `crates/cowfs-core/src/gc.rs:86`). `atime.bin` is loaded from `<root>/gc`, but nothing writes real data, so every pack has coldness 0 and the sort falls back to most dead bytes first (V) |
| 9 | Doc matches code on hint bonus | OPEN, small | `docs/v1-gc.md` says hints can raise a pack's effective dead ratio via `Options::cold_dead_bonus`. No such option exists in `Options` (`lib.rs:106-125`) (V) |
| 10 | Sweep candidates "unmarked blocks older than a threshold" (`design.md:42`) | OPEN, decision | The implemented policy is pack level: `dead_ratio` and `min_dead_bytes`. No age threshold exists. Recommendation below is to amend design.md, not to add one |
| 11 | Free-space or pressure trigger, resource watchdog in the daemon | OPEN, new | No statvfs or low-space logic in `crates/cowfs-daemon/src` or `cowfs-cli/src`. The "watchdog" hits in docs are test deadlock guards only (V) |

Rows 6, 7, 8, 9, 10, 11 are the OPEN set.
Rows 1 to 5 are closed by the triage in `docs/reviews/open-issues-triage-20261009.md:106-111`, which this memo confirms and refines.
The triage missed rows 7 to 9: the hint store is DONE and tested, but it is a library with no producer, so the "last-accessed hints" half of the title is not delivered end to end.

## Facts that constrain the design

1. A cycle is safe against concurrent writers by construction.
   It marks without a barrier, then takes a short reference barrier, re-walks, and only then unlinks (`lib.rs:12-19`, `docs/v1-gc.md`).
2. If the barrier or the pinned set is unavailable, nothing is freed and the cycle reports `roots_error` (`docs/v1-gc.md` ExtraRoots contract).
   The handler turns that into `ErrorCode::Busy` only when the run is a dry run or `freed_bytes == 0` (`handler.rs:379-385`).
   If a later per-pack poll fails after some packs were unlinked, the result is `Ok` with `roots_error` set.
   So a scheduler that runs while a mount is busy degrades to a no-op, not to data loss.
3. Only one GC runs at a time, and `close` cancels it and waits up to 120 s (`backend.rs:199`, `close` at `:688`).
4. A cycle copies at most `io_budget_bytes` (2 GiB default), so its duration and I/O rate are bounded (`lib.rs:112`).
5. Gross removed bytes overstate savings.
   Net is `gross - rewrite` and is signed (`docs/gc-space-accounting.md`).
   A policy that reacts to "freed" must read net.
6. The marks cache is derived data and is root specific (issue #82).
   A fresh collector per cycle is therefore correct and cheap, and a scheduler need not keep a collector alive.
7. Hints are a dead-end for liveness by design: reachability decides, the barrier re-checks (`lib.rs:21-24`).

## (a) Scheduling policy

### Options

| Option | How | Cost | Risk |
|--------|-----|------|------|
| A. Daemon-internal timer | One thread in the daemon calls `Handler::gc` every N seconds | Small: reuses the single-run slot and cancel machinery | Daemon owns policy; every deployment gets a new knob. Default OFF mitigates |
| B. External cron or launchd calling `cowfs gc` | Operator wires it | Zero code | Needs a documented unit file per OS; no access to daemon state; two runs collide on `Busy` (safe) |
| C. On-demand when free space is low | Daemon checks statvfs on the store volume and starts a cycle at a threshold | Medium: new platform call, thresholds, hysteresis | Most useful, most surprising: GC competes for I/O exactly when the disk is tight, and a cycle first writes (rewrite) before it frees |
| D. Event driven (after `snapshot rm` or `reset`) | Nudge a timer when garbage was just created | Small once A exists | Bursts of rm cause bursts of GC unless debounced |

### Recommendation

Do A, with a minimum-gap and an idle gate, default OFF, and document B as the supported alternative for people who do not want daemon policy.
Add D later as a debounce nudge into A, not as a separate path.
Do not do C in the first release.
Reason for C: a rewrite-based collector needs headroom to run (rewrite bytes land before the unlink), so firing it at low space can fail with `StorageFull` and make the problem worse.
Design.md says "on demand and on a schedule" and does not mention space pressure, so C is out of scope for #10 and becomes its own issue if wanted.

### Policy details for A

- Flag: `--gc-interval <duration>` on `cowfs-daemon`, absent means OFF.
- Minimum interval floor of 60 s so a typo cannot spin the daemon.
- First tick happens one full interval after start, never at start, so a restart loop cannot trigger a GC storm.
- Each tick calls `Handler::gc(GcParams { dry_run: false }, &OpContext::detached())`, as `handler.rs:1036` already does.
  It must not call `backend.collect_garbage` directly, because the handler is where `roots_error` becomes `Busy` and where post-rewrite failures become errors.
  No second code path means the barrier, cancel and close behaviour are inherited unchanged.
- Both "a collection is already running" and "writers did not let the collector hold still" return `ErrorCode::Busy` (`backend.rs:660`, `handler.rs:379`).
  The first slice treats both the same: log at info and back off.
  Telling them apart would need a new error detail and is not worth a protocol change.
- On `Busy`, back off exponentially (interval x2, capped at 8x) until one cycle completes.
  This is the backpressure rule: a busy mount slows GC down rather than being pushed on.
- Skip the tick if the last cycle's net reclaimed was at or below zero and nothing changed since, which is detectable from the store block count and snapshot count.
  This avoids rewriting a store that has no garbage.
  Marked (U): the "nothing changed" test needs a cheap generation number; the first slice can use only the backoff and leave this out.
- Rely on `io_budget_bytes` for I/O pressure.
  Do not add a second throttle in the first slice.

### Safety while mounts are in use

- Nothing new is needed for correctness: the barrier already handles live writers (facts 1 and 2).
- Open files: open orphans and uncommitted chunk lists are the `pinned_blocks` set, polled several times per cycle and unioned.
- Snapshots: every live snapshot is a root, so a snapshot in use by a mount cannot lose a block.
  A `snapshot rm` during a cycle is covered by the re-walk under the barrier, and `crates/cowfs-gc/tests/race.rs` runs snapshot removes against collects (V for the test, U that the tests cover every ordering).
- Leases: treehouse leases appear to be snapshots created through the control API (`crates/cowfs-treehouse/src/ctl.rs:123`), so they would be roots (U).
  A released lease followed by `snapshot rm` is what creates garbage, which is why D (nudge) is the natural trigger.
- Pause and resume: the scheduler tick should use the existing cancel flag.
  Pause is "do not start a tick", implemented as a daemon-wide atomic that a control command could set later.
  Resume is the same flag cleared.
  Not needed in slice 1.
  Cancelling a running cycle is already supported and leaves the store consistent (`docs/v1-gc.md`, cancel bounds work started).
- Shutdown: `close` cancels and waits, so the timer must stop first and must not start a new cycle after `closing` is set.
  `collect_garbage` already returns `Busy` once `closing` is true, which makes this safe even if the timer races.

### Failure modes

| Failure | Behaviour | Required test |
|---------|-----------|---------------|
| Barrier unavailable | `Busy`, nothing freed, back off | Scheduled tick over a backend that reports `roots_error` doubles the gap |
| Cycle errors after rewrite | Reported with real gross, rewrite, net (`handler.rs:395-413`) | Reuse `FailingGc` fixture |
| Daemon stops mid cycle | Cancelled, `close` waits | Reuse existing close test, add timer stop |
| Disk full during rewrite | Cycle fails, partial pack indexed as data, next cycle retries (`docs/v1-gc.md`) | Existing; assert the timer does not retry faster than the backoff |
| Timer thread panics | Daemon continues serving, GC stops, logged | Test that a panic in a tick is caught and does not take down the control server |
| Clock jumps | Use a monotonic clock (`Instant`), never wall time | Unit test with injected clock |

### What this memo rejects

- A GC that decides by age alone.
  Design.md says sweep candidates are "unmarked blocks older than a threshold".
  The implemented sweeper works on packs and reachability and already satisfies the safety half.
  An age threshold adds a second liveness-like input that must never decide anything, so it buys nothing.
  Amend design.md to say candidates are packs whose dead fraction passes a threshold, ordered coldest first.

## (b) Last-accessed hints

### Semantics

- A hint is a pair (block id, epoch second) meaning "this block was last returned to a reader at about this time".
- It is advisory.
  Absence of a hint means unknown, treated as cold (`state.rs:137-139`).
- The only consumer is the sweep order: coldest pack first, where pack coldness is the mean hint of its records (`state.rs:133-143`).
- Hints never remove a block from the live set and never add one.
  This is already enforced by structure: the live set is built from roots before hints are consulted, and the barrier re-walk runs after the sort.

### Where the data lives

- In memory in the long-lived process that serves reads, in a map capped at `max_hints` (1,048,576, about 36 MB at 36 bytes per entry plus map overhead (U)).
- On disk in `<root>/gc/atime.bin`, the existing append-only 36 byte record file.
  No format change.
- It is not in redb meta.
  Putting it in meta would turn reads into transactions, which is the thing design.md forbids.
- Because the daemon makes a fresh `Collector` per request, the in-memory map must be owned by something that outlives a collector.
  That owner is the `Core` (or `CoreBackend`), which holds a shared `Hints` and hands a clone to each `Gc::open`.
  This is the one structural change needed, and it is internal.

### Hot read path cost

Target: zero allocation, no lock contention that a reader can feel.

- The recording call sits where a chunk is fetched from the store in `cowfs-core`.
- Do not take a global mutex per read.
  Use a sharded or per-thread buffer of ids that is drained into the map by a periodic flusher, or one relaxed atomic "dirty epoch" per block cache entry if a block cache exists (U: not checked).
- Simplest option meeting the target: a small fixed array of shards, each a `Mutex<Vec<BlockId>>` or `try_lock` that drops the hint on contention.
  Dropping on contention is correct, because a lost hint is a lost optimisation (`docs/v1-gc.md`).
- Coarse time: store the second, and skip the record if the block was already recorded within the current minute, to keep write amplification of the map low.
- The flusher thread appends to `atime.bin` every N minutes and at shutdown, not on each read.
- Acceptance bound for the hot path: read throughput of the existing benchmark (`docs/v1-benchmarks.md`) within noise (target under 1 percent) with hints on versus off, measured on the cachyos box, N at least 5 runs, with a do-nothing baseline.

### Correctness rules a critic can check

1. A test that sets every hint to "hot" for a dead block and asserts the block is still freed.
2. A test that sets every hint to "cold" for a live block and asserts it is not freed.
3. A test that corrupts or truncates `atime.bin` and asserts the cycle gives the same freed set as with no file (extends `control.rs`).
4. A test that drops all hints (cap 0) and asserts the same freed set.
5. A read-path test asserting a read never performs a file or meta write (extends `control.rs:358`).

## (c) Options, recommendation, slices

### Option set for the whole of the OPEN work

| Option | Delivers | Cost | Verdict |
|--------|----------|------|---------|
| 1. Do nothing, amend design.md to "on demand" and say hints are library only | Rows 6 to 10 closed by decision | Docs only | Honest but leaves a disk-growth problem for long-lived daemons and ships dead code |
| 2. Scheduler only (A), amend design.md on hints | Rows 6, 9, 10 | Small | Good first step |
| 3. Scheduler (A) then feed hints | Rows 6 to 10 | Small then medium | Recommended |
| 4. Everything plus space pressure (C) | Adds row 11 | Large, risk of making full disks worse | Reject for now, separate issue |

### Recommendation

Option 3 in three slices, each independently shippable, each default OFF or default no-op, none changing an on-disk format.

#### Slice 1: docs reconciliation (no code)

- Edit `docs/design.md` GC section: candidates are packs by dead fraction, ordered coldest first, not "blocks older than a threshold".
- Edit `docs/v1-gc.md`: remove or implement `cold_dead_bonus` (recommend remove, YAGNI).
- Acceptance: grep for `cold_dead_bonus` in docs and code returns nothing, or both agree.
- Acceptance: design.md and v1-gc.md describe the same ordering rule.

#### Slice 2: scheduled GC in the daemon, default OFF

Files: `crates/cowfs-daemon/src/main.rs`, `daemon.rs`, `lib.rs` (config), plus one new small module for the tick loop.
Behaviour as in section (a).
Acceptance criteria:

1. Without the flag, no thread is spawned.
   Test: daemon start spawns no GC thread, observable via a counter on the config.
2. With `--gc-interval 60s` on a fake clock, N ticks produce N collect calls on a backend stub.
   A `Busy` reply doubles the gap, capped at 8x.
   A success resets it.
   Both Busy causes (already running, roots_error) are exercised and both back off.
3. A manual `cowfs gc` during a scheduled run gets `Busy`.
   The scheduled run is unaffected.
4. `Daemon::stop` during a running scheduled cycle cancels it.
   It then joins the timer thread and returns within `GC_STOP_PATIENCE`.
5. Interval below 60 s is rejected at parse time with a clear message.
6. End to end on the real core: create garbage with a snapshot rm and wait one scaled interval.
   Assert `freed_bytes > 0` and that every surviving file reads back (reuse the `core_reclaim.rs` fixture shape).
7. The tick goes through `Handler::gc`.
   Test: a backend whose report has `roots_error` and `freed_bytes == 0` yields `Busy` to the scheduler, not a success.
   `cowfs_ctl::GcReport` (`crates/cowfs-ctl/src/types.rs:196`) has no `roots_error` field (V), so the scheduler cannot see a partial roots failure.
   It backs off only on `Busy`, and an `Ok` resets the gap.
8. Failing-first: tests 2 to 5 fail on origin/main for lack of the feature.
9. CI and a cachyos run are green, with no cargo on the Mac.

#### Slice 3: feed the hints, default ON but lossy

Files: `crates/cowfs-core` read path, `crates/cowfs-gc/src/state.rs` (shared `Hints`), `crates/cowfs-core/src/gc.rs`.
Acceptance criteria:

1. The correctness tests 1 to 5 in section (b).
2. A test that reads block X through `Core`, then runs a cycle with two otherwise identical candidate packs, and asserts the unread pack is rewritten first.
3. Hot-path benchmark within 1 percent, N at least 5, with baseline, reported with variance.
4. `atime.bin` is appended by the flusher only, never by a read (strace or a write-counting wrapper in the test).
5. Record format of `atime.bin` is byte-identical to today (golden file test).
6. Hints survive a daemon restart and a fresh `Collector`.
7. `atime.bin` size is bounded.
   Today `Hints::flush` only appends (`state.rs:172` `set_len`) and load keeps the maximum per id, with no rewrite step, so a file fed by real reads grows without bound.
   Slice 3 must add compaction (write whole through a temp file, fsync, rename, as the marks file does at `state.rs:391`) when the file exceeds a multiple of the live entry count.
   Test: after M flushes of the same K ids the file is at most a fixed multiple of K records.
8. There is exactly one appender.
   `Collector` creation per request opens a `Gc` that loads and flushes its own `Hints` (`lib.rs:174`, `:841`).
   With a shared owner in `Core`, `Gc::open` needs a constructor taking the shared instance, and the end-of-cycle flush goes through that owner.
   Test: two concurrent `Gc` handles never produce interleaved or duplicated appends.

### Decisions needed from Zee

- D1: approve Option 3 and its slice order, or choose Option 1 (document on demand only).
- D2: confirm space-pressure triggering (C) is out of scope for #10.
- D3: confirm removing `cold_dead_bonus` and the "older than a threshold" wording rather than implementing them.

### Why no code in this pass

A timer in the daemon touches shutdown ordering, the single-run slot and a new CLI flag.
That is a concurrency change with several failure modes, and it needs a fake-clock seam that does not exist yet.
It is not small and fully specified until D1 is decided.
Per the standing rule, the cautious first slice is this decision memo.
