# APFS clone + sccache vs plain clone on macOS (2026-10-10)

Question: does adding sccache (shared cache dir, `RUSTC_WRAPPER=sccache`) to `cp -cR` clone slots improve time or disk versus plain clones (arm B of PR 337)?
Follows `docs/reviews/clone-vs-cowfs-mac-20261010.md`; same project, same pinned commit, same edits and units, same df method.
Raw CSV: `docs/reviews/clone-sccache-mac-20261010.csv` (one row per timed step, native reference rows included).

## Verdict

sccache does not pay off here.
Disk: the whole saving over plain clones comes from turning incremental compilation off, not from sccache (B2 is already as small as E).
Time: E is no faster than B2 and is slower on small edits.
Rule 1 NOT MET, rule 2 NOT MET, rule 3 MET (but see the caveat under rule 3).

## Setup

- Project: `git clone` of the local cowfs repo, checked out at `65a551957134c361c70fd7c843ef265a2327dc7c` (same pin as PR 337), `cargo fetch` before any timing.
- Machine: Apple M3 Max, APFS Data volume, rustc/cargo 1.99.0 (cargo 5f94df478), sccache 0.18.0 (Homebrew). No RUSTFLAGS.
- Build command in every round: `cargo test --no-run --workspace` (113 units in a full build, as PR 337).
- Arms, 4 slots each, slots created with `cp -cR base slot-N` from a base already built with that arm's settings:
  - B: default dev profile (incremental on), no sccache.
  - B2: `CARGO_INCREMENTAL=0`, no sccache.
  - E: `CARGO_INCREMENTAL=0`, `RUSTC_WRAPPER=sccache`, `SCCACHE_DIR` shared by all four slots and the base, own server port 4627, idle timeout off.
- E cache is cold at the start of the arm: the base build (R0) is the cold build and fills the cache.
- Rounds: R1 create slot, R2 leaf edit (cowfs-cli, cowfs-treehouse, cowfs-daemon, cowfs-fuse for slots 1 to 4, `// exp slot K` appended to the crate root), R3 store edit on top of R2 (cowfs-store, 7 units), R4 `cargo clean` + full rebuild, R6 `cargo test -p cowfs-snapname` (expects `ok. 7 passed`, as PR 337), plus new R7.
- R4w (extra): a second `cargo clean` + full rebuild in the same slot, to separate "cache warmed by the base" from "fully warm for this exact path".
- R7: fresh `cp -cR` of the source-only tree (no target) to a new path, full build, 4 paths per variant, interleaved B, B2, E-cold (a brand new empty cache dir per run), E-warm (the arm E cache).
- Native reference (the 1.0x): in each arm's own base checkout, apply the same edit, build, time it; for R3 the leaf edit then the store edit; then revert and rebuild untimed.
  R4 native: `cargo clean` + full build in the base, 4 times, last step of the arm.
  Native is measured per arm, so there are three native references; the 1.0x for the rules is native B (default settings), because that is what a user gets without any of this.
- Disk: `sync; sleep 10; df -k /System/Volumes/Data` after each timed slot step (as PR 337), plus `du -sk` of the sccache dir after every step, plus a post-run `du` of one freshly built target per arm (see below).
- Smoke: one full arm E slot (base, R1, native, R2, R3, R4, R4w, R6) ran first and passed before the batch.
  It exposed a wrong R6 command (my first guess, `-p cowfs-core snapname`, runs 4 tests and compiles 16 units); fixed to `-p cowfs-snapname`, which reproduces PR 337's "ok 7 / ok 0, 3 Compiling".
  Smoke numbers are not in the CSV.
- Every cargo run: nonzero exit or a line starting with `error` aborts the driver; per-build cap 600 s; the whole run completed with no failure.
- The machine was not quiet: other agents were building, 1-minute load average 4 to 33 during the run.
  PR 337 B figures do not reproduce (see anomalies), so B was rerun in full (4 slots) rather than reusing PR 337 numbers.

## Per-arm per-round table

Seconds are the mean wall time per slot over 4 slots.
Hit rate is sccache Rust hits / (hits + misses) summed over the round.

| round | B s | B2 s | E s | E hits/misses (rate) |
| --- | --- | --- | --- | --- |
| R0 base build (1 run, cold cache for E) | 29.02 | 15.03 | 15.30 | 0 / 145 (0%) |
| R1 create slot | 6.70 | 0.76 | 0.99 | n/a |
| R2 leaf edit | 3.09 | 2.02 | 3.41 | 0 / 5 (0%) |
| R3 store edit | 14.53 | 6.50 | 9.62 | 0 / 24 (0%) |
| R4 clean | 5.39 | 0.27 | 0.29 | n/a |
| R4 full rebuild | 24.10 | 12.84 | 14.31 | 488 / 92 (84%) |
| R4w clean | 3.39 | 0.34 | 0.43 | n/a |
| R4w full rebuild | 23.39 | 12.53 | 13.50 | 580 / 0 (100%) |
| R6 snapname tests (all PASS 7/7) | 2.23 | 1.96 | 1.19 | 10 / 6 (62%) |
| R7 fresh clone, full build | 16.81 | 12.66 | cold 13.94 / warm 12.70 | cold 0 / 580 (0%), warm 488 / 92 (84%) |
| Native R2 (base edit) | 1.24 | 1.03 | 1.20 | 0 / 5 |
| Native R3 | 10.69 | 6.30 | 9.69 | 4 / 20 |
| Native R4 clean | 5.31 | 0.33 | 0.32 | n/a |
| Native R4 full rebuild | 19.46 | 13.67 | 10.94 | 580 / 0 (100%) |

Per-slot R2 (s): B 3.12, 3.07, 5.08, 1.09; B2 2.21, 1.88, 3.14, 0.83; E 2.33, 6.72, 3.67, 0.93.
Compiling lines: R2 1,1,2,1 in every arm; R3 7 in every slot; R4 and R7 113.
Unit counts match PR 337.
R4 full-rebuild hits/misses per slot: 122 / 23 (the 23 misses are the workspace crates, whose cache key changes with the slot path; registry crates hit).
R4w, native R4 and the repeated base path hit 145 / 0.

### Disk

Disk is the dominant, most reliable figure from the post-run `du` of a freshly built full target per arm (clone of the source, one `cargo test --no-run --workspace`, `du -sk target`).
After clean + full rebuild every slot target is private (nothing shared with the base), so this is the per-slot footprint at the end of R4.

| item | B | B2 | E |
| --- | --- | --- | --- |
| target dir, one slot (du, GiB) | 3.89 | 2.66 | 2.66 |
| 4 slot targets (GiB) | 15.56 | 10.64 | 10.64 |
| sccache dir at end of R4w (du, GiB) | n/a | n/a | 0.38 |
| total at end of R4 (GiB) | 15.56 | 10.64 | 11.02 |

sccache dir growth for arm E (du, GiB): 0.10 after the base build, 0.16 after R2, 0.20 after R3, 0.38 after R4, 0.38 after R4w and native R4, 0.65 at the very end of the whole experiment (after R7, 4 cold runs excluded because each used its own deleted cache dir; the extra growth is R7-warm and the du-check build misses on new paths).
Each new slot path adds about 23 workspace-crate entries (about 45 MiB per path); the registry-crate entries are shared.

The df-based cumulative deltas (R1 through R4, 4 slots, the same method as PR 337) agree for B and B2 and are noisy for E:

| round, df GiB used | B | B2 | E |
| --- | --- | --- | --- |
| R1 | -0.10 | 0.01 | 0.83 |
| R2 | 0.64 | 0.42 | 2.50 |
| R3 | 7.74 | 4.48 | 4.52 |
| R4 clean + full | 4.75 | 3.53 | 3.42 |
| R1 to R4 total | 15.56 | 10.11 | 13.27 (plus 0.10 base cache = 13.37) |
| R4w (second clean + full) | 0.00 | 0.00 | 6.22 |

The E R1, R2 and R4w df figures include writes by other agents on the same volume (free space moved by several GiB between steps while the sccache dir did not change: R4w cache du is identical before and after), so I take the du-based table as the disk answer.
B total of 15.56 GiB for 4 slots matches PR 337 (26.58 GiB for 8 slots, about 13.3 for 4).

## Rule evaluation

Rule 1 (E edit-rebuild R2 and R3 per slot within 2x of native): NOT MET.
- R2: E 3.41 s vs native B 1.24 s = 2.75x (median E 3.0 s = 2.4x); vs native E 1.20 s = 2.8x.
- R3: E 9.62 s vs native B 10.69 s = 0.90x, within the bound (vs native E 9.69 s = 0.99x).
- R2 fails and R3 passes, so the rule as written (both) is NOT MET.
- Context: B itself is 2.5x native on R2 (3.09 vs 1.24 s) and B2 1.6x (2.02 vs 1.03 s), so slot R2 is slower than native in every arm, mostly independent of sccache; absolute cost is 1 to 7 s.
- E is slower than B2 on edits: R2 3.41 vs 2.02 s, R3 9.62 vs 6.50 s (+48%).
  E R3 had 24 misses, 0 hits across 4 slots (the changed crate and its dependents are new content, so sccache can only add overhead plus cache writes).
- E is faster than B on R3 (9.62 vs 14.53 s) because incremental is off; B2 shows the same gain without sccache.

Rule 2 (E clean full rebuild R4, warm cache, at least 2x faster than B2 R4): NOT MET.
- E R4 14.31 s vs B2 R4 12.84 s = 0.90x (E is 11% slower) with 84% hit rate.
- E R4w (100% hit rate, fully warm) 13.50 s vs B2 R4w 12.53 s = 0.93x.
- Fully warm native-in-base E 10.94 s vs B2 native 13.67 s = 1.25x faster, the best case seen, still far under 2x.
- R7 fresh clone: E warm 12.70 s vs B2 12.66 s (1.00x); E cold 13.94 s (+10% over B2, the wrapper cost on a cold cache).
- Likely reason (inferred, not profiled): the test executables are linked every time and are not cacheable by sccache; its end-of-run stats listed "crate-type" as the non-cacheable reason for 209 requests.
  Time is dominated by linking and not by rustc codegen of cached rlibs.
  I did not measure the link share.
- The biggest speedup in the table is B to B2 (24.10 to 12.84 s on R4, 14.53 to 6.50 s on R3), i.e. incremental off, not sccache.

Rule 3 (E total disk, slot targets + sccache dir, lower than B at end of R4): MET, with a caveat.
- du-based: E 11.02 GiB vs B 15.56 GiB (-29%); df-based E 13.37 GiB vs B 15.56 GiB.
- Caveat: B2 (no sccache) is 10.64 GiB, smaller than E by the 0.38 GiB cache.
  The saving over B is entirely the non-incremental target (2.66 vs 3.89 GiB per slot, no incremental session dirs), not sccache.
- The sccache dir amortizes: it is shared, so its cost does not scale with slot count, while its benefit on disk is zero here.
  With many more slots E would approach B2 from above.

## Anomalies

- B did not reproduce PR 337: base build 29.0 s (PR 337: 18.8 s), R4 full 24.1 s (16.7 s), R3 14.5 s (10.9 s), R2 3.1 s (4.6 s).
  Load average 4 to 33 during this run; the arms ran sequentially, so drift between arms is possible (B first, E last).
  B native R4 (19.5 s) vs B slot R4 (24.1 s) shows how much the same machine moved between steps.
- R1: B slot create 6.70 s vs B2/E 0.76/0.99 s.
  The plain clone has far more files because of incremental session dirs; the arm B target is 3.89 GiB vs 2.66 GiB.
- Slot R2 slower than native for the same edit in all arms (see rule 1).
  Possible cause: cold page cache on freshly cloned files (not tested).
- E R2 slot 2 (cowfs-treehouse) took 6.72 s vs 1.5 s for the same edit natively; the 4-slot mean is skewed by it.
- `cargo clean`: 5.4 s in B vs 0.3 s in B2/E (incremental dirs are many small files).
- B native R4 clean ranged 2.7 to 12.9 s across 4 reps.
- E R4w df delta of +6.22 GiB is inconsistent with the unchanged sccache dir and with B2's 0.00; attributed to other agents writing to the volume (not verified).
- R6: first guess `-p cowfs-core snapname` was wrong, fixed in smoke (see Setup).
  E R6 recompiled 3 units with 10 hits / 6 misses, 1.19 s vs B 2.23 s, but with 1 to 2 s steps that gap is within noise.
- sccache stats include a few non-compilation probe calls (e.g. 3 "compilation failures", 3 "missing input"); they are not build failures.
  No cargo build failed.
- A stray default-port sccache server was started once by an exploratory `sccache --show-stats` at the start; it showed zero activity and was not used by any timed run (those used port 4627 and the arm's own cache dir).

## What I could not verify

- Timing differences under 15% between arms (B2 vs E, native vs slot): single run per slot on a loaded shared machine, no repeats, no randomized arm order.
- Why slot R2 is slower than native (cold cache hypothesis untested).
- The link-dominance explanation for the missing sccache speedup; only the non-cacheable crate-type counts were seen, no profile.
- Behaviour with `--remap-path-prefix` or a different workspace path scheme; here workspace crates miss on every new path (23 misses per new path), registry crates hit.
- Whether sccache helps on a cross-machine or CI-shared cache (local disk cache only).
- Release profile and `cargo build` (non-test) builds; only `cargo test --no-run --workspace` was measured.
- Linux behaviour (this report is macOS only).
- PR 337 numbers were not reused; B was rerun with a new driver (the original harness script was not available in the repo), so B here is comparable inside this report, only roughly to PR 337.

## Estimates and labels

All numbers above are measured, except: the "about 45 MiB per path" figure (derived: 0.38 GiB cache growth in R4 over 4 paths minus nothing else, estimate), the "within noise" statements, and the link-dominance explanation (hypothesis).
