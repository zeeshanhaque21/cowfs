# Ready g5: xfstests-generic, matched native and cowfs

Task: isolated Linux xfstests acceptance for gate g5 (`progress/plan.json`, "xfstests-generic no worse than native").
Lease: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/11/cowfs`, branch `verify/xfstests-g5`.
Everything on the Pi stayed under `/home/moonscape/cowfs-ready-wave/task-g5/`.

## Verdict

**UNMEASURABLE, prerequisite block. Zero measured assertions. g5 stays OPEN.**

No generic case can run on this host. The reason is the suite's own startup gate plus two build artifacts it needs, not a harness decision and not a cowfs defect.
The harness now answers the question with evidence rather than inference, and refuses on every path that previously produced a false pass.

Nothing in this change says cowfs is or is not xfstests conformant, because no xfstests assertion ran on either arm.

## What ran, with real exit codes

Every number is a real process exit code, captured from the child itself and never from a pipeline.

| step | command | rc | result |
|---|---|---|---|
| suite build | `make` at the xfstests root | not attempted | `include/builddefs` absent, no `configure`, no autoconf |
| preflight | `xfstests_gate.py preflight --xfstests ref/xfstests` | 2 | UNMEASURABLE, three blocking entries, evidence written |
| preflight probe | the suite's own runner on `generic/010`, run by the harness | 1 | `fsstress not found or executable`, outcome SKIPPED |
| matched run, six reviewed ids | `xfstests_gate.py run --cases 005,236,245,309,360,755` | 2 | UNMEASURABLE at preflight, no case executed |
| arm placement control, real arms | `arm_placement_control.py real` on the Pi | 0 | 22/22 checks, ext4 vs `fuse.cowfs` |
| harness unit suite | `python3 -m unittest discover -s bench` | 0 | 130 tests |
| lint | `ruff check --isolated --select F,E9` on four files | 0 | clean |
| evidence controls | `evidence_controls.py` | 0 | 32/32 |
| behaviour controls, new gate | `behaviour_controls.py` | 0 | 14/14 |
| behaviour controls, old gate at 6a8075a | `behaviour_controls.py` | 1 | 3/13 |

The last row is the regression evidence: the same controls against the previous commit fail 10 of 13, and the detail for each failure is in the table under "What the review found".

## Provenance

| thing | value |
|---|---|
| xfstests remote | `https://git.kernel.org/pub/scm/fs/xfs/xfstests-dev.git` |
| pinned tree sha | `3e1ee800e52a0f53d7d9a7809be1ffc80ec1788f`, committed 2026-08-28T18:11:59+08:00 |
| clone | `git clone --depth 1`, into `task-g5/ref/xfstests`, never into the repo |
| tree state at preflight | clean; `git status --porcelain -uall` empty |
| reviewed set | 6 ids, `case_count 6`, allowlist sha of the id list `5eab2e8a473b26613e279266bfb1812426e2a8bfbd256aef2eae9920cb2c6671` |
| host | `Linux moonscapenas 6.12.109+rpt-rpi-2712`, aarch64, Debian, 4 cores, 16 GiB |
| cowfs binary | `cargo build -p cowfs-cli` from this lease, dev profile, at base `46b0f26` |
| cowfs daemon | pid 899604, start 2026-10-04 16:15:54, store `task-g5/store`, mount `task-g5/mnt`, socket `task-g5/run/control.sock`, re-verified by pid, argv, start time, socket and mount table before every use |
| native arm | `task-g5/native`, ext4 on `/dev/sda2`, device 2050 |
| cowfs arm | `task-g5/mnt/wt`, a writable imported snapshot inside the private mount, `fuse.cowfs`, device 143 |

## The prerequisite gate, item by item

`common/config` checks these in order and exits on the first miss.
A non-root `PATH` has no `/usr/sbin`, so `mkfs` and `xfs_io` are installed and merely invisible.
With `/usr/sbin:/sbin` on `PATH`, eight of the ten original entries pass. Three entries block.

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

The third entry is new in this revision and is a real finding, not a restatement: `check` resolves a testlist entry by grepping it against the group's `group.list`.
Absent that file, `check` runs zero cases and still prints a summary, which is precisely the shape of a false pass. The gate now treats its absence as blocking and detects the `unknown test, ignored` line as a non-pass.

Measured capability probes on the Pi, all four absent: `autoconf`, `automake`, `libtool`, `m4`.
`getfattr` and `setfattr` are absent too, so the attr cases are unrunnable regardless.
These are probe results in the evidence file, not literals in the code, and they are what makes the earlier claim in the previous revision of this document checkable: on this host `libtool` and `m4` are genuinely absent, while on the Mac they are present, and a fixed string could not have told the two apart.

## Bypasses considered and declined

Three routes around the missing prerequisites were available. All three were declined, because each manufactures a result rather than measuring one.

1. Put any executable at `ltp/fsstress` and `ltp/fsx`. `common/config` only tests `-x`. Every case that then calls `$FSSTRESS_PROG` or `$FSX_PROG` compares a no-op against its expectations and exits 0.
2. Hand-write `include/config.h` and `include/builddefs` so `ltp/*.c` compiles. An empty `config.h` claims the host has no optional features, which is false, and the resulting helpers would differ from the suite's own build in ways no local check could confirm.
3. Hand-write `tests/generic/group.list` so `check` can resolve a testlist entry. That is a build artifact; writing it by hand makes the runner resolve a case it was never given a list for.

The first is not hypothetical as a hazard. An early probe of `generic/002` in the previous revision exited 0 while printing `unary operator expected` thirteen times, having asserted nothing.
That is why the gate now reads the suite's own verdict grammar and why a direct invocation can never be a pass.

## The harness

`bench/xfstests_gate.py`, with `bench/xfstests-allowlist.txt` as machine-readable pin data and `bench/test_xfstests_gate.py` as its test suite.
Four subcommands: `preflight`, `classify`, `run`, `report`.
Exit codes match `bench/compare.py`: 0 PASS, 1 FAIL, 2 UNMEASURABLE, 3 INVALID, and INVALID and UNMEASURABLE are now produced by different causes so a CI step can tell a moved tree from an unbuilt one.

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

The child-reported device numbers differ between the arms, which is the property the previous revision could not demonstrate.

### The suite answers, not the harness

`preflight` checks each `common/config` entry by name, then runs a real case through the suite's own runner and treats the result as the answer.
A missing prerequisite prints UNMEASURABLE and exits 2. A source-pin failure prints INVALID and exits 3. There is no code path from either to PASS.

### Success needs positive evidence

There are two runners, and the difference decides whether a pass is admissible at all.

`check` is the suite's own runner and the default. Its verdict grammar is the only one xfstests supports: it names each case in a `Ran:` line, prints `Passed all N tests` or `Failed M of N tests`, and exits 0 or 1 to match.
A PASS requires that witness, the exit code to agree with it, no `Not run:` entry, and nothing on the `unknown test, ignored` line.

`direct` invokes the case script without the suite's runner. It exists for diagnosis and can never produce a PASS: there is no supported success witness, so its ceiling is UNMEASURABLE.
Recording it as a pass is the failure this gate was written after.

Beyond the runner, the child reports on itself: the directory it was given, its device and fstype, its exit code, whether it did real I/O there, what it left behind, and the log size it measured.
A case is INVALID, never PASS, when the observer block is missing or incomplete, when it names a case other than the one requested, when the log carries a skip signature, when it did no I/O in its own directory, when the log lost bytes the observer measured, or when the suite's own `_fatal` line is in the log.

Finally, two arms that exited 0 over visibly different work are not a pass: the residue each left behind is compared, and a difference makes the case UNMEASURABLE.

### Source is pinned, not named

The allowlist carries the tree sha, the sha256 of every reviewed case, and the sha256 of every `common/*` file those cases execute.
The transitive closure is read from the sources rather than trusted by filename: `common/preamble` and `common/filter` come from the case, `common/rc` from `common/preamble`, `common/report` from `common/rc`.

Every attempt re-reads the tree and refuses before the first case subprocess when the tree sha moved, a reviewed case or a transitive file changed, the worktree is dirty outside the pinned set, the reviewed set drifted from the classifier, or a case pulls in a file the pin does not name.
Build output under `src/`, `ltp/`, `lib/` and `include/` is tolerated by extension, because building the suite leaves object files there; anything else modified is a refusal, and there is no blanket exemption.
A per-case source hash is recorded next to every executed case.

### Only what was measured is recorded

Capability notes are probe results, never literals, and the probe that produced each one is named in the record.
`report` returns 1 on FAIL, 3 on INVALID and 2 on UNMEASURABLE, so a CI step wired to it cannot go green on a failing run.
`classify` returns 3 on drift, matching what this document and the PR body claim.
`--require-full` compares against the pinned `case_count`, and the denominator includes suffixed entries such as `069_o_tmpfile` and `307_recovery` while excluding `005.cfg`.

## What the review found, and what each finding became

Every row is a control in `bench/out/ready-g5/repair/`. The "old" column is the same control run against `6a8075a`; it is measured, not asserted.

| # | finding | old | new | control |
|---|---|---|---|---|
| 1 | arms never ran on the given roots; the gate said PASS | FAIL: no per-arm test directory in the record | 14/14 | `b_case_runs_in_arm_root`, `b_logs_stay_outside_the_arm`, `b_identical_arms_refused` |
| 2 | rc 0 with an empty or assertion-free log scored PASS | FAIL: `gate rc=0 verdict=PASS` | 32/32 | `c_empty_log_cannot_pass` and eight more shapes |
| 3 | a native failure became a counted pass; cowfs skips unread | FAIL: `verdict=PASS` | 14/14 | `b_native_failure_not_a_pass`, `b_cowfs_skip_never_ignored`, `c_residue_difference` |
| 4 | nothing bound executed source to the allowlist | FAIL: no per-case hash recorded | 32/32 | `c_source_pin_controls`, `c_tree_sha_and_drift` |
| 5 | transitive `common/*` trusted by filename | FAIL: no closure read | 32/32 | `c_closure_is_read`, `c_pull_in_unpinned_source` |
| 6 | `capabilities_absent` was a literal, false on this Mac | FAIL: hardcoded, unprobed | 32/32 | `c_capabilities_probed` |
| 7 | `classify` exited 0 on drift | FAIL: `classify rc=0` | 14/14 | `b_classify_drift_exit` |
| 8 | a missing probe case exited 1 and wrote no evidence | FAIL: `FileNotFoundError` traceback | 14/14 | `b_missing_probe_no_traceback` |
| 9 | `report` exited 0 on a FAIL run | FAIL: `FAIL->0 wanted 1` | 14/14 | `b_report_exit_codes` |
| 10 | `--require-full` counted only bare-numeric files | FAIL: `has_suffixed=False` | 14/14 | `b_denominator` |
| 11 | `ruff --select F,E9` failed on an unused import | FAIL: `F401` | 0 | `ruff check --isolated --select F,E9` |
| 12 | the timeout kill was a bare process-group signal | FAIL: `no_literal_-9=False race_guarded=False` | 14/14 | `b_timeout_keeps_real_status` |
| 13 | `preflight.jsonl` was append-only under a fixed name | fixed: per-run `preflight-<runid>.jsonl` | | visible in the evidence paths |

Control files, all runnable:

| file | what it proves | result |
|---|---|---|
| `arm_placement_control.py real` | arms placed and witnessed on two real filesystems | 22/22 on the Pi, 0 |
| `arm_placement_control.py ident` | the same code path where one tmpfs cannot host two arms | 18/18 on the Mac, labelled harness proof |
| `behaviour_controls.py` | old-fail and new-pass using only the pre-existing API | 3/13 old, 14/14 new |
| `evidence_controls.py` | one control per finding, including the adversarial shapes | 32/32 |

The adversarial shapes finding 2 asked for are all present and all negative: empty log, whitespace log, no-op case with rc 0, no I/O in the case's own directory, a pass reported for a case that was not the requested one, a log that mimics a pass banner, one arm producing no assertion, a runner that exits 0 saying nothing, a runner that prints a success line and exits nonzero, `check`'s own `Not run:` grammar, `check` reporting the case as ignored, the suite's own `_fatal` line, and a direct invocation with no witness.

### Synthetic fixtures versus acceptance

Every control above uses a synthetic tree in a temp directory.
The synthetic `check` stands in for the suite's runner and reproduces its grammar, so the verdict parser is tested against the format rather than against my assumption of it.
None of that is xfstests acceptance evidence, and none of it is presented as such.

The only real measurements in this document are: the preflight against the pinned suite on the Pi, and the arm placement control against the two real filesystems, which uses a synthetic case only because no real xfstests case can execute.

## Safety classification of all 802 cases

The classifier is a hypothesis about safety, not proof, and it is what refuses the other 796. It has been wrong three times, each time caught by reading sources: it missed `$here/src/...` because the suite reaches helpers through `$here`, it missed `_test_cycle_mount` because `\bmount\b` does not match inside an underscore-joined name, and it missed `_user_do` and `mknod` because a case can need a second user without saying `sudo`. A fourth pass caught `026`, which reaches `getfacl` and `setfacl` as bare words. Each is now a unit test.

| verdict | count |
|---|---|
| NEEDS_SCRATCH | 511 |
| NEEDS_HELPER | 108 |
| NEEDS_DEVICE | 72 |
| UNREAD_SOURCE | 41 |
| UNSAFE | 26 |
| NEEDS_ROOT | 26 |
| NEEDS_LONG | 11 |
| SAFE | **6** |
| NEEDS_BIG_SPACE | 1 |

Per-case records with the line number and the reason: `bench/out/ready-g5/linux/classify-final.jsonl`, ignored and local.

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
- The allowlist, not the classifier, is the authority on what may run. The classifier only refuses.
- A static scan cannot see a helper a case reaches indirectly at run time. The per-case log check is what catches that, and it makes such a case INVALID rather than passing it.
- 755 cannot fail by its own logic, so its pass would be weaker evidence than the other five. It is listed rather than hidden.

## Declined scope

No substitute conformance suite was built, and the harness's own synthetic tests are not presented as xfstests results.
The arm write-read-unlink probe and the arm placement control are arm-preparation evidence, not filesystem conformance results.
No xfstests assertion ran on cowfs, so there is nothing here that says cowfs is or is not xfstests conformant.

## Evidence, all ignored and local

| file | what |
|---|---|
| `bench/out/ready-g5/linux/preflight-final.jsonl` | host, PATH, git provenance, pin verdict, 23 probed capabilities, all eleven gate entries, the probe record and its reason, per-case source hashes |
| `bench/out/ready-g5/linux/classify-final.jsonl` | 802 records, one per case, with verdict, line number and reason |
| `bench/out/ready-g5/linux/final-run.log` | the matched run's real output and exit code, six reviewed ids requested |
| `bench/out/ready-g5/repair/behaviour-controls-old.json` | 10 of 13 controls failing against `6a8075a`, with the reason for each |
| `bench/out/ready-g5/repair/behaviour-controls-new.json` | 14/14 against the new gate |
| `bench/out/ready-g5/repair/evidence-controls-new.json` | 32/32, one control per finding |
| `bench/out/ready-g5/repair/new-pass-arm-placement-real.json` | the Pi control, two real filesystems, 22/22 |
| `bench/out/ready-g5/repair/new-pass-arm-placement-ident.json` | the Mac control, labelled harness proof, 18/18 |
| `bench/out/ready-g5/repair/arm_placement_control.py` | the arm placement control, both modes |
| `bench/out/ready-g5/repair/behaviour_controls.py` | old-fail and new-pass controls |
| `bench/out/ready-g5/repair/evidence_controls.py` | per-finding controls including the adversarial shapes |

Raw artifacts stay on the Pi under `task-g5/out/`.

## Next safe acceptance conditions

In order, each one sufficient to move the gate forward, none of them assumed here, and none of them a change to the harness.

1. **Authority to install build packages, or a container that has them.** The suite's own build produces `include/builddefs`, `include/config.h`, `tests/generic/group.list` and `ltp/fsstress`, `ltp/fsx`. On this host that means `autoconf`, `automake`, `libtool` and `m4`, all measured absent. A container with the distro's xfstests build dependencies is the cleaner route and leaves the host untouched. Disk space is not the constraint and authorises nothing.
2. **Keep `/usr/sbin` on `PATH` for the run.** Eight of the eleven gate entries are installed and hidden from a user shell otherwise. The harness records the `PATH` it used in every run's meta record.
3. **Re-run the harness, not a hand-written invocation.** `xfstests_gate.py run --xfstests DIR --native-root DIR --cowfs-root DIR`, with the native root a private ext4 directory and the cowfs root a writable snapshot inside the private mount. The arms must be on different devices, which the private ext4 directory and the FUSE mount already are.
4. **Widen the reviewed set deliberately**, one read-through per batch, because the 6 that survive now are the residue after four classifier passes. `UNREAD_SOURCE` 41 and `NEEDS_HELPER` 108 are the largest honest growth, and each needs its sources read before it enters the pin.
5. **A capability decision for the 511 scratch-device cases.** A loop-backed scratch device needs root; a container with `CAP_SYS_ADMIN` and its own device namespace would provide it without touching the host. Until then those cases stay refused and the coverage line stays honest about it.
6. **Only claim g5 when `--require-full` passes.** That flag exits 2 unless every case in the pinned set ran, and the pinned `case_count` is what it compares against.

## Dependencies on other lanes

No cowfs source was touched, so there is nothing for #45, #42, #19 or #43.
`bench/compare.py` was not modified and this gate does not read it.
Main was merged in at `724f81c` (PR #89), which added `bench/test_compare_coverage.py` and changed `bench/test_gates.py`; the discovery total moved from 89 to 130 tests because of that merge and the 41 tests added here. No file from PR #89 was edited.