# Gate g4: matched fsx acceptance on a real mounted cowfs

PR 102, branch `verify/fsx-g4`.
This is the canonical document and it lives in the main checkout.
`docs/verification/evidence/fsx-g4-repair.md` holds the control table.

Reviewed twice: at `f816b5e96624967f16a28444a0631ddc9672892b` by
`docs/reviews/mounted-fsx-g4-final.md`, and after the repair at `ff45b0a3c6ab1e6fcfd94bf0fdf733812a1fc38c`
by `docs/reviews/mounted-fsx-g4-repair-final.md`.
The first review found seven faults in the harness, four of which could turn a wrong run into a pass.
The second confirmed all seven closed and found nine residual defects, all in reporting or
documentation. Those nine are repaired here.

## What the gate asks

One real upstream `fsx` binary, one declared flag set, one seed per case, one op count, two arms
whose only difference is the directory.

- native: `/home/moonscape/cowfs-ready-wave/task-g4/native`, resolved to `/` on `ext4`, device 2050
- cowfs: `/home/moonscape/cowfs-ready-wave/task-g4/private/mnt/base`, resolved to
  `/home/moonscape/cowfs-ready-wave/task-g4/private/mnt` on `fuse.cowfs`, device 171, served by
  `cowfs-daemon --backend core` with its own store, socket, mount and pid file

Both roots are attested from the kernel before a single case runs, and again for every file fsx
writes.
Nothing in this gate trusts a directory name.

This is mounted acceptance.
`crates/cowfs-core/tests/critic.rs`'s `fsx` test runs over `Core` in process, and
`crates/cowfs-fuse/tests/battery/fsx.c` is a 107-line exerciser of this repo's own, neither of which
is the upstream tool against a mount.
`bench/mount.sh` mounts a `PathVfs` over the macOS NFS loopback, whose backing store is a plain
directory, so that measures the path backend rather than `cowfs-core`.

## The tool under test

| | |
| --- | --- |
| upstream | `https://git.kernel.org/pub/scm/fs/xfs/xfstests-dev.git` |
| ref | `v2026.09.22`, tag object `3ea00236e9ce5bb7770a67e078de770ddc0315a0` |
| commit | `22348afe338c0f6d540c0d7ef0db749eaa51b218` |
| path | `ltp/fsx.c`, sha256 `871575069de4dd749c69779e850c66fa9dfcaa56d3c8f1a1b49ff8de13aa735f` |
| `src/global.h` | `7513204113b9d256e87922ad884dc27843f1504828095281374c87f969c6c1a9` |
| `src/statx.h` | `3e1d287ab9c45dce4db06a7dd5318b76456a1278e55ea4b50ea0654a8945d3d0` |
| config header | `db7b634c9e73de553f7bf4c31271b25642a2a557464a373e3b0975389e29b9de` |
| binary sha256 | `dc93eda70a3d0d445fb50ae55446b9f6164d6a77fcf79d247f2021887c43e9af` |
| compiler | `cc (Debian 12.2.0-14+deb12u1) 12.2.0`, aarch64 |
| host | `Linux 6.12.109+rpt-rpi-2712 aarch64` |

`fsx/` was removed from xfstests before the oldest pinnable tag, so the surviving upstream file is
`ltp/fsx.c`.
The source is unmodified.
The compile line is xfstests' own from `include/builddefs.in` plus `-I src`, plus
`-include getopt.h -include linux/kernel.h` because `ltp/fsx.c` calls `getopt_long` and `roundup`
without including their headers.
`-DXFS` is absent because without xfsprogs headers the `-x` preallocation block will not compile,
and this gate never passes `-x`.

`fsx-gate.json` carries the approved manifest: expected binary digest, the three source digests, the
compile line, the config header digest and the compiler.
The runner refuses a binary that does not match before it runs anything and records the check as
`tool_check` in the meta row.
`--allow-unpinned-fsx` exists for a mutation control and stamps its own record as not an acceptance
claim.

The binary's own usage text lists 42 flags it actually compiled in.
An earlier draft of this document said 38, then quoted 23.
The corrected extraction reads 42.

There is no darwin build of this source: `ltp/fsx.c` includes `<linux/mman.h>` unconditionally.
So the measured arm is the Linux FUSE mount and the macOS NFS mount is not covered by this gate.

## The subject under test

Read from the running process and the files beside it, not inferred from the branch.

| | |
| --- | --- |
| serving process | pid 1209860, start time 6259868, from `/proc/1209860/stat` field 22 |
| `cowfs-daemon` sha256 | `769cf9e124b4d59d442146ec30075c7209643380a4566fd43110aa6e93d2e338`, 6178056 bytes |
| `cowfs` CLI sha256 | `a95c5b4b51aa066723ec9352b2e522e4f4b81bc689cb4d2f990b9ca3df9caf70`, 7036672 bytes |
| FUSE artifact | none separate: `cowfs-fuse` is a library linked into `cowfs-daemon` |
| source tree built | `/home/moonscape/cowfs-ready-wave/task-g4/src`, 572 `.rs`, `.toml` and `.lock` files, tree digest `c25aec8bf42dd784f481697d94872c4cb123e84f919292361a67253d3e2a8c99` |
| compiler | `rustc 1.95.0 (59807616e 2026-04-14)` |
| build log | `task-g4/out/build-cowfs.log`, 3m08s, release profile |

Two things about that are worth stating rather than glossing.

The build tree is a read copy with no `.git`, so **no commit can be named for the binaries**.
The identification that holds is the binary digest: that is what ran, read from the running process's
own `argv[0]`, and that is what the independent review copied and compared.
The source tree digest says which tree was built; it is not a verified correspondence between that
tree and those binaries, and nothing in this gate checks one against the other after the fact.
Anyone re-verifying this measures the digests above.

The branch's own head is not the subject and is not claimed to be.
Main's head moves while the gate runs, and no production source changed at any point in this lane.

## The measured run

One bounded run on this lane's private mount, under the wave's Linux heavy lock.

| | |
| --- | --- |
| started | 2026-10-04T19:25:04-07:00, about 4m50s |
| status in the run's own record | UNMEASURABLE, 30 cases, 12 passed, 0 failed, 3 unmeasurable |
| exit code under this runner's convention at that revision | 3 |
| exit code under the convention in force now | 2 |
| arms | every fsx exit 0, every op count the declared one |
| restart leg | generation 1191050 to 1209860, 15 files read back by a separate process, 0 problems |
| bytes written per arm | 3018150 cowfs, 2904582 native |

Evidence, all of it gitignored because it holds the copied per-case files:

| what | where | what it is |
| --- | --- | --- |
| the run this document reports | `bench/out/ready-g4/repair-batch2/` | copied from `task-g4/out/repair-batch2`, the run's own `cases.jsonl`, `summary.md` and stdout as `run.log` |
| the earlier FAIL run | `bench/out/ready-g4/repair-batch-historical-fail/` | copied from `task-g4/out/repair-batch`, the same three files, with `WHAT-THIS-IS.md` |
| the exit-code measurements | `bench/out/ready-g4/repair-batch2/exit-taxonomy.json` | the eleven invocations and their process exit codes |
| the mutation controls | `bench/out/ready-g4/repair-batch2/mutations.json`, `mutations.log` | five controls and their exits |

Every table below is produced by `bench/fsx-gate/tabulate.py` from a run's own `cases.jsonl`, not
typed from a terminal:

```sh
python3 bench/fsx-gate/tabulate.py bench/out/ready-g4/repair-batch2/cases.jsonl --markdown
```

| mode | seed | ops | native sha256 | cowfs sha256 | st_dev n/c | fstype n/c | stream | divergence | status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| smoke | 1 | 200 | d0fe6b0f16f0 | d0fe6b0f16f0 | 2050/171 | ext4/fuse.cowfs | same | none | PASS |
| matched | 1 | 20000 | a0f8ba4f9829 | a0f8ba4f9829 | 2050/171 | ext4/fuse.cowfs | same | none | PASS |
| matched | 2 | 20000 | affc7bb0fe32 | affc7bb0fe32 | 2050/171 | ext4/fuse.cowfs | same | none | PASS |
| matched | 3 | 20000 | f83d1e25ed70 | f83d1e25ed70 | 2050/171 | ext4/fuse.cowfs | same | none | PASS |
| matched | 5 | 20000 | 57d35466347a | 57d35466347a | 2050/171 | ext4/fuse.cowfs | same | none | PASS |
| matched | 8 | 20000 | aa8469375411 | aa8469375411 | 2050/171 | ext4/fuse.cowfs | same | none | PASS |
| matched | 13 | 20000 | f9e70376df13 | f9e70376df13 | 2050/171 | ext4/fuse.cowfs | same | none | PASS |
| matched | 21 | 20000 | 33cb1d6a7257 | 33cb1d6a7257 | 2050/171 | ext4/fuse.cowfs | same | none | PASS |
| matched | 34 | 20000 | 094764ee3775 | 094764ee3775 | 2050/171 | ext4/fuse.cowfs | same | none | PASS |
| matched | 55 | 20000 | 528d3640e3cd | 528d3640e3cd | 2050/171 | ext4/fuse.cowfs | same | none | PASS |
| matched | 89 | 20000 | 75019d232bb7 | 75019d232bb7 | 2050/171 | ext4/fuse.cowfs | same | none | PASS |
| sync | 7 | 10000 | 54f1bd3d92c4 | 54f1bd3d92c4 | 2050/171 | ext4/fuse.cowfs | same | none | PASS |
| full | 1 | 10000 | ba2b13e21734 | 01b243774851 | 2050/171 | ext4/fuse.cowfs | differs | 1: fallocate vs skip fallocate | UNMEASURABLE |
| full | 2 | 10000 | b822b4d8212f | ae76029c0c9b | 2050/171 | ext4/fuse.cowfs | differs | 6: fallocate vs skip fallocate | UNMEASURABLE |
| full | 3 | 10000 | ac6e97a10b76 | ece487f0bec6 | 2050/171 | ext4/fuse.cowfs | differs | 10: fallocate vs skip fallocate | UNMEASURABLE |

The matched modes disable the fallocate family on **both** arms.
That is a declared subset of fsx's capability set, and it is what makes the two operation streams
identical, which is the strongest thing the gate can check: the same operations, in the same order,
against the same file, produce the same bytes.

## The earlier FAIL run is kept, and it is a different run

`task-g4/out/repair-batch` started 2026-10-04T19:10:47-07:00 and finished FAIL: 30 cases, 12 passed,
**3 failed**.
Its full-mode pairs ran at 20000 operations and its restart leg went from generation 1175979 to
1191050.
The reported run started fourteen minutes later, at 10000 operations for the full mode, and finished
UNMEASURABLE with 0 failed and its restart leg from 1191050 to 1209860.
None of their numbers match, and an earlier draft of this document pointed a reader at the FAIL run's
directory while quoting the UNMEASURABLE run's figures.

The difference is the attribution rule, and it is the honest kind.
The FAIL run counted operation totals, found the byte-bearing deltas in the full mode unexplained,
and reported FAIL.
That reads as a `cowfs` defect it cannot support, because in the full mode the mount has no
`fallocate`, fsx records `skip fallocate` where the native arm records `fallocate`, and every
operation after that point moves with the file's offsets.
The revision that followed locates the first divergence in fsx's own recorded stream and reports the
pair UNMEASURABLE.
Both records are kept and neither is edited.

## The full mode is UNMEASURABLE, and that is the answer

The mount has no `fallocate`.
`bench/fsx-gate/fallocate-matrix.py` records 75 of 75 mode, offset and length combinations answering
`ENOTSUP` on the mount where the same 75 answer `ok` on the native `ext4` control.
That is `crates/cowfs-fuse`, the FUSE conformance lane's surface, reproduced and filed as issue 103.

So in fsx's own default mix the cowfs arm records `skip fallocate` where the native arm records
`fallocate`, and every operation after that point moves with the file's offsets and lengths.
The gate locates that first divergence and reports it, per seed: operation 1, 6 and 10, each of them
that same operation.

The read, write, mapread, mapwrite and truncate differences that follow are consequences of that
skip, not independent differences, and the run records all of them verbatim on each pair.
Two arms that did different work produce two files that cannot be compared, so fsx exiting 0 on both
is not execution equivalence and the pair is UNMEASURABLE.
It is not a pass and it is not a failure.
It stays UNMEASURABLE until the mount implements fallocate.

The attribution is mechanical, and it comes from the tool's own record.
fsx writes every operation it attempts, and an operation the filesystem refuses is written as
`skip <op>`, so the two arms' recorded streams can be compared position by position and the first
operation they disagree on can be named.
If that operation is one an arm is recorded as not having, the arms did different work and the pair
is UNMEASURABLE with every delta on the record.
If it is anything else, a capability gap explains nothing from there on and the pair fails.
A different operation taking a gap operation's place is not a skip of it.

fsx keeps only the last `LOGSIZE` operations in the file it records, `LOGSIZE` being 10000 at
`ltp/fsx.c` line 79.
A run longer than that leaves a tail, and where two arms part company cannot be located in a tail.
The gate treats that as UNMEASURABLE with the length stated, never as a pass, which is why the full
mode runs at 10000 operations.
The matched modes stay at 20000 and rely on the digest comparison, which covers the whole run.

## The exit contract

The repository-wide result contract, the one `bench/compare.py` already uses:
**0 PASS, 1 FAIL, 2 UNMEASURABLE, 3 INVALID.**

This runner previously used 2 for a usage error and 3 for UNMEASURABLE, so exit 3 meant two different
things and a dispatcher reading only the code could not tell "the filesystem lacks an operation" from
"the tool or the provenance is wrong".
It now agrees with the rest of the repository.
The earlier run's recorded exit of 3 is left as it is; that was the convention in force when it ran.

What decides the code is the kind of the reason, set where the reason is produced and never parsed
back out of a message:

| kind | meaning | exit |
| --- | --- | --- |
| `unsupported` | an operation this filesystem does not have, so the arms did different work | 2 |
| `invalid` | the input, the provenance, the tool pin, an arm's identity or the evidence itself is wrong | 3 |
| `divergence` | the two arms really did something different and nothing explains it | 1 |

INVALID outranks FAIL.
If the provenance, the tool pin, an arm's identity or the evidence is wrong, then a divergence
reported alongside it is not trustworthy either, so the run cannot say which happened.
FAIL outranks UNMEASURABLE: a real divergence is FAIL even when coverage is also incomplete.

`bench/fsx-gate/exit-taxonomy.py` measures all of this through the process exit code, eleven
invocations, no predicate inside the runner and no mount needed for seven of them.
All eleven matched and all four codes were reached:

| case | exit | what produced it |
| --- | --- | --- |
| a real matched pair on both arms | 0 | smoke seed 1, 200 ops, the pinned fsx |
| a capability gap | 2 | full seed 1, the mount has no fallocate |
| the tool is not the pinned binary | 3 | `--fsx-bin /bin/sh` |
| the arm is not a cowfs mount | 3 | the cowfs arm pointed at a plain directory |
| a malformed invocation | 3 | `--mode no-such-mode` |
| a stream difference in a matched mode | 1 | smoke, the two arms recorded different operations |
| a divergence in a capability mode | 1 | full, first divergence at operation 3, native `mapwrite` against cowfs `write`, and nothing says this filesystem lacks `write` |
| a plan the per-arm total cannot hold | 1 | a per-arm total of 1000 against a batch of 15 files, refused before any child |
| over budget, native arm only | 1 | native wrote 400000 against 262144, over by 137856 |
| over budget, cowfs arm only | 1 | cowfs wrote 400000 against 262144, over by 137856 |
| over budget, both arms | 1 | both wrote 400000, each named with its own figure |

## Byte budget, and what the plan is

Two figures, kept apart because they mean different things.

- per-case maximum: `max_file_bytes` 262144, which reaches fsx as `-l`. One file per case.
- per-arm total: `max_bytes_written_per_arm` 3932160, a whole-invocation budget for one arm.

The worst case for one arm is one file per case at the per-case maximum, so for the declared batch
of 15 seed pairs it is 15 x 262144.
`max_bytes_written_both_arms` is the same figure for both arms together, 7864320.

The earlier document's per-arm figure was 15 x 2 x 262144: it multiplied the case count by the two
arms and then called the result a per-arm total.
Both measured arms are under either reading, 3018150 and 2904582 against 3932160, so the correction
changes the label and the declared budget rather than the verdict of the measured run.

The plan is derived once from the modes and seeds actually requested, not multiplied in place.
`--seeds 2,3` runs 4 modes x 2 seeds, so 8 cases per arm, and a narrowed run reports partial coverage
rather than the batch's figure.

A plan the per-arm total cannot hold is refused before an arm is even attested and long before a child
exists, with both numbers in the refusal, and nothing is deleted to make room.
Measured: a per-arm total of 1000 exits 1, writes one verdict row, creates no case row and no attempt
directory, and leaves a file the caller had put on the native arm untouched.
An unexpected exceedance at run time is still reported FAIL, per arm, naming the arm, its bytes, the
budget and the overage.

## What the repaired gate refuses

1. a cowfs arm that is not a cowfs mount, by filesystem type from the kernel's mount table, never by
   path label
2. a native control that is itself a cowfs mount, or that shares a device with the cowfs arm
3. an arm whose data file has no device, no realpath or no filesystem-type witness
4. a nonzero fsx exit, an executable that cannot be spawned, a missing op count, an op count that is
   not the declared one
5. an empty, missing or unreadable result
6. a data file over the declared `-l` cap, a plan over the declared per-arm total, and an arm over
   that total at run time
7. an operation stream that differs in a mode declared to run one mix, with no exception
8. an operation stream that differs at an operation nothing says the filesystem lacks
9. a fresh open, by a separate process, that reads different bytes or a different size
10. a restart that leaves the daemon's pid and start time unchanged, or that changes the store, the
    socket, the mount or the daemon binary
11. a mount that stops being attested after the restart
12. a reused case directory, and any result file that was not produced by this invocation
13. an fsx binary that is not the one the approved manifest names
14. a device that backs more than one mount, when the path does not say which, or a mount table that
    cannot be read

Items 1, 2, 3, 5, 10, 11, 13 and 14 are INVALID.
Items 4, 7, 8 and 9 are FAIL.
A plan over the per-arm total is FAIL, reported before anything runs.

## Mount identification

The kernel's mount table is the authority, and it is consulted twice: by path, taking the longest
containing entry, and by device.

`mountinfo_for_device` used to return the first line matching a device, which reports whichever mount
the kernel listed first rather than the one containing the file.
On the review host that returned `mountpoint "/"` for a file inside a bind mount.
It now takes the path and returns the longest containing entry among those on that device, which is
the exact mount instance that was asked for.
A device backing several mounts with no path, or with a path that none of its entries contains, is
AMBIGUOUS: every candidate is listed and no filesystem type is stated.
An unreadable table is an absence rather than an ambiguity and says so.
There is no fallback to a foreign mount, and none to the name `statfs` guesses, because `statfs`
reports the generic `fuse` for every FUSE filesystem and cannot tell cowfs from any other.

## Performance: no claim

Nothing here is a throughput or ratio claim.
The raw per-case seconds are in `bench/out/ready-g4/repair-batch2/run.log`, which is the reported run's
own stdout: at 20000 operations the matched seeds ran 7.1 to 8.6 s on native and 9.3 to 11.1 s on
cowfs, and the sync seed ran 10.3 s and 13.1 s, on a host with other workers and other cowfs daemons
running.
The review recorded re-runs of 24.2 to 30.1 s against 8.9 to 9.0 s on a host running two other cowfs
daemons and several workers.
Neither is a quiet measurement and neither is used as one.
The 1.5x criterion in `docs/design.md` is not what this gate measures, and no claim here touches it.

## Durable write path, and what "restart" does not mean

Three things cover the write path: the `sync` mode's `-y`, the `fsync_readback` probe, and a readback
after the daemon has been stopped and started on the same store.

That is a clean reopen of a live store.
It is not a durability acknowledgement, not a crash injection and not a power-loss result.
Those belong to the crash lane.

The restart leg requires a real generation change: pid and start time before and after, plus the
store, socket, mount and daemon binary digest, plus a fresh mount attestation, plus each file's
digest and size read by a separate process after the hook returns.
The measured run went from pid 1191050 to pid 1209860 and rehashed 15 files with no problems.
A hook that does nothing is INVALID, because nothing replaced the daemon.

`fsx` never records an `fsync` operation, so no operation count in this run is an fsync count.

## Verification

| measurement | value |
| --- | --- |
| gate unit tests, `python3 -m unittest bench.test_fsx_gate` | 111, one skipped where `/proc` is absent |
| branch tree alone, `python3 -m unittest discover -s bench` | 147 |
| CI at this head, run 37266237441, ubuntu and macos | 391 each |
| distinct gate test names present in the CI log | 111 |
| duplicate test names in CI discovery | 0 |
| mutation controls against the private mount | 5, 0 leaks |

The three figures are different questions and are not added together.
111 is this gate's own module.
147 is the branch tree in isolation, which is what a reader reproduces locally.
391 is what CI reported, on a merge commit that also collects the other lanes' bench tests.

The five controls, all named because a count without names is not a list: 5d, the cowfs arm pointed at
a directory that is not a cowfs mount; 5f, the native arm pointed at the cowfs mount; 16, a synthetic
child instead of the pinned binary; 19, a child that writes nothing over a stale case directory; and
17, a no-op restart hook.
`bench/fsx-gate/mutations.py` runs them.
A control that comes back PASS is a defect in the gate, not a control that failed.

No production source changed at any point.

## Ownership

The lane owns `bench/fsx-gate/**`, `bench/test_fsx_gate.py`, this document and
`docs/verification/evidence/fsx-g4-repair.md`.

- The fallocate `ENOTSUP` gap is `crates/cowfs-fuse`, the FUSE conformance lane's surface. Reproduced,
  filed as issue 103, not patched here.
- A torn or stale read on a mounted run would be the FUSE conformance lane's reproduction, not a gate
  fix.
- `crates/cowfs-daemon/src/import.rs`, the namespace durability work and the shutdown server belong
  to their own lanes and were not touched.
- Shared daemon 15263 on the Mac and every other worker's mount and store are not test fixtures.
  Every run here uses its own store, socket, mount and pid under
  `/home/moonscape/cowfs-ready-wave/task-g4/`.
- The private daemon this document reports on, pid 1209860 with start time 6259868, was left running
  and untouched throughout the R1 to R9 repair: not signalled, not restarted, not unmounted, and
  never written into by anything but this lane's own fsx cases.

## Reproducing

```sh
D=/home/moonscape/cowfs-ready-wave/task-g4
sh harness/build-cowfs.sh $D/src $D/target /home/moonscape/cowfs-ready-wave/linux-heavy.lock
sh harness/build-fsx.sh $D/fsx-work
sh harness/cowfs-mount.sh start $D/private
cowfs --socket $D/private/rt/control.sock snapshot create base
sh harness/locked-run.sh /home/moonscape/cowfs-ready-wave/linux-heavy.lock \
  python3 harness/run-fsx-gate.py \
    --native-root $D/native --cowfs-root $D/private/mnt/base \
    --fsx-bin $D/fsx-work/build/fsx --out $D/out/batch \
    --daemon-pid-file $D/private/daemon.pid --expect-backend core \
    --expect-native-fstype ext4 \
    --restart-cmd "sh $D/harness/cowfs-mount.sh restart $D/private"
python3 harness/mutations.py --runner harness/run-fsx-gate.py --config harness/fsx-gate.json \
    --work $D/mutations --fsx $D/fsx-work/build/fsx --native-root $D/native \
    --cowfs-root $D/private/mnt/base --daemon-pid-file $D/private/daemon.pid
python3 harness/exit-taxonomy.py --runner harness/run-fsx-gate.py --config harness/fsx-gate.json \
    --work $D/taxonomy --fsx $D/fsx-work/build/fsx --native-root $D/native \
    --cowfs-root $D/private/mnt/base --daemon-pid-file $D/private/daemon.pid
python3 harness/tabulate.py $D/out/batch/cases.jsonl --markdown
python3 harness/fallocate-matrix.py $D/native $D/private/mnt/base
sh harness/cowfs-mount.sh stop $D/private
```

Exit codes: 0 PASS, 1 FAIL, 2 UNMEASURABLE, 3 INVALID.

Unit tests, no mount and no root required:

```sh
python3 -m unittest bench.test_fsx_gate -v
python3 -m unittest discover -s bench -v
```

CI runs `python3 -m unittest discover -s bench -v`, which finds `bench/test_fsx_gate.py`, which
loads the gate's own test module by path and adds no assertion of its own.
The CI workflow file itself was not edited.

## The false pass this gate exists to kill

The first measured run on this branch reported PASS and was wrong.
Both arms wrote inside the evidence directory, which lives on the native filesystem, so the "cowfs"
arm never touched the mount and matched native by construction.
The contradiction was inside the same run: the runner's own probe reported `ENOTSUP` for
`punch_hole` on the mount while fsx's log claimed 11 punch holes had run.

Two changes fixed it, and the review confirmed both by removing each guard from a copy and watching
the old behaviour return: the case directory is created inside the arm root with `O_EXCL` semantics,
and a compare whose two data files report the same device is refused.