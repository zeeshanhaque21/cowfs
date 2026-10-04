# Partial gate coverage in the criterion 2 comparator (issue #80)

## The defect

`bench/compare.py` compared a gate only when both arms had reps for it, and `continue`d past every gate that did not.
A comparison whose native arm ran `g1` and `g3` while the cowfs arm only ever produced `g1` reps printed one `g1` row and then `RESULT: PASS`.
Nothing said that `g3` had no cowfs data, so a partial comparison read as though the whole criterion had been exercised.

Reproduced on the baseline (`ceb96c6`, `bench/compare.py` at `ee0d1ea`) through the real CLI, with `bench/out/coverage80/fixture/native1.jsonl` and `native2.jsonl` carrying `g1` and `g3`, `cowfs1.jsonl` carrying only `g1`, and `cowfs1`'s meta recording that it had been asked for `g1,g3`.

```
gate  n_nat n_cow  native_s   cowfs_s   med_x   min_x   max_x   load  result
----------------------------------------------------------------------------------------
g1        2     1    8.1000    9.6000   1.200   1.200   1.200    1.0  PASS (bar 1.5x)
...
g5   not run (no input has g5 reps)
RESULT: PASS
rc=0
```

`g3` appears nowhere in the comparison and nowhere in the verdict, although the native arm ran it twice and the cowfs arm was asked for it.
The same shape held on real `gates.py` output: two native files with `g5,g6` against a cowfs file with `g5` printed a `g5` row, `RESULT: PASS`, and no `g6` at all.
`docs/bench-g5-regression.md` line 45 already claimed that unmatched gates "are reported as not run, never as a pass".
That claim was false at the time it was written.
It is true now, and this document is where that behaviour is specified.

## What is printed now

After the per-gate table, before the noise floor:

```
gate coverage  1 of 6 compared (g1)
  g3  not compared: the cowfs arm has no g3 data, native has 2 reps; cowfs1 requested it and recorded 0 reps, native1 requested it and recorded 1 reps, native2 requested it and recorded 1 reps
coverage {"compared": ["g1"], "gates_known": 6, "not_compared": [...]}
```

Three parts, one set of facts each.

- `gate coverage` counts what was compared out of the six gates the harness knows.
- One indented line per gate that some input requested or that has reps in one arm only, naming the arm with no data for it, how many reps the other arm recorded, and every input that asked for the gate together with how many reps that input recorded.
- One `coverage {...}` line, the same facts as JSON, with a `not_compared` entry for every gate that produced no comparison, not only the in-scope ones. Each entry carries `missing_in` (`cowfs`, `native` or `any input`), `reps` per arm, and `requested_by` mapping each input that asked for the gate to the reps it recorded.

The aggregate verdict carries its own scope, so a partial result cannot read as a whole-criterion verdict:

```
RESULT: PASS  scope: compared 1 of 6 (g1), not compared g2 g3 g4 g5 g6
RESULT: FAIL (1)  scope: compared 1 of 6 (g1), not compared g2 g3 g4 g5 g6
RESULT: 1 gate(s) unmeasurable, 0 failed  scope: compared 2 of 6 (g1 g3), not compared g2 g4 g5 g6
```

The `RESULT: PASS` and `RESULT: FAIL (n)` prefixes are unchanged, so anything grepping the verdict still finds it.
The noise-floor table gains one line when the noise-floor file lacks a gate the native arm ran: `noise floor has no g3 data, which the native arm ran`.

## What is not changed

- Matched `g1`-only and `g1,g3`-only comparisons are still valid and still exit 0, with `g5   not run (no input has g5 reps)` unchanged.
- Exit codes are unchanged: `PASS` 0, a genuine `FAIL` 1, `UNMEASURABLE` 2, `INVALID` 3.
- Malformed inputs are still refused with exit 3 and no verdict, and no coverage block is printed on that path.
- No gate present in both arms is still refused with exit 3 rather than printed as a zero-gate pass.
- g5 is still all or nothing across the supplied inputs, refusal included, and a g5-less run still gets no g5 verdict.
- No performance bar moved, and no flag was added.
  The existing output could not express coverage, which is why the reporting is text, but the verdict needs no new input.

## What this does not establish

A scoped `PASS` is a statement about the gates it compared and about nothing else.
`compared 1 of 6 (g1)` means one gate cleared its bar and the other five are unmeasured, not that the filesystem is fast.
No result in this change is a performance measurement, and none of it is evidence about any production gate.
The coverage report reads the harness's own output.
It cannot tell a hand-written but internally consistent file from a real run, exactly as `docs/bench-g5-regression.md` already records for byte accounting.

Known limits, all deliberate:

- A meta record that lists no gate, or a gate name outside the known six, is treated as recording no request.
  The report then says which arm has the data and names nobody as having asked for it.
  No input is refused for a gate list it does not recognise, because refusing a file that was valid before is a bigger change than reporting on it.
- A gate requested by an input that recorded nothing is reported as a gap, not treated as a crash.
  Which of those two it was is a question about the run, not about the result file.
- Coverage is derived from the reps present, so a rep row dropped from a truncated file cannot be recovered from the report.

## Evidence

Real CLI, same artifacts before and after, both in `bench/out/coverage80/` (ignored).
Nine scenarios each: the reported one-sided `g1,g3` case, the same case on real `gates.py` output, one-sided in the other direction, a matched `g1`-only pair, a matched `g1,g3` pair, a genuine over-bar `FAIL`, the g5 one-sided refusal, a meta with no gate list, and a fully matched pair.

| case | before | after |
|---|---|---|
| native `g1,g3` twice, cowfs `g1`, noise floor set | `RESULT: PASS`, `g3` absent | rc 0, `g3` named with the cowfs arm as the one with no data, `compared 1 of 6 (g1)` |
| real `gates.py` native `g5,g6` twice, cowfs `g5` | `RESULT: PASS`, `g6` absent | rc 0, `g6` named with the cowfs arm, `compared 1 of 6 (g5)` |
| native `g1`, cowfs `g1,g3` | `RESULT: PASS`, `g3` absent | rc 0, `g3` named with the native arm |
| matched `g1` only | rc 0 | rc 0, `compared 1 of 6 (g1)`, no gap lines |
| matched `g1,g3` | rc 0 | rc 0, `compared 2 of 6 (g1 g3)`, no gap lines |
| one compared gate over the bar | rc 1 | rc 1, scope appended |
| g5 in one arm only, where every supplied input has g5 | rc 0, the other gate dropped | rc 0, the other gate named |
| g5 in one arm and absent from another supplied input | rc 3 | rc 3, no coverage block |
| meta with no gate list | rc 0 | rc 0, nobody named as having asked |

## Tests

`python3 -m unittest discover -s bench`, the command CI runs.

`bench/test_compare_coverage.py` drives `python3 bench/compare.py` as a subprocess in every case, because the defect was silent output from the real entrypoint and an in-process `main()` call sees neither the exit code nor the stdout and stderr split a caller sees.
It covers one-sided gates in both directions, all three shapes of a two-gate pair, partial verdicts at exit 0, 1 and 2, matched scoped comparisons, a meta with no gate list, gates neither arm requested, the preserved refusals (g5 all or nothing, disjoint gates, malformed and missing files), a noise floor that is missing a gate and one that is not, and one case that builds its arms with the real `gates.py` CLI at `COWFS_BENCH_SCALE=0.001` so the gate list the report reads is the one the harness writes.

Against the baseline comparator, 10 of those 15 fail.
The 5 that pass are the pins on behaviour that must not change: the three refusals, the preserved exit-0 scoped comparison, and the two cases that must print nothing.
Against the changed comparator all 15 pass, and the 36 tests already in `bench/test_gates.py` still pass.
