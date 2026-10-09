# PR 106 final delivery review

Reviewed head: `a2c9132721bbb8cec170ccafd3acec0256914836`, against the head this lane reviewed before, `5f4ec7a4896daf50bcd22a45d80eccc450ee9b85`.
PR: #106, `test(g3): matched pjdfstest acceptance harness for the real-Core mount`.
Scope: the test-collection and setup delta plus the two Markdown files, nothing else.
No checkout, reset, stash, source edit, commit, push, merge or lease return was performed.

## Verdict

**PASS.** The delta collects the five verdict properties that discovery had never run, and it does so without touching the production classifier.
Nothing here is a merge blocker.

The one substantive question in this delta, whether a verified source with nothing pairable may read as `PASS 0`, is not settled by this change and is not presented as settled.
It is pre-existing behaviour, reproduced identically on both heads, and the delta documents it as an open question for the gate's owner.
That is the honest handling, and it is the reason this is a PASS rather than a block.

| item | result |
| --- | --- |
| the five verdict properties, now collected | **PASS**, 5 collected, 5 run, 0 failures, 0 skips |
| production classifier unchanged | **PASS**, blob-identical, no non-test non-doc file in the delta |
| fixture, COPYING, PROVENANCE carried | **PASS**, all 20 paths blob-identical, COPYING `e12b8e42…` |
| real fixture verdict preserved | **PASS**, 25 established, 26 unpairable, exit 1 |
| cached missing-tool refusal preserved | **PASS**, absent source still INVALID 3 |
| F3 digest disclosure | **PASS**, stated in both docs, and the absolute paths it names are real |
| module default run | **PASS**, 73 collected, 73 run, exit 0, 0 skipped |
| scoped ruff and py_compile | **PASS** |
| CI at the new head | **one snapshot, `linux-fuse` success, the two matrix checks in progress**, not green yet |
| PR draft, no auto-close | **PASS**, draft, `closingIssuesReferences` 0, no closing phrase in any subject |
| merge into main | **clean**, `merge-tree` wrote `49774579…` with no conflict |

## What the delta actually is

Three files, 112 insertions, 28 deletions:

| file | change |
| --- | --- |
| `bench/test_pjdfstest.py` | +74, the five prefixes, a `pinned_context()` helper, `run_verdict` gains `test`, `context`, `cowfs_plan` |
| `docs/verification/evidence/pjdfstest-g3-repair.md` | +43, the "Five verdict properties that were never collected" section and the digest caveat |
| `docs/verification/ready-g3.md` | +23, counts restated to 73 and the digest disclosure |

Nothing else.
The classifier `bench/pjdfstest.py` is blob `834571a08924ed85877bbce7509ba76c73e7274e` at both heads, and the whole fixture subtree is blob-identical across all twenty paths, `tool/COPYING` included at `e12b8e42b14e014b3e02f19a6b49de44dfb5f16dec55db1ace0f110be2d71330`.
So the licensing, privacy and portability findings from the previous review carry over unchanged and were not re-run, which is the correct call given the bytes did not move.

I proved the delta's inputs before trusting them: the archive extracted from `a2c91327` matched the commit 554 of 554 on path and blob id, and matched again after I removed my own `__pycache__` and `.ruff_cache` from it.

## The five, collected, with what each actually proves

`getTestCaseNames(VerdictStates)` returned 0 usable tests at `5f4ec7a` and returns 5 ids now.
Run explicitly, all five pass with 0 skips:

| test id | what it now proves |
| --- | --- |
| `test_established_regression_is_fail` | one established regression under the verified pinned closure is `FAIL 1`, and the regression count is asserted, not just the state |
| `test_a_pass_with_nothing_pairable_is_disclosed_as_coverage` | a verified source with nothing pairable is `PASS 0` with a disclosure, and the same fixture is `INVALID 3` with the source absent |
| `test_synthetic_records_are_refused_by_the_verdict_itself` | a synthetic record is refused by `verdict()` itself, with its own message |
| `test_malformed_json_is_invalid_input_not_a_pass` | malformed `cases.jsonl` is invalid input, exit 3 |
| `test_mixed_raw_formats_are_refused` | a record set mixing raw and raw-less cases is refused, with its own message |

The two that were stale are now correct rather than merely collected.
`test_established_regression_is_fail` gained a `pinned_context()` that verifies the committed curated closure against its pins inside the check itself, and asserts that verification is clean before using it.
It then uses `open/17.t`, whose pinned script makes three assertions in a fixed order, so the pairs come from script-proven slots rather than from operation text.
I confirmed the closure it depends on verifies with no problems, so the context is genuinely verified and not assumed.

The three that were already right only gained the prefix, and each still asserts its own specific message, so they did not collapse into duplicates of the new prerequisite checks.

## The PASS-0 question, answered as existing behaviour

This is the part worth being precise about, so I tested it on both heads rather than reading the docstring.

The scenario: the committed curated closure verified, a record set where the cowfs arm emitted fewer assertions than the pinned script proves, so nothing pairs.
Result on `5f4ec7a` and on `a2c91327`, identical:

| | state | exit | established | unpairable | reason kinds |
| --- | --- | --- | --- | --- | --- |
| verified source, nothing pairable | PASS | 0 | 0 | 5 | COVERAGE only |
| source absent | INVALID | 3 | | | INTEGRITY |

The two disclosures are real and present: "5 assertion(s) cannot be paired across the arms" and "identity is unrecoverable for 5 assertion(s) with no operation text".
`state_from` is unchanged and does exactly what it always did: integrity outranks everything, then a real divergence, then an absent capability, and coverage moves no exit on its own.

So three things are established rather than assumed.

**It is pre-existing, not introduced.** The classifier blob is the same at both heads, and both produce the same verdict for the same input.
**It is not a policy change.** The delta adds no production code; the only non-test, non-doc file list for the delta is empty.
**It is not an accidental false PASS regression.** The approved contract is that a *missing classification prerequisite* never passes, and that gate still holds: the same fixture with the source absent is `INVALID 3`, asserted inside the very test that asserts the `PASS 0`, so both halves are pinned in one place.

The distinction the review asked to be stated truthfully, and which the delta does state, is that a source available but a wholly unpairable scope is not the same failure as a missing prerequisite.
The first is a coverage limit on a classification that did run; the second means nothing could be classified at all and is an integrity refusal.

What I want to be careful about is the honest reading of the residual risk, which the delta itself names rather than hides.
`PASS 0` on a scope where nothing could be paired is the same shape as the false pass this harness was built to refuse: evidence that could not be read.
The difference is that here a verified source exists and the coverage disclosures are emitted, so the run is not silently claiming conformance on unread evidence; but the exit code alone does not distinguish it from a clean bill of health.
That is a real question about the taxonomy, it belongs to the gate's owner, and this delta does not settle it.
The method is renamed to what it asserts, `test_a_pass_with_nothing_pairable_is_disclosed_as_coverage`, and the docstring says in terms that whether an entirely empty scope should read as a pass is the owner's question, recorded here rather than settled by a test.
That is the correct disposition: it is recorded, not silently ratified.

Nothing in either document claims that an empty scope is filesystem acceptance.
`ready-g3.md` still carries "Not acceptance. g3 stays open: macOS FAIL on that live receipt, Linux UNMEASURABLE", and the taxonomy table keeps COVERAGE as "disclosed, never an exit alone".
I found no sentence anywhere in the two changed documents asserting that a `PASS` means the mount is conformant.

## Test counts, honestly

Module default run at the new head: **73 collected, 73 run, exit 0, 0 skipped**, from both `python3 bench/test_pjdfstest.py` and `python3 -B -m unittest bench.test_pjdfstest`.
The per-class breakdown accounts for all 73, with `VerdictStates` contributing the 5 that were previously absent:

| class | collected |
| --- | --- |
| `ExitTaxonomy` | 3 |
| `FixtureIsPortableAndDeclared` | 12 |
| `Guards` | 7 |
| `MountState` | 4 |
| `PairingInvariant` | 8 |
| `ReconcileNeverOverwrites` | 17 |
| `RuntimeIdentityIsFailClosed` | 11 |
| `ScriptProfile` | 5 |
| `TestList` | 1 |
| `VerdictStates` | 5 |
| total | **73** |

On skipping: the module still contains one `@unittest.skipIf(os.geteuid() == 0, ...)` on `test_an_unwritable_directory_is_a_typed_refusal`.
This host runs as uid 501, so it executed; I confirmed it by name in a verbose run, and the run reports 0 skipped.
Under root it would skip, so "none skipped" is a true statement about this run and not an unconditional property.

I am not restating a total I did not measure.
The 104 figure from `discover -s bench` was measured before this round and is not re-run here, because `bench/test_gates.py` belongs to a lane parked on issue #125 and is outside this PR's scope.
The delta handles this correctly rather than papering over it: `ready-g3.md` now says that count "is the one measured before this round's five were collected, and it is left as measured rather than restated as a fresh total."
I am also making no fresh ordering claim.
The unchanged "29 checks, three seeded shuffles" row cites `--randomize-seed`, which this interpreter does not support; it is rejected as an unrecognized argument, as it was in the previous review.
That row is inherited text and I neither re-ran the matrix nor endorse it.

Scoped static checks on the one file this delta changes: `ruff check bench/test_pjdfstest.py` clean, `py_compile` clean.
The classifier was not re-linted, since it did not change.

## F3, the digest disclosure

The previous review found that the cited payload digest is environment-bound.
Both documents now say so, and both say it accurately.
`ready-g3.md` at line 442: "That digest is a witness for one checkout at one commit, not a content address of the verdict", naming the embedded fields and stating that only the input hashes, the analyser's own sha256 and the transcript hashes should be compared across machines.
The repair document's row now reads "identical within this checkout", with the reason: the digest moves if the run directory or the clone moves.

I verified the disclosure rather than accepting it.
A three-CWD run at the new head gave one distinct digest across all three working directories, so the CWD-independence claim still holds, and the verdict is preserved at 25 established, 26 unpairable, exit 1, with the declared scope still disclosed.
The four fields the disclosure names are all genuinely present and absolute: `analysis.identity_receipt.path`, `analysis.tests_root`, `analysis.ambient_checkout.top_level` and `analysis.ambient_checkout.head`, which in my run carried the ambient checkout's own unrelated branch head.
There is a fifth, `provenance.jsonl`, which the disclosure does not name but which is also absolute; that is an incompleteness in the list, not a false statement in it.
The environment-independent parts the document tells a reader to compare are present and are exactly the input hashes: `cases.jsonl` `edc6ca52…`, `identity.json` `84dc788f…`, ten transcript hashes, and `analyser_sha256` `b4221dec…`.

My digest differs from the documented `ca26de0f649457fe`, as it must, because my run directory and checkout are elsewhere.
That is the disclosure working, not the disclosure failing, and no portable content-address claim is made anywhere.

## Preserved behaviour, spot-checked rather than re-derived

The fixture verdict and the missing-tool refusal are the two things the delta's new test leans on, so I re-measured both instead of inheriting them.
Both hold at the new head: real fixture 25 established / 26 unpairable / exit 1 with the sanitised-reference disclosure present, and absent source `INVALID 3`.
The licensing, privacy, portability and CWD matrices from the previous review were not re-run, because `bench/pjdfstest.py` and all twenty fixture paths are byte-identical; re-running them could only reproduce the earlier result and would have consumed the budget better spent on the collection delta.

## Gate, PR and CI state, read-only

PR #106 at the new head: draft, open, base `main`, head `a2c9132721bbb8cec170ccafd3acec0256914836`, mergeable, `closingIssuesReferences` 0, 25 changed files.
Nine commit subjects, none containing a closing phrase, so nothing auto-closes on merge.
Issues #107, #108, #109 and #110 are all still open.
`git merge-tree --write-tree main a2c91327` wrote tree `49774579c2f661c4e7a02ca1b5bca9d5a053e6d4` with no conflict, and `main` is still `c07aabce311df4202736a50a28bcccd0377ca511`; I make no claim about a combined runtime, since none was measured.

CI, one snapshot, no polling, no dispatch, no rerun, no runner configuration change:

| check | status | conclusion |
| --- | --- | --- |
| `linux-fuse` | completed | success |
| `check (macos-latest)` | in_progress | null |
| `check (ubuntu-latest)` | in_progress | null |

Workflow run 37266747536 on `a2c91327` is `in_progress`.
So CI at this head is **not yet green**, and I am not treating the green run at `5f4ec7a` (37264677577) as evidence for the new head.

## The primary mirrors

Both primary-checkout copies carry the new content exactly.
`git hash-object` on `docs/verification/ready-g3.md` in the primary checkout returns `b1b1f3662244ec68ce137bebe75b864efb5ba4ea`, which is the `a2c91327` blob, and byte comparison against `git show a2c91327:...` is identical.
The repair document likewise hashes to `b03eec4d7674f7fdece1160a235b2eeff4870175`, the `a2c91327` blob, identical by `cmp`.

The two digests named in the brief are file sha256 sums of those untracked primary copies, `4ecd9179…` and `959d7997…`, and they are not git blob ids.
Both files are untracked in the primary checkout, so the mirrors are present and current but not published.
I checked attribution rather than assuming it: the primary `ready-g3.md` contains the new "73 checks in" line and the digest disclosure at line 442, and the primary repair document contains the "within this checkout" wording, so neither is a stale copy of the previous revision mislabelled as current.

## What this review did not do

No live filesystem was measured; no daemon started, no mount made, no store or socket touched, no build run, no 238-case batch attempted.
g3 remains macOS `FAIL` on the live receipt with Linux `UNMEASURABLE`, and #107 to #110 remain open.
No POSIX or performance acceptance work was started.
No production source was modified: the delta contains no production file, and I made none.
No signal was sent, no mount walked, no borrowed state cleaned, no install, no sudo, no sysctl, no reboot, no force or rebase, no workflow change, no commit, no push, no merge, no lease return.
The originals `bb55fe49`, `1d64dabc`, `eb2ff124`, `31d92ea0` and this lane's earlier derived artefacts were not mutated.
Shared state was observed only: the local daemon at pid 15263 and its store, mount, socket and the g4 and g5 pids were left untouched.

The only writes are this report in the primary checkout and this lane's own evidence under `bench/out/pr106-final-delivery-review/**` in the assigned worktree, which `.gitignore` covers at `/bench/out/`.
Two tooling artefacts of mine (`__pycache__`, `.ruff_cache`) landed inside my own archive copy and were removed, after which the archive was re-proved byte-identical to `a2c91327`, 554 of 554.
The assigned worktree still sits on `investigate/integrity-21` at `016769e7f4076a5c0fc712a65932c546048052f7` with zero tracked-file changes.

Other lanes are untouched and their work is not read or attributed here: the 92 final review, 112 repair, 116 meta, 102 fsx, 114 CI, and the parked 125 and 128.

## Recommendation

Land the delta.

It does what it says: five verdict properties that discovery had never run are now collected and passing, the classifier and the fixture are untouched, and the counts in both documents match what I measured.
The previous review's two findings are addressed honestly rather than papered over: F3 is now disclosed as a within-environment witness with the embedded paths named, and the collection hole is fixed and written down instead of quietly renumbered.

One item for the gate's owner, not a blocker and not a new task: decide whether a verified source with a wholly unpairable scope should keep exiting `PASS 0`.
The delta records the question accurately and changes no behaviour to suit an answer, which is the right way to leave it, but the exit code currently does not distinguish that case from a clean bill of health, and that is the gate owner's call to make rather than a test's.