# fsx gate g4: controls, and what each review finding was

What each finding is, what it was worth before, and what proves it is closed.

Two reviews. `docs/reviews/mounted-fsx-g4-final.md` ran against
`f816b5e96624967f16a28444a0631ddc9672892b` and found F1 to F7.
`docs/reviews/mounted-fsx-g4-repair-final.md` ran against
`ff45b0a3c6ab1e6fcfd94bf0fdf733812a1fc38c`, confirmed all seven closed on its own private mount, and
found R1 to R9, which are reporting and documentation defects.

The review columns below are the reviews' measurements, on their own mounts.
The proof column is what this branch can show for itself: a named unit test, or a control this lane
ran against its own private mount on 2026-10-04.

| fault | what the review found | at this head | proof at this head |
| --- | --- | --- | --- |
| F1 | a tmpfs labelled `cowfs` passed in the `full` mode, because nothing asserted the arm's filesystem type | refused before any case, from the kernel's mount table | controls 5d and 5f; `ArmAttestation` (3) |
| F2 | one explained hole gap excused every other difference, including 900 fewer reads and different bytes | the difference is attributed at the first operation the two recorded streams disagree on | `CompareCase` (20), `FirstDivergence` (7), `CapabilityEvidence` (4); the measured full-mode run locates the divergence at operations 1, 6 and 10 |
| F3 | `--restart-cmd /bin/true` produced a clean restart row | the leg compares the daemon generation before and after | control 17; `RestartLeg` (6) |
| F4 | the case directory was reused, so stale bytes passed as this run's work | every attempt is a fresh directory, nothing is deleted to make room | control 19; `ImmutableAttemptDir` (3) |
| F5 | the binary digest was recorded and compared to nothing | `fsx-gate.json` carries an approved manifest and a mismatch is refused before anything runs | control 16; `ToolPin` (4) |
| F6 | a byte budget nothing compared against | the cap is checked against the plan before a child exists, and an unexpected exceedance is reported per arm | `PlannedBudget` (10), `ByteCaps` (3) |
| F7 | the tests were never discovered by CI | `bench/test_fsx_gate.py` loads the module by path from the command CI runs; the workflow file is untouched | 111 distinct gate test names in the CI log at run 37266237441 |

| residual | what the review found | at this head | proof at this head |
| --- | --- | --- | --- |
| R1 | the evidence pointer named the earlier FAIL run while the document quoted the UNMEASURABLE run's figures | each run's evidence is in a directory named for that run, and the earlier FAIL run is kept with a note saying what it is and why it differed | `tabulate.py` re-derives the tables from each run's own `cases.jsonl` |
| R2 | a per-case seconds path that does not exist, and 103 quoted where CI reported 237 | the seconds are in the reported run's own stdout, named correctly; 111, 147 and 391 are stated separately, with what each one counts and which run reported it | `DirectCommandLine`, `Tabulate` (4) |
| R3 | the two documents disagreed on how many controls ran | five, named: 5d, 5f, 16, 19, 17 | `mutations.py`, five controls, 0 leaks |
| R4 | `FirstDivergence` has 7 tests, not 6 | every class count in this document is read from the suite | counted from the loaded suite |
| R5 | the over-budget failure hard-coded the cowfs arm | every arm that breached is reported, with its bytes, the budget, the overage and the per-case maximum | three measured over-budget runs: native only, cowfs only, both |
| R6 | `planned_files` multiplied the declared seeds by the requested ones, and a comment claimed a refusal the code did not do | the plan is derived once from what was asked for, the accounting is named, a plan the per-arm total cannot hold is refused before any child, and the comment matches | `PlannedBudget` (10), `DirectCommandLine.test_a_cap_the_plan_cannot_hold_is_refused_before_any_child_exists` |
| R7 | the device-keyed lookup returned the first entry for a device, not the containing one | the path picks the longest containing entry among those on the device; several mounts and no containing entry is AMBIGUOUS with candidates listed and no type stated; an unreadable table is an absence, not an ambiguity | `DeviceDisambiguation` (9) |
| R8 | the subject under test had no digest or revision in the document | daemon and CLI digests, the pid and start time, the source tree digest, the compiler and the host, read from the running process and the files beside it, with the two limits stated | measured on the host |
| R9 | two exit codes meant different things in two harnesses, and unsupported could not be told from invalid | the repository-wide 0 PASS, 1 FAIL, 2 UNMEASURABLE, 3 INVALID, with the code decided by the kind of the reason rather than by parsing a message | `ExitContract` (10) and eleven measured process exits |

Two controls the review ran that this lane did not re-run: the historical false pass reproduced by
removing the `st_dev` guard from a copy of the runner, and the controls that already failed before the
repair and still do.
At this head the same false pass is unreachable through two independent guards, which is why control
5f refuses twice over; that is the reason, not a re-measurement of the review's control 3.

## The controls this lane ran

`bench/fsx-gate/mutations.py`, against this lane's own private mount on moonscape: own store, own
socket, own mount, own pid file, under `/home/moonscape/cowfs-ready-wave/task-g4/`.
Report: `bench/out/ready-g4/repair-batch2/mutations.json` and `mutations.log`, gitignored with the
rest of the raw output.

| control | what it feeds in | exit | result |
| --- | --- | --- | --- |
| 5d | the cowfs arm pointed at a directory that is not a cowfs mount | 3 | INVALID: the kernel reports ext4 there |
| 5f | the native arm pointed at the cowfs mount | 3 | INVALID: refused twice, the control is on `fuse.cowfs` and both arms resolve to device 171 |
| 16 | a synthetic child that prints the A-OK line and writes a five-op-type stream | 3 | INVALID: the binary is not the manifest's |
| 19 | a child that writes nothing, over a stale case directory holding real bytes | 3 | INVALID: the fresh attempt's result is empty, and the stale directory is neither reused nor deleted |
| 17 | `--restart-cmd /bin/true` | 3 | INVALID: pid and start time unchanged, so nothing replaced the daemon |

Five controls, zero leaks.
A control that came back PASS would be a defect in the gate, not a control that failed.

Every one of these is INVALID rather than FAIL, which is the taxonomy doing its job: each is a fact
about the harness, the tool pin or the invocation, not about the filesystem under test.

## What each fault was

**F1, arm identity.** The old preflight recorded `fstype` for both roots and then only ever checked
the native arm for the string `fuse`.
Nothing asserted the cowfs arm was a cowfs mount, so a tmpfs passed in the `full` mode.
A different `st_dev` was never proof on its own either.
Now `bench/fsx-gate/mount-manifest.py` reads the kernel's mount table, resolves the longest matching
prefix, keys the filesystem type on the device rather than the path, and prefers the table's type over
`statfs`.
The runner refuses before it runs a case, and each data file carries its own witness: realpath, device,
and the filesystem the kernel reports for that device.

Two bugs of this lane's own were found by running it, not by reading it:
`/proc/PID/cmdline` was read twice, so every daemon option parsed as `None`, and `f_fstypename` was
read from offset 8 of `struct statfs`, which is empty on this kernel for every filesystem.
Both produced a refusal with a wrong reason and are fixed.

**F2, attribution.** The old rule excused every difference once any one capability gap existed.
Counting totals was not enough either: the first repair attributed each count difference on its own,
and the full mode's byte-bearing deltas came out unexplained, which reported FAIL and reads as a
`cowfs` defect it cannot prove.
The attribution is now mechanical, from the tool's own record, and the measured full-mode run reports
the divergence at operations 1, 6 and 10, all of them `fallocate` against `skip fallocate`.

`OP_CAPABILITY` covers the whole fallocate family: `punch_hole`, `zero_range`, `write_zeroes`,
`fallocate`, `collapse_range`, `insert_range`, `exchange_range`, `dedupe_range`.
The last two were missing, and fsx's default mix attempts both, so their skips were invisible.
A gap needs evidence the filesystem wrote: a recorded `skip <op>`, or the runner's own probe, either
one on its own, and one arm's probe never speaks for the other.

**F3, what "restart" means.** `--restart-cmd /bin/true` returned 0 and the runner reported one file
rehashed with zero problems, because a command's exit status is not evidence of anything.
The leg now reads the daemon generation from the pid file and `/proc` before and after, and requires
pid and start time to change while the store, socket, mount and binary digest hold.
The measured run went from pid 1191050 to pid 1209860 and rehashed 15 files with no problems.

**F4, an attempt is an attempt.** The case directory was created with `exist_ok=True` and written
over, so a child that claims an op count and writes nothing left the previous run's bytes to be
hashed.
Every attempt is now a fresh directory created with `mkdir`, which is `O_EXCL` in effect, and nothing
is deleted to make room: a failed or interrupted attempt is evidence.
A child that cannot be spawned at all is a failed case with a named reason rather than a traceback.

**F5, the tool is pinned.** The old runner recorded the binary's digest and compared it to nothing, so
any executable that exited 0 was accepted as an arm.
The approved manifest now names the expected binary digest, the three source digests, the compile
line, the config header digest and the compiler, and a mismatch is refused before anything runs.

**F6 and R5, the numbers.** The declared byte budget was 262144 per file while the measured total was
2915185, and nothing compared anything against anything.
The per-arm total is now 3932160, one file per case on that arm at the per-case maximum, with
`max_bytes_written_both_arms` declared separately at 7864320.
The measured run wrote 3018150 on cowfs and 2904582 on native, under either reading.
An over-budget run is reported per arm with its own bytes and overage, which is the one thing a budget
failure exists to say.

**F7, CI never saw the tests.** `python3 -m unittest discover -s bench` finds `test_*.py` directly
under `bench/`, and `bench/fsx-gate/` has a hyphen in its name, so the gate's own tests were never
discovered.
`bench/test_fsx_gate.py` loads the gate's module by path and adds no assertion of its own, so no
assertion is duplicated and the workflow file is not edited.
At run 37266237441 the CI log carries all 111 gate test names and 0 duplicates.

## Defects the new controls found

Two, both of which the repair introduced or exposed, and both of which would have made a measured
result meaningless rather than wrong.

**An unreadable result crashed the runner.** `sha256_file` returns `(None, the reason)` when it cannot
read a file, so the data size came back a string, comparing it against the per-case cap raised a
`TypeError`, and the run died with a traceback and exit 1.
That is the worst shape a defect can take in a gate: the exit code was right by accident and nothing
recorded why.
An unreadable result is now a missing result, reported as INVALID with the reason, and
`UnreadableResult` (4) holds it down.

**A test that passed on macOS for the wrong reason.**
`test_an_unreadable_table_is_an_absence_not_an_ambiguity` patched `read_mountinfo` on one module
instance and then asked a freshly loaded one for the answer, so it never exercised the patch.
macOS has no `/proc/self/mountinfo`, so the real reader fails there anyway and the test passed for the
wrong reason; on Linux CI the patch did not stick and the test errored.
The test now holds one module instance for its whole body, and it takes its device from the real mount
table rather than assuming one, because a fixture that assumes a device number tests the host rather
than the code.
All 111 gate tests pass on Linux as well as on macOS.

## The unit tests that pin all of this

`bench/fsx-gate/test_run_fsx_gate.py`, discovered by CI through `bench/test_fsx_gate.py`.
111 tests, one skipped where `/proc` is absent.

| class | tests | what it holds down |
| --- | --- | --- |
| `ArmAttestation` | 3 | a plain directory is not a cowfs mount; the longest mount prefix wins; no filesystem type required is not the same as a type matching nothing |
| `Attribution` | 3 | every delta attributed on its own; the family map complete; the byte-bearing ops named |
| `ByteCaps` | 3 | the per-arm total covers the declared batch; a data file over the declared `-l` is reported; the budget is a whole-invocation figure |
| `CapabilityEvidence` | 4 | a recorded skip or a probe is the evidence; neither present means no gap; one arm's probe does not speak for the other |
| `CompareCase` | 20 | controls 12, 13 and 14; a tmpfs arm; a missing witness; a matched-mode difference; a contaminated device; the pure capability case; the measured full-mode shape; a recorded stream too short to attribute; unreadable op streams; an integrity fault outranking a real divergence |
| `DeviceDisambiguation` | 9 | the containing entry wins; a longer prefix wins; a device with several mounts and no path is ambiguous; a path none contains is ambiguous; a single-mount device is answered; an unknown device is no entry; an ambiguous device's type is unknown; an unreadable table is an absence |
| `DirectCommandLine` | 6 | an undeclared mode is INVALID; an unpinned binary is INVALID; a non-cowfs arm is INVALID; a plan over the cap is refused with no child and no deletion; the refusal names both figures; a narrowed run is not refused for asking less |
| `ExitContract` | 10 | the four codes are the repository's; no local usage code survives; a reason is text and carries its kind; PASS 0, capability 2, divergence 1, integrity 3, no case at all 3, restart identity 3, restart bytes 1 |
| `FirstDivergence` | 7 | the index and both operations named; a skip of the same operation is a capability difference; a skip of an operation nobody lacks is not; an unrelated operation in the place of a gap is not; identical streams; only the first divergence matters; one stream ending early |
| `FsxIdentity` | 2 | the usage text and the digest are read from the binary itself |
| `ImmutableAttemptDir` | 3 | a fresh empty directory; two calls, two directories; stale content not reused |
| `MountIdentity` | 3 | the mount answer comes from the table, not from a name |
| `PlannedBudget` | 10 | the default plan is the declared batch; a narrowed run plans only what it runs and reports partial coverage; one mode and fewer seeds is its own arithmetic; a plan the cap cannot hold names both numbers; a fitting plan is not refused; a zero budget is not a free pass; every breaching arm is reported; both arms over are both reported; the config states its accounting |
| `Probe` | 3 | the probe records what it tried and what the errno was |
| `ReadbackComparesAgainstTheRecord` | 2 | the restart row carries both generations and the expectations it compared against |
| `ReadbackIsAFilesystemRead` | 2 | the digest is of the file, the empty file included |
| `RestartLeg` | 6 | an unchanged pid; an unchanged start time; a changed store; a lost mount attestation; the generation that passes; an unreadable pid file |
| `SeparateProcessReadback` | 1 | a missing file is an error, not a digest |
| `Summary` | 2 | the raw summary carries every count and the verdict |
| `Tabulate` | 4 | it reads the record rather than being told; the markdown names the directory it came from; a located divergence appears in the table; a missing record is refused |
| `ToolPin` | 4 | the manifest exists; a wrong binary is refused before anything runs; the override is marked a control |
| `UnreadableResult` | 4 | the digest helper returns a reason and no size; a readable file gives an integer; a missing data file is INVALID, not a crash; the cap comparison only ever meets a number |

## Reproducing

```sh
python3 -m unittest bench.test_fsx_gate -v
python3 -m unittest discover -s bench -v
```

The controls that need a real mount ran on this lane's own private daemon and mount.
The reviews' own controls ran on separate mounts of their own.
Neither touched another worker's daemon, store or mount.