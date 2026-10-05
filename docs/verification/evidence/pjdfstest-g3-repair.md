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

## What was not done

- No production source was patched.
- The Linux FUSE arm was not attempted.
- The 198 `EIO` rows are attributed to the documented `MKNOD` policy by inference only.
- The `NFS3ERR_NOTSUPP` to `EIO` client translation was not proven; it lives in XNU and no own RPC
  control run was made.
- Two timestamp-order checks remain hypotheses with no mechanism.
- No performance, soak, capacity or power-loss claim is made anywhere.