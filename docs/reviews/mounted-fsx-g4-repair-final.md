# Independent review: PR 102 repair, `ff45b0a3c6ab1e6fcfd94bf0fdf733812a1fc38c`

Critic for the ready-wave g4 fsx lane.
Reviewed head `ff45b0a3c6ab1e6fcfd94bf0fdf733812a1fc38c` against the head my previous review named,
`f816b5e96624967f16a28444a0631ddc9672892b`.
PR 102 is open, `mergeable_state: clean`, no auto-merge, zero comments, zero reviews, no closing
keywords in the body or in any of the eight commit subjects.
Issue 103 is open.
The delta is 8 files, 2484 insertions, 634 deletions, all under `bench/`, `docs/` and
`docs/verification/`.
No production crate changed and `.github/workflows/ci.yml` is untouched, as the document claims.

## Verdict

All seven of my earlier false-PASS findings are closed at this head.
I reproduced the false PASS for each of them against the reviewed code, then confirmed the repair
refuses, on my own private Core FUSE mount.

No BLOCK.
Nine residual defects, all reporting or documentation, none of which can turn a wrong run into a
pass.
One of them, R1, points a reader at the wrong evidence directory, and that directory holds a FAIL
run.

Two claims in the documents are wrong on their face and are listed as R2 through R4.

## What I ran, and on what

My own mount, not the builder's.
I read-copied the daemon binary and the pinned fsx into my own directory and compared digests
before use; the copies match the originals.
I started my own daemon against my own store, socket and mount, and stopped only that one.

| | |
| --- | --- |
| my root | `/home/moonscape/cowfs-ready-wave/task-g4-review/attempt-ff45b0a` |
| my daemon binary sha256 | `769cf9e124b4d59d442146ec30075c7209643380a4566fd43110aa6e93d2e338`, identical to the borrowed original |
| my fsx sha256 | `dc93eda70a3d0d445fb50ae55446b9f6164d6a77fcf79d247f2021887c43e9af`, and equal to `expected_binary_sha256` in the approved manifest |
| harness sha256 | `run-fsx-gate.py` `421bd5232df92e0a0d4915c28a9aa0b094ce638e49197253ab404d4de20d8858`, byte-identical to the builder's copy at `task-g4/harness/` |
| my fstype | `fuse.cowfs` from `/proc/self/mountinfo`, device 0:207, statfs only says the generic `fuse` |
| my native arm | `/dev/sda2` on `ext4`, device 2050 |

My own 200-operation sample on both arms, with a real restart leg:

| | |
| --- | --- |
| compare | smoke seed 1, 200 ops, PASS, stream identical, sha256 `d0fe6b0f16f0` on both arms |
| devices | 2050 native, 207 cowfs; fstype `ext4` and `fuse.cowfs` |
| restart | generation `1271277`/`6576139` to `1271420`/`6576484` |
| unchanged across the restart | store, socket, mount, daemon binary digest |
| readback | 1 file rehashed by a separate process, no problems |
| mount after restart | still attested `fuse.cowfs`, device 207 |
| verdict | `PASS: 2 cases, 1 passed, 0 failed, 0 unmeasurable` |

The fallocate gap reproduces on my own mount, independently of the builder's:

| arm | 75 combinations of mode, offset, length and file state |
| --- | --- |
| native `ext4` | 75 `ok` |
| my `fuse.cowfs` mount | 75 `ENOTSUP` |

Issue 103 is live and unchanged.

## F1 through F7

### F1, an arm that is not the filesystem under test: PASS

Attestation comes from the kernel, and there is no fallback anywhere in the path.
`resolve_mount` picks the longest containing `/proc/mounts` entry with no `realpath`, and
`mountinfo_for_device` supplies a second, device-keyed opinion on the filesystem type.
The two must both be acceptable or the arm is refused.
`statfs` alone is not enough: it reports the generic `fuse` for every FUSE filesystem, and I
confirmed that when I broke only the device-keyed lookup the attestation went `ok: False` with
`the kernel reports fuse ... not fuse.cowfs/cowfs/nfs`.

Controls I ran, all on my own mount:

| control | result |
| --- | --- |
| 5d, a non-cowfs directory as the cowfs arm | exit 3, refused before any case |
| 5f, the native arm pointed at the cowfs mount | exit 3, refused twice over |
| a real `tmpfs` mount at `/dev/shm/...` as the cowfs arm, `full` mode | exit 3, refused |
| the same tmpfs mount, `matched` mode | exit 1 |
| a symlink into the cowfs mount as the cowfs arm | exit 3, refused |
| a dead symlink | exit 3, refused |
| a pid file naming the borrowed builder daemon 1209860 | exit 3, `the daemon serves ... but the cowfs arm is ...` |
| the mount table returning `(None, reason)` | `UNKNOWN`, no filesystem type in the result at all |
| the mount table returning an empty list | `UNKNOWN`, `no mount table entry contains ...` |

The refusal precedes any child.
For the borrowed-pid case the record contains only `meta` and `verdict` rows; no `case`, `compare`
or `restart` row exists and no attempt directory was created.
The runner never signals, unmounts or deletes anything itself: `rg` for `kill|signal|pkill|umount|
rmtree` over `run-fsx-gate.py` returns only `shutil.rmtree` on its own evidence temp directory and a
`shutil.copy2` of the copied per-case files.

Daemon identity is read from `/proc/<pid>/cmdline` and `/proc/<pid>/stat`, never from what the hook
prints: pid, starttime, store, socket, mount, backend and the sha256 of `argv[0]`.

I checked the competing-mount-entry concern directly.
This host has 11 `/proc/self/mountinfo` entries on device 8:2 and 2 on 0:22, and
`mountinfo_for_device` returns the first match, not the entry containing the path.
For a path inside `/run/omv-writecache/var_log/lower` the witness therefore reports
`mountpoint "/"` rather than the containing entry.
On this host every entry sharing a device also shares its filesystem type, and each of the three
`fuse.cowfs` mounts has its own device, 0:143, 0:171 and 0:207, so no false attribution is
reachable here. See R7.

### F2, one explained gap excusing everything else: PASS

The gate now locates the first operation the two recorded streams disagree on, and requires the bare
operation names to be equal and to name an operation the filesystem is recorded as not having.

My own controls, all with a synthetic child and `--allow-unpinned-fsx`, on my own mount:

| first divergence on the cowfs arm | mode | result |
| --- | --- | --- |
| `skip fallocate` against native `fallocate` | `full` | exit 3, UNMEASURABLE |
| `skip exchange_range` against native `fallocate` | `full` | exit 1, FAIL |
| `skip dedupe_range` against native `fallocate` | `full` | exit 1, FAIL |
| `skip read` | `full` | exit 1, FAIL |
| `skip write` | `full` | exit 1, FAIL |
| `mapread`, an unrelated operation in the place of the gap | `full` | exit 1, FAIL |
| `skip fallocate` | `matched` | exit 1, FAIL |

A skip of one operation is not a skip of another, and a mode that declares one mix has no
exception. That is the rule the review asked for.

A fabricated first-skip log does not hide content corruption:

| control | result |
| --- | --- |
| identical fabricated stream on both arms, first op `skip fallocate`, bytes differ by filesystem | exit 1, FAIL, `identical op streams produced different bytes: native d010f6d7..., cowfs b821a72d...` |
| fabricated stream diverging at a capability op with different bytes | exit 3, UNMEASURABLE, never PASS |

Same stream plus different bytes fails regardless of any known skip, which is the case that
originally leaked.

The measured run's three full-mode pairs are consistent with this and I recomputed them from the raw
record rather than trusting the table:

| mode | seed | ops | divergence index | native | cowfs | status |
| --- | --- | --- | --- | --- | --- | --- |
| full | 1 | 10000 | 1 | `fallocate` | `skip fallocate` | UNMEASURABLE |
| full | 2 | 10000 | 6 | `fallocate` | `skip fallocate` | UNMEASURABLE |
| full | 3 | 10000 | 10 | `fallocate` | `skip fallocate` | UNMEASURABLE |

All 15 rows of the document's table match the raw `cases.jsonl` exactly: mode, seed, ops, both
digests, stream equality and status. Zero mismatches.

The consequences that follow the skip are recorded, not excused into a pass.
Each full pair lists `deltas_unexplained` of `mapread, mapwrite, read, truncate, write` and still
returns UNMEASURABLE rather than FAIL, which is the behaviour asked for: not auto-failing a
legitimate downstream effect, and not converting it into a pass either.
The run's aggregate is 30 cases, 15 pairs, 12 passed, 0 failed, 3 unmeasurable, status
UNMEASURABLE. That matches the document.

The documented first-divergence row is real fsx output: the rows come from the pinned child at
commit `22348afe338c0f6d540c0d7ef0db749eaa51b218`, and the divergence shape is the gate's own rule
applied to it. No stale child, no helper binary, no synthetic PASS.

What is not covered, and the documents do not say so: `OP_CAPABILITY` has eight entries,
`punch_hole`, `zero_range`, `write_zeroes`, `fallocate`, `collapse_range`, `insert_range`,
`exchange_range`, `dedupe_range`. `clone_range`, `copy_range` and `write_atomic` are skipped by both
arms in `full` mode and are deliberately not in the map, so a divergence landing on one of them
would be a FAIL rather than an attributed capability difference. That is the conservative direction
and it costs nothing at this head. See R6.

### F3, a no-op restart reading as clean: PASS

The leg compares the daemon generation before and after, read from the pid file and `/proc`.
It requires pid and starttime to change and store, socket, mount and binary digest to hold.

| control | result |
| --- | --- |
| shipped control 17, a hook that does nothing | exit 1, `daemon pid 1271420 is unchanged after the restart hook, so nothing replaced it` |
| the real hook, on my own mount | pid `1271277`/`6576139` to `1271420`/`6576484`, 1 file read back by a separate process, mount still attested |

The measured run's own leg is pid 1191050 to 1209860, 15 files rehashed, no problems.

Ownership before signalling is the hook's job and `cowfs-mount.sh` does it: `owned_pid` reads
`/proc/<pid>/cmdline` and refuses any pid whose argv is not the canonical target binary, with
shared daemon 15263 named in the comment as the reason. The runner does not re-check ownership
before it runs the hook, so a caller who supplies an arbitrary `--restart-cmd` string is trusting
that string. The document's reproduce command uses the harness hook, and a hook that does nothing
fails, so no false pass is reachable. This is a scope statement, not a defect.

### F4, a reused case directory: PASS

`fresh_attempt_dir` builds a unique name under a parent and creates it with `os.mkdir`, which fails
if it exists, retrying up to 4096 times. Nothing is removed to make room.

I confirmed it on my own mount: the attempt directory list grew across runs and a pre-existing
`stale-case` directory survived every one of them.
Shipped control 19 records `stale_preserved: true` and exits 1 because the result is empty.

### F5, a digest compared to nothing: PASS

`fsx-gate.json` carries the approved manifest: expected binary digest, three source digests, the
compile line, the config header digest and the compiler.

| control | result |
| --- | --- |
| shipped control 16, a synthetic child instead of fsx | exit 3, `hashes to 6941545d... but the approved manifest pins dc93eda7...` |
| the same child without the override flag | exit 3 |

A scope note, not a leak: the runner enforces the binary digest and the usage-exit probe at run time.
The source digests, the compile line and the compiler are verified by `build-fsx.sh` before
compiling and are recorded in the manifest, but nothing re-checks them when a binary is presented,
because once a binary exists those fields describe how it was made rather than what it is.
The approval therefore means "this digest, built this way", and user input is never sufficient on
its own: `--allow-unpinned-fsx` is the only bypass and it stamps its own record as not an acceptance
claim.

### F6, the byte budget nothing compared: PASS on enforcement

The runner measures bytes actually written per arm and fails an over-budget run.

| control | result |
| --- | --- |
| budget reduced to 1000 | exit 1 |
| budget reduced to 50000 | exit 1, `the cowfs arm wrote 126813 bytes against a declared per-arm budget of 50000` |

The declared figures are right: budget 7864320 per arm is 15 seed pairs times two arms times the
262144 file cap, and the measured run wrote 3018150 cowfs and 2904582 native, both under.
Nothing is deleted to fit a budget. See R3, R4 and R5 for the reporting defects around this.

### F7, CI never collecting the gate's tests: PASS

`bench/test_fsx_gate.py` is a discovery bridge that loads `test_run_fsx_gate.py` by path, and it
defines no `TestCase` of its own, so it adds visibility and not assertions.

| measurement | result |
| --- | --- |
| gate module alone | 67 tests, 1 skipped, the skip being `RestartLeg.test_a_manifest_helper_reads_a_generation` where `/proc` is absent |
| CI's exact command on the branch tree in isolation | 103 tests |
| CI at this head, from the job log of run 37257411193 | `Ran 237 tests`, on both ubuntu and macos |
| distinct gate test names in the CI log | 67 |
| duplicate test names in CI discovery | 0 |

The CI run built merge commit `6727e55e05983a2e5e4ec9a1008aab0da8d04b2b`, whose first parent is main
`3a6935b244d0209ef7c75ddf9539f54dc137105b`, which is where `bench/test_daemon_crash.py` and
`bench/test_compare_coverage.py` come from. So the gate's 67 tests are genuinely collected and run
by the workflow at this head. The bridge works.
The document's `103` is the branch tree in isolation and is not what CI reported. See R2.

## Residual defects

None of these can turn a wrong run into a pass. All are reporting or documentation.

### R1, the raw-evidence pointer names the wrong run, and that run is a FAIL

`docs/verification/ready-g4.md` line 77 says
`Raw evidence: bench/out/ready-g4/repair-batch/`.
On the host, `task-g4/out/repair-batch/` is the earlier run whose verdict is
`FAIL: 30 cases, 12 passed, 3 failed`.
The run the document actually reports is `task-g4/out/repair-batch2/`, whose verdict is
`UNMEASURABLE: 30 cases, 12 passed, 0 failed, 3 unmeasurable`.

Nothing in that directory matches the document's numbers:

| figure | `repair-batch` | `repair-batch2`, which the document reports |
| --- | --- | --- |
| verdict | FAIL | UNMEASURABLE |
| failed pairs | 3 | 0 |
| bytes, cowfs | 2915185 | 3018150 |
| bytes, native | 3065937 | 2904582 |
| full seed 1 digests | `a5ebc282c25f` / `a0f8ba4f9829` | `ba2b13e21734` / `01b243774851` |
| restart generation | 1175979 to 1191050 | 1191050 to 1209860 |

A reader who follows the pointer finds a FAIL and no explanation of why it became UNMEASURABLE.
This is the single most consequential residual, because it is the difference between the repaired
gate's verdict and the gate as it first ran.

### R2, a quoted path that does not exist, and a test count CI did not report

`ready-g4.md` line 172 says the per-case seconds are in
`bench/out/ready-g4/repair-batch/run.log`.
No `run.log` exists in `repair-batch/`, `repair-batch2/` or `repair-smoke/`.
The per-case seconds the sentence quotes are in `task-g4/out/repair-batch2.log`, outside the
directory the sentence names, and the run named there is the wrong run per R1.

The same section's bullet says `103 through CI discovery`.
CI reported 237. 103 is what `python3 -m unittest discover -s bench` yields on the branch tree alone.
The claim understates the real number, so it is wrong in the harmless direction, but it is the
number a reader will check against CI.

### R3, the two documents disagree on how many controls ran

`ready-g4.md` says `4 mutation controls against the private mount` and then lists four.
`evidence/fsx-g4-repair.md` says `Five controls, zero leaks` and its table lists five: 5d, 5f, 16,
19, 17.
The shipped control set contains all five. I ran all five against my own mount and every one behaved
as declared: 5d exit 3, 5f exit 3, 16 exit 3, 19 exit 1, 17 exit 1.

### R4, a test-class count is one short

`evidence/fsx-g4-repair.md` says `FirstDivergence` has 6 tests.
It has 7: the index and both operations named, a skip of the same operation, a skip of an operation
nobody lacks, an unrelated operation in the place of a gap, identical streams, only the first
divergence matters, one stream ending early.
Every other class count in that document matches what I measured.

### R5, the over-budget failure names the wrong arm and quotes the wrong number

`run-fsx-gate.py` lines 1252 to 1259 compute `over_budget` over both arms and then hard-code the
cowfs arm in the message.

Measured on my own mount, with a synthetic child that writes more on the native control than on the
mount, and a budget between the two:

| | |
| --- | --- |
| written per arm | native 8192, cowfs 1024 |
| budget | 4000 |
| arm actually over budget | native |
| text in the record | `the cowfs arm wrote 1024 bytes against a declared per-arm budget of 4000` |

The verdict is still FAIL, so nothing passes that should not. But the evidence record misstates
which arm breached the budget and by how much, which is the one thing a budget failure exists to
say. `over_budget` already holds the arm names; they are simply not used.

### R6, `planned_files` over-counts whenever `--seeds` is passed, and the code comment contradicts the behaviour

Line 1241 computes
`planned_files = sum(len(m["seeds"]) * (1 if args.seeds is None else len(args.seeds.split(","))))`,
which multiplies the declared seed count by the requested one.

| invocation | reported `planned_files` | files that actually run | reported `planned_worst_case` | declared budget |
| --- | --- | --- | --- | --- |
| default | 15 | 15 | 7864320 | 7864320 |
| `--seeds 2,3` | 30 | 8 | 15728640 | 7864320 |

So a narrowed run records a planned worst case of twice the declared budget and still reports
`budget_matches_declared_caps: true`. Enforcement is unaffected, because the check uses measured
`written`, not the plan.
`ByteCaps.test_the_config_budget_covers_the_declared_batch` checks the config arithmetic and never
reaches this line, so no test covers it.

Separately, the comment above that block says a run exceeding the budget `is refused before it
starts rather than reported after it grew`. It is not refused; the runner only prints a warning when
the budget is smaller than two maximum files, and reports FAIL afterwards.
`ready-g4.md` describes the behaviour correctly and the comment is what is wrong.

### R7, the device-keyed lookup returns the first entry for a device, not the containing one

`mountinfo_for_device` returns the first `/proc/self/mountinfo` line whose major:minor matches.
`resolve_mount` is path-keyed and picks the longest prefix; the two can disagree about which mount a
file is on.

Measured: for a path inside `/run/omv-writecache/var_log/lower` the witness reports
`mountpoint "/"` and `fstype ext4`, where the path-keyed answer is that bind mount, also `ext4`.
So on this host the filesystem type always agrees, and each `fuse.cowfs` mount has a unique device
0:143, 0:171, 0:207, so the cowfs arm cannot be misattributed either.
The mismatch is confined to the reported `mountpoint` field.
Making the device lookup path-aware, or reporting the containing entry from `resolve_mount`, would
close it. Low severity, and it does not produce a wrong verdict here.

### R8, the subject under test has no digest or revision in the canonical document

The document is careful about the tool: digest, three source digests, tag object, commit, compile
line, config header digest and compiler are all in a table.
The system under test is named only as `cowfs-daemon --backend core` in prose.
Its digest `769cf9e124b4d59d442146ec30075c7209643380a4566fd43110aa6e93d2e338` is recoverable only
from the gitignored evidence file, and no compiled revision is stated anywhere in either document.
The document makes no claim about main `3a6935b`, so there is no new-main claim to strike, but the
measured subject's identity is not in the document a reader is pointed to.

### R9, two exit codes mean different things in two harnesses in this repository, and the gate cannot separate unsupported from invalid

`run-fsx-gate.py` defines `EXIT_PASS, EXIT_FAIL, EXIT_USAGE, EXIT_UNMEASURABLE = 0, 1, 2, 3`.
`ready-g4.md` states exactly that and reports `exit 3, UNMEASURABLE`, which is correct for this
runner.
`bench/compare.py` uses the other convention: 0 PASS, 1 FAIL, 2 unmeasurable, 3 INVALID, and its
docstring says an invalid input exits 3 with no comparison printed.
So exit 3 means UNMEASURABLE here and INVALID there.

I measured every verdict kind by direct CLI run rather than by reading the enum:

| verdict kind | how I produced it | exit |
| --- | --- | --- |
| PASS | smoke seed 1, 200 ops, both arms, my own mount | 0 |
| FAIL | shipped control 19, empty result | 1 |
| FAIL | reduced budget, 50000 | 1 |
| FAIL | fabricated identical stream with different bytes | 1 |
| FAIL | shipped control 17, no-op restart | 1 |
| usage | `--mode no-such-mode` | 2 |
| UNMEASURABLE | cowfs arm not a cowfs mount | 3 |
| UNMEASURABLE | binary does not match the manifest | 3 |
| UNMEASURABLE | full-mode capability divergence, the measured run | 3 |

The premise I was asked to test was that `exit 3 UNMEASURABLE` contradicts an agreed taxonomy of
PASS 0, FAIL 1, UNMEASURABLE 2, INVALID 3. The code and the document agree with each other, and the
taxonomy in that premise matches `bench/compare.py` rather than this gate, so I am not relabelling
anything: 3 is UNMEASURABLE here, measured, and the document's stated exit is right.
What is true and worth fixing is the collision between the two harnesses, and that this gate maps
both a genuinely unsupported operation and an invalid arm or tool to the same 3.
A dispatcher reading only the exit code cannot tell "the filesystem lacks an operation" from
"the tool or the provenance is wrong".

## Scope this gate does not establish

| | |
| --- | --- |
| the whole `fallocate` family on a mount | not measured. 75 of 75 answer `ENOTSUP` on my mount where 75 of 75 answer `ok` on ext4, so the full mode is UNMEASURABLE, not passing. Issue 103 is open. |
| macOS | not measured. `ltp/fsx.c` includes `<linux/mman.h>` unconditionally, so there is no darwin build and the NFS loopback is not covered. This is a source fact, not a Darwin result. |
| durability | not measured. The restart leg is a clean reopen of a live store: write path plus readback by a separate process. It is not a durability acknowledgement, not crash injection, not power loss. The document says so. |
| fsync counts | none exist. `OP_FSYNC == OP_MAX_FULL` at line 138 and `op = rv % OP_MAX_FULL` at line 2411 in the pinned source, so no random stream records an fsync. |
| throughput or the 1.5x criterion | no claim, and none is made. `docs/design.md`'s criterion is not what this gate measures. |
| `clone_range`, `copy_range`, `write_atomic` divergence attribution | not in the capability map, deliberately, so a divergence there would be a FAIL. |

## Borrowed state, untouched

The builder's private daemon `task-g4/private/daemon.pid` was `1209860` with starttime `6259868`
when I began and was `1209860` with starttime `6259868` when I finished.
I never signalled it, never read its mount, and never wrote into its fixtures.
I used its binaries as read-copied inputs and verified the digests.

The g5 mount and the builder's mount were present throughout and still are.
My own daemon was started, used and stopped; identity was checked before the signal, the pid is
gone, my mount row is gone, and my evidence is preserved rather than deleted.

## Evidence

Canonical report: `docs/reviews/mounted-fsx-g4-repair-final.md`, this file.
Reviewed source archived at sha256 under
`bench/out/fsx-g4-repair-critic/source/`, with `logs/REVIEWED_SHA.txt` naming the exact commit and
`logs/archived-source-sha256.txt` listing every archived file.
Reviewed SHA: `ff45b0a3c6ab1e6fcfd94bf0fdf733812a1fc38c`.

Remote, all under my own namespace:
`/home/moonscape/cowfs-ready-wave/task-g4-review/harness-ff45b0a/` is the reviewed harness.
`/home/moonscape/cowfs-ready-wave/task-g4-review/attempt-ff45b0a/` is my store, socket, mount and
evidence, including `out/sample/cases.jsonl`, `evidence/mutations.json`,
`evidence/fallocate-matrix-ff45b0a.txt` and the case records from every control.

Evidence and claims are kept apart on purpose.
The raw records are gitignored and are what I measured from.
The document's claims I checked against those records and report the ones that disagree.

No commit, no push, no merge, no lease return. PR 102 stays open.