# pjdfstest g3 fixture

A bounded copy of one receipted run's records, so the checks in `bench/test_pjdfstest.py` can
exercise classification and the analysis writer without a tool cache, a mount, a daemon or the
ignored output tree.

It is a record set, not new evidence.

## What is here

| path | what it is | from |
| --- | --- | --- |
| `run/cases.jsonl` | 10 case records, 2 arms x 5 cases | run `20261005T025742Z`, `cases.jsonl` `eb2ff1245baa4aa3...` |
| `run/raw/*.tap` | the 10 pjdfstest transcripts, byte for byte | the same run's `raw/` |
| `run/identity.json` | the runtime identity receipt, byte for byte | the same run, `31d92ea0d2bd89d7...` |
| `tool/tests/*/*.t` | 5 upstream case scripts, byte for byte | `pjd/pjdfstest` `85a8aea9e685999ef0540392fd80535f873d7ff7` |

The only edit to the record set is the `raw` field of each record: absolute paths naming the run
directory become `raw/<file>`, so the fixture is portable. Each record's `raw_sha256` is unchanged
and still verifies, because the transcript bytes are unchanged.

The five scripts are pinned in `bench/pjdfstest.py` by their sha256 in the pinned commit, and the
harness verifies a curated closure against those literals. A closure whose bytes differ is refused,
so this directory cannot be edited into an approval of arbitrary source.

## Licence

`pjdfstest` is distributed under the 2-clause BSD licence, Copyright (c) 2004 Ian Lance Taylor and
others. The five `.t` files here are verbatim excerpts of that project, redistributed under the same
terms. `tests/open/17.t` and `tests/unlink/14.t` carry their own copyright lines.

## What this fixture is not

- Not new filesystem evidence. The transcripts were captured once, on 2026-10-05, by run
  `20261005T025742Z` on this machine against the real-Core mount.
- Not acceptance. `g3` stays open: macOS FAIL on that live receipt, Linux UNMEASURABLE.
- Not a substitute for the receipt. The live run directory and its `identity.json` remain the
  source of truth; this copy exists so the classifier has a fixed, self-contained input.
- Not synthetic. No record carries the harness's `synthetic` marker, because these are real
  transcripts. A synthetic record is refused by `guard_case`, which is why none can appear here.