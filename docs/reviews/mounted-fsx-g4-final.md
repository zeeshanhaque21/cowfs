# g4 mounted fsx acceptance: independent review

Reviewed PR 102 at `f816b5e96624967f16a28444a0631ddc9672892b`, branch `review/mounted-fsx-g4`, lease `1246013cd5b485fe16d2479db0647c45` under `/Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/14/cowfs`.
No commit, no push, no merge, no lease return. Source read-only. Nothing outside `docs/reviews/mounted-fsx-g4-final.md` and the ignored `bench/out/ready-g4-critic/**` was written in the lease.

## Verdict

The measured g4 result is real and I reproduced it on my own private mount.
`PASS` for the matched subset, scoped as the doc scopes it.
The harness does not prove three things the gate's own summary claims, and one class of the false pass this lane exists to kill is still open.

Recommendation: do not merge on the strength of "PASS" alone.
Either fix F1, F2, F4 and the F6 numbers and wire the tests into CI, or land it with the verdict restated as "matched subset measured, full family UNMEASURABLE, arm identity unasserted".

## What I ran on my own private mount

Own store, socket, mount, pid and daemon binary under `/home/moonscape/cowfs-ready-wave/task-g4-review/`.
Copied the pinned fsx binary and the harness read-only, verified by digest.
The builder's daemon `1008785` and the g5 mount were never signalled, unmounted or written to.

| what | native arm | cowfs arm | result |
| --- | --- | --- | --- |
| my daemon | n/a | pid 1053895, starttime 5549122, `fuse.cowfs`, `st_dev 207` | own store, own socket |
| smoke seed 1, 200 ops | exit 0, 126813 B, `d0fe6b0f16f0`, `st_dev 2050` | exit 0, 126813 B, `d0fe6b0f16f0`, `st_dev 207` | runner PASS, exit 0 |
| sync seed 7, 10000 ops, `-y` | exit 0, 111141 B, `54f1bd3d92c4` | exit 0, 111141 B, `54f1bd3d92c4` | PASS, stream identical |
| restart leg after sync | n/a | pid 1053895 stopped, 1058979 started, same store, 1 file rehashed | exit 0, 0 problems |
| matched seeds 2, 3, 20000 ops | exit 0, `affc7bb0fe32`, `f83d1e25ed70` | exit 0, same digests | PASS, stream identical |
| restart leg after matched | n/a | pid 1058979 stopped, 1115487 started, 2 files rehashed | exit 0, 0 problems |
| fallocate matrix, my own arms | 75 of 75 `ok` | 75 of 75 `ENOTSUP` | reproduces issue 103 |

Every readback above was a separate process reading through the mount, not the runner hashing its own copy.
Final readback before I stopped my own daemon: matched seed 2 `affc7bb0fe325eaa`, seed 3 `f83d1e25ed70317a`, sync seed 7 `54f1bd3d92c4beb7`, all after the third daemon generation.
Operation stream files are 10000 lines for a 20000 operation run, which is `LOGSIZE`, and the doc says so.

Own daemon stopped after pid, argv, store, socket and mount identity were checked; borrowed daemon `1008785` verified still running afterwards, untouched.

## The builder's evidence recomputed

I loaded the builder's `cases.jsonl` and re-ran the HEAD runner's `compare_case` and `verdict` over its own recorded case dicts.
`PASS`, 30 cases, 0 failures, 12 of 15 compares stream-identical, and every `op_count_deltas` identical to what was recorded.
So the recorded PASS is faithful to the code at this SHA. The claims below are about what the code checks, not about arithmetic.

Case accounting is exact: smoke 1 seed, matched 10, sync 1, full 3 = 15 seed pairs = 30 arm executions = 15 compares, 270200 declared operations per arm, 2915185 bytes on the cowfs arm.

Unit tests: 49 pass on this machine and 49 pass on the host. `ruff check --select F,E9 bench/fsx-gate/` reports 1 finding, `F841` unused `e` at `run-fsx-gate.py:109`. The repo has no ruff config and CI does not run ruff.

CI at this exact SHA: `check (ubuntu-latest)` success, `check (macos-latest)` success, `linux-fuse` success. Three of three, none skipped.

## F1, the cowfs arm is never asserted to be cowfs

`run-fsx-gate.py:626-641` records `fstype` for both arms and then only ever checks the native arm for `fuse`.
There is no assertion anywhere that the cowfs arm's fstype is a cowfs mount, and no assertion that the cowfs arm is served by a daemon at all.

Control 5d, my own run: real pinned fsx, native arm on ext4, "cowfs" arm pointed at `tmpfs` on `/dev/shm`, `--mode full`.
Runner exit 0, status `PASS`, zero failures.
The meta row itself records `"fstype": "tmpfs"` and nothing acts on it.

In the matched modes the same wrong arm does fail, but for the wrong reason: `fsx`'s recorded skip offsets differ between ext4 and tmpfs, so the op stream differs and `require_identical_op_stream` catches it.
Control 5c and 5e, both FAIL. That is luck, not a guard.

So three of the four declared modes in the measured batch are protected by an accident of stream comparison, and the `full` mode is not protected at all.
The PR body line "a cowfs root that is not a mount ... is refused" is true only for a path that is not a mount at all. A different mount is accepted.

The measured batch's cowfs arm really was `fuse.cowfs`, which the meta row records, so the measured result stands. The harness does not prove it, and it would not catch a re-run pointed at the wrong mount.

Fix: in preflight, refuse unless the cowfs arm's `fstype` contains `cowfs`, and record the mountpoint it resolved to in the compare row.

## F2, one explained capability gap excuses every other difference

`run-fsx-gate.py:478-483`:

```
if require_same_stream and not stream_match:
    problems.append(...)
elif not gaps and consequent:
    problems.append(...)
```

`consequent` is computed but only becomes a failure when `gaps` is empty.
Once a single hole capability is explained, an arbitrary difference in any other operation type passes with no problem recorded.

Controls, in process, no filesystem:

| control | input | result |
| --- | --- | --- |
| 12 | `punch_hole` gap explained, plus 900 fewer `read` on the cowfs arm, different bytes | no problems, `hashes_compared` false |
| 13 | the same 900-fewer-reads delta with no capability gap anywhere | FAIL, "no capability gap on either arm" |
| 14 | the gap on the **native** arm alone, same 900-fewer-reads delta | no problems |

Control 12 and 13 differ only in whether one hole gap exists.

What this let through in the measured batch: each of the three `full` compares carries 8 `consequent_deltas`, of which none is in the gate's `OP_CAPABILITY` map:
`collapse_range`, `fallocate`, `insert_range`, `mapread`, `mapwrite`, `read`, `truncate`, `write`.
`collapse_range`, `fallocate` and `insert_range` are hole-family operations the capability map does not list at all, so even the attribution is incomplete.
`read`, `write`, `mapread`, `mapwrite` and `truncate` are the ordinary operations, and a 900-operation difference in those passes.

The doc is accurate that these are "recorded as `consequent_deltas`, not as capability gaps, and the bytes are not compared".
It is not accurate, and neither is the PR body, that "an operation stream difference that no recorded capability explains" is a failure.
In a non-matched mode it is not a failure at all once any capability gap exists.

Fix: in a non-matched mode, require every non-hole delta to be inside a declared tolerance, and either extend `OP_CAPABILITY` to the whole fallocate family or say in the record which deltas were accepted without explanation.

## F3, the restart leg does not prove the daemon restarted

The runner shells out to `--restart-cmd`, records the command string and its exit code, and re-hashes by path.
It never compares a pid before and after, never checks the mount went away and came back, and never ties the command to the same store.

Control 17: `--restart-cmd /bin/true` against my own real mount.
Runner exit 0, `PASS`, one file rehashed, zero problems, restart row exit 0.

The measured batch's restart row does contain real evidence: `stopped pid 997367` then `started pid 1008785`, and `1008785` was still the live borrowed daemon when I looked, 1h17m after start.
So the measured claim is true. The harness would pass a command that does nothing.

Also: a graceful stop and start is not a durability acknowledgement and not crash injection.
The `-y` sync mode, the `fsync_readback` probe and the post-restart readback together cover the write path and a clean reopen.
They do not cover power loss, and the doc should not let "restart leg" read as more than that.

Fix: have the restart leg record the daemon pid and starttime before and after, and fail when they are unchanged.

## F4, the case directory is not cleared, and "fsx did work" is not observable

`run_case` does `os.makedirs(case_dir, exist_ok=True)` and writes over whatever is there.
Op counts, the op stream digest and the data digest all come from files on disk after the child exits.
Nothing ties them to this invocation.

Control 19: I ran the real fsx seed 1 in a case directory on my own mount, then re-ran the same mode and seed with a child that prints `All 50 operations completed A-OK!` and writes nothing at all.
Runner exit 0, `PASS`, data digests identical on both arms, op stream digests identical, `problems` empty.
The bytes it compared were the previous run's bytes.

With the real `fsx` this is masked, because `fsx` opens the data file `O_RDWR|O_CREAT|O_TRUNC` and rewrites the ops file.
That is the tool's behaviour, not the harness's, and the harness is the component that claims "an empty or missing result never passes".
This is the same shape as the false pass the g5 critic found in the sibling lane.

Control 8, a child that claims the op count and writes bytes but no op stream, FAILs correctly.
Control 9, a child that runs one op type only, FAILs correctly on the required-op check.
Those two are sound. Control 19 is the gap.

Fix: remove the case directory before each case, or record the case directory's mtime and size before the child runs and fail when the data file was not touched.

## F5, the fsx binary is recorded but never checked against a pin

`fsx-gate.json` pins three source digests and `build-fsx.sh` verifies them before compiling.
`run-fsx-gate.py` records `fsx_identity`'s digest in the meta row and never compares it to anything.

Control 16: a synthetic child that writes 4096 bytes and a well-formed five-op-type stream passes the smoke mode with exit 0.
Same class as F5 in the g3 lane, where a non-worker was accepted as an arm.

The measured run recorded the right digest, `dc93eda7...`, and I confirmed the binary I ran carries that digest and that the three upstream source files on the host match the pinned digests at commit `22348afe338c0f6d540c0d7ef0db749eaa51b218`, tag `v2026.09.22`, unmodified.
So the measured run is honest. The harness just does not require it.

Fix: add the expected binary digest to `fsx-gate.json` and refuse a mismatch.

## F6, numbers in the doc and the config do not match the evidence they cite

| claim | where | the evidence says |
| --- | --- | --- |
| "The 38 flags" | `ready-g4.md:66` | the list on line 69 has 42 tokens, `identity.json` has 42, and the batch's own meta row recorded 23 before the `[: ]` regex fix. Three different numbers, none of them 38 |
| cowfs arm `st_dev 234` | `ready-g4.md:154`, `:229` | every cowfs case in the batch records `171`. `234` appears in the earlier smoke2, smoke3 and attribution-fix runs |
| "13 of 15 compares" matched | PR 102 body | the doc says 12 of 15 and the record says 12 |
| "about 21 s on the cowfs arm against 8 s native" | `fsx-gate.json:63` | the batch's own `seconds` are 9.35 to 10.30 cowfs against 7.00 to 8.06 native, about 1.3x. My re-runs were 24.2 to 30.1 against 8.9 to 9.0 on a host running two other cowfs daemons and several workers |
| "Byte caps are enforced by the runner, which refuses a data file over `max_file_bytes` and reports the total written per arm" | `fsx-gate.json:41` | the runner never mentions `max_bytes_written`, and no comparison of `data_size` against any cap exists. The cowfs arm wrote 2915185 bytes against a declared `max_bytes_written_per_arm` of 2621440, with no complaint |
| "Per-seed results are in `bench/out/ready-g4/batch/summary.md`" | `ready-g4.md:164` | that path is gitignored and does not exist in the repo. The real location is the host's `/home/moonscape/cowfs-ready-wave/task-g4/out/batch/` |
| "This is the wave g4 gate from `docs/ready-wave-dispatch.md`" | `ready-g4.md:5`, `locked-run.sh:6` | that file is untracked in the main checkout and absent from this worktree |

"30 cases, 15 seeds" is correct and the case accounting above is exact.
The 38-versus-42 flag discrepancy is a direct consequence of the regex fix in this very PR: the doc was updated, the batch meta row was not re-run.

## F7, the 49 unit tests do not run in CI

`.github/workflows/ci.yml:25` runs `python3 -m unittest discover -s bench -v`.
That discovers 36 tests, all from `test_gates.py`.
`bench/fsx-gate` is not an importable package, the hyphen alone is enough, so the 49 tests the doc and the PR body cite are never executed by CI.

The documented command in the test docstring and in `ready-g4.md:286` is correct and does run all 49, on this machine and on the host.
But "unit tests: 49 pass" is a local claim, and the guards in F2 and F4 that the doc calls regression-protected have no CI coverage at all.

Fix: add `bench/fsx-gate` to the CI discovery, or add an explicit `python3 -m unittest discover -s bench/fsx-gate -p 'test_*.py'` step.

## Scope, gate by gate

| gate family | my verdict | why |
| --- | --- | --- |
| matched subset, 12 of 15 compares | accepted, scoped | reproduced independently on my own mount: smoke, sync, two matched seeds, all byte-identical, both arms on different devices |
| `full` family hole coverage | UNMEASURABLE, not PASS | 75 of 75 `fallocate` modes answer `ENOTSUP` on the mount where ext4 answers `ok`, reproduced on my own arms. Issue 103, FUSE conformance lane. The doc and the PR body both say this plainly; the "PASS" headline does not carry it |
| fallocate attribution | incomplete | `OP_CAPABILITY` lists only `punch_hole`, `zero_range`, `write_zeroes`. `fallocate`, `collapse_range` and `insert_range` are recorded as unexplained deltas, not as gaps |
| durability, crash, power loss | not measured here | `-y`, the `fsync_readback` probe and a clean stop/start reopen are not a durability acknowledgement and not crash injection. That is g6's surface |
| POSIX mmap sweep | not measured | `mapread` and `mapwrite` appear in `fsx`'s own random stream, with `msync` in fsx's `mapwrite` path. That is scheduled coverage, not a full POSIX mmap conformance sweep |
| Darwin | non-measurable by construction | `ltp/fsx.c` includes `<linux/mman.h>` and `<sys/syscall.h>` unconditionally, no darwin build. The macOS NFS loopback is a model mount and is not this gate. The doc says so |
| performance | no claim made, none accepted | nothing in the PR claims 1.5x. My numbers come from a loaded host with two other cowfs daemons and are recorded as observation only |

## What the builder got right

The false pass it found was real and it fixed it properly.
I reproduced the historical behaviour as a false PASS by removing the guard from a copy of the runner, control 3, and confirmed the guard catches it, control 2.
Both use the real pinned fsx against my own real mount, and the only difference between them is the `st_dev` check.

The two reporting faults it found in its own batch are also real.
`fsync` cannot appear in a random stream because `op = rv % OP_MAX_FULL` and `OP_FSYNC == OP_MAX_FULL`, which I confirmed in the pinned source at lines 138 and 2411.
And the `KEEP_SIZE` misattribution is visible in the earlier `batch-prev-attribution-fix` run and gone in `batch`.

`LOGSIZE` is 10000, the op count comes from fsx's own A-OK line rather than the truncated ops file, and the doc says both.
Alias and overlap handling is correct: a symlink to the mount passes with `st_dev 207`, a symlink or subdirectory of the native root is UNMEASURABLE, the mount as its own native control is UNMEASURABLE, a dead native root is UNMEASURABLE.
CI is green at this SHA with nothing skipped.

## Reproducing my part

```sh
R=/home/moonscape/cowfs-ready-wave/task-g4-review
python3 $R/mutations.py \
  --runner $R/harness/run-fsx-gate.py --config $R/harness/fsx-gate.json \
  --work $R/mutations --fsx $R/bin/fsx \
  --native-root $R/native --cowfs-root $R/private/mnt/base \
  --second-arm /dev/shm/g4critic-arm --report $R/mutations.json
```

Every control's raw records are one JSONL line per case in `bench/out/ready-g4-critic/remote-evidence/`, the control table is `remote-evidence/mutations.json`, the fallocate matrix is `remote-evidence/fallocate-matrix.txt`, and my own daemon log with all three generations is `remote-evidence/daemon.log`.
`bench/out/ready-g4-critic/mutations.py` is the control script, and `bench/out/ready-g4-critic/builder-batch-cases.jsonl` is the builder's raw batch record I recomputed.
All of it is gitignored raw evidence, not published.

## Prerequisite and tooling notes

`codebase-memory-mcp` was not available in this session, so code reading was `rg` plus direct reads.
No xfstests rebuild: the pinned binary's digest, the three source digests, the commit and the tag were verified read-only on the host, which is what the digest is for.
A rebuild on another host would produce a build-specific digest and is not required to check this claim.
`ruff check --select F,E9` is clean on my own fixture and reports one pre-existing finding in the delivered runner.