# The false PASS spike

A bounded spike, run before the repair that closed it, because the bug had already survived two
fixes and the loop was the suspect.

## The bug

A reconciliation re-derives a verdict from a run's preserved records.
Pairing is proved from the upstream case scripts.
When those scripts are absent, every assertion falls through to the operation-text route, which
needs operation text the suite does not print on a pass, so almost nothing pairs, nothing can be
called a regression, and the gate reaches PASS.

A PASS there is worse than a failure: it reports a filesystem as tested when no classification ran.

## The fixture

`bench/out/pjdfstest-fresh-clone-spike/run/` is a copy of the receipted run `20261005T025742Z`:
its ten case records, its ten transcripts and its identity receipt.
Only the `raw` field of each record changed, from an absolute path in the original run to the copy's
own file.
The transcript bytes are identical, so every recorded `raw_sha256` still verifies.
The original was never written to: `cases.jsonl` there still hashes `eb2ff124` after every run.

## The two comparisons

Same input, one variable.
Then working side against broken side.
Both are in the spike, and the expected values are literals in it rather than imports from the code
under test, because an oracle that shares the bug is no oracle.

| reading | tests root | profiles | established | unpairable | exit | state |
| --- | --- | --- | --- | --- | --- | --- |
| with the pinned checkout, before the repair | 5 scripts | 5 of 5 | 25 | 26 | 1 | FAIL |
| without it, a checkout with no cache, before the repair | none | 0 of 5 | 0 | 170 | 0 | PASS |
| without it, after the repair | none | 0 of 5 | 0 | 170 | 3 | INVALID |
| with the pinned checkout, after the repair | 5 scripts | 5 of 5 | 25 | 26 | 1 | FAIL |

The boundary was instrumented before any classification: which test directory was read, whether the
source verified, how many profiles existed for how many compared cases, and only then the counts.
What decided the verdict was not the comparison but the boundary in front of it.

Directly calling `verdict()` with `tests_root=None` returned `PASS exit 0` with 170 unpairable
before the repair and `INVALID exit 3` after, with integrity among the kinds present.

## What the spike demanded, and what the repair did

The spike did not propose a fix; it fixed the question.
The answer was that the pinned scripts are a prerequisite rather than an aid, so their absence is an
integrity failure with a real exit status, and the check happens before anything is classified.

That is the whole change.
The comparison, the pairing routes and the taxonomy are untouched, and the case that does have the
scripts still scores 25 established regressions and exits 1.

## Where the recordings live

`bench/out/pjdfstest-fresh-clone-spike/` is ignored, so the artifacts are local:

- `spike.py` is the spike, and takes `SPIKE_WITHOUT_TOOL_EXIT` so the same run can be recorded
  against either behaviour
- `spike-finding-old.json` is the recording against `22340a9`, the false PASS included
- `spike-finding.json` is the recording against this repair, with the old one nested inside it

The numbers quoted in `docs/verification/ready-g3.md` and
`docs/verification/evidence/pjdfstest-g3-repair.md` come from those two files.
