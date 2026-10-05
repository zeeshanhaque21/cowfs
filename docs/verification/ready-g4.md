# Gate g4 repair: fsx acceptance, after independent review

Status: the measured result and the repaired gate are below.
This is the canonical document, written in the main checkout.
PR 102, branch `verify/fsx-g4`.

Reviewed at `f816b5e96624967f16a28444a0631ddc9672892b` by `docs/reviews/mounted-fsx-g4-final.md`.
The review reproduced the measured result on its own private mount and then found seven faults in
the harness. Four of them could turn a wrong run into a pass, so the verdict at that SHA is not
relied on: the repairs below re-measure on the builder's own private mount.

## What the gate asks

One real upstream `fsx` binary, one declared flag set, one seed per case, one op count, two arms
whose only difference is the directory.

- native: `/home/moonscape/cowfs-ready-wave/task-g4/native` on `ext4`
- cowfs: `/home/moonscape/cowfs-ready-wave/task-g4/private/mnt/base` on `fuse.cowfs`, served by
  `cowfs-daemon --backend core` with its own store, socket, mount and pid file

This is mounted acceptance. `crates/cowfs-core/tests/critic.rs`'s `fsx` test runs over `Core`
in process, and `crates/cowfs-fuse/tests/battery/fsx.c` is a 107-line exerciser of this repo's
own, neither of which is the tool against a mount. `bench/mount.sh` mounts a `PathVfs` over the
macOS NFS loopback, whose backing store is a plain directory, so that measures the path backend
rather than `cowfs-core`.

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
`ltp/fsx.c`. The source is unmodified. The compile line is xfstests' own from
`include/builddefs.in` plus `-I src`, plus `-include getopt.h -include linux/kernel.h` because
`ltp/fsx.c` calls `getopt_long` and `roundup` without including their headers. `-DXFS` is absent
because without xfsprogs headers the `-x` preallocation block will not compile, and this gate never
passes `-x`.

The binary's own usage text lists the flags it actually compiled in, and `fsx-gate.json` carries
the manifest: expected binary digest, the three source digests, the compile line and the config
header digest. The runner refuses a binary that does not match before it runs anything.
`--allow-unpinned-fsx` exists for a mutation control and stamps its own record as not an
acceptance claim.

The count of flags in the usage text is 42, not the 38 an earlier draft of this document said.
The earlier draft was written against a list of 23 that the meta row had recorded before a regex
fix; the corrected extraction reads 42. The number matters only as evidence that what the gate
can exercise is a fact about the binary.

## What the repaired gate refuses

1. a cowfs arm that is not a cowfs mount, by filesystem type from the kernel, never by path label
2. a native control that is itself a cowfs mount, or that shares a device with the cowfs arm
3. an arm whose data file has no device, no realpath or no filesystem-type witness
4. a nonzero fsx exit, a missing op count, an op count that is not the declared one
5. an empty, missing or unreadable result
6. a data file over the declared `-l` cap, and an arm over the declared byte budget
7. an operation stream that differs in a mode declared to run one mix, with no exception
8. an operation stream that differs in a capability mode with any difference a capability does not
   account for, including any difference in the operations that carry bytes
9. a fresh open, by a separate process, that reads different bytes or a different size
10. a restart that leaves the daemon's pid and start time unchanged, or that changes the store,
    the socket, the mount or the daemon binary
11. a mount that stops being attested after the restart
12. a reused case directory, and any result file that was not produced by this invocation
13. an fsx binary that is not the one the approved manifest names

## Scoped result

Repaired gate, builder's own private mount, one bounded run. Not the full batch.

| what | value |
| --- | --- |
| mode smoke, seed 1, 200 ops | see the run table below |
| mode sync, seed 7, 10000 ops, `-y` | see the run table below |
| restart leg | one real daemon generation change, verified by pid and start time |
| `full` mode | UNMEASURABLE by construction on this mount, issue 103 |

The `full` mode cannot pass and is not claimed to.
75 of 75 fallocate combinations answer `ENOTSUP` on the mount where `ext4` answers `ok`, so fsx
records a different operation stream on each arm, the two files are not comparable, and fsx
exiting 0 on both is not execution equivalence.
The gate reports that pair UNMEASURABLE and says which operations differed.
That is the honest verdict and it stays that way until the mount implements fallocate.

## Performance: no claim

Nothing here is a throughput or ratio claim.
The measured run at the earlier SHA recorded 9.35 to 10.30 s on the cowfs arm against 7.00 to
8.06 s native at 20,000 operations, on a host with other workers and other cowfs daemons
running. The review's own re-runs on a busier host were 24.2 to 30.1 s against 8.9 to 9.0 s.
Neither is a quiet measurement and neither is used as one.
The 1.5x success criterion in `docs/design.md` is not what this gate measures, and no gate claim
here touches it.

## Byte budget

`max_file_bytes` 262144 and `max_op_bytes` 16384 reach fsx as `-l` and `-o`, and the runner
refuses a data file larger than `max_file_bytes`.
`max_bytes_written_per_arm` is this gate's own whole-run budget: 15 seed pairs times two arms
times the file cap, which is 7864320 bytes per arm for the declared batch.
A run that exceeds it is reported FAIL after the fact, because the per-file cap is what fsx
enforces and the total is a budget rather than a limit fsx knows about.
Nothing is deleted to fit a budget.

## Durable write path, and what "restart" does not mean

Three things cover the write path: the `sync` mode's `-y`, the `fsync_readback` probe, and a
readback after the daemon has been stopped and started on the same store.

That is a clean reopen of a live store.
It is not a durability acknowledgement, not a crash injection and not a power-loss result.
Those belong to the crash lane.

The restart leg now requires a real generation change: pid and start time before and after,
compared, plus the store, socket, mount and daemon binary digest, plus a fresh mount attestation,
plus each file's digest and size read by a separate process after the hook returns.
A hook that does nothing fails.

## Ownership

The lane owns `bench/fsx-gate/**`, `bench/test_fsx_gate.py`, this document and
`docs/verification/evidence/fsx-g4-repair.md`.
No production source changed at any point.

- The fallocate `ENOTSUP` gap is `crates/cowfs-fuse`, the FUSE conformance lane's surface (#45,
  slot 8). Reproduced, filed as issue 103, not patched here.
- A torn or stale read on a mounted run would be #45's reproduction, not a gate fix.
- `crates/cowfs-daemon/src/import.rs`, the namespace durability work and the shutdown server
  belong to their own lanes and were not touched.
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
python3 harness/fallocate-matrix.py $D/native $D/private/mnt/base
sh harness/cowfs-mount.sh stop $D/private
```

Unit tests, no mount and no root required:

```sh
python3 -m unittest bench.test_fsx_gate -v
python3 -m unittest discover -s bench -v
```

CI runs `python3 -m unittest discover -s bench -v`, which finds `bench/test_fsx_gate.py`, which
loads the gate's own test module by path and adds no assertion of its own.
The CI workflow file itself was not edited.

## The false pass this gate exists to kill

The first measured run at this branch reported PASS and was wrong: both arms wrote inside the
evidence directory, which lives on the native filesystem, so the "cowfs" arm never touched the
mount and matched native by construction.
The contradiction was in the same run: a direct probe reported `ENOTSUP` for `punch_hole` on the
mount while fsx's own log claimed 11 punch holes had run.

Two changes fixed it, and the review confirmed both by removing the guard from a copy and
watching the old behaviour return: the case directory is created inside the arm root with
`O_EXCL` semantics, and a compare whose two data files report the same device fails.
