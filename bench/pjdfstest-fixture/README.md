# pjdfstest g3 fixture

A bounded, host-free copy of one receipted run's records, so the checks in `bench/test_pjdfstest.py`
can exercise classification and the analysis writer without a tool cache, a mount, a daemon or the
ignored output tree.

It is a sanitised derivative, not evidence and not acceptance.

## What is here

| path | what it is | from |
| --- | --- | --- |
| `PROVENANCE.json` | source hashes, the field map, every fixture file's hash | written by the transform |
| `run/cases.jsonl` | 10 case records, 2 arms x 5 cases | run `20261005T025742Z`, source `cases.jsonl` `eb2ff1245baa4aa3...` |
| `run/raw/*.tap` | the 10 pjdfstest transcripts, byte for byte | the same run's `raw/` |
| `run/identity.json` | the identity receipt with host facts replaced, and a declared scope | the same run, source `31d92ea0d2bd89d7...` |
| `tool/tests/*/*.t` | 5 upstream case scripts, byte for byte | `pjd/pjdfstest` `85a8aea9e685999ef0540392fd80535f873d7ff7` |
| `tool/COPYING` | the upstream licence notice, byte for byte | the same commit, `COPYING`, `e12b8e42b14e014b...` |

`PROVENANCE.json` is the machine-readable version of this table: the sha256 of each source
file, the sha256 of every file the transform wrote, which is everything here except this
README and `PROVENANCE.json` itself, and the list of identity fields that were rewritten.

## Licence

`pjdfstest` is distributed under the 2-clause BSD licence.
`tool/COPYING` is that project's notice, copied byte for byte from the pinned commit, and it is the
authoritative statement of terms and of the copyright holder:

> Copyright (c) 2006-2012 Pawel Jakub Dawidek <pawel@dawidek.net>

The five `.t` files carry no per-file copyright notice of their own; `unlink/14.t` carries only an
`$FreeBSD$` identifier keyword, which is not a notice.
Each script's sha256 is pinned in `bench/pjdfstest.py`, and the harness verifies a curated closure
against those literals, so this directory cannot be edited into an approval of arbitrary source.
The closure may carry `COPYING` and `README.md` beside its pinned cases and nothing else; any other
file is refused.

## What the transform changed, and why

The captured records named the machine, the account and a private export.
Thirty occurrences of an absolute host path and the maintainer's username were removed from the
committed copy; the transcripts themselves contain none.
Replacement is by rule rather than field by field, so the map is mechanical and checkable:

- the tool checkout prefix becomes `tool`
- the run directory prefix becomes relative to the copy, so `case_dir` reads `native/mkdir_00.t`
- the remaining lease prefix becomes repo-relative, so the daemon argv reads
  `target/release/cowfs-daemon`
- the private export name becomes `<private-export>` and `mounted by` becomes `mounted by <user>`

`raw` was made run-relative, `raw/<file>`, and `script_path` became `tool/tests/<case>`.
Every `raw_sha256` still verifies, because no transcript byte was touched.

`run/identity.json` is therefore a **sanitised reference, not an unaltered live receipt**, and it says
so in itself: `runtime_identity.declared_scope` is `sanitised-reference` and
`runtime_identity.sanitisation` records the rules, the source receipt's sha256, and the fields that
were kept.
A reconciliation of this fixture reports that declared scope as a coverage disclosure and repeats it
in its analysis block, so it cannot be read as an attestation of a live mount.

## What this fixture cannot prove

- **Not new filesystem evidence.** The transcripts were captured once, on 2026-10-05, by run
  `20261005T025742Z` on this machine against the real-Core mount.
- **Not a live identity receipt.** The kept fields, `st_dev`, `fstype`, `mountpoint`, the plan and
  the per-case counts, are the provenance of that capture. They say which filesystems the run saw
  and nothing about any filesystem now. The live run directory holds the only receipt that attests a
  filesystem.
- **Not acceptance.** `g3` stays open: macOS FAIL on that live receipt, Linux UNMEASURABLE.
- **Not synthetic.** No record carries the harness's `synthetic` marker, because these are real
  transcripts. `guard_case` refuses a synthetic record, which is why none can appear here.

## Regenerating it

`sanitise_run()` in `bench/test_pjdfstest.py` is the transform, and it only ever reads its source.
It is exercised by the checks against a synthetic source of the same shape, so the rules stay
tested without depending on a preserved run.