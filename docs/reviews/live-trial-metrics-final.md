# Final re-audit: live trial metrics (PR #75)

Auditor: native Sonnet 5.5.
Audited head: `7f8ffee4c7bf8098ee3f17bc8e1e7203fbf67f98` on `bench/live-trial-metrics`, diff base `8e4c158600ae993a76f48de70d8b1f1373bd45f6`.
Scope: read-only audit plus disposable fixtures under `bench/out/live-trial-critic-final/`, all removed.
No source, benchmark arm, store or daemon was touched.

## Verdict: BLOCK, one narrow wording fix

Everything else passes.
The block is a residual dedup inference in `docs/live-trial-metrics.md` lines 123 to 124.

## Block

1. `docs/live-trial-metrics.md:123-124` says a 949 MB rebuild "added no growth distinguishable from the other writers" and that this "is consistent with near-total dedup of repeated builds of identical output".
   The original audit said this is not evidence of dedup, and `docs/reviews/live-trial-metrics.md` says the claim is unsupported.
   The doc contradicts its own review.
   The observation is equally consistent with write-back bytes not yet in the index (`dirty_bytes` in `cowfs-core`), so it is not a dedup indication.
   Narrow fix: delete line 124, or replace it with "Index growth during cowfs clean builds is an upper bound only; no dedup inference is made."
   Related, non-blocking: line 107 "Dedup adds about 1.33x" attributes all of A / logical to dedup.
   Suggest "A / logical is 1.33 in this non-atomic reading; its composition is not separated."

## Pass: the five original storage blocks

| # | Original block | State at 7f8ffee |
|---|---|---|
| 1 | D lower bound | Fixed. Line 110 "D is not a lower bound", dirty bytes inflate, garbage and growth deflate, net unknown. No "lower bound" assertion remains in doc or PR body. |
| 2 | 176 B means no reclamation | Fixed. Line 100 and 204: 11 headers of 16 B, no GC history claimed. |
| 3 | NFS `st_blocks` allocated ratio | Fixed. 10,306,162,688, 2,807,406,592 and 3.67 removed. Only APFS single-tree 798,834,688 vs 792,466,404 (1.008) is kept, labelled single tree. |
| 4 | A is native baseline | Fixed. Line 56 and PR body: cowfs-reported, not an independent baseline; 85% is `target/`; S "not a certified native capacity saving". |
| 5 | Clean setup untimed | Fixed. Lines 148 to 150 list rm, reset, byte count and checkout as excluded; gaps 97, 70 and 166 s are upper bounds, not removal timings. |

Raw arithmetic is unchanged.
Numbers only in removed doc lines: the three allocated figures above, `1,070` (corrected to `1,056`) and `2.9`.
Every other changed number is an added correction (exclusions 11,243 B, `meta.redb` 119,873,536 to 239,742,976, binary sizes, 19.4 MB/s, epochs).
Recomputed from `metrics.jsonl`: A 10,286,735,518, D 1.333, C 0.3489, S 3.658, P/A 0.273; table medians for all 9 cells present in the doc; clean ratio 22.76 (17.02 to 23.07), no-op 88.02 (72.31 to 144.22), edit 50.24 (45.67 to 91.17).
27 non-smoke phase records.
Raw files unchanged in size (74,778 and 2,100 bytes).

Scope and exclusions: 6 B `test.txt` plus 19 pool-level files (11,243 B) explicit, whole-mount sample, hardlink check on one build tree only, all preserved.
Slowdown 17x to 144x stays qualified by load 10 to 24, debug daemon, unmatched load, and "no gate verdict".
No speedup claim and no production gate (doc lines 170, 183 to 190).
PR body and `bench/out/.../pr-body.md` carry the same qualifiers; neither contains lower bound, never-reclaimed or allocated-ratio claims.
One PR body bullet ("Every cowfs rep 17x to 144x slower, no speedup anywhere") has no inline qualifier but sits under "All numbers are provisional".

## Pass: new code

`parse_finished` independently tested: `1.23s`, `0.00s`, `6m 40s` (400), `1m 00s`, `75m 03s`, no trailing newline, CRLF, absent, `Finished` with no time, cut after "in", cut at "1.", cut at "12", first of two Finished lines.
All as expected.
A real tiny cargo build gave `Finished ... in 2.86s` (clean) and `0.19s` (no-op); parsed 2.86 and 0.19.
Low finding: "in 2m 0" (cut mid-seconds) parses as 120.0, not 123.
Cannot occur with `capture_output` of the full stderr, and `cargo_s` does not feed the summary, so not blocking.
The test named `test_truncated_output_still_parses` does not truncate the Finished line; it only tests preceding output.

`clean_target` guard, own disposable fixtures only:

| case | result |
|---|---|
| symlink `corpus-target` to unowned outsider | refused, outsider intact |
| symlink to owned sibling | link removed only, victim intact |
| `arm_dir` symlinked outside | refused, intact |
| sibling `native-evil` (string-prefix confusion) | refused, intact |
| counterfeit marker in wrong dirs | refused, intact |
| marker is a directory | refused, intact |
| marker with wrong content in the real base | removed (content never verified; same as old guard) |
| normal owned target | removed |
| dangling symlink | no-op |
| `python -O` subprocess, symlink to outsider, `__debug__` false | `GUARDED`, outsider intact |

The standard run and the `-O` run of `scripts/test_measure_live_trial.py` both pass 11 tests (1 skipped under `-O`, covered by its subprocess case).
The builder is right that the removed clauses were tautological: `t` is built as `arm_dir[arm] / "corpus-target"`, so `t.name` and `t.parent` are constant-true (checked for all three arms).
The old predicate and the new one allowed the same cases in every fixture.
No weakening: `owned()` still resolves symlinks and uses a separator-terminated prefix, the check is explicit `if/raise`, `rm -rf` on a symlink removes only the link, and a surviving target now raises.
My earlier low finding (assert stripped under `-O`) is closed.
Residual, unchanged, not blocking: marker content is never compared with the run id; a check-then-`rm` TOCTOU window exists.

Test gaps, non-blocking: no tests for symlink, prefix-sibling or counterfeit marker; `test_foreign_name_is_refused_and_survives` cannot fail because `clean_target` only ever names `corpus-target`; the `-O` fixture sets `run.native` to the arm directory, which is contrived.

## Review doc accuracy

`docs/reviews/live-trial-metrics.md` is a builder paraphrase of the first audit, labelled "Auditor: native Sonnet 5.5".
Its claims match what I found, apart from the abbreviated path `crates/cowfs-vfs/types.rs:93` (real: `crates/cowfs-vfs/src/types.rs:93`).

## Storage statement

The only supportable statement is a provisional, non-atomic point-in-time ratio: cowfs-reported apparent regular-file bytes 10,286,735,518 (85% `target/`) against a store physical footprint of 2,812,246,866, about 3.66, measured minutes apart under constant writes.
It is not a certified native saving, not a bound, and carries no dedup, GC or allocation conclusion.

## CI and review gaps

At time of audit, run 37102068706 on exact head `7f8ffee4`: `linux-fuse` success, `check (macos-latest)` and `check (ubuntu-latest)` in progress.
The earlier green run 37100768463 was on `8e4c158`, not this head.
PR is draft, no reviews, no comments, `mergeable_state` unstable (checks pending).
Re-check CI after the wording fix, since it adds a new head.

## Reproduction

- `python3 scripts/test_measure_live_trial.py` and `python3 -O scripts/test_measure_live_trial.py` in the worktree.
- `git diff 8e4c158..7f8ffee -- docs/live-trial-metrics.md scripts/measure-live-trial.py`.
- `gh api repos/zeeshanhaque21/cowfs/actions/runs/37102068706 --jq '{head_sha,status,conclusion}'`.
- Fixtures were built with `tempfile.mkdtemp` under `bench/out/live-trial-critic-final/` and removed after checking an own marker and path prefix.
