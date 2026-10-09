# pjdfstest g3 fixture: final review of the licensing, privacy and portability delta

Reviewed delta: `5f4ec7a4896daf50bcd22a45d80eccc450ee9b85` against base `9e91e75af7faa9eea992fefa90649741d1bfcd49`.
PR: #106, `test(g3): matched pjdfstest acceptance harness for the real-Core mount`, draft, open, base `main`.
Reviewer scope: the licensing, privacy and portable-fixture delta only.
No checkout, reset, stash, source edit, commit, push, merge or lease return was performed.

## Verdict

Licensing, privacy and portability all pass, each verified against independent evidence rather than against the harness's own declarations.
Two findings are worth carrying forward: one material inherited coverage defect, and one claim-precision problem with a cited payload digest.
Nothing here blocks landing the fixture itself.

| area | result |
| --- | --- |
| licensing and notice distribution | **PASS**, independently confirmed against the real upstream Git objects |
| privacy, host-path removal and provenance | **PASS**, zero host identifiers in the committed fixture |
| portability, path guards and the writer | **PASS**, every guard reproduced from the CLI |
| CWD independence of the payload | **PASS**, three working directories, one payload |
| test counts 68 direct / 104 discover / 0 skip | **counts true**, but see F1: five intended assertions never execute and two of those are stale |
| cited payload digest `ca26de0f` | **not reproducible here**, see F3 |

## What was actually reviewed

The delta is one commit touching nine files.
`bench/pjdfstest.py` +68, `bench/test_pjdfstest.py` +399, `bench/pjdfstest-fixture/` added wholesale, `docs/verification/ready-g3.md` +133, `docs/verification/evidence/pjdfstest-g3-repair.md` +73.
File modes: `bench/pjdfstest.py` is `100755`; all eighteen fixture files are `100644`.
`ruff check` clean, `py_compile` clean.

The assigned idle worktree held unrelated work, so the pinned head and base were extracted read-only with `git archive` into `bench/out/pjdfstest-fixture-final-critic/archives/`.
Both extractions were proved byte-identical to their commits before any conclusion was drawn from them: 554 of 554 tracked paths and blob ids matched for the head, 552 of 552 for the base.
That check was repeated at the end of the review after two of my own tooling artefacts were removed, and it still matched 554 of 554.

The task's pinned head `1065f4ec7a4896daf50bcd22a45d80eccc450ee9b85` is not a valid object.
The real head is `5f4ec7a4896daf50bcd22a45d80eccc450ee9b85`; the supplied string is `106` (the PR number) run into the head's own digest, whose tail matches exactly.
Recording this so the next reviewer does not repeat the search.

## F1, material and inherited: five fail-closed assertions never execute

`VerdictStates` in `bench/test_pjdfstest.py` collects **zero** tests.
Five methods are unmistakably test cases but lack the `test_` prefix, so `unittest` never sees them:

- `established_regression_is_fail`
- `a_pass_needs_a_pairable_scope`
- `synthetic_records_are_refused_by_the_verdict_itself`
- `malformed_json_is_invalid_input_not_a_pass`
- `mixed_raw_formats_are_refused`

Invoked manually, **two of the five fail**:

| assertion | expects | actually gets |
| --- | --- | --- |
| `established_regression_is_fail` | `FAIL` | `INVALID` |
| `a_pass_needs_a_pairable_scope` | `UNMEASURABLE` | `INVALID` |

Root cause, confirmed by printing the real reasons through the class's own `run_verdict` helper:
`run_verdict` calls `verdict(..., tool=None)`, so `classification_prerequisite` fires `INTEGRITY` with the message "the pinned pjdfstest source was not verified, so no assertion can be classified", and the verdict is `INVALID` before the assertion under test is ever reached.

These expectations were invalidated by the delta's own immediate predecessor, commit `9e91e75` ("fix(g3): a gate with nothing to classify is INVALID, never PASS").
The product behaviour is the stricter and correct one; the assertions are stale, and because they were never collected nobody was forced to notice.

**This is inherited, not introduced.**
The class exists at base `9e91e75` at line 292 with the same unprefixed name, and the delta touched none of the five.
The delta added 12 correctly prefixed tests (56 collected at base, 68 at head).

The PR body does not claim this coverage, so this is not a misstatement by this PR.
It is a live hole in the suite the PR sits on, and the assertions it hides are exactly the fail-closed ones: a pass needs a pairable scope, and an established regression is a failure.
Recommendation: prefix the five so they run, then reconcile the two stale expectations with the `INVALID` behaviour `9e91e75` introduced. That is a separate change from this delta and should not be smuggled into it.

## F2, disclosure is correct but rests on a single in-band field

This is the question the author raised, so it was tested rather than argued.
Taking an own copy of the fixture and deleting both `runtime_identity.declared_scope` and the whole `runtime_identity.sanitisation` block, then reconciling it:

| | as committed | declaration removed |
| --- | --- | --- |
| state | FAIL | FAIL |
| exit | 1 | 1 |
| established regressions | 25 | 25 |
| unpairable | 26 | 26 |
| COVERAGE reasons | 3 | 2 |
| any PASS | no | no |

So the answer is empirical and unambiguous: **removing the declaration cannot label the sanitised fixture as genuine acceptance, and does not trigger a refusal either.**
`COVERAGE` never moves the exit, which the code states and the measurement confirms.
What is lost is one disclosure line: "the identity receipt declares scope 'sanitised-reference', so this is a reading of the records it ships with and attests nothing about a live mount".

Two further facts bound the severity:

1. `PROVENANCE.json`, which carries the far stronger statements `what_this_is` = "a host-free derivative of one receipted run" and `not_a_live_receipt`, is **provably never read** by the reconcile path. Deleting the file leaves the payload byte-identical. It sits at `fixture/PROVENANCE.json` while `run_dir` is `fixture/run`, and `analysis_provenance` hashes only `cases.jsonl`, `identity.json`, `summary.json`, `daemon.json` and `raw/`.
2. `analysis.note` is emitted regardless, and says the file "says nothing about the runtime that captured them, and it does not replace any receipt in the run directory".

With the declaration gone the payload still carries live-receipt-shaped fields with no in-band marker: `st_dev` 436209661, `mountpoint` `mnt`, `path` `mnt/pjd`, `source` `localhost:/<private-export>`, and the read receipt carries `daemon.pid` 8004 and the `mount_table_line`.
A reader could mistake those for a live mount.

Judgement: this is a genuine single-point labelling dependency, and it is a **labelling-granularity** observation rather than a disclosure failure in the sense of a false acceptance.
The exit is `FAIL` either way, `analysis.note` independently disclaims the runtime, and a human opening the fixture directory sees three in-band markers (README, PROVENANCE, identity).
The README already discloses all of this accurately, including that a reconciliation repeats the declared scope in its analysis block.
No anti-forgery framework is warranted and none is proposed.
If the author wants the payload to carry the stronger statement, the cheap change is for `cmd_reconcile` to read the sibling `PROVENANCE.json` when present and copy `declared_scope` and `not_a_live_receipt` into `analysis.identity_receipt`.

## F3, the cited payload digest `ca26de0f` is environment-bound

The three-CWD byte-identity claim is **confirmed**: run from the fixture directory, its parent, and an unrelated directory, one process each, cache-free, all three payloads are identical.

The specific digest is not portable.
My fixture-directory run hashed `aaa07858ac25d291b56bcce46dc1b3606010d37286ce07233054933de522d79f`; the same fixture read at its archived path hashed `ee4d2574be6e587f...`.
The reason is that the payload embeds four absolute paths: `analysis.ambient_checkout.top_level`, `analysis.tests_root`, `provenance.jsonl` and `provenance.identity_receipt`.
A digest over a payload that contains the reader's own filesystem layout cannot be a content address.
Worth stating plainly so `ca26de0f` is not later cited as a portable invariant; it is a within-environment witness, and it is one more thing that changes if the run directory moves.

## Licensing: PASS

Verified against the real upstream Git objects, not against the harness's own literals.
`git ls-remote https://github.com/pjd/pjdfstest.git HEAD` returns `85a8aea9e685999ef0540392fd80535f873d7ff7`, so the pinned commit is independently confirmed to be upstream HEAD, and it was fetched depth-1 over HTTPS into the owned path for comparison.

`tool/COPYING` is **byte-identical** to `COPYING` in commit `85a8aea9`:

| | sha256 | bytes |
| --- | --- | --- |
| upstream `COPYING` at the pin | `e12b8e42b14e014b3e02f19a6b49de44dfb5f16dec55db1ace0f110be2d71330` | 1374 |
| vendored `tool/COPYING` | `e12b8e42b14e014b3e02f19a6b49de44dfb5f16dec55db1ace0f110be2d71330` | 1374 |

The notice itself, checked for each element rather than accepted on the README's word: `Copyright (c) 2006-2012 Pawel Jakub Dawidek` present, the full address `pawel@dawidek.net` present and not elided, both numbered redistribution conditions present, and the `AS IS` / liability disclaimer present. That is the 2-clause BSD the README names, and clause 1 is satisfied by shipping the notice.

All five case scripts are byte-identical to the same pinned commit:

| script | sha256 | bytes |
| --- | --- | --- |
| `tests/mkdir/00.t` | `bd017018a17cbaed6d0197ec9ee072a5a23f20edc0938f37d410b233e912ba35` | 2128 |
| `tests/mkfifo/00.t` | `f631099ba6afbf0f23ee03759278166b332127d649ed99aaffed2c9cf7a6f866` | 2132 |
| `tests/open/17.t` | `b2aa69d1662b85b4bb473c0831097a6d83a4b2087eff3d4a6ada2ace1639bab8` | 349 |
| `tests/rmdir/12.t` | `0078ce2fb06a08d51895a15d126da79319194e8447cb9610466d27cd6a235e03` | 664 |
| `tests/unlink/14.t` | `ce168a45c3fa61352f9f26f81dcc2fe17f77a460328c59fc9822a34bc39ac007` | 695 |

None of the five carries its own copyright notice, so the notice has to travel with them, which is why redistribution requires `COPYING`.
`unlink/14.t` carries only `# $FreeBSD$`, an id keyword rather than a notice, exactly as the README says.
The closure is a curation of **5 of the 238** upstream cases, and the README says so; claiming the whole suite would have been the overreach.

The guard against a caller manufacturing an approved closure was exercised, not assumed.
`COPYING` is pinned by hash in `CURATED_METADATA` exactly like a case, `README.md` is permitted by name only, and every other file is refused.
From the CLI: a closure carrying an extra script is refused with exit 3; a closure carrying an extra non-metadata file is refused with exit 3; an edited `COPYING` is refused with exit 3; a byte-perturbed case is refused with exit 3; the widest permitted closure (the five cases plus `COPYING` plus `README.md`) is accepted and scores the normal `FAIL` 1.
`pinned_blob_sha` derives the expected hash from the pinned commit rather than from anything the caller supplies, and `verify_curated_closure` honestly reports `source_sha256: None` because a curated closure cannot prove the suite's source.

## Privacy: PASS

The committed fixture contains **zero** host identifiers across all twenty files, README and PROVENANCE included: no `/Users/`, no `cowfs-<32 hex>` export name, no real username, no literal `mounted by <user>`.
The ten transcripts total 7282 bytes and contain no host path, no export pattern and no credential-shaped word.

All ten `raw_sha256` values verify and all ten `raw_bytes` counts match, which is the proof that no transcript byte was touched.
All ten raw paths are run-relative, and every `script_path` resolves under `tool/`.

`PROVENANCE.json`'s field map is an exact partition of the identity receipt.
Walking every leaf of the committed `identity.json` gives 50 leaves, of which 17 were added by the transform (`declared_scope` plus the `sanitisation` block), leaving 33 base fields.
`identity_fields_kept` is 22 and `identity_fields_rewritten` is 11; their union is exactly the 33 base fields, their intersection is empty, and neither names a transform-added path.
All eleven rewritten fields hold sanitised values: `target/release/cowfs-daemon`, `store`, `mnt`, `rt/c.sock`, `rt/c.sock`, `store`, the `mount_table_line` with both tokens, `mnt`, `mnt/pjd`, `localhost:/<private-export>`, `native`.

`PROVENANCE.fixture` holds 18 entries and every one verifies.
The two files on disk outside the map are exactly `README.md` and `PROVENANCE.json`, which is what the README promises.

`sanitise_run` was reproduced on a synthetic own source, never on the real receipts.
The source was byte-identical before and after, so it is genuinely read-only.
The lease prefix went from 9 occurrences to 0, the export name from 2 to 0, `mounted by <user>` appeared, `declared_scope` was written, and the recorded `source_identity_sha256` matches the source receipt.
The declared field map reproduced independently: 11 rewritten, and 23 kept against the committed 22, the one extra leaf being the `daemon.note` field my synthetic added, so the arithmetic is consistent rather than coincidental.
No lease prefix, export name, real username or `mounted by` literal appeared in any of the eleven files the transform wrote.

Transform scope, probed rather than assumed.
I injected a field named like a credential and it **survived** into the output.
So the transform is host-path, username and export-name scoped, and it is **not** secret scoped.
The declared `kind` says precisely that ("host paths, the capture host's user and the private export name replaced by tokens"), the declared `meaning` says "provenance of the capture, not an attestation of the filesystem it ran on", and neither output contains a claim of secret, password, credential or token removal.
Scope claimed equals scope implemented, and no stronger claim is made anywhere.

## Portability and the writer: PASS

Every guard was reproduced by driving the published CLI as a child process on copies, before any unit batch, as required.

| case | exit | first reason |
| --- | --- | --- |
| valid fixture, pinned closure | 1 FAIL | declared scope COVERAGE, 25 established |
| all raw streams missing | 3 INVALID | `raw stream is missing` |
| one record redirected to `../outside` | 3 INVALID | resolves outside the run directory |
| raw path through a symlink leaving the run | 3 INVALID | resolves outside the run directory |
| raw hash moved | 3 INVALID | `raw stream hash moved` |
| record naming a case wider than the closure | 3 INVALID | 1 compared case has no script profile |
| same records with the wider pair removed | 1 FAIL | proves the refusal above is on width, not damage |
| output path inside the run it reads | 3 | refused, no file created |
| output destination already exists | 3 | bytes unchanged, still the original content |
| absolute historical raw path used as written | 1 FAIL | taken as written, by documented design |

`stream_path` anchors relative paths on the run directory rather than the working directory, which is what makes the payload CWD-independent, and it resolves symlinks before the containment test, which is why the symlink case is caught rather than followed.
An absolute path is deliberately taken as written, documented in the docstring as support for historical read-only record sets; I note it plainly as a scope choice, not a defect, because the record set is the evidence being judged and a hash must still match.

The writer creates by staging in the target directory and then `os.link`-ing into place, which is the atomic exclusive creation, so there is no existence-check window and no overwrite.
A staged file that cannot be linked is kept as evidence rather than deleted, and only a staging name carrying this process's own pid is ever removed.
The existing-destination probe confirms the refusal is real: the pre-existing file's bytes were unchanged afterwards.

## Test counts, determinism and static checks

`python3 -m unittest bench.test_pjdfstest` runs **68** tests, OK.
Isolated discovery over `bench/` runs **104**, OK, being 68 here plus the 36 in `bench/test_gates.py`, each module also run alone in its own process.
No skip executed in any run.

Two qualifications, stated rather than smoothed over.

First, one skip exists and is conditional: `@unittest.skipIf(os.geteuid() == 0, "root ignores directory write permissions")` on `test_an_unwritable_directory_is_a_typed_refusal`.
This machine runs as uid 501, so it executed, and the verbose run reported zero skips.
Under root it would be skipped, so "0 skip" is true of this run and not unconditional.

Second, the ordering flags named in the brief do not exist in this interpreter.
Python 3.12.2's unittest here accepts only `-h -v -q --locals --durations -f -c -b -k`; `--randomize-seed` and a reverse flag are rejected as unrecognized arguments.
Order independence was therefore established by building suites explicitly: alphabetical, reverse, reverse-alphabetical, odd-indexed classes (30 tests) and even-indexed classes (38 tests) all green, plus `-f` and `-b` green, plus three identical repeat runs.
If the branch claims seed 1, 7 and 42 evidence, that specific claim is unverified in this environment and should not be relied on.

The class that guards this delta is `FixtureIsPortableAndDeclared`, 12 tests, all correctly prefixed and all executed.
Its names correspond one to one with the guards reproduced above by hand, which is why the hand reproduction agrees: no host identifiers and declared hashes, a closure with a non-metadata file refused, an extra script refused, an altered upstream notice refused, a record set with no readable raw stream invalid rather than pass, a relative raw path that cannot climb out, one that climbs out through a symlink, a run wider than the closure refused, an absolute raw path taken as written, the same records scoring identically from three directories, the transform replacing host facts by rule, and the verdict repeating the receipt's declared scope.

## PR and gate state, read-only

PR #106 is a draft, open, base `main`, head `verify/pjdfstest-g3` at `5f4ec7a4896daf50bcd22a45d80eccc450ee9b85`, mergeable clean, 25 changed files.
`closingIssuesReferences` is empty, and none of the seven commit subjects contains a closing phrase, so nothing closes on merge.
Issues #107, #108, #109 and #110 are all still open.
The fixture's own README states g3 is not accepted and stays open, with macOS `FAIL` on the live receipt and Linux `UNMEASURABLE`.

CI on the head, read once and not polled: three check runs, `linux-fuse`, `check (ubuntu-latest)` and `check (macos-latest)`, all `completed`/`success`, and workflow run 37264677577 `completed`/`success` on `5f4ec7a4`.
The combined legacy status endpoint reports `pending` only because it holds zero legacy contexts, which is not a failure.
No dispatch, rerun or runner configuration change was made.

Main carries no stale g3 runtime claim, so there is nothing on main to correct.
There is no `docs/verification/*g3*` file in `c07aabce`, and the single `pjdfstest` mention in main's documentation is `docs/spikes/2-nfs-loopback.md:208`, which reads "`pjdfstest`, `fsx`, `xfstests`: not run."
That is accurate, not stale.
`951045f` is the merge of PR #96, an NFS namespace-durability change, unrelated to g3.
PRs #96, #100, #111 and #119 are all merged and none asserts a g3 runtime result.

The prior reviews are present and the lineage is continuous.
`pjdfstest-g3-identity-final.md`, `-reconcile-final.md` and `-spike-final.md` exist; the spike review's sha256 begins `8f583ea5`, which is the last-sha reference in the brief.
Between them they cite `d004caac`, `576dfa0`, `22340a9`, `025bde2` and `9e91e75`, all ancestors of the reviewed head.

## What this review did not do

No live filesystem was measured.
No daemon was started, no mount made, no store or socket touched, no build run, and no 238-case batch attempted.
The `FAIL` reported throughout is a reading of preserved, sanitised records, which is exactly what the fixture says it is.
Linux therefore remains `UNMEASURABLE`, and there is **no measured live-filesystem gate in this review**; any runtime attestation gate must separately validate real pids and mounts before its cases run.

Shared infrastructure was left alone: the pre-existing daemon at pid 15263 and the two pre-existing nfs mounts were observed read-only and untouched.
No signal was sent, no borrowed tree walked, nothing cleaned up.
The original records `bb55fe49`, `1d64dabc`, `eb2ff124` and `31d92ea0` were not mutated, and no derived file was regenerated by hand.

Issue #125, the in-process gates/compare integration test, is open and owned by another worker writing to `bench/out/test-gates125-test-repair/**`.
I did not resume, delegate to or interrupt that work, and did not touch `test_gates`, `gates` or `compare`; the 36 `test_gates` tests pass as they stand.
Its fix is not attributed to this PR.

No installs, no sudo, no sysctl, no reboot, no workflow dispatch, no rerun, no polling, no commit, no push, no merge, no lease return.
Two of my own tooling artefacts (`__pycache__`, `.ruff_cache`) landed inside my archive copy and were removed; the archive was re-proved byte-identical to `5f4ec7a`, 554 of 554, afterwards.
The only writes anywhere are under `bench/out/pjdfstest-fixture-final-critic/**` in the assigned worktree, which `.gitignore` covers at `/bench/out/`.
Primary tracked-file changes remain the four that were already there before this review, and the assigned worktree has zero tracked-file changes.

## Recommendation

The fixture as committed is sound on the three axes it was submitted for.
Licensing is satisfied against the real upstream objects and the notice travels with the scripts, which is what clause 1 requires.
Privacy is complete, with the field map an exact partition and no secret-removal claim to be misled by.
Portability holds, with every guard reproduced from the CLI and the writer genuinely exclusive.

Two things should happen, neither of which blocks the fixture.
Fix the five missing `test_` prefixes and reconcile the two stale expectations, as a separate change, because the suite currently reports green while the assertions behind "a pass needs a pairable scope" and "an established regression is a failure" never run.
And stop citing `ca26de0f` as a portable digest, since the payload embeds the reader's own absolute paths.