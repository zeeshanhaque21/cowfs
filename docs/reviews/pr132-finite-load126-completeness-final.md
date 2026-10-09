# PR 132 final review: issue 126 remainder, a partly unknown load is now unknown

## Verdict

**PASS, scoped.** The remainder PR 129 left open is closed, and the whole tree is green on both variants of the exact commits under review.
I found no blocker.

The change is three lines in `loads()` and one test.
I reproduced the defect end to end through the published CLIs, confirmed the fix flips exactly the cases it claims and nothing else, and confirmed the finite do-nothing baseline, the ceiling, the skew guard, the schema refusals and the `--noise-floor` column all still behave as the merged contract requires.

One observation worth recording rather than blocking on: the head's `loads()` raises `ValueError` on an empty row list where the base returned `NaN`.
I could not reach that path through any public input, and the reason is structural rather than lucky; the evidence is below.
It is a latent difference in a helper, not a reachable behaviour change, and the simplest robust form is what the author wrote.

| item | result |
| --- | --- |
| defect reproduced on the base, public CLIs only | **reproduced**, exit 0 `RESULT: PASS` with an unreadable observation |
| both load-field positions, `nan` first and `nan` last | **0 to 2** on base to head, both arm slots |
| multi-row arm with one unreadable rep of three | **0 to 2** |
| arm order | **all four** orderings covered by the new test, both mixed positions each |
| finite quiet baseline, 3.5 / 3.5 | **PASS**, 0 on base and head |
| finite over-ceiling, 31 / 31 | **2** on both, message names the ceiling, not the new branch |
| finite skew, 8.0 vs 3.5 (2.29x, under ceiling) | **2** on both, unchanged mechanism |
| `inf`, `-inf`, missing key, negative | **3** on both, schema refuses before scoring |
| `getloadavgfailure` (all-`NaN` arm) | still **2**, never `INVALID 3` |
| `LOAD_CEILING`, `LOAD_SKEW`, `RATIO_BAR`, `GATES`, `verdict`, `is_nonneg_load`, `file_problems`, `meta_problem`, `g5_problem`, `ratios`, `spread`, `median` | **byte-identical**, verified by AST extraction |
| `gates.py` | **byte-identical**, blob `487d73e28b08` at base and head |
| new test on the merged classifier `2fd7c3c` | **FAIL** `0 != 2`, genuine |
| new test plus the four merged load tests, head | **4 passed** |
| `CompareRefuses` | **18** head, 17 base |
| `bench/test_gates.py` | **39** head, 38 base |
| whole bench discovery | **484** head, 483 base, both `OK (skipped=13)`, same 13 |
| `py_compile` | **clean**, all three files, both variants |
| lint | **not clean, and that is parity**, 5 findings, identical set before and after |
| CI at the exact head | **3 checks, 3 passed, 0 failed**, `ci.yml` active on bare `pull_request` |
| PR refs | `Refs #126` only, no closing phrase, #126 **open** |
| merge into current `main` | **clean**, tree `53b8e9ef`, merged `compare.py` equals the head's |
| scope of the diff | **3 files, +215 / -4**, matches the author's report |

## What was actually wrong

PR 129 (`dadc524`) closed the wholly-unreadable case.
Its guard is `math.isnan(la) or math.isnan(lb)` on the per-arm peak, and `loads()` answered that question by dropping every `NaN` before taking the maximum.

So an arm with some readable and some unreadable samples returned the readable maximum.
The merged guard could not observe the unknown observation, and the run finished `RESULT: PASS` with exit 0.

Against issue 126's exact requirement, "a nonfinite observation must not silently certify measurement quality", one unreadable sample certified the quality of the whole comparison.

This is the remainder, and it is a real defect rather than a stylistic one.
PR 129's own report already documented the mechanism, at `loads()`, and said that function was identical at base and head because that PR's delta did not touch it.
This PR touches it.

## Identity, pinned before every run

Base `48c06f9cf9d323bb626d902cd0e7b2247beaf055`, which is the current `main` HEAD and the parent of the PR head.
Head `5607c31a7fae23e5b14a0cfbe68dd3de5b8321b3`.

| file | base blob | head blob |
| --- | --- | --- |
| `bench/compare.py` | `2fd7c3c5139ec0afa5baffa3bac4d1ed573aac59` | `1376b8fc543a7a9bd3fbd5df536345efc84a8d6b` |
| `bench/gates.py` | `487d73e28b08722c2917059a11c94ad7961ec51c` | `487d73e28b08722c2917059a11c94ad7961ec51c` |
| `bench/test_gates.py` | `e9255843f98dc8778e3b9466126f1418479c02f3` | `98e0f277e64dcdcf10f1e748e77928c903c13857` |
| evidence doc | absent at base | `da0d14f5177cdbeb2d1cb4ead151ac6a4bff0650` |

`base:bench/compare.py` is the same blob as the merged PR 129 commit's, which confirms the base I ran against is the classifier the PR claims to fix on top of, not a drifted one.

I extracted both trees with `git archive` into a private archive and compared each extracted file's `git hash-object` against the tree's blob before anything ran.
Every file matched on both variants.
A bare `cp` succeeding would not have been proof of what a run loaded, so I used blob identity plus a SHA-256 of the exact file immediately before each command, and the SHA-256 differs between variants while the head value is stable across all of my head runs (`07db8641b027…` for `compare.py`).

The prompt's author SHA `6122d4aa1241…` does not resolve to any object in this repository.
`git cat-file -t 6122d4aa1241` reports "not a valid object name" and `rev-parse --disambiguate` returns nothing.
I could not confirm that identifier, and I pinned the author work by full blob equality instead, which is stronger than a truncated SHA.
This is reported rather than glossed: I verified blob equality, not the prompt's SHA.

## The receipts are real, and the mixed arms are labelled as derived

I ran the published `gates.py` unmodified against a tiny real project at `COWFS_BENCH_SCALE=0.001`, load supplied through `COWFS_BENCH_FAKE_LOAD1=3.5`, which is that script's own documented hook.
The writer emitted a real `meta` record and three real g5 reps:

```
meta: label=real-head big_bytes=1048576 scale=0.001 reps=3 gates=['g5']
rep 0 wall_s=0.005407 load 3.5->3.5 bytes=1048576 written=1048576 read=1048576
rep 1 wall_s=0.005237 load 3.5->3.5 bytes=1048576 written=1048576 read=1048576
rep 2 wall_s=0.005224 load 3.5->3.5 bytes=1048576 written=1048576 read=1048576
```

The byte accounting is the writer's own and it round-tripped: `written_bytes`, `read_bytes` and `bytes` all equal `meta.counts.big_bytes`, which is exactly what `g5_problem` requires.

The mixed arms cannot come from the writer, and I want to be exact about that rather than let the exit codes imply otherwise.
`gates.py` calls `load1()` twice per rep and writes both values into the same row, and a single `load1()` call cannot both fail and succeed.
So `nan` in one field and a finite value in the other is **derived input, not a fresh sampling result**.
Every derived arm carries a `_derived_by` field on every rep row naming the derivation, and I verified that each derived file preserves the writer's `wall_s` and byte values exactly, changing only the load fields.
That is the honesty line, and the table below is the whole of what I claim from those files.

Arms used, all derived from that one genuine receipt, all with `wall_s` and bytes preserved:

| arm | reps | load (before, after) |
| --- | --- | --- |
| `armA-finite` | 3 | 3.5, 3.5 |
| `armB-busy` | 3 | 31.0, 31.0 |
| `armC-skew` | 3 | 8.0, 8.0 |
| `armA-nan-before` | 3 | nan, 3.5 |
| `armA-nan-after` | 3 | 3.5, nan |
| `armA-one-bad-rep` | 3 | two reps 3.5/3.5, one rep nan/nan |
| `armA-inf` | 3 | inf, 3.5 |
| `armA-negative` | 3 | -1.0, 3.5 |

I did not infer which arm a fixture was from its filename.
My runner takes the arm as an explicit case-table field, because I hit the failure mode the author describes: an earlier pass of mine built a native argv as one unquoted string, so `compare.py` received a single path with an embedded space and answered `INVALID 3 unreadable`, which I had nearly read as a schema finding.
That run is discarded and the corrected argv is quoted per argument.

## The exit-code ledger, published CLI as a subprocess

Every row is a fresh `python3 compare.py --native … --cowfs …`, so the codes are the CLI's own contract.

| subject | slot | base `48c06f9` | head `5607c31` |
| --- | --- | --- | --- |
| finite 3.5 | native | 0 | 0 |
| finite 3.5 | cowfs | 0 | 0 |
| busy 31.0 | native | 2 | 2 |
| busy 31.0 | cowfs | 2 | 2 |
| skew 8.0 vs 3.5 | native | 2 | 2 |
| skew 8.0 vs 3.5 | cowfs | 2 | 2 |
| nan before, finite after | native | **0** | **2** |
| nan before, finite after | cowfs | **0** | **2** |
| finite before, nan after | native | **0** | **2** |
| finite before, nan after | cowfs | **0** | **2** |
| one unreadable rep of three | native | **0** | **2** |
| one unreadable rep of three | cowfs | **0** | **2** |
| inf | native | 3 | 3 |
| inf | cowfs | 3 | 3 |
| negative | native | 3 | 3 |
| negative | cowfs | 3 | 3 |

Six rows changed, all from 0 to 2, all of them the unknown-observation cases.
Nothing else moved.

The multi-row case is the one the base filter laundered most directly: two readable reps and one unreadable, and the base dropped the unreadable one and scored the remaining two.
The head refuses the arm.

The mechanism is visible in the printed reason, which matters because a bare 2 does not distinguish "we refused because the load is unknown" from "we refused because the load was high":

```
UNMEASURABLE: no finite load1 was recorded (native 3.5, cowfs nan), so the ceiling 30.0 cannot be checked
```

And the unchanged reason, on the finite over-ceiling arm, is still the ceiling and not the new branch:

```
UNMEASURABLE: load1 peak 31.0 (native 31.0, cowfs 31.0, ceiling 30.0, skew limit 2.0x)
```

That is the distinction the PR body asks for, and it holds.
The old `busy + NaN` case was already 2 before this change, and it was 2 because the finite sample was 31.0 and tripped the ceiling; it stays 2, now because the observation is unknown.
Same code, different and better-attributed reason.

The whole point is visible in one more line: the base printed `RESULT: PASS` for the mixed arms.
That is the requirement being violated, and the head does not.

## Contract preserved, checked rather than assumed

I extracted each function's source segment from both variants with `ast` and compared them, because a claim that a function is "untouched" is weak when it is read from a diff.

Byte-identical: `is_nonneg_load`, `verdict`, `file_problems`, `meta_problem`, `g5_problem`, `ratios`, `spread`, `median`.
Constants identical: `RATIO_BAR = 1.5`, `LOAD_CEILING = 30.0`, `LOAD_SKEW = 2.0`, `GATES` list.

The only changed function bodies in the entire file are `loads` and `main`, and `main`'s only change is one added comment line.
The whole diff is two hunks: `@@ -310,9 +310,16 @@` and `@@ -401,6 +408,7 @@`.

`NaN` remains admissible input and never becomes `INVALID 3`, which is the asymmetry the taxonomy requires: an unreadable load is a real condition `gates.py` writes on purpose, not a corrupt file.
`inf`, `-inf`, a missing key and a negative value are all refused by the schema on both variants, and I confirmed a missing `load1_before` key is caught with `INVALID` before `loads()` is ever reached, so there is no `KeyError` path either.
An arm file containing only a `meta` record and no reps is `INVALID 3` on both, so it also cannot reach `loads()` with nothing to measure.

`gates.py` is byte-identical at both commits, blob `487d73e28b08`, so the sampling side of the contract is unchanged by construction.

## The latent empty-list difference, and why it is not reachable

`loads([])` on the base returns `nan`; on the head it raises `ValueError: max() iterable argument is empty`.
The head dropped base's `return max(vals) if vals else float("nan")` in favour of the `any(...)` guard plus `return max(vals)`, which loses the empty case.

I tried to reach it and could not, for a structural reason rather than by luck.
`loads()` is called at exactly two sites, both `main` local: `loads(a), loads(b)` inside the gate loop, and `max(loads(a), loads(c))` in the noise-floor block.
In both, `a` and `b` come from `by_gate()`, which builds its lists by appending rows it actually read, and the loop skips any gate absent from either arm.
Every list reaching `loads()` therefore holds at least one row, and every such row passed `file_problems`, which requires both load fields to be present.
A file with no rep rows is `INVALID 3` before this point, and a file whose reps lack a load key is also `INVALID 3`.

So the difference is confined to a direct call with an empty list, which no public input produces.
I am recording it because it is a real behavioural difference in a helper and a future refactor could make it reachable, and the guard costs nothing:

```python
return max(vals) if vals else float("nan")
```

That is a note for the author, not a blocker, and it does not change the verdict.
I did not modify the source: the review owns only its archive and its report.

## Tests, counts and lint, measured

The new test is `CompareRefuses.test_a_partly_unknown_load_is_not_a_measurement`.
I read it rather than trusting its name, and it covers: both load-field positions (`nan` before and `nan` after), both arm slots (`native` and `cowfs`), both peers (quiet 3.5 and busy 31.0), and a three-rep arm where only one rep is unreadable.
That is four arm orderings times two mixed positions, plus the multi-row case.
Each case asserts exit 2, `UNMEASURABLE` present, `PASS` absent and `INVALID` absent, so the verdict and the input contract are both pinned.

Failing first, against the merged classifier, on a cross tree holding the head's `test_gates.py` with the base's `compare.py`:

| file | blob |
| --- | --- |
| `compare.py` | `2fd7c3c5139e` |
| `gates.py` | `487d73e28b08` |
| `test_gates.py` | `98e0f277e64d` |

```
AssertionError: 0 != 2 : ('native nan before, finite after with quiet peer', ...)
RESULT: PASS  scope: compared 1 of 6 (g1), ...
FAILED (failures=1)
```

On the head, that test plus the four load tests merged by 129 give 4 passed.
Nothing from the merged work is reverted or weakened; the four tests are untouched by the diff and all pass.

Counts I measured on the full extracted trees, so the whole bench suite really does include `scripts/` and is not a partial-tree artefact:

| check | base | head |
| --- | --- | --- |
| `CompareRefuses` | 17 | 18 |
| `bench/test_gates.py` | 38 | 39 |
| whole bench discovery | 483, `OK (skipped=13)` | 484, `OK (skipped=13)` |

The 13 skips are the same 13 on both sides, and all are environmental: no `/proc` on Darwin, no mount namespace on Darwin, Linux-only auto-mode routing.
No skip was added or removed.
`py_compile` is clean on `compare.py`, `gates.py` and `test_gates.py` for both variants.

Lint is **not** clean, and that is parity rather than a pass I could claim.
`ruff check` reports the same five findings on both variants: `RUF100` on `compare.py:74`, and `SIM117`, `RUF015`, `RUF059` and `RUF059` on `test_gates.py`.
The finding set is identical before and after; only the line number of the second `RUF059` moves, because the new test adds lines above it.
I made no unrelated lint fix, and none is warranted.

One earlier measurement of mine deserves recording rather than hiding.
Running discovery inside my first partial archive gave 350 tests with 3 errors, because `git archive bench` alone omits `scripts/verify-daemon-crash.py` and the Linux-only namespaces harness.
That was an artefact of my extraction, not a regression, and it is why I re-ran against full trees: 483 and 484, clean, same skips.
Had I reported the first number I would have reported a false failure.

## CI, PR and integration

`ci.yml` is active at the head with `on: pull_request` and no path filter, so a bench-only change does run the Python job (`python3 -m unittest discover -s bench -v`).
No bare-PR-to-zero-check inference is needed and I did not make one.

At the exact head, 3 checks: `check (ubuntu-latest)` pass, `check (macos-latest)` pass, `linux-fuse` pass.
Nothing pending, nothing skipped, nothing neutral.

PR body: `Refs #126`, no `Closes`/`Fixes`/`Resolves` phrasing, no empty GraphQL closing reference, issue 126 still **open**.
The body reports its own two earlier mistakes, a stale archive copy that made a first "after" run exercise the old comparator, and a filename-based arm heuristic; both are the same class of error as the discarded-copy problem in the 125 lane, and I hit the arm-misclassification variant myself while building my own harness, which is independent evidence that the correction was worth making.

Merge into the current `main`, computed with `git merge-tree --write-tree 48c06f9… 5607c31…` from the explicit commits rather than any fetched ref:

```
tree 53b8e9efd0ec704d05d0fe9385d155c993fc4a55
```

Clean, no conflict markers, and the merged tree's `bench/compare.py` is `1376b8fc543a`, identical to the head's, so what I reviewed is what would land.

The diff is 3 files and +215/-4: 12 lines in `compare.py`, 37 in `test_gates.py`, 170 in the evidence doc.
That is bounded, and the source-only part is 3 lines plus one test.

## Scope, ownership and cleanup

I verified before starting that my lease held no conflicting owner and was clean, that the author's lease sat at exactly the PR head with no source modifications pending, and that no process referenced either lease.
I did not check out, branch, edit, reset, stash, commit, push, merge, return a lease or take a new one.

All my work is inside `bench/out/finite-load126-completeness-final-critic/` in my own lease: the extracted trees, the genuine writer receipt, the derived arms, the runner scripts, the logs and the pycache.
Nothing was written to the shared production `bench/out`; I confirmed `gates.OUT` resolves inside my archive by printing it rather than assuming, and the repository shows no new files under `bench/out` and no tracked changes.
Disk headroom was 327 GiB against the 20 GiB floor, and the archive is 24 MiB.
Every heavy step ran as one foreground child through the wave's `mac-heavy.lock` with the documented 600-second bound and exit 75 on contention.
No process was signalled and nothing was deleted, so no flush-before-destroy check was needed.

The prior 129 receipt and report and the unapplied 125 fixtures are untouched: `docs/verification/evidence/finite-load126-repair.md` and the 125 diagnosis doc are unchanged in place, and the 125 patch remains unapplied.

## What this review does not claim

No real performance, fresh-conformance or filesystem-correctness finding is implied.
Nothing here measures cowfs performance; the only measured workload is a one-megabyte g5 write/read on a tiny project, which exists to make the receipts writer-shaped.

I did not touch the shared Mac load situation, any mount, any daemon, any store or any remote host.
Host load was around 13 during my runs, which is below the ceiling 30.0 but high enough that I make no performance claim at all, and every load value I scored is either the writer's documented `COWFS_BENCH_FAKE_LOAD1` hook or a derived fixture, labelled as such.
The `--noise-floor` column and the ceilings were verified as unchanged code and unchanged exit codes, not as a performance measurement.

Tools I could not use, stated rather than worked around: no live GitHub Actions run was dispatched, polled or re-run, and no runner configuration was touched, so the CI verdict is the recorded status of the three existing runs at the exact head, not a run I started.
No MCP graph tool was used for this review; the exploration was small and file-scoped, and `grep` on a pinned tree was the direct tool for blob identity and function extraction.

## Recommendation

**Merge-ready on the scoped evidence.**
The remainder of issue 126 is closed on both arms, in both load-field positions, and in the multi-rep shape, without weakening the finite ceilings, the skew guard, the schema or the exit taxonomy.
Issue 126 can close on the coordinator's call; I am not deciding that here, and I made no issue state change.

The one optional follow-up, which need not block: restore the empty-list guard in `loads()` so the helper cannot raise if a future caller passes no rows.
