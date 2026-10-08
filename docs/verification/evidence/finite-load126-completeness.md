# Issue 126 remainder: a partly unknown load was still certifying measurement quality

## Scope of this round

PR 129 (`dadc524`) fixed the wholly-`NaN` case, where every recorded load observation in an arm
was unreadable.
That guard asks whether `loads()` returned `NaN`.

`loads()` computed its answer by dropping every `NaN` before taking the maximum, so an arm with
some readable samples and some unreadable ones returned the readable maximum.
The merged guard could not observe the unknown observation, and the comparison was scored.

This is the remainder of the existing issue 126 requirement, which reads:

> A nonfinite observation must not silently certify measurement quality.

The hypothesis was recorded as unverified before any test.
It is now established end to end through the published CLIs.

## Reproduction through the published CLIs

The writer is the published `gates.py`, unmodified.
It records one load per observation, so a mixed pair cannot come from the writer alone.
The writer was still used to produce a real 1280-byte receipt carrying a genuine `meta` record,
real g5 byte accounting and a real `wall_s` of 0.007053 s, which establishes that the schema,
the accounting and the rep shape are `gates.py`'s own.
The mixed observation is then derived from that receipt by rewriting one load field, every
derived file is labelled with a `derived_by` field naming its source, and the harness verifies
each derived file actually contains the case's loads before scoring it.

`compare.py` was run as a subprocess, so every exit code is the published CLI's own.

The comparison ran inside a private archive of the pinned tree.
`gates.py` resolves `OUT` from `__file__`, so the archive's copy wrote into the archive's own
`bench/out`, which was confirmed by printing `gates.OUT` rather than assumed.
The shared `bench/out` was not written to by this round.

| case | recorded load, native / cowfs | required | before | after |
| --- | --- | --- | --- | --- |
| `NaN` before, finite after, native | nan / 3.5 versus 3.5 / 3.5 | 2 | **0** | 2 |
| finite before, `NaN` after, native | 3.5 / nan versus 3.5 / 3.5 | 2 | **0** | 2 |
| `NaN` and busy, cowfs | 3.5 / 3.5 versus nan / 31.0 | 2 | 2 | 2 |
| all finite, quiet | 3.5 / 3.5 | 0 | 0 | 0 |
| all finite, busy | 31.0 / 31.0 | 2 | 2 | 2 |
| all finite, skew | 3.5 / 3.5 versus 20.0 / 20.0 | 2 | 2 | 2 |
| all infinite | inf / inf | 3 | 3 | 3 |

Two cases changed from `0` to `2`.
Both were `RESULT: PASS` before, which is the requirement being violated: a single unreadable
observation certified the quality of the whole comparison.

The third mixed case already returned `2` before the change, but for the wrong reason.
Its finite sample was 31.0, so it tripped the ceiling rather than the missing observation.
It is retained as a case that distinguishes the two mechanisms.

The mechanism was also confirmed directly, without inference from the exit codes:

```
loads(NaN then 3.5) = 3.5      merged isnan(la) guard fires: False
loads(3.5 then NaN) = 3.5      merged isnan(la) guard fires: False
loads(NaN then 31.0) = 31.0    merged isnan(la) guard fires: False
loads(31.0 then NaN) = 31.0    merged isnan(la) guard fires: False
```

Both orderings were affected, which is why the tests cover both.

## The fix

Three lines in `loads()` in `bench/compare.py`, which is the seam the issue's own description
names:

```python
if any(math.isnan(v) for v in vals):
    return float("nan")
return max(vals)
```

The caller's existing `isnan(la) or isnan(lb)` guard then refuses the comparison, so no new
branch, no new exit code and no new message were needed.
`UNMEASURABLE` remains exit 2, and `NaN` remains valid input that never produces `INVALID 3`,
which is what the contract requires and what the merged 129 tests already assert.

Unchanged: `LOAD_CEILING`, `LOAD_SKEW`, `verdict()`, `is_nonneg_load`, the input schema,
`gates.py` load sampling, and the `--noise-floor` column, which feeds no exit code and is
confirmed absent from the diff.

## Tests

One test added, `test_a_partly_unknown_load_is_not_a_measurement`.
It covers `NaN` in either field, in either arm, against both a quiet and a busy peer, and a
three-rep arm where only one rep is unreadable, which is the realistic multi-rep shape.
Every case asserts exit 2, `UNMEASURABLE` present, `PASS` absent and `INVALID` absent.

The four load tests merged by 129 were left in place and still pass, so nothing from the merged
work is reverted.

Failing first was confirmed against the merged comparator at blob `2fd7c3c`, with my new test
and the unmodified merged `compare.py`:

```
AssertionError: 0 != 2 : ('native nan before, finite after with quiet peer', ...)
FAILED (failures=1)
```

## Verification

| check | result |
| --- | --- |
| new test against merged `compare.py` `2fd7c3c` | failed `0 != 2` |
| new test plus the four merged load tests, fixed | 4 passed |
| mixed-observation CLI, merged comparator | 2 of 7 cases wrong |
| mixed-observation CLI, fixed comparator | 0 of 7 cases wrong |
| `CompareRefuses` class | 18 passed |
| `bench/test_gates.py` module | 39 passed, base was 38 |
| whole bench discovery | 484 passed, 13 pre-existing skips |
| `py_compile` both files | clean |
| `ruff check bench/compare.py` | parity with base `48c06f9` |
| `ruff check bench/test_gates.py` | parity with base `48c06f9` |

Lint is at parity on both files, not merely no-worse: `3 E402` and `1 RUF100` on `compare.py`,
`1 RUF015`, `2 RUF059` and `1 SIM117` on `test_gates.py`, identical before and after.
`EXE001` is excluded from the comparison because the baseline copies are mode 644 while the
repository files are mode 755.

Whole-bench discovery was run because `compare.py` is central to the harness.

## Two corrections to my own work in this round

The first fixed-code CLI run reported the two mixed cases still returning `0`.
That was not the fix failing.
My `cp` of the fixed `compare.py` into the private archive had not landed, so the run exercised
the old comparator again.
The archive copy was checked by blob, found still at `2fd7c3c`, and only then re-copied and
confirmed at `1376b8fc`; the before and after runs above were then produced deliberately with
the archive blob verified before each run.
This is the same class of error as the discarded-copy mistake in the 125 lane, where a run was
reported without confirming which file it actually loaded.

My harness also chose which arm to write by testing whether `"native"` appeared in the
filename, which misfires on a case named `...-in-native.jsonl` used as the cowfs arm.
The arm is now passed explicitly, and each derived file is checked against its case before
being scored, so a mislabelled fixture aborts instead of quietly producing a wrong verdict.

## Identity

- base commit `48c06f9cf9d323bb626d902cd0e7b2247beaf055`, confirmed equal to `origin/main`
- `bench/compare.py` base blob `2fd7c3c5139ec0afa5baffa3bac4d1ed573aac59`, which includes the
  merged 129 guard
- `bench/compare.py` after this change `1376b8fc543a7a9bd3fbd5df536345efc84a8d6b`
- `bench/gates.py` `487d73e28b08722c2917059a11c94ad7961ec51c`, unmodified
- `bench/test_gates.py` base blob `e9255843f98dc8778e3b9466126f1418479c02f3`
- diff: 12 lines in `compare.py`, 37 in `test_gates.py`

Raw logs, the reproduction script, the real writer receipt and every derived receipt are under
`bench/out/finite-load126-completeness/` in the working lease and are not committed.

The earlier `finite-load126` receipts and the `finite-load126-repair.md` report are immutable
and untouched by this round.

## Not claimed

No real performance, fresh-conformance or filesystem-correctness finding is implied.
No claim is made about any measurement taken on a host whose load could not be read; such a
comparison is refused rather than scored, which is the whole point.

The existing `docs/design.md` load and acceptance rules were read before the change and are not
reinterpreted here.

Issue 126 stays open pending coordinator review of this remainder and an independent fresh
review of this diff.