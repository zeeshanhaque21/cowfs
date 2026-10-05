# fsx gate g4 repair: controls

What each review fault is, what it was worth before, and what proves it is closed at this head.

The review is `docs/reviews/mounted-fsx-g4-final.md`, run against `f816b5e96624967f16a28444a0631ddc9672892b`.
Its column below is what it recorded on its own private mount, and it is the review's measurement,
not this lane's.
The proof column is what this branch can show for itself: a named unit test, or a control this lane
ran against its own private mount on 2026-10-04.

| fault | the review's finding at `f816b5e` | at this head | proof at this head |
| --- | --- | --- | --- |
| F1 | a tmpfs labelled `cowfs` passed in the `full` mode, because nothing asserted the arm's filesystem type | refused before any case, from the kernel's mount table | control 5d exit 3, control 5f exit 3; `ArmAttestation`, `CompareCase.test_a_tmpfs_labelled_cowfs_is_a_failure` |
| F2 | one explained hole gap excused every other difference, including 900 fewer reads and different bytes | the difference is attributed at the first operation the two recorded streams disagree on | `CompareCase.test_control_12...`, `test_control_13...`, `test_control_14...`, `FirstDivergence` (6 tests); the measured full-mode run locates the divergence at operation 1, 6 and 10 |
| F3 | `--restart-cmd /bin/true` produced a clean restart row | the leg compares the daemon generation before and after | control 17 exit 1; `RestartLeg` (6 tests) |
| F4 | the case directory was reused, so stale bytes passed as this run's work | every attempt is a fresh directory, nothing is deleted to make room | control 19 exit 1; `ImmutableAttemptDir` (3 tests) |
| F5 | the binary digest was recorded and compared to nothing | `fsx-gate.json` carries an approved manifest and a mismatch is refused before anything runs | control 16 exit 3; `ToolPin` (4 tests) |
| F6 | a byte budget nothing compared against, wrong numbers in the document | the cap is enforced and the cap semantics are stated | `ByteCaps` (3 tests) |
| F7 | the tests in `bench/fsx-gate/` were never discovered by CI | `bench/test_fsx_gate.py` loads the module by path from the command CI runs; the workflow file is untouched | `python3 -m unittest discover -s bench` runs 103 tests |

Two controls the review ran, that this lane did not re-run: the historical false pass reproduced by
removing the `st_dev` guard from a copy of the runner, and the controls that already failed before
the repair and still do. At this head the same false pass is unreachable through two independent
guards, which is why control 5f refuses twice over; that is the reason, not a re-measurement of the
review's control 3.

## What each fault was

**F1, arm identity.** The old preflight recorded `fstype` for both roots and then only ever checked
the native arm for the string `fuse`.
Nothing asserted the cowfs arm was a cowfs mount, so a tmpfs passed in the `full` mode.
In the matched modes the wrong arm failed, but only because fsx's skip offsets differ between ext4
and tmpfs, which made the streams differ.
A different `st_dev` was never proof on its own either.

Now `bench/fsx-gate/mount-manifest.py` reads the kernel's mount table, resolves the longest matching
prefix, keys the filesystem type on the device rather than the path, and prefers the table's type
over `statfs`, because `statfs` returns the generic `fuse` for every FUSE filesystem and cannot tell
cowfs from any other.
The runner refuses before it runs a case, and each data file carries its own witness: realpath,
device, and the filesystem the kernel reports for that device.
There is no fallback: a directory that cannot be resolved in the table is UNKNOWN, and a dead tree is
not a mount.

Two bugs of my own were found by running it, not by reading it:
`/proc/PID/cmdline` was read twice, so every daemon option parsed as `None`, and `f_fstypename` was
read from offset 8 of `struct statfs`, which is empty on this kernel for every filesystem.
Both produced a refusal with a wrong reason and are fixed.

**F2, attribution.** The old rule excused every difference once any one capability gap existed.
The first repair attributed each operation-count difference on its own, which was better and still
wrong: the full mode's read, write, truncate, mapread and mapwrite deltas came out unexplained and
reported FAIL, which reads as a `cowfs` defect it cannot prove.

The attribution is now mechanical, from the tool's own record.
fsx writes every operation it attempts, and an operation the filesystem refuses is written as
`skip <op>`, so the two arms' recorded streams can be compared position by position.
Where they first disagree is readable, not inferred:

- the operation there is one an arm is recorded as not having, so the arms did different work and the
  pair is UNMEASURABLE, with every count delta on the record;
- the operation there is anything else, so a capability gap explains nothing from there on and the
  pair fails;
- a different operation taking a gap operation's place is not a skip of it, and fails;
- the recorded stream is shorter than the run it came from, because fsx keeps only its last `LOGSIZE`
  operations, so the divergence is not locatable and the pair is UNMEASURABLE with the length stated.

`OP_CAPABILITY` covers the whole fallocate family: `punch_hole`, `zero_range`, `write_zeroes`,
`fallocate`, `collapse_range`, `insert_range`, `exchange_range`, `dedupe_range`.
The last two were missing, and fsx's default mix attempts both, so their skips were invisible.
A gap needs evidence the filesystem wrote: a recorded `skip <op>`, or the runner's own probe, either
one on its own, and one arm's probe never speaks for the other.

The measured result on the builder's mount: three full-mode pairs, divergences at operations 1, 6 and
10, all of them `fallocate` against `skip fallocate`, all three UNMEASURABLE, none a pass and none a
failure.

**F3, what "restart" means.** `--restart-cmd /bin/true` returned 0 and the runner reported one file
rehashed with zero problems, because a command's exit status is not evidence of anything.
The leg now reads the daemon generation from the pid file and `/proc` before and after: pid, start
time, store, socket, mount and the daemon binary's digest, plus a fresh mount attestation, plus each
file's digest and size read by a separate process after the hook returns.
Unchanged pid or unchanged start time is a failure, and so is a changed store, socket, mount or
binary.
The measured run went from pid 1191050 to 1209860 and rehashed 15 files with no problems.

**F4, an attempt is an attempt.** The case directory was created with `exist_ok=True` and written
over, so a child that claims an op count and writes nothing left the previous run's bytes to be
hashed.
Every attempt is now a fresh directory created with `mkdir`, which is `O_EXCL` in effect, and nothing
is deleted to make room: a failed or interrupted attempt is evidence.
A child that cannot be spawned at all is a failed case with a named reason rather than a traceback.

**F5, the tool is pinned.** The old runner recorded the binary's digest in the meta row and compared
it to nothing, so any executable that exited 0 was accepted as an arm.
`fsx-gate.json` now carries an approved manifest: expected binary digest, the three source digests,
the compile line, the config header digest and the compiler.
The runner refuses a mismatch before it runs anything and records `tool_check`.
`--allow-unpinned-fsx` exists for a mutation control and stamps its own record as not an acceptance
claim.

**F6, the numbers.** The declared byte budget was 262144 per file while the measured total was
2915185, and nothing compared anything against anything.
`max_bytes_written_per_arm` is now 15 seed pairs times two arms times the file cap, 7864320, the
measured run wrote 3018150 on cowfs and 2904582 on native, and an over-budget run is reported FAIL
after the fact.

**F7, CI never saw the tests.** `python3 -m unittest discover -s bench` finds `test_*.py` directly
under `bench/`, and `bench/fsx-gate/` has a hyphen in its name, so the gate's own tests were never
discovered.
`bench/test_fsx_gate.py` loads the gate's module by path and adds no assertion of its own, so no
assertion is duplicated and the workflow file is not edited.

## The controls this lane ran

`bench/fsx-gate/mutations.py`, against the builder's own private mount on moonscape:
own store, own socket, own mount, own pid file, under `/home/moonscape/cowfs-ready-wave/task-g4/`.
Report: `bench/out/ready-g4/repair-batch/mutations.json`, gitignored with the rest of the raw output.

| control | what it feeds in | exit | result |
| --- | --- | --- | --- |
| 5d | the cowfs arm pointed at a directory that is not a cowfs mount | 3 | refused: the kernel reports ext4 there |
| 5f | the native arm pointed at the cowfs mount | 3 | refused twice: the control is on `fuse.cowfs`, and both arms then resolve to device 171 |
| 16 | a synthetic child that prints the A-OK line and writes a five-op-type stream | 3 | refused: the binary is not the manifest's |
| 19 | a child that writes nothing, over a stale case directory holding real bytes | 1 | refused: the fresh attempt's result is empty, and the stale directory is neither reused nor deleted |
| 17 | `--restart-cmd /bin/true` | 1 | refused: pid and start time unchanged, so nothing was replaced |

Five controls, zero leaks.
A control that came back PASS would be a defect in the gate, not a control that failed.

## The unit tests that pin these

`bench/fsx-gate/test_run_fsx_gate.py`, discovered by CI through `bench/test_fsx_gate.py`.
67 tests, one skipped where `/proc` is absent.
103 through CI discovery.

| class | what it holds down |
| --- | --- |
| `Attribution` | every delta attributed on its own; the family map complete; the byte-bearing ops named |
| `CapabilityEvidence` | a recorded skip or a probe is the evidence; neither present means no gap; one arm's probe does not speak for the other |
| `FirstDivergence` | the index and both operations named; a skip of the same operation is a capability difference, an unrelated operation is not; only the first divergence matters; one stream ending early is a divergence |
| `CompareCase` | controls 12, 13 and 14; a tmpfs arm; a missing witness; a matched-mode difference; a contaminated device; the pure capability case; the measured full-mode shape; a recorded stream too short to attribute; the same difference inside the window; unreadable op streams |
| `RestartLeg` | an unchanged pid; an unchanged start time; a changed store; a lost mount attestation; the generation that passes; an unreadable pid file |
| `ImmutableAttemptDir` | a fresh empty directory; two calls, two directories; stale content not reused |
| `ToolPin` | the manifest exists; a wrong binary is refused before anything runs; `--allow-unpinned-fsx` is marked a control |
| `ArmAttestation` | a plain directory is not a cowfs mount; the longest mount prefix wins |
| `ByteCaps` | the per-arm budget covers the declared batch; a data file over the declared `-l` is reported; the budget is a whole-run figure |
| `ReadbackIsAFilesystemRead` | the digest is of the file, the empty file included |
| `Verdict` | FAIL outranks UNMEASURABLE; an unmeasurable pair is never a pass; the gap names its own capability; no case at all fails |
| `Summary`, `Probe`, `MountIdentity`, `FsxIdentity` | the raw summary carries every count; the probe records what it tried; the mount identity is read, not named |

## Reproducing

```sh
python3 -m unittest bench.test_fsx_gate -v
python3 -m unittest discover -s bench -v
```

The controls that need a real mount ran on this lane's own private daemon and mount.
The review's own controls ran on its separate mount under `task-g4-review/`.
Neither touched another worker's daemon, store or mount.