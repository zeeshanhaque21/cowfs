# Ready g5: xfstests-generic, matched native and cowfs

Task: isolated Linux xfstests acceptance for gate g5 (`progress/plan.json`, "xfstests-generic no worse than native").
Lease: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/11/cowfs`, branch `verify/xfstests-g5`.
Everything on the Pi stayed under `/home/moonscape/cowfs-ready-wave/task-g5/`.

## Verdict

**UNMEASURABLE, prerequisite block. Zero measured assertions. g5 stays OPEN.**

No generic case can run on this host.
The reason is the suite's own startup gate plus the build artifacts it needs, not a harness decision and not a cowfs defect.
The harness answers the question with evidence rather than inference, and refuses on every path that previously produced a false pass.

Nothing in this change says cowfs is or is not xfstests conformant, because no xfstests assertion ran on either arm.

## What ran, with real exit codes

Every number is a real process exit code, captured from the child itself and never from a pipeline.

| step | command | rc | result |
|---|---|---|---|
| suite build | `make` at the xfstests root | not attempted | `include/builddefs` absent, no `configure`, no autoconf |
| preflight | `xfstests_gate.py preflight --xfstests ref/xfstests` | 2 | UNMEASURABLE, three blocking entries, evidence written |
| preflight probe | the suite's own runner on `generic/010`, run by the harness | 1 | `fsstress not found or executable`, outcome SKIPPED |
| matched run, six reviewed ids | `xfstests_gate.py run --cases 005,236,245,309,360,755` | 2 | UNMEASURABLE at preflight, no case executed |
| pin closure, measured on the reviewed tree | `closure_driver.py 005,236,245,309,360,755` | 0 | 16 common files in the union, 0 unresolved source references |
| pin matrix, old gate with old pin | `pin_matrix.py` | 0 | ACCEPTED, and the closure it verified held only 4 files |
| pin matrix, new gate with old pin | `pin_matrix.py` | 0 | REFUSED, 72 problems, naming common/config, common/exit, common/test_names |
| pin matrix, new gate with new pin | `pin_matrix.py` | 0 | ACCEPTED, closure union 16, 0 problems |
| arm placement control, real arms | `arm_placement_control.py real` on the Pi | 0 | 22/22 checks, ext4 vs `fuse.cowfs` |
| harness unit suite | `python3 -m unittest discover -s bench` | 0 | 162 tests, 1 skipped for want of `/proc` |
| lint | `ruff check --isolated --select F,E9` on the gate and its tests | 0 | clean |
| evidence controls | `final-repair/evidence_controls.py` | 0 | 20/20 findings, 86 named tests |
| behaviour controls | `final-repair/behaviour_controls.py` | 0 | 11/11 old-fail new-pass |

The pin matrix rows are the regression evidence for the source closure: the same tree and the same pin shape that the previous revision accepted, refused once the closure was actually read.

## Provenance

| thing | value |
|---|---|
| xfstests remote | `https://git.kernel.org/pub/scm/fs/xfs/xfstests-dev.git` |
| pinned tree sha | `3e1ee800e52a0f53d7d9a7809be1ffc80ec1788f`, committed 2026-08-28T18:11:59+08:00 |
| clone | `git clone --depth 1`, into `task-g5/ref/xfstests`, never into the repo |
| tree state at every measurement | clean; `git status --porcelain -uall` empty |
| reviewed set | 6 ids, `case_count 6` |
| pinned common files | 16, the union of what the six cases reach, each by sha256 |
| pinned executor | `check`, sha256 `53dd21653ec6eabd15b3d7e73b821b9fcc98202d3c791a65768329897bd69e18` |
| host | `Linux moonscapenas 6.12.109+rpt-rpi-2712`, aarch64, Debian, 4 cores, 16 GiB |
| cowfs binary | `cargo build -p cowfs-cli` from this lease, dev profile, at base `46b0f26` |
| cowfs daemon | pid 899604, start 2026-10-04 16:15:54, store `task-g5/store`, mount `task-g5/mnt`, socket `task-g5/run/control.sock`, re-verified by pid, argv, start time, socket and mount table before every use |
| native arm | `task-g5/native`, ext4 on `/dev/sda2`, device 2050 |
| cowfs arm | `task-g5/mnt/wt`, a writable imported snapshot inside the private mount, `fuse.cowfs`, device 143 |

## The prerequisite gate, item by item

`common/config` checks these in order and exits on the first miss.
A non-root `PATH` has no `/usr/sbin`, so `mkfs` and `xfs_io` are installed and merely invisible.
With `/usr/sbin:/sbin` on `PATH`, eight of the ten original entries pass.
Three entries block.

| gate entry | status | detail |
|---|---|---|
| `common/config:114 mkfs` | present | `/usr/sbin/mkfs` (dpkg `e2fsprogs` 1.47.0-2+b2`) |
| `common/config:117 mount` | present | `/usr/bin/mount` |
| `common/config:120 umount` | present | `/usr/bin/umount` |
| `common/config:129 perl` | present | `/usr/bin/perl` |
| `common/config:132 awk` | present | `/usr/bin/awk` |
| `common/config:135 sed` | present | `/usr/bin/sed` |
| `common/config:143 df` | present | `/usr/bin/df` |
| `common/config:147 xfs_io` | present | `/usr/sbin/xfs_io` (dpkg `xfsprogs` 6.1.0-1`) |
| `common/config:123 ltp/fsstress` | **absent** | built from `ltp/*.c` through autoconf-generated `include/config.h` |
| `common/config:126 ltp/fsx` | **absent** | same |
| `check:370 tests/generic/group.list` | **absent** | the suite build generates it; without it `check` answers `unknown test, ignored` and runs nothing |

The third entry is a real finding, not a restatement: `check` resolves a testlist entry by grepping it against the group's `group.list`.
Absent that file, `check` runs zero cases and still prints a summary, which is precisely the shape of a false pass.
The gate treats its absence as blocking and detects the `unknown test, ignored` line as a non-pass.

Five prerequisites are absent on this host, and preflight's three blocking entries cover all five transitively:

| absent prerequisite | reached through |
|---|---|
| `ltp/fsstress` | `common/config:123` |
| `ltp/fsx` | `common/config:126` |
| `tests/generic/group.list` | `check:370` |
| `include/config.h` | the autoconf build that produces `ltp/fsstress` and `ltp/fsx` |
| `include/builddefs` | the same build, which is why `make` was not attempted |

Measured capability probes on the Pi, all four absent: `autoconf`, `automake`, `libtool`, `m4`.
`getfattr` and `setfattr` are absent too, so the attr cases are unrunnable regardless.

The capability record holds 23 entries, and this is what each one is:

| kind | count | what it is |
|---|---|---|
| measured | 21 | 15 PATH lookups, 5 `stat` checks on tree files, 1 `os.getuid() == 0` |
| derived | 1 | `sbin_on_path`, computed from the two `mkfs` and `xfs_io` lookups |
| stated, not probed | 1 | `block_scratch_device`, which this gate never requests and whose own `how` says so |

Every entry carries a `how` field naming the measurement, so no value has to be taken on trust.
The count is a code fact, and a fixed string could not have distinguished this host from the Mac.

## Bypasses considered and declined

Three routes around the missing prerequisites were available.
All three were declined, because each manufactures a result rather than measuring one.

1. Put any executable at `ltp/fsstress` and `ltp/fsx`. `common/config` only tests `-x`. Every case that then calls `$FSSTRESS_PROG` or `$FSX_PROG` compares a no-op against its expectations and exits 0.
2. Hand-write `include/config.h` and `include/builddefs` so `ltp/*.c` compiles. An empty `config.h` claims the host has no optional features, which is false, and the resulting helpers would differ from the suite's own build in ways no local check could confirm.
3. Hand-write `tests/generic/group.list` so `check` can resolve a testlist entry. That is a build artifact; writing it by hand makes the runner resolve a case it was never given a list for.

The first is not hypothetical as a hazard.
An early probe of `generic/002` in an earlier revision exited 0 while printing `unary operator expected` thirteen times, having asserted nothing.
That is why the gate reads the suite's own verdict grammar and why a direct invocation can never be a pass.

## The harness

`bench/xfstests_gate.py`, with `bench/xfstests-allowlist.txt` as machine-readable pin data and `bench/test_xfstests_gate.py` as its test suite.
Four subcommands: `preflight`, `classify`, `run`, `report`.
Exit codes match `bench/compare.py`: 0 PASS, 1 FAIL, 2 UNMEASURABLE, 3 INVALID.
INVALID and UNMEASURABLE come from different causes, so a CI step can tell a moved tree from an unbuilt one.

### Arms run where they were told to run

Each arm's per-case directory is created inside its own `--native-root` or `--cowfs-root`, and the case itself reports the directory it was given, the device that directory is on, and the filesystem type.
Logs and meta stay under `--out`, so the evidence does not land on the filesystem under test.

Root pairs that cannot support a comparison are refused before any case runs, with every reason listed at once: the same directory, one inside the other, a symlink, a missing root, the same device and mount, a cowfs root that is not a FUSE mount, a native root that is a FUSE mount, and the same fstype on both arms.

Real-arm control on the Pi, 22/22 checks:

```
native  fstype=ext4       dev=2050 mount=/     src=/dev/sda2   fstype_from=findmnt
cowfs   fstype=fuse.cowfs dev=143  mount=task-g5/mnt  src=cowfs fstype_from=findmnt
child native rc=0 case=005 devid=2050 readback=witness dir=.../native/run-.../005-native/testdir
child cowfs  rc=0 case=005 devid=143  readback=witness dir=.../mnt/wt/run-.../005-cowfs/testdir
```

The child-reported device numbers differ between the arms, which is the property that a single tmpfs cannot demonstrate.

### A case is signalled only while the harness still owns the pid

The timeout path used to call `os.killpg(os.getpgid(pid), 9)` on a pid it had read at kill time, which can select a foreign group after a pid is reused.
That is gone, and so is every other group or pattern signal in the file.

What replaced it:

- the child is registered at spawn, before anything can go wrong, with its pid, its argv, its cwd and what the kernel said about it at that moment
- `start_new_session=True` makes the child's session and process group equal its pid, which is what distinguishes a case from the harness itself
- immediately before any signal, four things are re-read and must all hold: the pid is in the registry, the child handle has not been reaped, the kernel's start time, process group and session still match the spawn-time values, and the process group is neither the harness's group nor the harness's session
- any failure of those means no signal at all, and the refusal is recorded with its reason
- the signal goes to the single pid the harness holds a handle for, SIGTERM then SIGKILL, and the case log is flushed and fsynced first so a failure row keeps the output the case produced
- the record carries `descendants_contained: false`, because a single-pid signal does not contain a descendant, and that is stated rather than implied

A `ProcessLookupError` between the check and the signal is recorded as a refusal, not an exception.
Nine unit tests build the unsafe states directly, including an injected pid reuse, an exited child, a reaped handle, a missing identity, a vanished pid, a child in the harness's own group, and a child that left its own session.
One test spawns a real child, verifies ownership, and signals it exactly once; on a host with no `/proc` it skips rather than claiming a pass.

### The suite answers, not the harness

`preflight` checks each `common/config` entry by name, then runs a real case through the suite's own runner and treats the result as the answer.
A missing prerequisite prints UNMEASURABLE and exits 2.
A source-pin failure prints INVALID and exits 3.
There is no code path from either to PASS.

### Success needs positive evidence

There are two runners, and the difference decides whether a pass is admissible at all.

`check` is the suite's own runner and the default.
Its verdict grammar is the only one xfstests supports: it names each case in a `Ran:` line, prints `Passed all N tests` or `Failed M of N tests`, and exits 0 or 1 to match.

`direct` invokes the case script without the suite's runner.
It exists for diagnosis and can never produce a PASS, because there is no supported success witness, so its ceiling is UNMEASURABLE.
Recording it as a pass is the failure this gate was written after.

Under `check`, the case is wrapped by the suite, so it cannot print an observer block.
The harness does the observing itself instead: it measures real I/O in the case's own test directory by writing and reading back a file there, records the directory, the device and fstype, the exit code and the residue.

A PASS needs a receipt, and every probe in it is measured:

| probe | what it refuses |
|---|---|
| `check_present`, `check_executable` | a missing or non-executable runner |
| `reviewed_pin_readable` | a gate whose shipped pin cannot be read |
| `tree_sha_is_reviewed` | any tree but the reviewed suite, judged by the pin that ships with the gate |
| `tree_clean` | a dirty worktree |
| `check_sha_is_pinned` | a replaced runner, compared against the reviewed pin's own sha for `check` |
| `group_list_selects_case` | a missing `group.list`, or one that does not select the requested case |
| `case_named`, `no_missing_ids`, `no_extra_ids`, `no_duplicate_ids` | a runner that ran something other than exactly what was asked for |
| `runner_exit_zero` | an exit code that disagrees with the verdict |
| `suite_reported_pass` | no pass summary from the suite |
| `suite_count_matches_request` | a count that disagrees with the ids the runner named |
| `one_testlist_line` | more than one `Ran:` line, which means the stream is not the runner's alone |
| `nothing_not_run`, `nothing_ignored` | the suite's own `Not run:` and `unknown test, ignored` grammar |

The suite streams the case's output into its own, so a case can print a line shaped exactly like the summary.
That is why the witness is read by position and by count rather than by pattern alone: the runner names its testlist once, so a second `Ran:` line refuses the run, and the summary is the runner's last line, so a banner a case printed earlier in the stream is not the verdict.

The receipt records its own provenance: the tree sha, the executor's path, size and sha, the suite's `group.list` sha and the ids it selects, and the sha of the case file as read.

Beyond the receipt, a case is INVALID, never PASS, when the observer block is missing or incomplete, when it names a case other than the one requested, when the log carries a skip signature, when it did no I/O in its own directory, when the log lost bytes the observer measured, or when the suite's own `_fatal` line is in the log.
Two arms that exited 0 over visibly different work are not a pass: the residue each left behind is compared, and a difference makes the case UNMEASURABLE.

A case that exits nonzero is judged FAILED before any of that bookkeeping, as long as the runner attributed the run to the requested case.
Otherwise a real filesystem failure could be explained away by an arm that could not be measured, and a FAIL would be reported as an UNMEASURABLE.

### The reviewed suite is the one this gate ships a pin for

The pin that decides acceptance lives beside the gate and is not selectable from the command line.
`--allowlist` chooses which reviewed cases run; it does not get to say which suite is the suite.

Pointing the gate at a tree this lane built, with a pin matching that tree, therefore produces a receipt that verifies the runner but is labelled harness proof, and the case is not a PASS.
That is enforced in code, not asserted in a document: the receipt compares the tree sha against the shipped pin, and a mismatch marks the receipt `acceptance: false` with a label saying why.
Thirteen unit tests drive the full command line for this, including the forge the review named: own tree, own matching pin, real CLI, and a refusal naming `tree_sha_is_reviewed`.

### Source is pinned, not named

The allowlist carries the tree sha, the sha256 of every reviewed case, the sha256 of every `common/*` file those cases reach, and the sha of the suite's own runner.

The closure is read from the sources, and both shell spellings are read:

```
common/rc:5        . common/config
common/preamble:36 . common/exit
common/preamble:37 . common/test_names
common/preamble:55 . ./common/rc
common/rc:246      . ./common/report
```

The previous revision's regex required a `./` after the dot, so the first three lines matched nothing.
Measured against the reviewed tree, that omission left `common/config`, `common/exit` and `common/test_names` out of the closure, and the pin named four files where the six cases reach a union of sixteen.
`common/config` is the file whose `_fatal` calls implement the entire prerequisite gate this exercise turns on.

The pin has been regenerated from the real sources at the depth the gate verifies, and the matrix on the Pi shows all three combinations: the old gate with the old pin accepted a closure of four files, the new gate with the old pin refuses with 72 problems naming the three missed files, and the new gate with the new pin accepts with the full union and no problems.

A source line this scan cannot resolve to a file is recorded as unresolved and refuses, rather than being read as a clean result.

Generation and verification walk the same graph, at one depth, `CLOSURE_DEPTH`, so a pin cannot claim coverage the verifier does not check.

Every attempt re-reads the tree and refuses before the first case subprocess when the tree sha moved, a reviewed case or a reachable file changed, the worktree is dirty outside the pinned set, the reviewed set drifted from the classifier, or a case pulls in a file the pin does not name.
Build output under `src/`, `ltp/`, `lib/` and `include/` is tolerated by extension, because building the suite leaves object files there; anything else modified is a refusal, and there is no blanket exemption.
The full dirty list decides that refusal; only the recorded copy is capped, so a modified pinned file past the twentieth entry still refuses.

### What is pinned, what is an artifact, and what is synthetic

Three different things are easy to confuse, so they are named rather than blurred:

| thing | status |
|---|---|
| the six case files | real source, pinned by sha256 |
| the sixteen `common/*` files | real source, pinned by sha256, measured per case |
| the suite's `check` | real source, pinned by sha256 |
| `tests/generic/group.list` | a build artifact, absent, therefore not pinned; its absence blocks at preflight, and when it exists a receipt checks that it selects the requested case |
| the harness's own synthetic trees | never acceptance evidence; used for controls, and labelled |

### Only what was measured is recorded

Capability notes are probe results, never literals, and each entry records how it was measured.
`report` returns 1 on FAIL, 3 on INVALID and 2 on UNMEASURABLE, so a CI step wired to it cannot go green on a failing run.
`classify` returns 3 on drift, matching what this document and the PR body claim.
`--require-full` compares against the pinned `case_count`, taken from the same verified read of the pin the run already made, so the denominator cannot come from a pin that changed after the cases ran.

## The coverage denominator, measured

`--require-full` and the coverage line use the classifier's own candidate set, which is not the same as every file whose name starts with a digit.

Measured on the reviewed tree:

| count | what |
|---|---|
| 1609 | entries in `tests/generic` whose name starts with a digit |
| 807 | build outputs: 800 `.out`, 2 `.out.default`, 2 `.cfg`, 1 `.out.xfsquota`, 1 `.out.nojournal`, 1 `.out.nfs` |
| 802 | case files, the denominator |
| 0 | directories among them |
| 0 | suffixed ids in this tree |

Two rules matter and both are stated in the code:

- a suffixed id such as `069_o_tmpfile` is a case and is counted, which is the fix for finding 11
- a directory is skipped and is not counted, so upstream's `307_recovery`, which is a directory, is outside the denominator rather than inside it

This tree has neither a suffixed id nor a `307_recovery`, so the denominator is 802 plain files and neither rule changes it here.
An earlier revision of this document claimed the denominator included `307_recovery`, which was wrong twice over: it is a directory, and it is not in this tree.

## What the review found, and what each finding became

Every row is a control.
The `final-repair/evidence_controls.py` mapping ties each finding to named tests and runs exactly those; `final-repair/behaviour_controls.py` drives the gate as it was at `6a8075a` and the gate as it is now with one fixture and one set of arguments.

| # | finding | old | now | held by |
|---|---|---|---|---|
| 1 | arm roots never used; the gate said PASS | roots recorded in meta only | 7 of 7 bad pairs refused, same-filesystem fallback refused | 6 tests |
| 2 | per-case dir not inside its own root | both arms under `--out`, roots empty | child-reported `TEST_DIR` inside each root, logs under `--out` | 2 tests |
| 3 | empty or assertion-free log scored PASS | `PASS` | SKIPPED | 4 tests |
| 4 | a native failure became a counted pass | `PASS` | INVALID, and the cowfs skip is read | 3 tests |
| 5 | nothing bound executed source to the allowlist | id-list hash only | clean accepted, tamper cases refused | 8 tests |
| 6 | transitive `common/*` trusted by filename | no closure read | both spellings read, 16 files pinned, unpinned pull-in refused | 10 tests |
| 7 | `capabilities_absent` hardcoded and false here | literal string | 21 measured, 1 derived, 1 stated, each with its `how` | 2 tests |
| 8 | `classify` exited 0 on drift | rc 0 | rc 3 | 2 tests |
| 9 | a missing probe case exited 1 and wrote no evidence | traceback, rc 1 | UNMEASURABLE, evidence written | 2 tests |
| 10 | `report` exited 0 on a FAIL run | rc 0 | FAIL 1, INVALID 3, UNMEASURABLE 2, PASS 0 | 6 tests |
| 11 | `--require-full` denominator too narrow | bare numeric files | suffixed ids counted, directories skipped | 2 tests |
| 12 | the timeout kill was a bare group signal | `killpg(pid, 9)`, 0 guards | no group signal anywhere, four ownership checks, single pid | 12 tests |
| 13 | `preflight.jsonl` append-only under a fixed name | one path for every run | per-run `preflight-<runid>.jsonl` | 2 tests |
| NEW 1 | the closure regex misses the form the suite uses | `. common/config` matched nothing | both spellings read, pin regenerated, old-shape pin refused | 4 tests plus the Pi matrix |
| NEW 2 | a forged pass banner is accepted | `PASSED` from a case's own stdout | 13 full-CLI negatives, receipt binds executor, tree, ids, count and selection | 12 tests |
| NEW 3 | an undeclared second workstream | `compare.py` diffed against the pre-merge base | blobs identical to integrated main | ancestry and blob comparison, below |
| NEW 4 | three false or overstated document statements | denominator, probe record, capability count | corrected above, each with the number it should have had | 3 tests |
| NEW 5 | four smaller items | dirty list truncated, IO asserted, pin re-read, appended classify log | all four fixed | 3 tests |

The adversarial shapes finding 2 and finding 3 asked for are all present and all negative: empty log, whitespace log, a no-op case with rc 0, no I/O in the case's own directory, a pass reported for a case that was not the requested one, a log that mimics a pass banner, one arm producing no assertion, a runner that exits 0 saying nothing, a runner that prints a success line and exits nonzero, `check`'s own `Not run:` grammar, `check` reporting the case as ignored, the suite's own `_fatal` line, a direct invocation with no witness, a forged banner from inside the case, a silent runner, an empty runner, a count larger than the ids run, a runner naming another case, a replaced runner, a missing `group.list`, a `group.list` that omits the case, and a stand-in tree under the shipped pin.

Control files, all runnable:

| file | what it proves | result |
|---|---|---|
| `final-repair/behaviour_controls.py` | old-fail and new-pass on one fixture and one set of arguments | 11/11 |
| `final-repair/evidence_controls.py` | one row per finding, each a named runnable test | 20/20, 86 tests |
| `repair/arm_placement_control.py real` | arms placed and witnessed on two real filesystems | 22/22 on the Pi |
| `repair/arm_placement_control.py ident` | the same code path where one tmpfs cannot host two arms | 18/18 on the Mac, labelled harness proof |

### The comparator, checked first

The review raised this as an undeclared second workstream, so it was checked by ancestry before anything else.

- `git merge-base --is-ancestor 724f81c HEAD` holds: integrated main is an ancestor of this head
- `bench/compare.py` is blob-identical between `724f81c` and this head, sha `67e2d25d36f63dee43beeb43c738cd782d57a265`
- `bench/test_compare_coverage.py` is blob-identical between `724f81c` and this head, sha `1086fdceec91913a77062a8920d134ba1350e3bf`

The diff against `6a8075a` shows that work because `6a8075a` is the pre-merge base, and this branch merged main at `a15abf0`, whose parents are `6a8075a` and `724f81c`.
So the comparator is unchanged relative to integrated main, this branch incorporates the already merged PR #89, and nothing from PR #89 was authored or edited here.
No branch split, no revert and no new issue #80 work was needed, because there is no ownership question to settle.

### Synthetic fixtures versus acceptance

Every control above uses a synthetic tree in a temp directory.
The synthetic `check` stands in for the suite's runner and reproduces its grammar, so the verdict parser is tested against the format rather than against an assumption about it.
None of that is xfstests acceptance evidence, and the gate now refuses to call it that: a receipt from a tree the shipped pin does not name is labelled harness proof and its case is not a PASS.

The only real measurements in this document are: the preflight against the pinned suite on the Pi, the closure and pin matrix read against that same pinned tree, and the arm placement control against the two real filesystems, which uses a synthetic case only because no real xfstests case can execute.

## Safety classification of all 802 cases

The classifier is a hypothesis about safety, not proof, and it is what refuses the other 796.
It has been wrong four times, each time caught by reading sources: it missed `$here/src/...` because the suite reaches helpers through `$here`, it missed `_test_cycle_mount` because `\bmount\b` does not match inside an underscore-joined name, it missed `_user_do` and `mknod` because a case can need a second user without saying `sudo`, and it missed `026`, which reaches `getfacl` and `setfacl` as bare words.
Each is now a unit test.

Counts from the final scan, 802 records and 802 distinct ids:

| verdict | count |
|---|---|
| NEEDS_SCRATCH | 511 |
| NEEDS_HELPER | 88 |
| NEEDS_DEVICE | 93 |
| UNREAD_SOURCE | 40 |
| UNSAFE | 26 |
| NEEDS_ROOT | 26 |
| NEEDS_LONG | 11 |
| SAFE | **6** |
| NEEDS_BIG_SPACE | 1 |

Per-case records with the line number and the reason: `bench/out/ready-g5/linux/classify-final.jsonl`, ignored and local.

The `classify.jsonl` on the Pi that an earlier revision cited holds 3208 rows for the same 802 distinct ids, which is four accumulated runs of the old append-mode writer, not 3208 distinct cases.
The 802 is corroborated by counting distinct ids; the append defect itself is fixed, since the writer now truncates.

The six, and what each asserts:

| id | assertion | note |
|---|---|---|
| 005 | `touch` through a 25-deep symlink chain returns ELOOP | relative `rm` only |
| 236 | a hard link bumps the target inode ctime | sleeps 1s |
| 245 | `mv` of a directory onto an existing name | renames inside `$TEST_DIR` |
| 309 | `mv` into a directory bumps that directory's mtime and ctime | real `status` increments, so a pass is a real assertion |
| 360 | `readlink` of a 1019-byte target, md5 of the result | needs perl, present |
| 755 | unlinking a hard link bumps the target inode ctime | weak: it echoes on a mismatch and never raises `status` |

Known limits of this set, stated rather than hidden:

- 6 of 802 is 0.7 per cent of the group. Even with the prerequisite block lifted this cannot carry the gate.
- The allowlist, not the classifier, is the authority on what may run. The classifier only refuses, and a case it newly calls SAFE is not run until it is read and pinned.
- A static scan cannot see a helper a case reaches indirectly at run time. The per-case log check is what catches that, and it makes such a case INVALID rather than passing it.
- 755 cannot fail by its own logic, so its pass would be weaker evidence than the other five. It is listed rather than hidden.

## Declined scope

No substitute conformance suite was built, and the harness's own synthetic tests are not presented as xfstests results.
The arm write-read-unlink probe and the arm placement control are arm-preparation evidence, not filesystem conformance results.
No xfstests assertion ran on cowfs, so there is nothing here that says cowfs is or is not xfstests conformant.

## Evidence, all ignored and local

| file | what |
|---|---|
| `bench/out/ready-g5/linux/preflight-final.jsonl` | host, PATH, git provenance, pin verdict, the capability record with each entry's `how`, all eleven gate entries, the probe record and its reason, per-case source hashes |
| `bench/out/ready-g5/linux/classify-final.jsonl` | 802 records, one per case, with verdict, line number and reason |
| `bench/out/ready-g5/linux/final-run.log` | the matched run's real output and exit code, six reviewed ids requested |
| `bench/out/ready-g5/final-repair/linux/closure-final-repair.json` | the closure of each case, measured on the reviewed tree, per case and in union |
| `bench/out/ready-g5/final-repair/linux/verify-pin-final-repair.json` | the regenerated pin verified against that tree: 802 records, 6 SAFE, 0 problems |
| `bench/out/ready-g5/final-repair/linux/pin-matrix-final-repair.json` | old gate with old pin, new gate with old pin, new gate with new pin |
| `bench/out/ready-g5/final-repair/behaviour-controls-*.jsonl` | 11 old-fail new-pass controls, with the revision each was measured against |
| `bench/out/ready-g5/final-repair/evidence-controls-*.jsonl` | 20 findings, each with the tests that ran and their exit codes |
| `bench/out/ready-g5/final-repair/behaviour_controls.py` | the behaviour controls |
| `bench/out/ready-g5/final-repair/evidence_controls.py` | the finding-to-test mapping |
| `bench/out/ready-g5/repair/arm_placement_control.py` | the arm placement control, both modes |
| `bench/out/ready-g5/repair/behaviour_controls.py` | the previous round's old-fail and new-pass controls |
| `bench/out/ready-g5/repair/evidence_controls.py` | the previous round's per-finding controls |

Raw artifacts stay on the Pi under `task-g5/out/`.

## Tests and lint, measured

| suite | count |
|---|---|
| `bench/test_xfstests_gate.py` | 110, of which 1 skips on a host with no `/proc` |
| `bench/test_compare_coverage.py` | 16, inherited from main |
| `bench/test_gates.py` | 36, inherited from main |
| discovery over `bench` | 162 |

`ruff check --isolated --select F,E9` on `bench/xfstests_gate.py` and `bench/test_xfstests_gate.py`: clean.

For comparison, `bench/test_xfstests_gate.py` had 37 test methods at `6a8075a` and 78 at `b005753`.

## Next safe acceptance conditions

In order, each one sufficient to move the gate forward, none of them assumed here, and none of them a change to the harness.

1. **Authority to install build packages, or a container that has them.** The suite's own build produces `include/builddefs`, `include/config.h`, `tests/generic/group.list` and `ltp/fsstress`, `ltp/fsx`. On this host that means `autoconf`, `automake`, `libtool` and `m4`, all measured absent. A container with the distro's xfstests build dependencies is the cleaner route and leaves the host untouched. Disk space is not the constraint and authorises nothing.
2. **Keep `/usr/sbin` on `PATH` for the run.** Eight of the eleven gate entries are installed and hidden from a user shell otherwise. The harness records the `PATH` it used in every run's meta record.
3. **Re-run the harness, not a hand-written invocation.** `xfstests_gate.py run --xfstests DIR --native-root DIR --cowfs-root DIR`, with the native root a private ext4 directory and the cowfs root a writable snapshot inside the private mount. The arms must be on different devices, which the private ext4 directory and the FUSE mount already are.
4. **Widen the reviewed set deliberately**, one read-through per batch, because the 6 that survive now are the residue after four classifier passes. `UNREAD_SOURCE` 40 and `NEEDS_HELPER` 88 are the largest honest growth, and each needs its sources read before it enters the pin.
5. **A capability decision for the 511 scratch-device cases.** A loop-backed scratch device needs root; a container with `CAP_SYS_ADMIN` and its own device namespace would provide it without touching the host. Until then those cases stay refused and the coverage line stays honest about it.
6. **Only claim g5 when `--require-full` passes.** That flag exits 2 unless every case in the pinned set ran, and the pinned `case_count` is what it compares against.

## Dependencies on other lanes

No cowfs source was touched, so there is nothing for #45, #42, #19 or #43.
This gate does not read `bench/compare.py`, and `bench/compare.py` and `bench/test_compare_coverage.py` are unchanged relative to integrated main, as the blob comparison above shows.
Main is merged in at `724f81c` (PR #89) through merge commit `a15abf0`, and the discovery total moved from 89 to 162 because of that merge plus the 41 tests this lane added across two revisions.