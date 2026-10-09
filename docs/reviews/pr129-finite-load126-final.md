# PR 129 final review: finite recorded load before comparator acceptance

Reviewed head: `98057e7b68c6b557356ca9e431919d6c087f8870`, branch `fix/finite-load-126`.
Author's base: `42a2efacfbabcd122488db88c2058fb89145cef7`. Current primary `main`: `00065ce75dcd554e1fb4bb084d1c70b2e2a21a87`.
Tracker: issue #126, open.
Scope: the seven production lines in `bench/compare.py`, the narrow `bench/test_gates.py` regression, and the evidence receipt.
No checkout, branch change, source edit, reset, stash, commit, push, merge or lease return was performed.

## Verdict

**PASS.** The defect is real, reproduced through both published CLIs, and closed on both arm orders, including the order the author's own table omits.
Nothing here is a blocker.

One documentation claim in the brief is contradicted by both the workflow config and the head's own check runs, and it is recorded below rather than repeated.

| item | result |
| --- | --- |
| the defect: non-finite load scored as a clean measurement | **reproduced**, exit 0 `RESULT: PASS` on the unfixed source |
| the fix, both arm orders | **PASS**, all four orderings now UNMEASURABLE 2 |
| the order the author's table omits (quiet native / NaN cowfs) | **PASS**, 2 on the head, 0 on the base |
| finite do-nothing baseline | **PASS**, 3.5 / 3.5 is still PASS 0 |
| finite ceiling and skew guards | **PASS**, 31 / 31 is 2, 3.5 / 20 is 2, unchanged |
| inf, -inf, missing, negative | **PASS**, all INVALID 3 on base and head |
| `LOAD_CEILING`, `LOAD_SKEW`, `verdict`, `is_nonneg_load`, schema, sampling | **untouched**, verified unchanged |
| new tests fail on the unfixed source | **2 of 3**, named and attributed; the third is the baseline |
| scoped test counts | `CompareRefuses` 17, `test_gates` 38, whole bench 483 with 13 pre-existing skips |
| scoped lint | **not clean, and that is parity**, pre-existing, one finding fewer at head |
| CI at the exact head | **3 check runs, all completed/success** |
| PR refs | `closingIssuesReferences` empty, no closing phrase, #126 open |
| merge into current `main` | **clean**, tree `40735240…`, no conflict |

## Base drift: the delta main shows is not the author's delta

This matters, so it is stated first.
Diffing current `main` against the head shows eight files and 1,262 deletions, which looks alarming and is entirely an artefact of the branch point.
The merge base of the head and current `main` is exactly the author's base `42a2efac`, and `main` has advanced by one commit, `00065ce7` "test(nfs): integrate reviewed server requirements harness (#113)".
That commit touched `Cargo.lock`, `crates/cowfs-nfs/Cargo.toml`, `crates/cowfs-nfs/tests/requirements19.rs` and two docs, and **neither `bench/compare.py` nor `bench/test_gates.py` moved**: both blobs are `67e2d25d…` and `f9d00e07…` at the branch point and at current `main`.

So the source pin holds, the branch applies to current `main` unchanged, and the real delta is the author's own: three files, +207, -2.

I extracted three trees read-only and proved each byte-identical to its commit before relying on it: the head `98057e7` at 632 files, the author's base `42a2ef` at 631, and current `main` `00065ce7` at 634.

## The defect, reproduced end to end

Every receipt below was produced by the **published `gates.py` CLI**, unmodified, with the load supplied through `COWFS_BENCH_FAKE_LOAD1`, which is that script's own documented hook, and then fed to `compare.py` **as a subprocess**, so every exit code is the published CLI's own.
The workload was a real but tiny `g6` metadata storm at `COWFS_BENCH_SCALE=0.02`, which resolves to ten files.
That is not a CPU benchmark and makes no performance claim; it exists only so the receipts are records the harness itself wrote, with real `wall_s`, real `metrics` and fsynced append-and-flush per rep.

`gates.py` writes to `bench/out` through the module constant `OUT` with no flag. I confirmed by import that `OUT` resolves inside my private archive and not into the production tree, so the shared directory was never a target; every receipt also carried a lane-unique label and was copied out before `compare.py` read it.
The production `bench/out` was not written, and the primary checkout shows zero tracked changes under `bench/`.

### What `loads()` does, and why the guard vanished

`loads()` drops every `NaN` and returns `float("nan")` when nothing finite remains, and that function is **identical at base and head**; the delta does not touch it.
The acceptance seam then asked `peak > LOAD_CEILING`, and every comparison against `NaN` is `False`, so an unreadable load satisfied the load guard.
The guard did not lose precision, it disappeared.

The sharp form is worse, and this is the part worth being exact about: `max()` is not symmetric on `NaN`.
I measured `max(31.0, nan)` is `31.0`, `max(nan, 31.0)` is `nan`, and `max(3.5, nan)` is `3.5`.
So a single unreadable native arm poisons the peak and launders a genuinely over-ceiling cowfs arm into a `PASS`.
Verbatim from the unfixed source, native arm `NaN`, cowfs arm `31.0`:

```
g6  2  2  0.0015  0.0058  4.258  0.826  7.689  nan  report (no bar)
RESULT: PASS  scope: compared 1 of 6 (g6), not compared g1 g2 g3 g4 g5
```

and the same inputs at the head:

```
g6  2  2  0.0015  0.0058  4.258  0.826  7.689  nan  UNMEASURABLE: no finite load1 was recorded (native nan, cowfs 31.0), so the ceiling 30.0 cannot be checked
RESULT: 1 gate(s) unmeasurable, 0 failed  scope: compared 1 of 6 (g6), not compared g1 g2 g3 g4 g5
```

## The full matrix, measured through the public CLIs

Both columns are the same receipts, the same `compare.py` invocation, differing only in which tree's classifier ran.
Every head value was also checked for tracebacks; none occurred.

| case | load native / cowfs | want | base | head | verdict |
| --- | --- | --- | --- | --- | --- |
| finite-low | 3.5 / 3.5 | 0 | 0 | 0 | do-nothing baseline holds |
| finite-over | 31 / 31 | 2 | 2 | 2 | ceiling guard intact |
| finite-skew | 3.5 / 20 | 2 | 2 | 2 | skew guard intact |
| nan-both | nan / nan | 2 | **0** | 2 | defect closed |
| nan-native-only | nan / 3.5 | 2 | **0** | 2 | defect closed |
| nan-cowfs-only | 3.5 / nan | 2 | **0** | 2 | defect closed, the order the author's table omits |
| nan-hides-busy | nan / 31 | 2 | **0** | 2 | defect closed, the sharp case |
| inf-both | inf / inf | 3 | 3 | 3 | validation preserved |
| neg-inf-both | -inf / -inf | 3 | 3 | 3 | validation preserved |
| missing-load | absent | 3 | 3 | 3 | validation preserved |
| negative-load | -1 / -1 | 3 | 3 | 3 | validation preserved |

Eleven rows, zero head mismatches.
`inf`, `-inf` and the negative value came from the public writer through the documented hook; `missing-load` is a real receipt with only the two load keys removed, and it round-trips through the same reader.
All four report `INVALID 3` with a typed message naming the offending field, on both base and head, so the delta did not weaken existing validation.

`nan-cowfs-only`, quiet native against NaN cowfs, is the ordering the author's evidence table does not list, and it was the one that would have survived a lazier fix.
The author's comment says exactly why: an `isnan(peak)` guard would fix `nan-hides-busy` and leave this order still passing.
Testing the arms rather than the peak is the correct choice, and the measurement confirms it closes both.

## Admissibility is not measurability, and the change says so

This is the substantive judgement in the delta, so it is worth stating precisely.

`is_nonneg_load` admits `NaN` **on purpose**, because `gates.py` writes `NaN` when `os.getloadavg()` fails, which is a real condition rather than a corrupt file.
The fix therefore does **not** touch `is_nonneg_load`, and `NaN` remains valid input that never produces `INVALID`.
That is correct, and the issue's own instruction requires it: "distinguish INVALID input 3 from finite unsupported/unmeasurable 2", and "do not weaken load gates to make tests pass".

What changed is the acceptance seam: a comparison with **no finite recorded load can no longer support a verdict**, and reports `UNMEASURABLE` 2 instead.
The declared meaning is the right one: the sentinel is a valid *record* of an unmeasured quantity, and an unknown load cannot prove the machine was quiet.
So the boundary the author drew is "the record is admissible, the measurement is not", and it is the boundary the tracker asked for.

I checked whether any document intentionally licenses unknown-measurement acceptance, since that would have made this a policy block rather than a pass.
Nothing does.
`docs/design.md` and the ready-g documents treat an unmeasured quantity as unmeasured, and issue #126 explicitly requires that "a nonfinite observation must not silently certify measurement quality".
The evidence doc states the contract as unchanged and says a run with no finite load reports `unmeasurable` "rather than claiming a measurement", which is exactly the distinction above.
So this is a policy-conforming fix, not a policy change smuggled in behind a classifier edit, and no block is warranted.

## The fix, and what was deliberately left alone

Seven lines at the acceptance seam, before the existing load guard, testing the two arms rather than the peak:

```python
if math.isnan(la) or math.isnan(lb):
    print(f"UNMEASURABLE: no finite load1 was recorded (native {la:.1f}, "
          f"cowfs {lb:.1f}), so the ceiling {LOAD_CEILING} cannot be checked")
    unmeasurable += 1
    continue
```

Verified untouched, by reading the head source rather than taking the doc's word: `RATIO_BAR = 1.5`, `LOAD_CEILING = 30.0`, `LOAD_SKEW = 2.0`, `verdict()`, `is_nonneg_load`, the input schema, and `gates.py`'s load sampling.
The finite ceilings are not weakened; they are still enforced, as rows `finite-over` and `finite-skew` show.

One scope judgement is correct and I checked it rather than assuming: the `--noise-floor` column still prints `peak` with no NaN flag.
I confirmed that block contains no `isnan` guard and still carries `flag = "unmeasurable" if peak > LOAD_CEILING`, and that the only exits after it are driven by `unmeasurable` and `fails` from the compared gates.
The noise floor therefore feeds no exit code and is not an acceptance path, so leaving it alone is right rather than an oversight.

The measured-source-context false-PASS protection from PR #106 is untouched, since this delta adds no production file and modifies only the load seam.

## The three tests, and which two actually catch the defect

The pre-existing `test_nan_load_is_allowed_but_zero_wall_is_unmeasurable` asserted the defect as intended, with `assertEqual(..., 0)` on an all-`NaN` load.
That is the honest history: the suite was not merely silent, it was pinning the wrong behaviour, and the issue reverses the assertion.
It is renamed and replaced, not duplicated; the old name appears nowhere in the tree.

To test "fails on the unfixed source" honestly I built a cross tree holding the **new** `test_gates.py` with the **base** `compare.py`, confirmed by hash (`c4fe5785…` for the tests, `42675c46…` for the base classifier), and ran the three there:

| test | unfixed source | line | why |
| --- | --- | --- | --- |
| `test_nan_load_is_valid_input_but_cannot_support_a_verdict` | **FAIL** | 502 | `0 != 2` |
| `test_a_nan_arm_cannot_launder_an_over_ceiling_run` | **FAIL** | 526 | `0 != 2` on `nan native, busy cowfs` |
| `test_finite_load_still_decides_the_verdict` | PASS | | the do-nothing baseline; a finite 3.5 was already scored on the base |

**Two of three fail**, matching the author's report, and the third passing is correct rather than a weak test: it is the baseline that stops the fix from being "refuse everything".
Each of the two defect tests also asserts its own specific message rather than a bare number, and `test_nan_load...` asserts **no `INVALID`**, which pins the input contract as well as the verdict.
The four arm orderings are all covered.

No misattribution: the passing third test is genuinely new code, and it is labelled in the evidence doc as the do-nothing baseline.

## Counts and lint, measured rather than quoted

| suite | collected | result |
| --- | --- | --- |
| `CompareRefuses` | 17 | OK |
| whole `bench/test_gates.py` | 38 | OK |
| whole bench discovery | 483 | OK, 13 skips |

Whole-bench discovery was run twice and returned `Ran 483 tests, OK (skipped=13)` both times, so the count is stable rather than a lucky run.
The 13 skips are pre-existing and platform-conditional: no `/proc`, no mount namespace, non-POSIX helper, a root-caller uid check, and the root permission test.
None was silenced, disabled or ignored by this delta, and none is in the files it touches.
I did not patch or disable issue #125's `test_gates_writes_scale_into_meta_that_compare_accepts`; it ran and passed.

On the author's ruff claim, the honest reading is that "parity" means *not clean*, and that is accurate.
Base has six findings across the two files, head has five.
Nothing was introduced; one pre-existing `RUF059` disappeared because the two `err` sites were inside the old `NaN` test that this change replaces, so the reduction is a side effect of deleting dead locals rather than a suppression.
Every finding at head sits at line 74, 177, 194, 210 or 551, and the delta's hunks are all at 494 and above, so no finding is on a line this delta added.
The pre-existing `RUF100` on `import gates  # noqa: E402` is at line 74 and untouched; fixing it would be unrelated lint scope, which I did not take.
`py_compile` is clean on both files.

## CI, and the claim that is contradicted

The brief carried the author's position that no checks are configured.
One read-only snapshot at the exact head, and the workflow file itself, contradict it:

| check | status | conclusion |
| --- | --- | --- |
| `linux-fuse` | completed | success |
| `check (ubuntu-latest)` | completed | success |
| `check (macos-latest)` | completed | success |

`total_count` is 3.
`ci.yml` at this head triggers on `push` to `main` and on `pull_request` with **no branch or path filter**, so every pull request is in scope, and it declares exactly the two jobs observed: a `check` matrix over `ubuntu-latest` and `macos-latest`, plus `linux-fuse`.
The combined legacy status endpoint reports `pending` only because it holds zero legacy contexts, which is not a failure and is not a missing check.

So this is neither "no checks reported" nor "pending": CI is **green at `98057e7`**, which is the usual merge gate and is satisfied here on its own evidence.
No dispatch, rerun, poll or runner configuration change was made, and the runners were left alone.

## PR, refs and integration

PR #129 is **not a draft**: open, base `main`, head `fix/finite-load-126` at `98057e7b68c6b557356ca9e431919d6c087f8870`, `mergeable_state` clean, 3 changed files, +207, -2.
`closingIssuesReferences` is empty and the single commit subject "fix(bench): refuse a verdict when the recorded load is not finite" contains no closing phrase, so nothing auto-closes; the body says "Untouched: ... and CI", which is accurate.
Issue #126 remains **open**, correctly, since the tracker asked for the current-main behaviour to be verified and that verification is this review rather than the PR.

Integration against the **explicit** current `main` `00065ce7`, not a fetch head: `git merge-tree --write-tree 00065ce7 98057e7` wrote tree `40735240da415ec2351090c46a2697637aa5883b` with no conflict.
Main's one intervening commit touched neither file this delta modifies, which is the source-pin proof that the branch still applies as reviewed.
No combined-runtime claim is made: nothing was built or measured through a merge.

The evidence receipt in the primary checkout verifies at `2e5a4a9e50db6b4c3be08f3a9d92f16d02b5c56cc4bf94745ce4fa354e037e05`, matching the digest given for it, 7088 bytes.
Its `git hash-object` is `9b7053b0…`, identical to the blob committed at `98057e7`, and byte-comparison against `git show 98057e7:…` is identical, so the mirror is the committed document and not a local variant.
It also correctly refuses the historical blob as evidence, noting every result was produced against `67e2d25d`, which is the discipline this review followed as well.

## What this review did not do

No performance measurement, no fresh-conformance claim and no filesystem-correctness finding; this is validation behaviour only, as the author states and as the data supports, since the workload was ten files.
No new production field, no numeric field, no schema or framework change.
No broad mutant matrix; the matrix above is the eleven rows the issue names plus the two orderings that close the sharp case.
No signal sent, no mount walked, no borrowed state cleaned, no install, sudo, sysctl, reboot, force, reset, rebase, workflow change, dispatch, rerun, poll, commit, push, merge or lease return.
Shared local state was observed only: the daemon at pid 15263 with its store, mount and socket, the g4 and g5 pids, and the two nfs mounts.
The parked issue #125 artefacts were not modified; I noted that the paths named for them are not present under this worktree's `bench/out`, which I recorded rather than treated as permission to create or repair them.

The only writes are this report in the primary checkout and this lane's own evidence under `bench/out/finite-load126-final-critic/**` in the assigned worktree, which `.gitignore` covers at `/bench/out/`.
My own `__pycache__` artefacts were confined to the private archives and removed; the assigned worktree still sits on `investigate/integrity-21` at `016769e7f4076a5c0fc712a65932c546048052f7` with zero tracked-file changes, and the production `bench/out` was never a write target.
The builder lane for issue #122 owns `BUILDTRAIN5` GC tests and control/common only and was not touched.

## Recommendation

Land it.

The defect was real and severe in kind: a load guard that vanished rather than degraded, able to certify a run at load 31 against a 30 ceiling as a clean pass.
The fix is seven lines at the correct seam, tests the arms rather than the peak so both `max()` orderings are covered, preserves the input contract that `NaN` is a valid record of an unmeasured quantity, preserves every finite ceiling and skew guard, leaves the four existing `INVALID 3` validations untouched, and ships a do-nothing baseline so the change cannot degenerate into refusing everything.
The tests fail on the unfixed source for the right reason and are attributed honestly.
CI is green at the exact head on its own evidence, the merge into current `main` is clean, and issue #126 should close on merge with a human setting it.

Nothing in this delta needs another pass.
The one item worth carrying forward is not a defect but a documentation correction: the position that no CI is configured is wrong, and any future summary of this PR should say CI is green rather than absent.