# fsx gate g4 repair: control table

Every control is a check that fails at the repaired head and did not fail before.
The runner is the delivered one at this branch; the controls are exact inputs to it.

| control | what it feeds in | before the repair | at this head |
| --- | --- | --- | --- |
| 2 | both arms real `fuse.cowfs` and `ext4`, different devices | PASS | PASS, still |
| 3 | the `st_dev` check removed from a copy of the runner, real pinned fsx, my own mount | PASS, the false pass | FAIL, the same-device guard fires |
| 5d | real pinned fsx, cowfs arm pointed at `/dev/shm` tmpfs, `--mode full` | PASS, exit 0 | UNMEASURABLE, exit 3: "the kernel reports tmpfs for the cowfs arm" |
| 5c | same but `--mode matched` | FAIL by accident: the op streams differ | UNMEASURABLE by the fstype guard, before any case |
| 5e | tmpfs with a real fsx-shaped op stream forced identical | PASS | UNMEASURABLE |
| 12 | `punch_hole` gap explained, 900 fewer `read` on the cowfs arm, different bytes | PASS | FAIL: `read` is unexplained and byte-bearing |
| 13 | the same delta with no capability gap anywhere | FAIL | FAIL, same reason |
| 14 | the gap on the native arm alone, same delta | PASS | FAIL |
| 17 | `--restart-cmd /bin/true` against a real mount | PASS, one file rehashed | FAIL: pid and start time unchanged |
| 19 | real fsx seed 1 in a case dir, then a child that prints the A-OK line and writes nothing | PASS on the previous run's bytes | FAIL: refused, `O_EXCL` attempt directory already exists |
| 16 | a synthetic child that writes 4096 bytes and a five-op-type stream | PASS | UNMEASURABLE: the binary is not the manifest's |
| 17b | `--allow-unpinned-fsx` with the same synthetic child | not available | UNMEASURABLE on the arm attestation, and the record says it is a mutation control |
| 8 | a child that claims the op count and writes bytes but no op stream | FAIL | FAIL, still |
| 9 | a child that runs one op type only | FAIL on the required-op check | FAIL, still |

## What each one is a test of

**5d, 5c, 5e, F1.** The old preflight recorded `fstype` for both arms and then only ever checked
the native arm for the string `fuse`. Nothing asserted the cowfs arm was a cowfs mount, so a
tmpfs passed in `--mode full`.
In the matched modes the wrong arm failed, but only because fsx's skip offsets differ between ext4
and tmpfs, which made the streams differ.
Now `bench/fsx-gate/mount-manifest.py` reads the mount table, resolves the longest prefix, and
refuses anything whose kernel-reported type is not a cowfs type, and the runner refuses before it
runs a single case.

**2 and 3, the st_dev guard.** Control 2 is the honest run and still passes.
Control 3 removes the guard from a copy of the runner, with the real pinned fsx against a real
mount, and the old false pass returns.
That is what proves the guard is what stops it.

**12, 13, 14, F2.** The old rule excused every difference once any one capability gap existed, so
900 fewer reads on the cowfs arm passed with different bytes.
Now each operation-count difference is attributed on its own.
`OP_CAPABILITY` covers the whole fallocate family: `punch_hole`, `zero_range`, `write_zeroes`,
`fallocate`, `collapse_range`, `insert_range`.
`BYTE_BEARING_OPS` names the operations that carry the bytes, and a difference in any of them is a
failure that no hole capability can excuse.
The three cases above are the review's controls 12, 13 and 14 and all three fail.

**17, F3.** `--restart-cmd /bin/true` returned 0 and the runner reported one file rehashed with
zero problems, because a command's exit status is not evidence of anything.
Now the leg reads the daemon generation before and after from the pid file and `/proc`: pid,
start time, store, socket, mount and the daemon binary's digest, plus a fresh mount attestation,
plus each file's digest and size read by a separate process.
Unchanged pid or start time is a failure, and so is a changed store, socket, mount or binary.

**19, F4.** The case directory was created with `exist_ok=True` and written over, so a child that
claims an op count and writes nothing left the previous run's bytes to be hashed.
Now `fresh_attempt_dir` creates each attempt with `mkdir`, which is `O_EXCL` in effect, and never
deletes anything to make room: a failed or interrupted attempt is evidence.
A run whose attempt directory already exists is refused rather than reusing it.

**16, F5.** The old runner recorded the binary's digest in the meta row and compared it to nothing,
so any executable that exited 0 was accepted as an arm.
`fsx-gate.json` now carries an approved manifest: expected binary digest, the three source
digests, the compile line, the config header digest and the compiler.
The runner refuses a mismatch before it runs anything and records `tool_check` in the meta row.
`--allow-unpinned-fsx` exists for a mutation control and stamps its own record as not an
acceptance claim.

**8 and 9.** These already failed and still do. They are in the table because they are the controls
that show the repair did not weaken anything that was working.

## The unit tests that pin these

`bench/fsx-gate/test_run_fsx_gate.py`, discovered by CI through `bench/test_fsx_gate.py`.

| test class | what it holds down |
| --- | --- |
| `Attribution` | every delta is attributed on its own; the family map is complete; the byte-bearing ops are named |
| `CapabilityEvidence` | a gap needs the probe and fsx to agree; either alone is not enough |
| `CompareCase` | controls 12, 13, 14, the tmpfs arm, a missing witness, a matched-mode difference, the contaminated device, the pure capability case |
| `RestartLeg` | an unchanged pid, an unchanged start time, a changed store, a lost mount attestation, and the generation that passes |
| `ImmutableAttemptDir` | a fresh empty directory, two different directories, stale content not reused |
| `ToolPin` | the manifest exists, a wrong binary is refused before anything runs, `--allow-unpinned-fsx` is marked a control |
| `ArmAttestation` | a plain directory is not a cowfs mount; the longest prefix wins |
| `ReadbackIsAFilesystemRead` | the digest is of the file, including the empty file |
| `Verdict` | FAIL outranks UNMEASURABLE; an unmeasurable pair is never a pass; the gap names its own capability |
| `CompareCase` byte caps | a data file over the declared `-l` is reported |

## Reproducing

```sh
python3 -m unittest bench.test_fsx_gate -v
python3 -m unittest discover -s bench -v
```

The controls that need a real mount were run against the builder's own private daemon on moonscape,
`/home/moonscape/cowfs-ready-wave/task-g4/`, with its own store, socket, mount and pid file.
The reviewer's own controls were run on its own separate mount under `task-g4-review/`.
Neither touched another worker's daemon, store or mount.
