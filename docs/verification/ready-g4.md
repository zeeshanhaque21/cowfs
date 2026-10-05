# Gate g4: matched fsx acceptance on a real mounted cowfs

PR 102, branch `verify/fsx-g4`.
This is the canonical document and it lives in the main checkout.
`docs/verification/evidence/fsx-g4-repair.md` holds the control table.

Reviewed at `f816b5e96624967f16a28444a0631ddc9672892b` by `docs/reviews/mounted-fsx-g4-final.md`.
The review reproduced the earlier PASS on its own private mount and then found seven faults in the
harness, four of which could turn a wrong run into a pass.
The PASS at that SHA is therefore not relied on.
The gate was repaired, the repairs were measured again on the builder's own private mount, and the
numbers below are the ones the repaired runner produced.

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

## The tool

| | |
| --- | --- |
| upstream | `https://git.kernel.org/pub/scm/fs/xfs/xfstests-dev.git` |
| ref | `v2026.09.22`, tag object `3ea00236e9ce5bb7770a67e078de770ddc0315a0` |
| commit | `22348afe338c0f6d540c0d7ef0db749eaa51b218` |
| path | `ltp/fsx.c`, sha256 `871575069de4dd749c69779e850c66fa9dfcaa56d3c8f1a1b49ff8de13aa735f` |
| `src/global.h` | `7513204113b9d256e87922ad884dc27843f1504828095281374c87f969c6c1a9` |
| `src/statx.h` | `3e1d287ab9c45dce4db06a7dd5318b76456a1278e55ea4b50ea0654a8945d3d0` |
| config header | `db7b634c9e73de553f7bf4c31271b25642a2a557464a373e3b0975389e29b9de` |
| binary | `dc93eda70a3d0d445fb50ae55446b9f6164d6a77fcf79d247f2021887c43e9af` |
| compiler | `cc (Debian 12.2.0-14+deb12u1) 12.2.0`, aarch64 |
| host | `Linux-6.12.109+rpt-rpi-2712-aarch64-with-glibc2.36` |

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
The number matters only as evidence that what the gate can exercise is a fact about the binary.

There is no darwin build of this source: `ltp/fsx.c` includes `<linux/mman.h>` unconditionally.
So the measured arm is the Linux FUSE mount and the macOS NFS mount is not covered by this gate.

## The measured run

One bounded run on the builder's own private mount, under the wave's Linux heavy lock.
Raw evidence: `bench/out/ready-g4/repair-batch/`, which is gitignored because it holds the copied
per-case files.

| | |
| --- | --- |
| started, finished | 2026-10-04T19:25:04-07:00, about 4m50s |
| exit | 3, UNMEASURABLE |
| cases | 30, which is 15 pairs on both arms |
| pairs | 12 PASS, 0 FAIL, 3 UNMEASURABLE |
| arms | every fsx exit 0 on both arms, every op count the declared one |
| restart leg | daemon generation 1191050 to 1209860, 15 files read back by a separate process, 0 problems |
| bytes written per arm | 3018150 cowfs, 2904582 native, against a declared budget of 7864320 |

Every matched pair is byte-identical and stream-identical.

| mode | seed | ops | native sha256 | cowfs sha256 | stream | status |
| --- | --- | --- | --- | --- | --- | --- |
| smoke | 1 | 200 | `d0fe6b0f16f0` | `d0fe6b0f16f0` | same | PASS |
| matched | 1 | 20000 | `a0f8ba4f9829` | `a0f8ba4f9829` | same | PASS |
| matched | 2 | 20000 | `affc7bb0fe32` | `affc7bb0fe32` | same | PASS |
| matched | 3 | 20000 | `f83d1e25ed70` | `f83d1e25ed70` | same | PASS |
| matched | 5 | 20000 | `57d35466347a` | `57d35466347a` | same | PASS |
| matched | 8 | 20000 | `aa8469375411` | `aa8469375411` | same | PASS |
| matched | 13 | 20000 | `f9e70376df13` | `f9e70376df13` | same | PASS |
| matched | 21 | 20000 | `33cb1d6a7257` | `33cb1d6a7257` | same | PASS |
| matched | 34 | 20000 | `094764ee3775` | `094764ee3775` | same | PASS |
| matched | 55 | 20000 | `528d3640e3cd` | `528d3640e3cd` | same | PASS |
| matched | 89 | 20000 | `75019d232bb7` | `75019d232bb7` | same | PASS |
| sync | 7 | 10000 | `54f1bd3d92c4` | `54f1bd3d92c4` | same | PASS |
| full | 1 | 10000 | `ba2b13e21734` | `01b243774851` | differs at operation 1 | UNMEASURABLE |
| full | 2 | 10000 | `b822b4d8212f` | `ae76029c0c9b` | differs at operation 6 | UNMEASURABLE |
| full | 3 | 10000 | `ac6e97a10b76` | `ece487f0bec6` | differs at operation 10 | UNMEASURABLE |

The matched modes disable the fallocate family on **both** arms.
That is a declared subset of fsx's capability set, and it is what makes the two operation streams
identical, which is the strongest thing the gate can check: the same operations, in the same order,
against the same file, produce the same bytes.

## The full mode is UNMEASURABLE, and that is the answer

The mount has no `fallocate`.
`bench/fsx-gate/fallocate-matrix.py` records 75 of 75 mode, offset and length combinations answering
`ENOTSUP` on the mount where the same 75 answer `ok` on the native `ext4` control.
That is `crates/cowfs-fuse`, the FUSE conformance lane's surface, reproduced and filed as issue 103.

So in fsx's own default mix the cowfs arm records `skip fallocate` where the native arm records
`fallocate`, and every operation after that point moves with the file's offsets and lengths.
The gate locates that first divergence and reports it, per seed:

- seed 1: operation 1, native `fallocate` against cowfs `skip fallocate`
- seed 2: operation 6, the same operation
- seed 3: operation 10, the same operation

The read, write, mapread, mapwrite and truncate differences that follow are consequences of that
skip, not independent differences, and the run records all of them verbatim on each pair.
Two arms that did different work produce two files that cannot be compared, so fsx exiting 0 on both
is not execution equivalence and the pair is UNMEASURABLE.
It is not a pass and it is not a failure.
It stays UNMEASURABLE until the mount implements fallocate.

One earlier version of this gate counted operation totals, found the byte-bearing deltas
unexplained and reported FAIL, which reads as a `cowfs` defect it cannot prove.
The attribution is now mechanical and comes from the tool's own record: fsx writes every operation it
attempts, and an operation the filesystem refuses is written as `skip <op>`.
Where the two recorded streams first disagree is therefore readable, not inferred.

fsx keeps only the last `LOGSIZE` operations in the file it records, `LOGSIZE` being 10000 at
`ltp/fsx.c` line 79.
A run longer than that leaves a tail, and where two arms part company cannot be located in a tail.
The gate treats that as UNMEASURABLE with the length stated, never as a pass, which is why the full
mode runs at 10000 operations.
The matched modes stay at 20000 and rely on the digest comparison, which covers the whole run.

## What the repaired gate refuses

1. a cowfs arm that is not a cowfs mount, by filesystem type from the kernel's mount table, never by
   path label
2. a native control that is itself a cowfs mount, or that shares a device with the cowfs arm
3. an arm whose data file has no device, no realpath or no filesystem-type witness
4. a nonzero fsx exit, an executable that cannot be spawned, a missing op count, an op count that is
   not the declared one
5. an empty, missing or unreadable result
6. a data file over the declared `-l` cap, and an arm over the declared byte budget
7. an operation stream that differs in a mode declared to run one mix, with no exception
8. an operation stream that differs at an operation nothing says the filesystem lacks
9. a fresh open, by a separate process, that reads different bytes or a different size
10. a restart that leaves the daemon's pid and start time unchanged, or that changes the store, the
    socket, the mount or the daemon binary
11. a mount that stops being attested after the restart
12. a reused case directory, and any result file that was not produced by this invocation
13. an fsx binary that is not the one the approved manifest names

## Performance: no claim

Nothing here is a throughput or ratio claim.
The raw per-case seconds are in `bench/out/ready-g4/repair-batch/run.log`: at 20000 operations the
matched seeds ran 7.1 to 8.6 s on native and 9.3 to 11.1 s on cowfs, and the sync seed ran 10.3 s
and 13.1 s, on a host with other workers and other cowfs daemons running.
The review recorded re-runs of 24.2 to 30.1 s against 8.9 to 9.0 s on a host running two other
cowfs daemons and several workers.
Neither is a quiet measurement and neither is used as one.
The 1.5x criterion in `docs/design.md` is not what this gate measures, and no claim here touches it.

## Byte budget

`max_file_bytes` 262144 and `max_op_bytes` 16384 reach fsx as `-l` and `-o`, and the runner reports
a data file larger than `max_file_bytes`.
`max_bytes_written_per_arm` is this gate's own whole-run budget: 15 seed pairs times two arms times
the file cap, which is 7864320 bytes per arm.
The measured run wrote 3018150 on cowfs and 2904582 on native, both under it, and an over-budget run
is reported FAIL after the fact because the per-file cap is what fsx enforces and the total is a
budget rather than a limit fsx knows about.
Nothing is deleted to fit a budget.

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
A hook that does nothing fails.

`fsx` never records an `fsync` operation, so no operation count in this run is an fsync count.

## Verification

- 67 gate unit tests, 103 through CI discovery, one skipped where `/proc` is absent
- 4 mutation controls against the private mount, 0 leaks: a non-cowfs second arm exits 3, a synthetic
  child instead of the pinned binary exits 3, a child that writes nothing over stale bytes exits 1,
  and a no-op restart hook exits 1
- the honest paired run above

`bench/fsx-gate/mutations.py` runs the controls.
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
python3 harness/fallocate-matrix.py $D/native $D/private/mnt/base
sh harness/cowfs-mount.sh stop $D/private
```

Exit codes: 0 PASS, 1 FAIL, 2 usage, 3 UNMEASURABLE.

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
and a compare whose two data files report the same device fails.