# pjdfstest g3 classification repair: independent review

Reviewer lane: read-only review of PR #106 at head `9e91e75af7faa9eea992fefa90649741d1bfcd49`, against the head I reviewed previously, `22340a9db83c87bfbd248bf4fd310543ab9d5da9`.
Lease `a53321161c0b6660c9124671c6c6654c`, branch `review/pjdfstest-g3`, fetched over HTTPS by exact SHA.
No production source edited, no checkout, no commit, no push, no merge, no lease return, no reset, no stash, no workflow dispatch, no rerun, no poll.

| lane | verdict |
| --- | --- |
| the four blockers I raised at `22340a9d` | **all four fixed in code and demonstrated** |
| pairing and identity carried forward | **PASS**, 27 of 27 byte-identical |
| curated fixture provenance against upstream | **PASS**, independently verified |
| curated closure scope | **PASS**, five cases only, extras refused |
| ordering isolation of the 17 safety cases | **PASS**, six permutations plus solo plus repeat |
| default test execution in a clean archive | **56 direct, 0 skips, exit 0**; `discover` 92 with an observed flake, see below |
| **licence attribution in the committed fixture** | **BLOCK**, wrong copyright holder |
| **absolute host path and username published in the fixture** | **BLOCK**, 30 occurrences |
| discover-suite flake | **not attributed**, see below |
| macOS NFS against private real-Core mount | **FAIL**, unchanged |
| Linux FUSE arm | **UNMEASURABLE**, not attempted |

## The four previous blockers, each closed and shown closed

I ran the cache-free before/after on my own copies, never on another lane's inputs.
I extracted the committed fixture with `git archive` into my own archive, confirmed all ten `raw` paths are relative and resolve inside my copy with no `..` escape, and confirmed every `raw_sha256` still verifies.

| | old `22340a9d` | new `9e91e75a` |
| --- | --- | --- |
| `--tool` pinned scripts available | **FAIL, exit 1**, 25 established, 26 unpairable, 24 textless | **FAIL, exit 1**, identical |
| no `--tool` | **PASS, exit 0**, 0 established, 170 unpairable, kinds `{COVERAGE: 2}` | **INVALID, exit 3** |

That is exactly the discrimination the round called for, and the reason kinds change from `COVERAGE` alone to an `INTEGRITY` refusal, which is the correct classification: a run with no way to classify its assertions has not passed, it has not been measured.

1. **Silent `PASS 0` is gone.** `classification_prerequisite` refuses when the pinned source was not verified, when verification reported problems, when no test directory was read, or when **any** compared case lacks a profile. It runs before anything is classified rather than after the count, and it documents the exact failure mode in its docstring.
2. **The staging write no longer leaks.** `write_exclusive` wraps the staged write, so an unwritable or absent parent is a typed refusal. I measured `/System/nope.json` returning exit 3 with the message "could not be staged: [Errno 1] Operation not permitted", where the previous head produced a traceback and exit 1.
3. **Symlink redirection is closed.** `resolve_output` resolves the parent and never the final component. A live symlink named as the output is refused with exit 3 and its target's bytes are unchanged; a dangling symlink is refused the same way.
4. **False revision attribution is gone.** `source_head` no longer exists. `script_revision` records `analyser_revision` **only** when `git rev-parse HEAD:<rel>` equals `git hash-object <script on disk>`, and otherwise records `UNKNOWN` with evidence. The enclosing checkout is kept but explicitly labelled "the checkout this ran in, not the origin of this analysis".

My own staging is the adversarial case for that last one and it behaves correctly: my script sits at a path the enclosing repository does not have at HEAD, so the receipt reads `analyser_revision: UNKNOWN` with the evidence "the script on disk is not the blob HEAD records for ...". An unrelated Git parent cannot manufacture a verified revision, which was the whole point.

## Carry-forward, verified rather than assumed

I hashed the AST source segment of 27 pairing, identity and tool functions at both heads.
All 27 are byte-identical, including all ten identity functions, `pair_case`, `compare`, `script_profile`, `guard_case`, `verdict`, `state_from`, `validate_runtime_identity` and `verify_tool_source`.
My manual 36-script closure therefore carries over unchanged, and I am relying on explicit byte identity rather than on the absence of any heuristic signal.

What changed is confined to the guard and writer layer: new `classification_prerequisite`, `verify_curated_closure`, `verify_tool`, `script_revision`, `git_optional`, `resolve_output` and `_discard_staged`, with `verdict`, `cmd_reconcile`, `write_exclusive` and `analysis_provenance` extended.
Modes are `100755` for the harness with its shebang and `100644` for the test with none, `py_compile` passes, and `ruff check` reports **All checks passed** at those modes.

## The fixture is authentic, and I proved origin independently

Eighteen files: the README, `cases.jsonl`, `identity.json`, ten transcripts and five scripts.

The strongest check available is comparing the vendored scripts against the upstream git object store, not against any hash the harness declares.
I read `tests/mkdir/00.t`, `mkfifo/00.t`, `open/17.t`, `rmdir/12.t` and `unlink/14.t` out of the pinned checkout with `git show 85a8aea9:tests/...` and compared bytes.
All five are **identical** to the committed fixture: `bd017018`, `f631099b`, `b2aa69d1`, `0078ce2f`, `ce168a45`.
That is origin proof from the upstream repository, independent of the harness's own claim.

All ten transcripts and `identity.json` are byte-identical to the original run, `identity.json` hashing `31d92ea0` as documented.
The only edit to the record set is the `raw` field, rewritten to a relative path, and every `raw_sha256` still verifies against the committed transcript, so the rewrite did not weaken the evidence.
The README is also careful about what the fixture is not: not new evidence, not acceptance, not a substitute for the live receipt, and not synthetic, with the note that no record carries the `synthetic` marker because a synthetic record is refused by `guard_case`.

`verify_curated_closure` pins those five paths by sha256 in literals inside the harness and refuses any extra script, so the closure cannot be edited into approving arbitrary source, and it records `source_sha256: None` because in an archive it genuinely cannot prove `pjdfstest.c` provenance.
Its docstring states the scope limit plainly: it can only prove the cases it pins, and a run reaching beyond them is refused by the classification prerequisite.
I confirmed the positive path: `--tool` pointing at my own copy of the curated closure, which is not a git checkout, still yields FAIL exit 1 with 25 established.
So a wider 238-case run cannot borrow a five-case closure, and the five-case scope is enforced rather than documented.

## Blockers found in this round

### 1. The fixture credits the wrong copyright holder

The README states:

> `pjdfstest` is distributed under the 2-clause BSD licence, Copyright (c) 2004 Ian Lance Taylor and others.

The upstream `COPYING` at the pinned commit says:

> Copyright (c) 2006-2012 Pawel Jakub Dawidek <pawel@dawidek.net>

The clause count in the README is right; the attribution is not.
I also checked the five vendored files for per-file copyright: `open/17.t`, `mkdir/00.t`, `mkfifo/00.t` and `rmdir/12.t` carry none, and `unlink/14.t` carries only an `$FreeBSD$` identifier keyword, not a notice.
So `COPYING` is the authoritative notice, and the fixture names a person who is not in it.

BSD-2-Clause clause 1 requires that redistributions of source retain the above copyright notice, the conditions and the disclaimer.
This repository is public, the five files are verbatim excerpts, and the `COPYING` file itself is not vendored.
Naming the wrong holder in a public redistribution is a licensing defect, not a documentation nit, and it is the one finding here I would not merge past.
The fix is to vendor `COPYING` and attribute Pawel Jakub Dawidek.
Per instruction I diagnosed only and changed nothing.

### 2. The committed fixture publishes an absolute host path and the maintainer's username

The README says the only edit is the `raw` field, and that is accurate about `raw`.
It does not disclose what remains.
I scanned every committed fixture file:

| file | occurrences of `/Users/` | where |
| --- | --- | --- |
| `run/cases.jsonl` | 20 | `case_dir` in all 10 records, `script_path` in all 10 |
| `run/identity.json` | 10 | `path`, `mountpoint`, and the daemon's `store`, `socket`, `argv` entries |

Thirty confirmed occurrences of `/Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/...` in tracked files destined for a public repository.
These are facts, not a lexical guess: I parsed the records and counted the fields.

I also scanned for credentials and found none: no AWS keys, no GitHub tokens, no private key blocks, no bearer strings, no password or api-key assignments.
The generated `pjdfstest_<hex>` names in the transcripts are test fixtures, not host data.
So this is host and username disclosure, not a secret leak, and I am labelling it as exactly that.
Rewriting `case_dir` and `script_path` to run-relative paths, as `raw` already was, would close it.

### 3. The fixture's relative `raw` paths resolve against the process working directory

`verdict()` resolves `Path(record["raw"])` without anchoring it to the run directory, so the fixture only works when the current directory happens to be the fixture's parent.
Run from anywhere else, all ten records fail with "raw stream is missing" and the verdict becomes INVALID 3 for the wrong reason.
I saw both behaviours on the same bytes: exit 0 from the fixture directory, exit 3 with ten raw-stream complaints from one level up.
The README calls the rewrite what "makes the fixture portable", which is only half true.
Anchoring on `run_dir / record["raw"]` would make it genuinely portable and is a one-line change.

### 4. A discover-suite flake I can measure but not attribute

In a clean tracked archive with no `.git`, no `bench/out` and no tool cache, `python3 bench/test_pjdfstest.py` runs **56 tests, all pass, zero skips, exit 0**, which matches the round's expectation exactly.

`python3 -m unittest discover -s bench`, the command CI runs, collected **92** and failed once with:

> `FAIL: test_gates_writes_scale_into_meta_that_compare_accepts (test_gates.CompareRefuses...)`
> `AssertionError: 2 != 0`

That test is in `bench/test_gates.py`, a file PR #106 does not touch, and it shells out to `bench/compare.py`.
Re-running gave exit 0, and every ordering of it passed in isolation, so it is a flake.
Measuring it properly: **1 failure in 8 runs at this head, 0 in 8 at `22340a9d`, 0 in 8 on `main` `951045f`.**

I will not claim this PR caused it.
Eight runs cannot distinguish 1-in-8 from 0-in-8, the failing file is outside the diff, the assertion is on an exit code from a subprocess that can return 2 for unmeasurable, and the machine was heavily loaded throughout, which is the most likely mechanism.
The honest statement is that a run of the CI command is **not reliably green at this head**, that the cause is not established, and that per the repository rule about flaky tests it should be found and fixed regardless of which change exposed it.
CI currently reports 3 passed, 0 failed at `9e91e75af7faa9eea992fefa90649741d1bfcd49`, confirmed by GraphQL `headRefOid`, so this is not a stale snapshot.

### 5. Ordering and isolation, where the previous round's defect was fixed

`ReconcileNeverOverwrites` now has **17** cases, up from 7.
I checked isolation properly, since my first attempt used flags that do not exist in `unittest` and returned argparse exit 2 rather than a result.
Building suites by hand instead: all 17 pass in alphabetical order, reversed, odds-then-evens, and shuffled with seeds 1, 7 and 42; all 17 pass solo in their own process; and three consecutive runs in the same tree all pass with no staged residue.
The shared-fixture order dependence I found last round is genuinely fixed, not merely hidden by alphabetical luck.

One count caveat, reported as measured: I did not observe any skip in the clean archive, so the author's "zero skips" holds in this environment.
A permission-denied staging case would be root-only somewhere and could legitimately skip elsewhere; the counts here are 56 direct and 92 discovered, both fully executed.

## Writer negatives, all typed, nothing clobbered

Every one of these returned exit 3 with an `INTEGRITY` reason and no traceback: existing file, hard-linked name, live symlink, dangling symlink, missing parent directory, parent that is a regular file, unwritable parent, and `--output` aimed at the input `cases.jsonl` and at an input raw transcript.
Pre-seeded sentinel bytes were byte-identical afterwards in every case, including the live-symlink target, which confirms `NOFOLLOW` rather than merely asserting it.
The staging refusal message names the real errno, no staged residue was left anywhere, and my input's ten `raw_sha256` values still verify after being used as refusal targets.
Atomicity is still `os.link` after an fsynced exclusive staged write, so there is no precheck-then-write window.

## Preservation

I used only my own copies, in my own lease, under `bench/out/pjdfstest-spike-final-critic/**`, with this document as the single canonical exception in the primary checkout.
After every invocation in this review the other lanes' evidence is unchanged: `bb55fe49`, `1d64dabc`, `eb2ff124` and `31d92ea0` all verify, and the ten raw transcripts of the receipted run are intact.
The builder's lease is git-clean.
No daemon, no mount, no build and no full suite was started, which is appropriate for a writer and classifier delta; my earlier own receipted run and the author's receipted run both stand as the production evidence.

Protected state is untouched: pid 15263 alive, `~/.cowfs/mnt` still mounted, all leases held, the g4, g5, 987929D and 899604 mounts as I found them.
No signal, no unmount, no mount walk, no cleanup outside my own directory, no install, no sudo, no sysctl, no runner change.

## State of the repository and the gate

`main` is `951045fca4823611e196eda75db0c977a46d2c77`, "Merge pull request #96 from `fix/nfs-namespace-durability-90`", together with the earlier merge of #111.
Those are unrelated namespaces and translate work; this PR touches no `crates/` file and I make no new runtime acceptance claim for them.
PR #106 is open and draft, `closingIssuesReferences` is 0, the single added commit `fix(g3): a gate with nothing to classify is INVALID, never PASS` carries no closing keyword and no reference to #107 through #110, and all four issues are confirmed open by direct API read.
I did not change the draft state.

The three canonical documents match the committed blobs exactly: `docs/verification/ready-g3.md` `426ab798`, `docs/verification/evidence/pjdfstest-g3-repair.md` `cf2fda1e`, and the new `docs/verification/evidence/pjdfstest-g3-spike.md` `6a35da49`.
I verified that structurally, by comparing content hashes of the committed blobs, and I make no claim about how any of them renders.

**Acceptance is unchanged and open.**
macOS NFS against a private real-Core mount: **FAIL**, and this head preserves it exactly, 25 established regressions, 26 unpairable of which 24 textless, with `DIVERGENCE` outranking `COVERAGE`.
Linux FUSE arm: **UNMEASURABLE**, not attempted; the host is reachable with `/dev/fuse` and `fusermount3`, and the gap is a cross-build plus the shared Linux lock.
The historical 77, 71, 10050 and 5563 remain **diagnostics only**, and both superseded record sets remain INVALID because neither carries an identity receipt.
`rmdir/12.t` #4 and `unlink/14.t` #4 remain manual confirmations rather than algorithm-established pairs, correctly disclosed.
Still missing coverage: 80 suite-declined cases per arm, 1968 native and 1981 cowfs privilege-gated assertions, 11 absent host capability macros, and a native oracle failing 43 percent of all assertions with 3733 outside the verdict.

g3 does not close on this evidence and no merge should follow.

## Required before merge

1. Vendor the upstream `COPYING` and correct the fixture README to attribute Pawel Jakub Dawidek. This is a licensing defect in a public redistribution and is the one item I would not merge past.
2. Rewrite `case_dir` and `script_path` in the fixture to run-relative paths, as `raw` already is, and disclose the change in the README.
3. Anchor raw-stream resolution on the run directory so the fixture is portable regardless of the working directory, and soften the README's portability claim accordingly.
4. Find the `test_gates` flake. It is measurable, it is in the command CI runs, and per the repository rule it gets fixed whoever caused it.
5. Keep g3 open for the Linux arm, the 80 declined cases, the 3969 privilege-gated assertions, the 11 absent macros and the weak native oracle.