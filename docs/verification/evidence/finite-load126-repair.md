# Issue 126: a non-finite recorded load was scored as a clean measurement

## What was wrong

`compare.py` accepted `NaN` as a legal recorded load and then treated the resulting
comparison as if the load had been measured and found low.

`is_nonneg_load` deliberately admits `NaN`, because `gates.py` writes `NaN` when
`os.getloadavg()` fails, and that is a real condition rather than a corrupt file.
`loads()` drops every `NaN` and returns `float("nan")` when nothing finite is left.
The acceptance seam then asked `peak > LOAD_CEILING`, and every comparison against
`NaN` is `False`, so an unreadable load satisfied the load guard.

The guard therefore did not merely lose precision, it disappeared, and the run
finished with `RESULT: PASS` and exit 0, indistinguishable from a quiet machine.

The sharpest form is worse than a lost guard.
`max()` is not symmetric on `NaN`: `max(NaN, 31.0)` is `NaN`, but `max(3.5, NaN)`
is `3.5`.
A single unreadable native arm therefore poisoned the peak and laundered a
genuinely over-ceiling cowfs arm into a `PASS`.

## Reproduction through the published CLIs

Every receipt was produced by the published `gates.py` CLI, unmodified, with the
load value supplied through `COWFS_BENCH_FAKE_LOAD1`, which is `gates.py`'s own
documented hook for exactly this purpose.
`compare.py` was then run as a subprocess, so every exit code below is the
published CLI's own.

`gates.py` writes to `bench/out` through a module constant with no flag, so each
receipt carried a lane-unique label and was moved into this lane's directory
before `compare.py` ever read it.
One partial meta-only receipt left in the shared directory by an early harness
crash was identified and relocated, not deleted.
No other lane's inputs or derived files were written.

| case | recorded load, native / cowfs | expected | exit before | exit after |
| --- | --- | --- | --- | --- |
| `finite-low` | 3.5 / 3.5 | PASS 0, the do-nothing baseline | 0 | 0 |
| `finite-over` | 31 / 31 | UNMEASURABLE 2, load guard intact | 2 | 2 |
| `nan-both` | nan / nan | UNMEASURABLE 2 | **0** | 2 |
| `nan-native-only` | nan / 3.5 | UNMEASURABLE 2 | **0** | 2 |
| `nan-hides-busy` | nan / 31 | UNMEASURABLE 2 | **0** | 2 |
| `inf-both` | inf / inf | INVALID 3 | 3 | 3 |
| `neg-inf-both` | -inf / -inf | INVALID 3 | 3 | 3 |
| `missing-load` | absent | INVALID 3 | 3 | 3 |
| `negative-load` | -1 / -1 | INVALID 3 | 3 | 3 |

`inf-both`, `neg-inf-both`, `missing-load` and `negative-load` were already
correct before the change.
The defect is confined to `NaN`, which is the one non-finite value the input
contract admits on purpose.

The three bolded rows are the defect.
`nan-hides-busy` is the one that matters: a cowfs arm recorded at load 31.0,
above the 30.0 ceiling, was reported `PASS` because the native arm's load could
not be read.

## The fix

Seven lines in `bench/compare.py`, at the acceptance seam, before the existing
load guard:

```python
if math.isnan(la) or math.isnan(lb):
    print(f"UNMEASURABLE: no finite load1 was recorded (native {la:.1f}, "
          f"cowfs {lb:.1f}), so the ceiling {LOAD_CEILING} cannot be checked")
    unmeasurable += 1
    continue
```

The arms are tested rather than `peak`, because of the `max()` asymmetry above.
An `isnan(peak)` guard would have fixed `nan-hides-busy` and left
`quiet native / nan cowfs` still passing.

The contract is unchanged.
`NaN` remains valid input and still does not produce `INVALID`, because
`gates.py` writes it for a real `getloadavg` failure.
What changed is that a comparison with no finite recorded load can no longer
support a verdict, and reports `UNMEASURABLE` instead, which is exit 2 and says
`unmeasurable` rather than claiming a measurement.

Deliberately not changed: `LOAD_CEILING` 30.0, `LOAD_SKEW` 2.0, `verdict()`,
`is_nonneg_load`, the input schema, and `gates.py`'s load sampling.
The `--noise-floor` column still prints its `peak` without the new flag; it feeds
no exit code, so it is not an acceptance path and was left alone.

## Tests

The pre-existing `test_nan_load_is_allowed_but_zero_wall_is_unmeasurable`
asserted the defect as intended, with `assertEqual(..., 0)` on an all-`NaN` load.
It is replaced by three tests, since the issue reverses that assertion.

- `test_nan_load_is_valid_input_but_cannot_support_a_verdict` keeps the
  zero-wall coverage from the old test and now requires exit 2, `UNMEASURABLE`,
  no `PASS`, and specifically no `INVALID`, which pins the input contract as well
  as the verdict.
- `test_a_nan_arm_cannot_launder_an_over_ceiling_run` covers all four arm
  orderings, including the two that an `isnan(peak)` guard would miss.
- `test_finite_load_still_decides_the_verdict` is the do-nothing baseline: a
  finite load under the ceiling still scores 0, a finite load above it is still
  refused, and a finite skew is still refused.

Every assertion that inspects a return code passes the captured output as the
failure message, so a regression reports the verdict rather than a bare number.

Failing first was confirmed: the two defect tests fail with `0 != 2` against
unfixed `compare.py`, while the do-nothing baseline passes both before and after.

## Verification

| check | result |
| --- | --- |
| the three load tests, unfixed source | 2 failed, 1 passed |
| the three load tests, fixed source | 3 passed |
| `CompareRefuses` class | 17 passed |
| `bench/test_gates.py` module | 38 passed, was 36 |
| whole bench discovery | 483 passed, 13 pre-existing skips |
| `py_compile` both files | clean |
| `ruff check bench/compare.py` | parity with `origin/main`, 3 E402 and 1 RUF100 |
| `ruff check bench/test_gates.py` | one fewer error than `origin/main` |

The `RUF059` reduction is not a suppression.
The two `err` sites that disappeared were both inside the old `NaN` test that
this change replaces.

Whole-bench discovery was run because `compare.py` is central to the harness
rather than incidental to it.
No other consumer asserted the old behaviour: the remaining `isnan` uses in
`compare.py` are the load helper itself and the unrelated ratio median.

## Scope

Owned and modified: `bench/compare.py`, `bench/test_gates.py`.
Untouched: every other test, all production thresholds and ceilings, the harness
modes, runtime load sampling, CI and workflows.
No claim is made here about real measured performance; this is validation
behaviour only.

## Identity

- base commit `42a2efacfbabcd122488db88c2058fb89145cef7`, which was `origin/main`
  at the time of work
- `bench/compare.py` base blob `67e2d25d36f63dee43beeb43c738cd782d57a265`
- `bench/test_gates.py` base blob `f9d00e073cdafc273e8cfc0f2d96ba77be526507`
- diff: 7 insertions in `compare.py`, 46 in `test_gates.py`, 2 deletions total

Raw logs, the reproduction script and every receipt are under
`bench/out/finite-load126/` in the working lease and are not committed.

## Not claimed

The historical `NaN`-passes observation at `ee0d1eaf` is not the evidence here.
Every result above was produced against `67e2d25d`, and the historical blob
differs in both `compare.py` and `test_gates.py`.
