# pjdfstest g3 identity repair: independent review

Reviewer lane: read-only review of PR #106 at head `576dfa0993e1a1901d14f69e922630cbebd9edfc`, against the head I reviewed previously, `d004caacf42d0859d61018c7aa119cf4c529cc35`.
Lease `a53321161c0b6660c9124671c6c6654c`, branch `review/pjdfstest-g3`, fetched over HTTPS by exact SHA.
No production source edited, no commit, no push, no merge, no lease return, no reset, no stash, no workflow dispatch, no rerun, no poll.

| lane | verdict |
| --- | --- |
| the three blockers I raised at `d004caac` | **all three PASS**, one docstring inaccuracy noted below |
| pairing logic | **byte-identical** to what I audited; no coverage expansion |
| `validate_runtime_identity` fail-closed guard | **PASS**, all nine refusal cases present and tested |
| typed exit taxonomy | **PASS**, integrity outranks divergence, coverage has no exit |
| file modes and lint | **PASS**, 100755/100644 committed, ruff 0 |
| own receipted sample | **PASS**, author receipt independently verified; my earlier own run stands |
| macOS NFS against private real-Core mount | **FAIL**, independently reproduced |
| Linux FUSE arm | **UNMEASURABLE**, not attempted |

## Pairing is unchanged, and I checked that by bytes not by name

I hashed the AST source segment of every identity function at both heads.
All ten are byte-identical: `normalize_operation`, `operation_key`, `normalize_text`, `literal_pinned`, `script_profile`, `pair_case`, `outcome_text`, `first_call`, `ordinal_diagnostic`, `compare`.
All eight module-level regexes are identical: `PLAN_RE`, `RESULT_RE`, `TRIED_RE`, `GENERATED_RE`, `NAME_TOKEN_RE`, `ROOT_REQUIRED_RE`, `RESULT_DEPENDENT_RE`, `LITERAL_FOR_RE`.

Only five functions changed, and all five are the guard and receipt layer: `main`, `verdict`, `cmd_reconcile`, plus new `validate_runtime_identity`, `reason` and `state_from`.
That is precisely the requested scope and no more.

My manual closure therefore carries over intact.
Route 1 still fires for exactly 36 of 238 cases, and those 36 still have zero `for` loops, zero `while` loops, zero local function definitions, exactly one sourced file, and a transitive chain in which the only assertion-emitting helper is `create_file`, which the profile blocks on.

The two known conservatisms I raised last round were deliberately left in place rather than "fixed", because fixing them would have expanded coverage and the author chose not to.
They are still real and still correctly disclosed: `rmdir/12.t` is blocked by the word `if` occurring inside its own `desc=` string, and `unlink/14.t` under-counts sites because a line-continued `expect` loses its continuation.
Both over-block, which is the safe direction.
The consequence is unchanged and the documents say so: `rmdir/12.t` #4 and `unlink/14.t` #4 are **manual confirmations, not algorithm-established**.
I confirmed that myself rather than taking it on trust: pairing each of those two cases yields `route=operation text`, `established_reg=0`, all assertions unpairable.
They are established well enough for triage because both parent cases are short, six and seven assertions with no loops, the text names the same operation on both sides, and each matches the pinned script's fourth `expect`.
A pass prints no text, so the native side has no operation to pair, which is the honest limit of the method rather than a defect in it.

## Blocker 1: the unreceipted device number is withdrawn, and a real receipt replaces it

**PASS.**

Both documents now carry an explicit withdrawal, and the reason given is the right one: the number "existed only in a chat message".
The canonical write-up says the run now takes arm separation from `identity.json`, written while the mount was up.

I verified that receipt by reading it and re-deriving everything else from the raw streams, not from the summary.

`run/20261005T025742Z/identity.json`:

| | value |
| --- | --- |
| native | `fstype apfs`, `mountpoint /`, `st_dev 16777234`, `source /dev/disk3s1s1`, `problem null` |
| cowfs | `fstype nfs`, `mountpoint <run>/mnt`, `st_dev 436209661`, `source localhost:/cowfs-de4387245f4a6633cfc77c43fa2d25bd`, `problem null` |
| `validated` | `true`, `problems` empty |
| daemon | pid 8004, registered `2026-10-04T19:57:48-0700`, argv carries `--backend core` |
| `recorded_at` | `2026-10-04T19:57:49-0700` |
| `cowfs_build` | `cowfs-daemon` `4804a165...`, `cowfs` `33055fed...`, head `d004caac` |

Every claim checks out: native APFS 16777234 from `/dev/disk3s1s1`, cowfs NFS **436209661** from localhost, `problem` null on both, `validated` true.
The cowfs paths are this run's own, and the device differs from the `436209639` my own critic run produced, so this is not a copied receipt.
The device is genuinely live-attested rather than inferred: `mount_table_line` records the actual `localhost:/cowfs-de4387245... on <run>/mnt (nfs, nodev, nosuid)` entry, and the daemon's registered pid, argv, store and socket are bound into the same file.

The receipt is written at `main()` while the mount is up, immediately after validation and before any case child, so it survives teardown.
`verdict()` now takes the identity as an argument and persists it as `runtime_identity`, and records `provenance.identity_receipt`.
It no longer reads `summary.json` for identity before that file exists, which was the structural cause of the earlier all-null captures.

One correction to the task's framing: `cowfs_head` in the receipt is `d004caac`, not `025bde2`.
The binaries are the same bytes as before, `4804a165` and `33055fed`, and I confirmed `crates/` is untouched across all three repair commits, so production is genuinely unchanged and only the recorded commit string moved.
PR #106 is bench and documentation only, and claims nothing about Core POSIX behaviour being fixed.

## Blocker 2: the guard is now genuinely fail-closed

**PASS.**

`validate_runtime_identity` refuses on all nine required conditions, and I checked each against the source rather than the test names:

| condition | how it is caught |
| --- | --- |
| no identity at all | `if not identity` returns immediately |
| missing native or cowfs arm | `record = identity.get(arm) or {}`, then the null-device branch |
| a stat problem | `record.get("problem")` |
| null device | `device is None` |
| device not an int, or a bool, or non-positive | `not isinstance(device, int) or isinstance(device, bool) or device <= 0` |
| null or empty filesystem type | `not fstype or not isinstance(fstype, str)` |
| null mount point | `not record.get("mountpoint")` |
| equal devices | explicit equality comparison |
| wrong requested mount | `cowfs_mount != str(expected_cowfs_mount)` |

The Python `bool`-is-`int` edge is handled explicitly and deliberately, because `isinstance(True, int)` is `True` in Python and a naive check would have accepted `st_dev=True`.
There is a dedicated `RuntimeIdentityIsFailClosed` class with **11 tests**, one per refusal plus a positive control, and the device test iterates `(0, -1, "16777234", True)`.

The fail-open I reported at `d004caac` is now a regression test in its own right: both arms with `st_dev=None` are equal, so an equality-only check would have accepted them, and the test asserts INVALID and the presence of a "no st_dev" message.
That is the exact defect I flagged, converted into a permanent control.

No label-only pass is possible.
Identity is derived from `st_dev` plus a real mount-table lookup, and a device inequality alone proves only that two filesystems differ, never that the cowfs arm is cowfs.
The receipt therefore also binds the daemon's registered pid, argv including `--backend core`, store and socket path, and the two binary hashes, which is what actually attests the Core backend.
That is the correct division of labour and the documents now say it.

## Blocker 3: modes and lint

**PASS.**

`git ls-tree` on the new head shows `100755 bench/pjdfstest.py` and `100644 bench/test_pjdfstest.py`, and the file contents match: the harness carries `#!/usr/bin/env python3`, the test file no longer carries a shebang and opens with its docstring.
That is the correct fix for `EXE001` in each case, chmod the executable and drop the shebang from the non-executable, rather than silencing the rule.

With the committed modes restored, `ruff check` on both files reports **All checks passed**.
I should be precise about how I got there: my first run reported one `EXE001`, which was my own artifact, because I had staged the files with `git show >` and lost the tree mode.
After restoring 755 and 644 from the tree, zero findings.
So the "0 at this head" claim is true, and the seventeen findings I reported at the reviewed head are genuinely resolved.

## The exit taxonomy is now four kinds and honest about each

| kind | exit | meaning |
| --- | --- | --- |
| INTEGRITY | 3 | records or provenance cannot be trusted: malformed or truncated stream, moved raw hash, synthetic fixture, mixed record format, tool-source drift, missing or invalid runtime identity |
| DIVERGENCE | 1 | established assertions pass natively and fail on the mount |
| CAPABILITY | 2 | the run cannot measure, for example no matched arm pair or an arm that executed nothing |
| COVERAGE | none | a limit on what the transcript can conclude, disclosed only |

`state_from` resolves INTEGRITY, then DIVERGENCE, then CAPABILITY, else PASS.
That fixes the conflation I reported last round, where every integrity failure was funnelled into the same bucket as an unmeasurable run.
It also preserves the property that matters most: **a real filesystem divergence outranks partial coverage**, so a FAIL is never downgraded by the fact that much of the suite could not be paired.

Measured, with real process exit codes rather than a predicate over a dict:

| invocation | exit | state |
| --- | --- | --- |
| `--reconcile` a nonexistent run directory | **3** | INVALID |
| `--reconcile` the historical set `bb55fe49` | **3** | INVALID |
| `--reconcile` the superseded set `1d64dabc` | **3** | INVALID |
| `--reconcile` the receipted run `eb2ff124` | **1** | FAIL |

The two superseded sets now return INVALID precisely because they carry no `identity.json`, which is the correct qualification: they are diagnostics, not arm-placement acceptance.
Both documents say exactly that.
The historical set still reconciles its classifier numbers, so the diagnostics survive while the verdict does not: 77 established, 71 outside the privilege gate, 0 candidates, 10050 unpairable of which 5563 textless, plus 33 identity-level looser rows against the 855 ordinal looser diagnostic.
A unit test spawns a real subprocess and asserts `returncode == 3`, so the exit status cannot be satisfied by constructing a dict.

## Independent re-derivation of the author's run

I re-parsed all ten raw streams from disk rather than reading the summary, and re-ran the classifier on the result.

- 5 cases per arm: `mkfifo/00.t`, `open/17.t`, `mkdir/00.t`, `rmdir/12.t`, `unlink/14.t`.
- 88 assertions per arm. Native 66 passing and 22 failing, cowfs 43 passing and 45 failing.
- All ten child exits 0. Ten raw streams present, every `raw_sha256` matches, **0 re-parse mismatches** against the stored records, **0 guard-problem records**.
- Established regressions **25**, `mkfifo/00.t` 22 and `open/17.t` 3. Unpairable **26**, of which **24** with no operation text. Candidates **0**.
- `looser_not_a_pass` 4, reported and not scored as wins.
- `cases.jsonl` sha256 `eb2ff1245baa4aa3`, matching the documented digest.

Of the 25, twenty print operation text showing a `mkfifo` `EIO` or its `ENOENT` consequence, and five are textless `test_check` sites at `mkfifo/00.t` slots 29 to 33 that the script-slot route establishes.
They are the same cause, so the claim that every established regression is a non-regular create or a consequence of one holds.
`EIO` remains attributed **by inference**, unchanged, because `error.rs:74` maps `Corrupt` and `Io` to the same `EIO` and the `NFS3ERR_NOTSUPP` client translation lives in XNU.
Both documents still say inferred, not proven.

## Tests, CI, ownership

**39 unit tests, all pass, exit 0**, run directly as an isolated pair so the older module could not shadow the new one.
That is up from 25, and the new ones cover the identity refusals, the taxonomy precedence, and the process-level exit.

`git diff --name-only` against the merge base shows **0 files under `crates/`** changed by any of the three repair commits, so nothing about Core or either adapter is touched.

CI at this exact head is **1 passed, 2 pending**: `linux-fuse` passes, `ubuntu-latest` and `macos-latest` are pending.
The head is therefore **not** green, and I make no green claim.
The previous head had one pending leg; this one has two, because the head moved.

`main` is `3a6935b244d0209ef7c75ddf9539f54dc137105b`, "Merge pull request #111 from `fix/nfs-translate-security-43`".
PR #106 is open and draft.
`closingIssuesReferences` is `0` with an empty node list, so merging closes nothing.
The one added commit is `fix(g3): refuse a run whose arms cannot be placed, and persist the live identity`; it contains no closing keyword and no reference to #107 through #110, and the bare bullets in the body remain neutral.
No acceptance issue is at risk, and #107 through #110 stay open.
I filed nothing, fixed nothing in production, and delegated to no builder.

## One inaccuracy worth fixing, in the guard's own docstring

`validate_runtime_identity` says "this runs before any child process exists".
That is false and it is not fixable as written, because the cowfs arm's filesystem cannot be identified before the mount exists, so the daemon must already be running.
The ordering in `main()` is correct and is the only physically possible one: `start_daemon`, `create_snapshot`, then `fs_identity` on both arms, then `validate_runtime_identity`, then the `identity.json` receipt, then the refusal return, and only then `run_arm`.
The `main()` comment, "Fail closed before a single case runs", is accurate.
So the code is right and one docstring is wrong.
The honest statement is that it runs before any **case** child and before the receipt is trusted, with the daemon necessarily already up.
A reader who trusts the docstring would look for a pre-daemon check that does not exist.

## Disclosure: I wrote into another lane's artifact tree

Running `--reconcile` against the builder's preserved run directories caused the harness to write `reconciliation.json` into `20261005T004337Z`, `20261005T022515Z` and `20261005T025742Z` under lease 9.
Those files existed before, the two superseded ones being the author's own preserved receipts, and my invocations overwrote them with output from this newer head.
That was my error: `cmd_reconcile` writes into the run directory, and I should have copied the run directories into my own owned path before invoking it rather than pointing the CLI at another lane's tree.

Scope of the damage, checked immediately:

- **No primary evidence harmed.** All three `cases.jsonl` still hash to their recorded values: `bb55fe49`, `1d64dabc`, `eb2ff124`. The `identity.json` receipt is unchanged. All ten raw streams are present. The pinned tool source still hashes `a6c354f2` and the built binary `5fa40986`.
- **No tracked file changed anywhere.** `bench/out` is gitignored, and `git status` in the builder's lease is clean.
- **What was lost is derived and regenerable**: three `reconciliation.json` files, whose content now reflects this head rather than the head that produced them.
- The classifier numbers are unaffected, because they derive from the intact `cases.jsonl`, and I had already re-derived 77, 71, 0, 10050 and 5563 directly from that file in my previous review without the CLI.

I am flagging it rather than quietly leaving it, because an overwritten receipt is exactly the class of evidence loss this gate exists to prevent, even when the underlying streams survive.

## Process safety and preservation, re-verified at this head

The registry, tri-state mount parser and bounded-wait structure are unchanged from the head I already audited, and they still hold: registration of pid, argv, start time and `ps` line before any wait; re-verification of pid liveness, start time, argv and store or socket path before every signal; one child at a time with an argv containment check; no process group, no `pkill`.
`mount_table` still rejects a non-zero exit, empty output and a missing trailing newline as UNKNOWN, and UNKNOWN blocks any unmount, walk or deletion.
All waits remain bounded.
Teardown is still name-scoped to `c.sock` and `c.sock.lock`.
A teardown that ends UNKNOWN is still reported only in the log and does not affect the verdict; that residual is unchanged and still worth surfacing in the summary.

The socket path is 98 bytes against the 103-byte limit and is checked before creation, which is what let my own earlier run proceed after the first attempt was correctly refused.

I preserved everything else: the shared lock, pid 15263 and the `~/.cowfs/mnt` mount untouched, all leases held, the g4, g5, 987929D and 899604 mounts as I found them.
I wrote only under my own lease `bench/out/pjdfstest-identity-final-critic/**` plus this document.

## Acceptance, unchanged and still open

**macOS NFS against a private real-Core mount: FAIL.** Established, script-proven, matched regressions in a real receipted run, and reproduced by me.

**Linux FUSE arm: UNMEASURABLE, not attempted.** The host is reachable and has `/dev/fuse` and `fusermount3`; the gap is a cross-build plus the shared Linux lock.

Also still missing coverage: 80 suite-declined cases per arm, each with an explicit reason in the pinned source; 1968 native and 1981 cowfs privilege-gated assertions, none run; 11 absent host capability macros, all confirmed absent from the generated `config.h` with genuine compile-and-link probes and no stubbed macro; and a native oracle that fails 43 percent of all assertions on plain APFS, of which 3733 are outside the verdict.
No claim is made that cowfs POSIX conformance is fixed.
Nothing here supports closing g3, and no merge should follow from this review.

## Required before merge

1. Correct the `validate_runtime_identity` docstring: it runs before any **case** child, not before any child process, because the daemon must exist before the cowfs filesystem can be identified.
2. Wait for the `ubuntu-latest` and `macos-latest` checks, or state plainly that the head is unverified on both legs.
3. Optionally strip quoted strings before `RESULT_DEPENDENT_RE` and fold line-continued assertions into one site, which would let `rmdir/12.t` and `unlink/14.t` reach route 1 and move the two triage-relevant divergences out of the unpairable set. Correctly omitted as unnecessary coverage expansion, and correctly disclosed as manual-only, so this is a choice rather than a defect.
4. Surface a teardown that ends UNKNOWN in the summary rather than only in the log.
5. Keep g3 open for the Linux arm, the 80 declined cases, the 3969 privilege-gated assertions, the 11 absent macros and the weak native oracle.