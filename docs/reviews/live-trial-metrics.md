# Review: live trial metrics (PR #75)

Auditor: native Sonnet 5.5 (independent re-audit of PR #75).
Subject: `docs/live-trial-metrics.md` and `scripts/measure-live-trial.py` at audited head `8e4c158600ae993a76f48de70d8b1f1373bd45f6`.
Scope: audit only, no source edits to the measurement doc's raw numbers.

The auditor recomputed every table from `bench/out/live-trial-native/lt-20261002a/metrics.jsonl`, verified the actual content of all 497 tracked files against HEAD with zero mismatches and identical end state on all three arms, and confirmed the toolchain and config identities.
The build direction stands: every cowfs rep was 17x to 144x slower than native, no production gate.
The storage and report claims needed corrections, recorded below.

## Blocks fixed before merge

1. **D is not a proven lower bound.**
   A / `logical_bytes` mixes two non-atomic epochs.
   Dirty write-back bytes that `stat` reports but the index has not yet counted inflate the ratio; garbage and background growth deflate it.
   Net direction unknown.
   D, C and S are reworded as non-atomic point-in-time ratios only, neither a bound nor a certified native saving.

2. **The 176-byte pack-versus-stored gap is not a GC history.**
   It is 11 pack headers of 16 bytes.
   Every record was indexed at that instant, so the gap says nothing about reclamation or dead references.
   The "no space ever reclaimed" deduction was removed; GC history is unknown.

3. **NFS `st_blocks` is synthesized, not physical allocation.**
   `crates/cowfs-vfs/types.rs:93` synthesizes logical allocation over NFS.
   The allocated-based 3.67 ratio was deleted and no physical claim is made from NFS `st_blocks`.
   The only measured allocation figure is on APFS for a single build tree: allocated 798,834,688 versus raw 792,466,404, a ratio of 1.008.

4. **A is cowfs-reported, not an independent native baseline.**
   A is relabelled "cowfs-reported apparent regular-file bytes, hardlinks counted once", not an independently measured whole-mount native baseline.
   85% of it is `target/` build output, which is not transferable to source-only trees.
   A / P of about 3.658 is kept as a reported ratio and explicitly not a native-capacity verdict.
   The hardlink-once logic was checked on one build tree, not the whole mount.

5. **The clean timer excludes setup.**
   `rm -rf`, the reset, tree byte counting and `git checkout` are outside the timed section.
   Timestamp gaps before each cowfs clean were 97, 70 and 166 s against 1 to 5 s on native.
   Those are setup upper bounds, not removal timings.
   Reported as excluded cost with no new timing claim and no re-run.

## Additional audit corrections

- Exclusions: the 6-byte `test.txt` plus 19 pool-level files, 11,243 bytes.
- Epochs: arm walk 22:01:38 to 22:01:49, bench dir 22:02:59 to 22:03:02, store counters 22:03:02.
  The index grew 31.8 MB during the walk and 36.4 MB more by the counter reading.
- P includes `meta.redb` preallocation, which later doubled from 119,873,536 to 239,742,976 bytes, not only live user blocks.
- The near-total rebuild-dedup claim is unsupported.
  Cowfs clean index growth of 560, 316 and 755 MB against 949 MB written includes background growth and gives a per-run upper bound only, not a write-counter dedup.
- Binaries are not identical: 29,938,544 versus 29,855,424 bytes, though the `--help` output is identical.
- The shared `CARGO_HOME` and `TMPDIR` are on APFS for all arms, which favours the cowfs arm over the full-tree dependency layout.
- Summed `ps` CPU is a decaying-average heuristic, not a usage integral.
  The actual sum median was 648 to 1,056%, and the daemon itself ran at 24 to 175% during native phases.
- Native background growth reached 19.4 MB/s in short phases, so the "0 to 2.9 MB/s" assertion is wrong.
- The two native arms are effectively two time windows, not three independent quiet epochs.
- Absent historical telemetry: no per-op timings or past sizes exist.
  The gate harness, `git status` and mode (b) were not measured.

## Script fixes applied with this review

Both are narrow and audited:

- `clean_target` removal guard is now explicit `if/raise`, not `assert`, because `python -O` strips `assert`.
  The tautological name and parent clauses were removed; only the marker-ownership clause can fail, and it is the one tested.
- `cargo_s` now parses both `Finished ... in 1.23s` and the `Xm Ys` form via `parse_finished`, which was `None` for four phases (three cleans and edit rep 2) under the old single-line regex.

`scripts/test_measure_live_trial.py` adds 11 focused tests: the guard is fail-closed with and without `-O` on a representative tiny owned fixture, an unowned parent is refused and survives, an owned target is removed, and `parse_finished` covers seconds, minutes, long form, truncated output and absence.
The tests do not delete anything outside their temporary fixture.
Existing raw numbers are unchanged; wall clock stays authoritative.
