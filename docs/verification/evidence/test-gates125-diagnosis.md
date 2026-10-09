# test_gates discovery flake: diagnosis for issue #125

Reviewer lane: read-only diagnosis of the flake measured while reviewing PR #106.
Lease `a53321161c0b6660c9124671c6c6654c`, branch `review/pjdfstest-g3`.
Source read-only throughout: no production edit, no test edit, no commit, no push, no merge, no lease return, no reset, no stash, no rebase, no force, no workflow dispatch, no rerun, no poll, no install, no sudo, no sysctl.
No test was weakened, retried, skipped or ignored.

| | |
| --- | --- |
| failing test | `test_gates.CompareRefuses.test_gates_writes_scale_into_meta_that_compare_accepts` |
| assertion | `test_gates.py:524` `self.assertEqual(self.run_compare([f], f, f)[0], 0)` |
| observed | `AssertionError: 2 != 0` |
| suite | `Ran 92 tests in 7.710s`, `FAILED (failures=1)` |
| topology | `python3 -m unittest discover -s bench`, the command CI runs |
| position | 8th of 92, from the progress line `.......F...` |
| original log | `bench/out/test-gates125-diagnosis/log/ORIGINAL-t-disc.log`, 841 bytes, mode 444 |
| original log sha256 | `d182ae065bdadd821e255d00b1c6abe3a37d36c2cfd153499d7cf2893e145d72` |

## What kind of failure this is, and what it is not

My earlier framing in the PR #106 review was wrong on two points and I am correcting both here.

**There is no child process.** `run_compare` patches `sys.argv` and calls `compare.main()` in-process:

```python
def run_compare(self, native, cowfs, noise=None):
    argv = ["compare.py", "--native", *native, "--cowfs", cowfs] + (["--noise-floor", noise] if noise else [])
    with mock.patch.object(sys, "argv", argv), contextlib.redirect_stderr(io.StringIO()) as err, \
            contextlib.redirect_stdout(io.StringIO()) as out:
        return compare.main(), err.getvalue(), out.getvalue()
```

Measured on the published source at `9e91e75a`: `compare.py` contains **0** `subprocess`, **0** `Popen`, **0** `timeout`, **0** `os.system`.
So this is not a CLI exit code, not a usage error, not a timeout, and not a test that failed to run.
It is an in-process return value of `2` from `compare.main()`.

**It is not a library-source change.** Exact blob identity across refs:

| ref | `bench/test_gates.py` | `bench/gates.py` | `bench/compare.py` |
| --- | --- | --- | --- |
| `22340a9d` | `e325f204` | `487d73e2` | `ee0d1eaf` |
| `9e91e75a` | `e325f204` | `487d73e2` | `ee0d1eaf` |
| `951045f` | `f9d00e07` | `487d73e2` | `67e2d25d` |
| `c07aabce` (current main) | `f9d00e07` | `487d73e2` | `67e2d25d` |

PR #106 changed neither the failing test nor the oracle, so the flake is a pre-existing property of `test_gates.py` interacting with `compare.py`.

A correction to my own baseline, which matters for anyone repeating it: `main` has since moved to `c07aabce`, where both `test_gates.py` and `compare.py` differ from the flaking head.
My earlier "0 of 8 on main" therefore ran **different code** and is not a like-for-like baseline.
The like-for-like comparison is `22340a9d`, which has the identical triple and measured 0 of 8.
Within identical source: 1 of 8 at `9e91e75a`, 0 of 8 at `22340a9d`.

## The literal oracle

`compare.py` line 354 returns `2` only when `unmeasurable` is non-zero.
`unmeasurable` is incremented at line 319 under exactly one condition:

```python
if peak > LOAD_CEILING or skewed or math.isnan(med_r):
    print(f"UNMEASURABLE: load1 peak {peak:.1f} "
          f"(native {la:.1f}, cowfs {lb:.1f}, ceiling {LOAD_CEILING}, skew limit {LOAD_SKEW}x)")
    unmeasurable += 1
    continue
```

The literals are `LOAD_CEILING = 30.0` and `LOAD_SKEW = 2.0`.
`peak` comes from `loads(rows)`, which is the maximum of the **recorded** `load1_before` and `load1_after` fields of the JSONL rows, not from a live `os.getloadavg()` at comparison time.
The module docstring states the intent: "Ratios are refused, not printed, when the machine was too loaded for them to mean anything: load1 above 30 on either side, or the two arms more than 2x apart ... the gate is marked unmeasurable instead."

So `2` is a deliberate, documented UNMEASURABLE verdict. The test asserts `0`, which means it asserts "this gate was measurable".

## Structural fragility, and the falsifier

The test generates its fixture with `--reps 1`, so the file holds a meta row plus exactly **one** g5 rep row.
There is therefore no median to average out a transient excursion: one recorded sample decides the verdict.

I ran the published `gates.py` and `compare.py` from a clean `git archive` of `9e91e75a` in my own private copy, reproduced the test's own generation step in-process, and then varied only the input.

| input | recorded load | `compare.main()` |
| --- | --- | --- |
| as produced | 22.972 | **0** |
| same rows, load raised | 31.0 | **2** |
| same rows, load at the ceiling | 30.0 | 0 |
| same rows, load just under | 29.9 | 0 |
| same rows, NaN load | NaN | 0 |
| same rows, skew attempt with one row per arm | 25.0 | 0 |

Positive witness captured from the raised case:

```
g5  1  1  0.0130  0.0130  1.000  1.000  1.000  31.0  UNMEASURABLE: load1 peak 31.0 (native 31.0, cowfs 31.0, ceiling 30.0, skew limit 2.0x)
```

Two comparisons settle this: **same input two ways**, where identical bytes differing only in the recorded load flip `0 -> 2`; and **working side versus broken side**, where the as-produced file passes and the raised file returns `2`.
The boundary is `>` and not `>=`, which the 30.0 and 29.9 rows pin from both sides.
The `NaN` and `skew` sub-conditions did not fire and are not this mechanism.

## Reproduction status: UNREPRODUCED for the original event

The oracle and its boundary are measured and falsified above.
What is **not** established is that the load at the moment of the original failure actually exceeded 30.0.

The original artifact cannot settle it.
`run_compare` captures the child's stdout into `out` and returns it, but the test asserts only element `[0]`, so the `UNMEASURABLE` line was discarded and never reached the log.
The preserved 841-byte log contains **zero** occurrences of `UNMEASURABLE`, `load1 peak`, `ceiling` or `RESULT:`.

I am not claiming the load explanation as the confirmed cause of the original event.
I am claiming that the assertion is decidable by one recorded load sample, that the sample is the only thing that separates `0` from `2`, and that the artifact lacks the witness.

One further measured detail, offered without a causal claim: live `os.getloadavg()[0]` was between 13.8 and 17.8 across this session while `gates.py` recorded 22.972 for the same period.
The recorded value is not simply the live value sampled at comparison time.
I drew no scheduling conclusion, and n=8 cannot support one.

## Narrow falsifier plan, not a fix

One added line, in a private copy only, no source change:

run the `discover -s bench` topology with a `compare.main()` wrapper that, whenever it returns `2`, prints the recorded `load1_before` and `load1_after` values, the `unmeasurable` count, and the `UNMEASURABLE` line the test currently discards.

That single observation decides between the two live hypotheses:

- a recorded sample above 30.0 appears, and the cause is confirmed. The underlying defect is then that `test_gates` asserts a load-sensitive verdict on a `--reps 1` fixture, and the fix belongs in the fixture or the assertion, not in a retry, skip or ignore.
- no sample above 30.0 appears while `main()` still returns `2`, and one of the other two sub-conditions or an unexamined path is responsible, which the same print identifies.

Until that line exists, reproduction is UNREPRODUCED and no rerun-until-lucky is warranted.

## Scope, and what I deliberately did not do

This lane is diagnosis only.
PR #106's builder is repairing its own licence attribution, fixture metadata and raw-path portability issues, and those files are the other owner's; I did not review the changing head and did not touch `test_gates`.

All my inputs are my own copies under `bench/out/test-gates125-diagnosis/**`: a clean `git archive` of `9e91e75a`, the preserved original log, and the spike's generated and variant JSONL files.
No other lane's run directory was read for this diagnosis, and no derived artifact of another lane was written, which is the failure mode I recorded against myself in an earlier round.
No new daemon, mount, build or full-suite run was started; one representative CLI run of the single published test was executed in my own archive under the shared Mac lock, in one bounded foreground wait, and it passed.

Machine state during the work, for the record: load1 between 13.8 and 17.8 on a 16-core host, 361 GiB free.
The protected shared mount and its owning process, all leases, and the g4, g5, 987929D and 899604 mounts were left untouched, with no signal, unmount or mount walk.