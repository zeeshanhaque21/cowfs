# g3 repair evidence: what changed, and which number came from where

Companion to `docs/verification/ready-g3.md`, which is the canonical write-up.
This file is the compact audit trail: what the repair changed, and the provenance of every count.

Raw artifacts are preserved and ignored under `bench/out/ready-g3/**` in the worktree that ran
them.
No generated record was rewritten.

## Sources of every number

| source | what it is | sha256 |
| --- | --- | --- |
| historical record set | the author's full 238-case run at head `025bde2`, 476 records | `cases.jsonl` `bb55fe4912a36302980c9a1b0f8214a44f2af83afb4cfa3f60b96f36db6a8259` |
| historical summary | the author's own `summary.json` from that run | `399807` bytes, read but not rewritten |
| historical reconciliation | derived by the repaired harness from the record set above | `run/20261005T004337Z/reconciliation.json` |
| receipted small run | 5 cases per arm at this head, `identity.json` written before teardown, raw streams and hashes kept | `cases.jsonl` `eb2ff1245baa4aa306b2a6458cdde6e32abced1120987283898d5c03ff51ecdc` |
| superseded small runs | `20261005T022439Z` and `20261005T022515Z`, raw records preserved untouched | `cases.jsonl` `1d64dabc...`; both recorded `cowfs_fs` as `stat failed`, so neither can carry a verdict |
| pinned tool | `pjd/pjdfstest` at `85a8aea9e685999ef0540392fd80535f873d7ff7` | `pjdfstest.c` `a6c354f2c42015a1...`, binary `5fa40986f39bb903...` |

The historical record set keeps no raw per-case stream, so it is labelled legacy: the parsed
records can be checked against each other but not re-parsed from the source text.
The repaired run keeps every stream, so it is raw-attested.

## The identity repair

The old comparator paired assertions by their position in the stream.
It now pairs them by what the pinned script proves, in two routes:

1. **Script slot.** When the script makes no assertion whose position depends on an earlier result,
   calls no helper that injects a variable number of assertions, uses no jail case, has no
   line-continued assertion, and its assertion count equals the stream's plan, the k-th assertion in
   the transcript is the k-th assertion the script makes. That pair is established, and the script
   names the operation even when the stream printed no text for it.
2. **Operation text.** Otherwise, two assertions pair when their operation text matches after
   generated names are canonicalised to their position, and only when a literal in the operation
   pins it. A repeated operation with no literal `for` loop is a text duplicate and is rejected as
   ambiguous. A case where the stream contradicts the script's slot is rejected.

Outcome text is never part of an identity, and neither is a global ordinal across the two arms.

### Invariants proved before anything was scored

`bench/test_pjdfstest.py`, all records marked synthetic, and `verdict()` refuses synthetic records
so a fixture can never be scored as conformance:

| invariant | what it pins |
| --- | --- |
| a result removed from one arm does not shift the next identity | position is not identity |
| a result added to one arm does not shift the next identity | same |
| a repeated operation without a literal loop is unpairable | no arbitrary pairing of duplicates |
| a literal loop orders its repeats and keeps the iteration | the loop suffix is part of identity |
| generated names do not change identity; argument order is not provable from names alone | a candidate, not a defect |
| error text never enters an identity | outcomes cannot pair assertions |
| textless assertions are never paired | `test_check` has no operation |
| a text duplicate is ambiguous rather than paired | duplicates are rejected |
| a stream that contradicts the script slot is refused | the script is the authority |

25 tests, all pass, exit 0.

## Corrected accounting

| figure | value | where it comes from |
| --- | --- | --- |
| cases per arm | 238 | historical record set |
| executed / suite-declined per arm | 158 / 80 | historical record set |
| assertions per arm | 8686 | historical record set |
| native failing | 5701, of which 1968 privilege-gated and 3733 not | historical record set |
| cowfs failing | 5546, of which 1981 privilege-gated and 3565 not | historical record set |
| privilege-gated that passed | 0 | historical record set |
| ordinal regressions, total | 700 | derived diagnostic |
| ordinal regressions outside the privilege gate | 687 | derived diagnostic |
| of those, matching comparison text | 48, all textless on both sides | derived diagnostic |
| of those, structurally different | 639 | derived diagnostic |
| looser ordinal positions, matching text | 855 total, 0 matching text | derived diagnostic |
| looser `chown`/`lchown` rows | 714 | derived diagnostic |
| looser other rows | 141: `rename/09.t` 70, `rename/10.t` 60, `unlink/11.t` 10, `mkdir/10.t` 1 | derived diagnostic |
| partition A, direct `EIO` rows | 198, `mkfifo` 106 and `bind` 92 | derived diagnostic |
| partition B, `pathconf` case rows excluding A | 66 | derived diagnostic |
| partition C, `ENOENT` cascade rows | 360 | derived diagnostic |
| partition D, textless rows | 48 | derived diagnostic |
| partition E, every other answer | 15 | derived diagnostic |
| partition sum | 687 | derived diagnostic |
| **established regressions, script-proven** | **77, 71 outside the privilege gate** | repaired comparator |
| candidate regressions | 0 | repaired comparator |
| unpairable assertions | 10050 | repaired comparator |
| unpairable with no operation text in a non-provable case | 5563 | repaired comparator |
| established regressions by case | `unlink/00.t` 30, `mkfifo/00.t` 22, `mknod/00.t` 22, `open/17.t` 3 | repaired comparator |

Withdrawn: 617, 63 and 713 from the first version of the write-up.
They do not reproduce.
The "three small divergences" phrase that listed four checks is also withdrawn.

The classification labels A to E describe which row an assertion fell into.
They are diagnostics over the transcript, not a root-cause analysis and not a defect count.

## Repaired small run

| | value |
| --- | --- |
| cases per arm | 5: `mkfifo/00.t`, `open/17.t`, `mkdir/00.t`, `rmdir/12.t`, `unlink/14.t` |
| assertions per arm | 88 |
| native passing / failing | 66 / 22 |
| cowfs passing / failing | 43 / 45 |
| verdict | FAIL, exit 1 |
| established regressions | 25: `mkfifo/00.t` 22, `open/17.t` 3 |
| ordinal differential for the same cases | 27, 22 structurally different |
| unpairable | 26, of which 24 with no operation text |
| guard problems | 0 |
| arm separation, from `identity.json` written while the mount was up | native `apfs` `/` `st_dev` 16777234 source `/dev/disk3s1s1`; cowfs `nfs` `st_dev` 436209661 source `localhost:/cowfs-de4387245f4a6633cfc77c43fa2d25bd`, `problem` null, `validated` true |
| raw streams | 10 files under `raw/`, each hashed in `cases.jsonl` |
| repeat runs | same counts on every locked invocation of the repaired harness |
| measured build | `cowfs-daemon` `4804a16546a87679...`, `cowfs` `33055fed260adfc...`, identical to the reviewed manifest, `crates/` unchanged since `025bde2` |
| socket path | 98 bytes against the 103-byte `sun_path` limit, checked before creation |

Withdrawn: the `st_dev` 436209625 the earlier revision reported for the cowfs arm.
It is in no preserved file: both superseded runs recorded `cowfs_fs` as `stat failed`, and the
number existed only in a chat message.
The independent review's own sample carries its own receipt with `st_dev` 436209639
(`docs/reviews/pjdfstest-g3-repair-final.md`), cited here as reviewer evidence with its provenance.
NFS `st_dev` is per mount, so those last digits differing is expected and means nothing alone.

Named controls inside that run:

- `rmdir/12.t` #4: native `ok 4`, mount `not ok 4 - tried 'rmdir a/b/..', expected ENOTEMPTY|EEXIST, got EINVAL`
- `unlink/14.t` #4: native `ok 4`, mount `not ok 4 - tried 'open f O_RDONLY : unlink f : fstat 0 nlink', expected 0, got 1`

## Guards, and the exit taxonomy

`verdict()` refuses: a missing plan line, a plan that does not match the assertions emitted,
duplicate ids, non-contiguous ids, a `Bail out!`, a malformed line before the plan or between
results, a non-zero child exit, a timeout, a record under the wrong case name, a raw stream whose
hash moved, a record set mixing raw and raw-less cases, and a synthetic fixture.
Mount inspection is tri-state, with UNKNOWN blocking any unmount, walk or deletion.
Both arms must report different `st_dev` values or the run is refused before any case executes.

Reasons are typed and the exit follows the kind, not the wording:

| kind | covers | exit |
| --- | --- | --- |
| INTEGRITY | malformed or truncated stream, raw hash moved, synthetic fixture, case-integrity failure, mixed record format, tool-source drift, missing or invalid runtime identity | 3 |
| CAPABILITY | tool, prerequisite or capability absent, so nothing ran | 2 |
| DIVERGENCE | an established assertion passes on one arm and fails on the other | 1 |
| COVERAGE | unpairable assertions, identity unrecoverable | disclosed only |

A real divergence keeps exit 1 even where part of the scope is unpairable, integrity outranks both,
and coverage never decides an exit.
The command line is covered too: the suite runs the module as a child process and reads its real
exit status, so a refusal that only a predicate can see fails the test.

### Fail-closed runtime identity

Refused before any child process exists, each with a named negative test whose spawn callback is
never reached: no identity at all, a missing native arm, a missing cowfs arm, a `problem` set, a
null `st_dev` on both arms, a null filesystem type, a missing mount point, a non-integer or
non-positive device, the wrong mount point, and two arms reporting the same device.
A valid identity keeps the independent FAIL precedence.

## Tool provenance enforcement

The checkout must be clean at the pinned commit, and `pjdfstest.c` plus every case script must hash
to the pinned commit's own blob, read from git rather than supplied by a caller.
A mismatch is INVALID before any case runs.
The measured cowfs build is recorded by commit and by binary sha256.
The binary hash is build-specific, so it is recorded together with the `config.h` hash and the
compiler, not treated as a source identity.

## Lint

`ruff check` on the two owned files: 17 findings at the first reviewed head, and 0 at this head.
The two `EXE001` findings that survived the first repair were the file modes, and the fix follows
the repository's own convention rather than a suppression: `bench/pjdfstest.py` is a script, so it
keeps its shebang and its executable bit like `bench/compare.py` and `bench/gates.py`, while
`bench/test_pjdfstest.py` is a unittest module, so it loses the shebang and the executable bit like
`bench/test_gates.py`.
Verified at the tree level rather than in one working copy: `git ls-tree` shows the mode recorded
in the commit, not the mode on whichever disk the check ran on.
The repo has no ruff configuration and CI does not run ruff, so this is a standing-rule fix rather
than a project gate.
No `noqa` was added and no dependency was introduced.

## Analysis never writes into evidence

| rule | why |
| --- | --- |
| the default `--reconcile` destination is refused when anything is there | a derived file must not replace another derived file, least of all one from another lane |
| an explicit `--output` must sit outside the run directory it reads | `--output cases.jsonl` must not be a way to clobber evidence |
| the write is staged then `link`ed into place | `link` refuses an existing name atomically, so there is no check-then-write window |
| a staged file that cannot be linked is kept | a failed write leaves evidence, not a silent gap |
| every analysis carries its own revision and input hashes | a reconciliation is a fresh reading, and a reader must be able to tell which one |

Real exits, from the CLI run as a child process: an existing default destination exits 3 with a
sentinel byte-identical afterwards; a fresh isolated output over the receipted run exits 1 with the
FAIL verdict and an `analysis` block naming this analyser's sha256, the source head, the pinned tool
commit and the sha256 of every input; an integrity refusal still exits 3.
Input bytes were unchanged after all of it: `cases.jsonl` `eb2ff124...`, `identity.json`
`31d92ea0...`.
All of these ran against a fixture copied into `bench/out/ready-g3/reconcile-safety/**`, never
against a preserved run.

The guard wording was also corrected: `validate_runtime_identity` runs before any **case** child,
with the daemon necessarily already serving, because the cowfs arm's filesystem only exists once
the mount is up.

## Disclosure: derived files regenerated by the reviewer

The independent reviewer's `--reconcile` invocations against this lane's three preserved run
directories overwrote the `reconciliation.json` in each of them.
Every raw input is intact by hash: `cases.jsonl` `bb55fe49...`, `1d64dabc...`, `eb2ff124...`,
`identity.json` `31d92ea0...`, ten raw streams, pinned tool source and binary.
The three derived files now hold the reviewer's regenerations; their previous bytes are not
recoverable and are not reconstructed.
Their file times, 20:06 to 20:07 against capture times of 17:51, 19:25 and 19:57, are what shows the
rewrite rather than an original write.

## A false PASS, found by spike before the next patch

| number | source |
| --- | --- |
| exit 1 with 25 established and 26 unpairable, 5 of 5 profiles | `bench/out/pjdfstest-fresh-clone-spike/spike-finding-old.json`, `with_tool_exit` and `with_tool_established` |
| exit 0 PASS with 0 of 5 profiles and 170 unpairable | the same file, `old_without_tool_exit` 0 and `old_without_tool_state` PASS |
| exit 3 INVALID with 0 of 5 profiles, kinds coverage and integrity | `spike-finding.json`, `without_tool_exit` 3, `matches_expectation` true |
| direct `verdict(tests_root=None)` INVALID 3 | the same file, `direct_without_tool` |
| the copied record set hashes `eb2ff124` on the original, `8c6dea69` in the copy | recomputed at each spike run, printed by it |
| the original run's `cases.jsonl` still `eb2ff124` after every spike and every check | printed by the spike and re-verified after each direct CLI run |

The spike's oracle is literal: the expected exits, the expected count of established regressions
and the raw stream count are written into it by hand, and the only thing imported from the harness
is the call whose behaviour is in question.
The copied run's ten raw paths were re-pointed at the copy's own files, whose bytes are identical,
so each recorded `raw_sha256` still verifies and the original run was never written to.
The spike ran first, reproduced the false PASS three ways, and only then was the harness changed.

Both classification checks were then run against the previous source in a tree with no tool cache
and no `.git`, so nothing about the machine's cache could make them pass:

| check | previous source | this source |
| --- | --- | --- |
| a reconciliation without the pinned scripts must not read as a pass | failed, exit 0 | passes, exit 3 |
| `verdict` without the pinned scripts must refuse before classifying | failed, `PASS` | passes, `INVALID` |

## Refusals, all of them typed

Every line here is a check that spawns the CLI and reads the real exit status.

| condition | exit | evidence it leaves |
| --- | --- | --- |
| default destination exists | 3 | the input's every file hash unchanged, sentinel byte for byte |
| explicit destination inside the run | 3 | `cases.jsonl` unchanged |
| explicit destination naming a raw stream | 3 | that stream unchanged |
| live symlink as destination | 3 | the link is still a link, the foreign target unchanged |
| dangling symlink as destination | 3 | nothing created at the link's target |
| parent directory absent | 3 | `could not be staged`, no traceback |
| parent is a file | 3 | `could not be staged`, the file unchanged, no traceback |
| parent without write permission | 3 | nothing written, no staging file left, no traceback |
| `link` failing for another reason | 3 | the staged copy kept, named in the message |
| explicit destination already existing | 3 | that file unchanged |
| run with no identity receipt | 3 | `no runtime identity was supplied` |
| pinned source that is not the pinned commit | 3 | `is not the pinned blob`, nothing written |
| no pinned source at all | 3 | `nothing was classified and nothing was written` |
| the pinned closure present and honest | 1 | 25 established, 26 unpairable, one divergence reason |

Real exits from the CLI over the receipted run itself, whose bytes were unchanged afterwards:
default destination 3, fresh destination outside the run 1 with 25 established and 26 unpairable, a
tool directory that is not a checkout 3.

## Receipts bind their own bytes

| field | source | failure mode it closes |
| --- | --- | --- |
| `analyser_sha256` | the script that ran | none, this is the fact |
| `analyser_revision` | only when the blob HEAD records for the path is the script on disk | `UNKNOWN` for an unrelated checkout, an edited script or no repository |
| `ambient_checkout` | the nearest checkout, labelled not the origin of the analysis | a reader mistaking it for provenance |
| `inputs` | sha256 of every file the analysis read | two readings of one run looking like one |

`source_head` was removed and a check fails if it returns.
Against the receipted run at this head the block reads `UNKNOWN` for the revision with the lease's
HEAD `22340a9` in the ambient block, because the working tree is ahead of its own commit, which is
exactly the attribution the old field got wrong.

## The checks, and the machines they ran on

| check | source | result |
| --- | --- | --- |
| `python3 bench/test_pjdfstest.py` | this lane | 73 collected, 73 run, exit 0 |
| `python3 -m unittest test_pjdfstest.VerdictStates` | this lane | 5 collected ids, 5 run, exit 0 |
| `python3 -m unittest discover -s bench` in a copy of the tracked files with no `bench/out` and no `.git` | this lane | 104 checks, exit 0, no skips, of which 68 are this lane's and 36 are `bench/test_gates.py` |
| the writer, classification and fixture classes as written, reversed, odds then evens, three seeded shuffles, and each check alone | this lane | 29 checks, 0 failures each way, 0 skips |
| the two classification checks against the previous source | this lane, in a cache-free tree | both fail |
| `ruff check` on both files | this lane | clean |
| `python3 -m py_compile` on both files | this lane | clean |

The fixture is 20 files: a README, a provenance map, the sanitised identity receipt, ten transcripts,
ten records, five upstream scripts and the upstream notice.
The five scripts hash to what the pinned commit holds for them, verified against the checkout at
`85a8aea9` before they were copied.

## Licence, corrected against the upstream object

| number | source |
| --- | --- |
| `bench/pjdfstest-fixture/tool/COPYING` is byte-identical to `COPYING` at `85a8aea9` | `git show 85a8aea9:COPYING` piped through `diff -` against the fixture file, no output |
| its sha256 is `e12b8e42b14e014b3e02f19a6b49de44dfb5f16dec55db1ace0f110be2d71330` | that same git object's content, and the fixture file |
| the notice names Pawel Jakub Dawidek 2006-2012 | `grep -m1 Copyright` on the fixture file |
| the five scripts carry no per-file notice | independent review, confirmed by reading each file |
| a closure with an extra script, an extra file or an edited notice exits 3 | three checks, each spawning the CLI |
| `CURATED_METADATA` pins the notice, `CURATED_METADATA_NAMES` allows `COPYING` and `README.md` | `bench/pjdfstest.py` |

## Host facts removed from the committed copy

| number | source |
| --- | --- |
| 30 occurrences of the capture host and username before, 0 after | parsed JSON paths, then a scan of every committed fixture file for `/Users/`, `zeeshanhaque`, `.treehouse`, `/tmp/` |
| 11 identity fields rewritten, 22 kept | `PROVENANCE.json`, `identity_fields_rewritten` and `identity_fields_kept`, each compared against the source document |
| `run/cases.jsonl` `eb2ff1245baa4aa3` becomes `edc6ca52e1fd2cec` | both hashes recomputed after the transform |
| `run/identity.json` `31d92ea0d2bd89d7` becomes `84dc788f83a5fa28` | same, and the fixture is a derivative, not the receipt |
| the ten transcripts contain no host facts and are byte-identical | the source `raw/` was copied, not rewritten, and every `raw_sha256` verifies |
| the five scripts keep their pinned hashes | `bd017018`, `f631099b`, `b2aa69d1`, `0078ce2f`, `ce168a45`, each equal to its literal in the harness and to the upstream blob |
| the source run is unchanged by the transform | digests before and after, equal |
| the source run is unchanged by every check and every CLI run | `cases.jsonl` `eb2ff124`, `identity.json` `31d92ea0`, and the ten streams, re-verified after each batch |

The credentials scan found none, so this is host and username disclosure rather than a secret leak,
and it is labelled that way rather than inflated.
The fixture's identity declares `sanitised-reference`, the verdict reports that scope as a coverage
disclosure, and a copy with the declaration removed stops reporting it, so the disclosure follows the
receipt and not the fixture.

## Portability, measured the way a reader meets it

| number | source |
| --- | --- |
| three working directories, exit 1 each, payload sha256 `ca26de0f649457fe` identical within this checkout | the published CLI as a child process, outputs in three fresh directories; the digest also moves if the run directory or the clone moves, because the payload embeds absolute paths, so it is a within-environment witness rather than a portable digest |
| 25 established and 26 unpairable from all three | each payload's comparison |
| a relative raw path that climbs out of the run is refused by name | a check with `../outside/borrowed.tap`, exit 3 |
| a relative raw path through a symlink out of the run is refused | a check with `raw/link.tap`, exit 3 |
| an absolute raw path is taken as written | a check pointing every record at a copy outside the run, exit 1 |
| every raw stream unreadable is INVALID 3, not PASS 0 | a check with the raw directory emptied, exit 3 |
| a run wider than the closure is INVALID 3 | `rmdir/12.t` renamed to `rmdir/13.t` in both arms, exit 3 |
| a closure missing entirely is INVALID 3 | the CLI with no `--tool` against a checkout with no cache |
| the live receipt declares no scope, so no disclosure is added | the CLI over the real run, exit 1 |

## The flake, measured twice and still unresolved

| measurement | result |
| --- | --- |
| this lane, 8 runs of the CI command at this working tree | 0 failures, ambient `load1` 15.1 to 18.5 |
| this lane, 8 runs at `22340a9` | 0 failures, ambient `load1` 13.5 to 16.5 |
| independent review, 8 runs at that head | 1 failure, in `bench/test_gates.py`, outside this diff |
| independent review, 8 runs at `22340a9` and 8 on `main` | 0 failures |

16 clean-archive runs here did not reproduce it, so this lane does not claim a cause and does not
patch the file.
The mechanism available to it is in `bench/compare.py`: an unmeasurable gate returns 2 when the
recorded `load1` peak passes `LOAD_CEILING` 30.0 or the arms are skewed by more than `LOAD_SKEW` 2.0,
and those loads are the ones the gate run recorded, so a test that runs against real ambient load
inherits the machine's state.
`COWFS_BENCH_FAKE_LOAD1` exists as a hook in `bench/gates.py` and no check in `test_gates.py` sets
it.
#125 carries the fix, and asks for the preserved failure first, which nobody has.

## Five verdict properties that were never collected

`VerdictStates` held five methods whose names lacked the `test_` prefix, so default discovery never
collected them and every report of this module's coverage excluded them.
They are not new tests and they were not written this round; they were written, not collected.

| before | after |
| --- | --- |
| `getTestCaseNames(VerdictStates)` returns 0 usable tests | returns 5 ids |
| module collects 68 | module collects 73 |
| the class is never run by `python3 bench/test_pjdfstest.py` | it is, and passes |

Two of the five were stale the moment they ran, which is what collection would have shown:

| method | wanted | got | why |
| --- | --- | --- | --- |
| established regression is FAIL | FAIL 1 | INVALID 3 | the fixture passed no source, so the classification prerequisite refused |
| a pass needs a pairable scope | UNMEASURABLE 2 | INVALID 3 | same |

The first is corrected by giving the fixture the verified pinned context it needs: the committed
curated closure, checked against its pins inside the check itself, and a case whose pinned script
proves its own slot order, which is what the real transcript looks like.
One established regression in that context is FAIL 1, and the check asserts the regression count as
well as the state.

The second is not a fixture defect and is recorded rather than settled here.
The settled taxonomy discloses coverage and lets no other kind of reason move the exit, so a run with
a verified source and nothing pairable is PASS 0 with a disclosure, which is what the check now
asserts, along with the same fixture being INVALID 3 when the source is absent.
Whether an entirely empty scope should read as a pass is the gate owner's question: it has the same
shape as the false pass this harness refuses elsewhere, evidence that could not be read.
The method is renamed to what it asserts, and nothing in the production classifier changed.

The other three were already correct and needed only the prefix: a synthetic record is refused by the
verdict itself, malformed JSON is invalid input rather than a pass, and a record set mixing raw and
raw-less cases is refused. Each still asserts its own specific message, so the prefix did not turn
them into duplicates of the new prerequisite checks.

## What was not done

- No production source was patched.
- The Linux FUSE arm was not attempted.
- The 198 `EIO` rows are attributed to the documented `MKNOD` policy by inference only.
- The `NFS3ERR_NOTSUPP` to `EIO` client translation was not proven; it lives in XNU and no own RPC
  control run was made.
- Two timestamp-order checks remain hypotheses with no mechanism.
- No performance, soak, capacity or power-loss claim is made anywhere.