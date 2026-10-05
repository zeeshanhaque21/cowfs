# g5: what a passing receipt is made of, and what cannot make one

Companion to `docs/verification/ready-g5.md`.
It exists because the review at `b005753` found that a log shaped like a pass was still accepted, and because the fix is worth stating precisely rather than in one sentence.

**g5 is UNMEASURABLE and OPEN, with zero measured assertions.**
Nothing in this document is acceptance evidence.

## The shape that was accepted

A case writing these two lines to its own stdout, and exiting 0:

```
Ran: generic/005
Passed all 1 tests
```

was classified `PASSED`.
The reader took both lines out of the same file the case writes to, so it could not tell the suite's summary from a lookalike.

Three things follow from that, and each has its own control.

## The three lines of defence, and what each one actually stops

### 1. The runner's bytes are pinned

The reviewed pin names `check` by sha256.
A receipt whose executor differs is refused, and the receipt says so in the `check_sha_is_pinned` probe.

This stops runner replacement.
It does not stop a case printing a banner, because the real runner streams the case's output into its own.

### 2. The witness is read by position and by count

The suite prints its testlist once and its summary last.
The gate therefore reads the first `Ran:` line and the last summary line, and it counts how many of each there are:

- more than one `Ran:` line means the stream is not the runner's alone, and the run is refused
- the summary read is the last one in the stream, so a banner a case printed earlier is not the verdict
- the count in the summary must equal the number of ids the runner named, which must equal the number of ids requested, with none missing, extra or duplicated

This is positional, not a signature.
A signature over the log would be a stronger-looking answer to a question that is not the question being asked: the log is not the evidence, the reviewed tree and its pinned runner are.

### 3. Acceptance requires the reviewed suite, not a matching tree

The pin that decides acceptance ships beside the gate and cannot be redirected from the command line.
`--allowlist` chooses which reviewed cases run; it does not choose which suite is the suite.

Pointing the gate at a tree this lane built, with a pin matching that tree, verifies the runner and produces a receipt labelled harness proof, with `acceptance: false`, and the case is not a PASS.
This is the forge the review named, and `test_a_stand_in_tree_is_never_acceptance_under_the_shipped_pin` drives the full command line for it.

## The probes, and the refusal each one produces

Thirteen probes, each measured, each recorded whether it passed or failed, and a refusal lists the ones that failed.

| probe | refuses |
|---|---|
| `check_present` | a missing runner |
| `check_executable` | a runner that is not executable |
| `reviewed_pin_readable` | a gate whose shipped pin cannot be read |
| `tree_sha_is_reviewed` | any tree but the reviewed suite |
| `tree_clean` | a dirty worktree |
| `check_sha_is_pinned` | a replaced runner |
| `group_list_selects_case` | a missing `group.list`, or one that does not select the case |
| `case_named` | a runner that named a different case |
| `no_missing_ids`, `no_extra_ids`, `no_duplicate_ids` | a runner that did not run exactly what was asked |
| `runner_exit_zero` | an exit code that disagrees with the verdict |
| `suite_reported_pass` | no pass summary from the suite |
| `suite_count_matches_request` | a count that disagrees with the ids named |
| `one_testlist_line` | a stream that is not the runner's alone |
| `nothing_not_run`, `nothing_ignored` | the suite's own refusal grammar |

## The negatives, all runnable through the full command line

Thirteen tests in `bench/test_xfstests_gate.py`, each driving `xfstests_gate.py run` against a fixture tree and each ending in a refusal that names the probe that refused:

- a case that forges `Ran:` and `Passed all 1 tests` and `Failed 1 of 1 tests` on its own stdout
- a runner that exits 0 saying nothing
- a runner that says nothing at all
- a runner claiming three tests when it ran one
- a runner naming `generic/999`
- a runner reporting a case as ignored
- a runner reporting a case as not run
- a runner whose bytes were replaced after the pin
- a missing `tests/generic/group.list`
- a `group.list` that does not select the requested case
- a stand-in tree under the shipped pin
- a direct invocation, which has no witness at all
- a hard failure that must stay exit 1 rather than becoming UNMEASURABLE

## What this does not claim

- No signature, no hash of a log, and no per-assertion count: the suite does not emit per-assertion counts, and inventing one would be a fabrication.
- No claim that the real suite's output was parsed, because no real case has run.
- No claim about cowfs conformance, in either direction.