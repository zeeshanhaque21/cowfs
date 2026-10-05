# Gate g4: matched fsx acceptance on a real mounted cowfs

Status: see "Result" below.

This is the wave g4 gate from `docs/ready-wave-dispatch.md`.
It is not the g4 in `docs/v1-benchmarks.md`, which is the tree-walk gate with no bar.

## What the gate asks

One real `fsx` binary, one declared set of flags, one seed per case, one op count, two arms whose
only difference is the directory:

- native: `/home/moonscape/cowfs-ready-wave/task-g4/native` on ext4 (`/dev/sda2`)
- cowfs: `/home/moonscape/cowfs-ready-wave/task-g4/private/mnt/base` on `fuse.cowfs`

Native is the control, so the bar is not "the mount did not crash".
It is: for the same declared inputs, the mount produces exactly what native produces, and `fsx`
itself finds nothing wrong on the mount.

This is mounted acceptance. `crates/cowfs-core/tests/critic.rs`'s `fsx` test and
`crates/cowfs-fuse/tests/battery/fsx.c` are library and model arms over `Core` and `MemVfs`;
neither runs the tool against a mount, and neither is evidence for this gate.
The `PathVfs` over the macOS NFS loopback that `bench/mount.sh` sets up is a model mount too:
its backing store is a plain directory, so it measures the path backend, not `cowfs-core`.

## The tool

Upstream `fsx` from xfstests, unmodified.

| | |
| --- | --- |
| upstream | `https://git.kernel.org/pub/scm/fs/xfs/xfstests-dev.git` |
| ref | `v2026.09.22` (tag object `3ea00236e9ce5bb7770a67e078de770ddc0315a0`) |
| commit | `22348afe338c0f6d540c0d7ef0db749eaa51b218` |
| path | `ltp/fsx.c` |
| `ltp/fsx.c` sha256 | `871575069de4dd749c69779e850c66fa9dfcaa56d3c8f1a1b49ff8de13aa735f` |
| `src/global.h` sha256 | `7513204113b9d256e87922ad884dc27843f1504828095281374c87f969c6c1a9` |
| `src/statx.h` sha256 | `3e1d287ab9c45dce4db06a7dd5318b76456a1278e55ea4b50ea0654a8945d3d0` |
| config header | `bench/fsx-gate/fsx-config-linux.h`, sha256 `db7b634c9e73de553f7bf4c31271b25642a2a557464a373e3b0975389e29b9de` |
| host | moonscape, `Linux-6.12.109+rpt-rpi-2712-aarch64-with-glibc2.36` |
| compiler | `cc (Debian 12.2.0-14+deb12u1) 12.2.0`, aarch64 |
| binary sha256 (Linux) | `dc93eda70a3d0d445fb50ae55446b9f6164d6a77fcf79d247f2021887c43e9af`, rebuilt to the same digest |
| fsx usage exit | 90, its own usage text |

`fsx/` was removed from xfstests before the oldest tag this gate could pin, so the surviving
upstream file is `ltp/fsx.c`, the same tool unchanged since the xfstests 4.x era.

There is one binary for both arms. `build-fsx.sh` fetches the pinned ref, verifies the three file
digests against `fsx-gate.json`, compiles, runs the binary's own usage text and writes
`identity.json`. The flags are xfstests' own for every C file, from `include/builddefs.in`, plus
`-I src`, plus `-include getopt.h` and `-include linux/kernel.h` because `ltp/fsx.c` calls
`getopt_long` and `roundup` without including their headers. `-DXFS` is deliberately absent:
without xfsprogs headers it turns the `-x` preallocation block into a compile error, and this
gate never passes `-x`. The source is not modified.

### Why the gate runs on Linux only

`ltp/fsx.c` includes `<linux/mman.h>` at line 23 and `<sys/syscall.h>` at line 42, both
unconditional and outside any `#ifdef`. There is no darwin build of this source, and the older
`fsx/` tree that did build on macOS is gone from the pinned history. So this gate measures the
Linux FUSE mount, and the macOS NFS loopback mount is not covered by it. The harness itself is
platform neutral: `mount_identity` reads `/proc/mounts` on Linux and `mount(8)` elsewhere, the
lock wrapper and the runner are the same files, and only the tool build and the config header are
Linux specific.

The 38 flags this binary actually compiled in, read out of its own usage text:

```
C D F H I K L N O P R S T W X Y Z a b c d e f g h i j k l m n o p q r s t u w x y z
```

`-F -H -z -Y -C -I -u` are in it, so the fallocate family is compiled in.
Absent from it: `-A` and `-U`, so no libaio and no liburing; `-E`, so no `copy_file_range`;
`-J` and `-B`, so no clone range and no dedupe range; `-0`, so no exchange range.
Extended attributes are not compiled in either. None of those absences is a cowfs claim.

## Declared modes

| mode | flags | seeds | ops | stream |
| --- | --- | --- | --- | --- |
| smoke | `-f -F -H -z -Y -C -I -u` | 1 | 200 | must match |
| matched | `-f -F -H -z -Y -C -I -u` | 1 2 3 5 8 13 21 34 55 89 | 20000 | must match |
| sync | the same plus `-y` | 7 | 10000 | must match |
| full | `-f` | 1 2 3 | 20000 | may differ, and every difference must trace to a capability |

Byte caps: `-l 262144` is the maximum file and `-o 16384` the maximum operation, on both arms.
`-f` flushes and invalidates the cache after every I/O, so the arms read through the filesystem
rather than through the page cache.

The fallocate family is disabled on **both** arms in the three matched modes, and only there.
The mount answers `ENOTSUP` to every fallocate mode, measured below, so leaving them enabled
would make `fsx` record different operation streams on the two arms and there would be nothing
left to compare byte for byte. Disabling them on both arms keeps the arms matched; disabling
them on the cowfs arm alone would have made the mount look worse than it is.

## What each run checks

1. the two arms are on different filesystems, by `st_dev` on the data file
2. both arms exit 0
3. executed operations equal the declared op count on both arms, from fsx's own
   "All N operations completed A-OK!"
4. in a matched mode, the recorded operation stream (`.fsxops`) is byte-identical
5. in a matched mode, the data file is byte-identical, size and sha256
6. in the `full` mode, every stream difference traces to a capability one arm is recorded as
   lacking, and the bytes are reported but not compared
7. a fresh process (the runner, after fsx has exited) opens the cowfs file and reads the same
   sha256
8. after the daemon is stopped and started again on the same store, every cowfs data file still
   hashes the same
9. no required operation type is missing without a recorded capability reason
10. an empty or missing result never passes

`LOGSIZE` is 10000, so for a 20,000 operation run the `.fsxops` file is the last 10,000
operations. Both arms dump the same window of the same stream, so the comparison holds; the
authoritative operation count is fsx's own line, not the ops file.

## Capability probe

`run-fsx-gate.py` probes each arm directly before any run: `fallocate`, `keep_size`,
`punch_hole`, `zero_range`, `truncate_shrink`, `truncate_grow`, `truncate_to_hole`, `mmap_rw` and
`fsync_readback`, each with its own errno. `fsx` probes the fallocate modes itself and prints one
line per mode it disables; the direct probe is what turns "fsx said so" into evidence, and it
covers truncate, hole, mmap and fsync readback, which `fsx`'s own mix only exercises
statistically.

### fallocate on this mount

`bench/fsx-gate/fallocate-matrix.py` runs every mode against every offset, length and file state
on both roots. On moonscape's ext4 all 75 combinations succeed. On the `fuse.cowfs` mount all 75
return `ENOTSUP`, on an empty file, a sparse file and a fully written one, at offset 0, 4096,
4096+1 and past EOF, for `allocate`, `keep_size`, `punch_hole`, `zero_range_keep` and
`zero_range`.

That is a missing Linux extension, not a POSIX violation: `fallocate`, `punch_hole` and
`zero_range` are not in POSIX, and `docs/design.md`'s full-POSIX list does not include them. It
is still a gap against a native Linux filesystem, it is what makes the `full` mode's streams
diverge, and it is filed as its own issue rather than folded into this gate's verdict.

Holes through `ftruncate` do work on the mount: `truncate_to_hole` passes on both arms, with the
gap reading as zeros and `st_blocks * 512 == 0` after extending to 1 MiB.

## Result

PASS, on a real `fuse.cowfs` mount served by `cowfs-daemon --backend core`.

30 cases, 15 seeds, every arm exit 0, no failures.
Runner exit 0. The batch ran inside the Linux heavy lock, one foreground invocation, 4 m 19 s
wall from 17:00:07 to 17:04:26 on 2026-10-04.

| what | value |
| --- | --- |
| fsx binary sha256 | `dc93eda70a3d0d445fb50ae55446b9f6164d6a77fcf79d247f2021887c43e9af` |
| native root | `/home/moonscape/cowfs-ready-wave/task-g4/native`, `ext4` on `/dev/sda2`, `st_dev 2050` |
| cowfs root | `/home/moonscape/cowfs-ready-wave/task-g4/private/mnt/base`, `fuse.cowfs`, `st_dev 234` |
| ops executed | 200 on the smoke seed, 10000 on the sync seed, 20000 on each of the other 13 seeds; both arms, every seed |
| bytes | largest data file 262144, which is the declared `-l` cap; 2915185 bytes total across the cowfs arm's cases |
| matched modes | 12 of 15 compares: operation stream byte-identical and data byte-identical |
| full mode | 3 of 15 compares: stream differs, every difference traced to `punch_hole` and `zero_range` |
| restart leg | exit 0, 15 cowfs files rehashed after the daemon was stopped and started, 0 problems |
| fresh open | every compare re-opened the cowfs file from the runner's own process and read the same sha256 |
| evidence copies | every copied file's digest matched the file on its filesystem |
| unit tests | 49 pass, no mount required |

Per-seed results are in `bench/out/ready-g4/batch/summary.md`, and every case, probe, compare and
verdict record is one line of `bench/out/ready-g4/batch/cases.jsonl`.

Ten matched seeds, 20000 operations each, sha256 of the data file on both arms:

| seed | 1 | 2 | 3 | 5 | 8 | 13 | 21 | 34 | 55 | 89 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| prefix | a0f8ba4f9829 | affc7bb0fe32 | f83d1e25ed70 | 57d35466347a | aa8469375411 | f9e70376df13 | 33cb1d6a7257 | 094764ee3775 | 528d3640e3cd | 75019d232bb7 |
| native equals cowfs | yes | yes | yes | yes | yes | yes | yes | yes | yes | yes |

The sync seed 7 is `54f1bd3d92c4` on both arms at 10000 operations, and the smoke seed 1 is
`d0fe6b0f16f0` on both at 200.

Operation mix actually exercised, matched seed 1, 20000 operations, same counts on both arms:
1107 read, 1099 write, 579 mapread, 546 mapwrite, 533 truncate, and 6136 operations `fsx` itself
skipped as not applicable at the offset and length it picked. `mmap` and `truncate` are therefore
covered on the mount, by `fsx`'s own op stream, and `holes` through `ftruncate` are covered by the
direct probe.

### The three full-mode compares

`fsx`'s default mix on ext4 records `fallocate` 583, `punch_hole` 543, `zero_range` 552,
`collapse_range` 399 and `insert_range` 350 in seed 1.
On the mount every one of those is 0 and `skip` rises from 3708 to 6136.
`read`, `write`, `mapread`, `mapwrite` and `truncate` differ by a few tens of operations because
the file's size history is different once the fallocate operations stop changing it; those are
recorded as `consequent_deltas`, not as capability gaps, and the bytes are not compared because
the two arms did different work.
`fsx` named the reason itself, once per mode:

```
filesystem does not support fallocate mode FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, disabling
filesystem does not support fallocate mode FALLOC_FL_ZERO_RANGE, disabling
```

The runner's own probe agrees, and the 75-row matrix above agrees with both.


## Harness, and the false pass it caught

`bench/fsx-gate/`:

| file | what it is |
| --- | --- |
| `fsx-gate.json` | pinned tool identity, declared modes, seeds, op counts, caps, accept rules |
| `fsx-config-linux.h` | the `config.h` xfstests would generate, hand-written and commented |
| `build-fsx.sh` | fetch, verify, build, and write `identity.json` |
| `build-cowfs.sh` | the daemon and client build, through the wave lock |
| `cowfs-mount.sh` | start, stop, restart and status for one private daemon |
| `run-fsx-gate.py` | the gate |
| `fallocate-matrix.py` | the capability matrix, on any two roots |
| `locked-run.sh` | the dispatch doc's lock recipe, unchanged |
| `test_run_fsx_gate.py` | 49 unit tests over the parsing, the probes and the verdict |

The first real run reported PASS and was wrong.
`run_case` put the fsx data file in the evidence directory, which lives on the native
filesystem, so the "cowfs" arm never touched the mount and matched native by construction.
The evidence that it was wrong was in the same run: my direct probe reported `ENOTSUP` for
`punch_hole` on the mount, while fsx's own log claimed 11 punch holes had executed.

Two changes fixed it and keep it fixed:

- the case directory is created inside the arm root, so the work happens on that filesystem
- every compare fails if the two arms' data files report the same `st_dev`

The first fixed-seed run after that reported `st_dev 2050` native and `st_dev 234` on the mount,
with byte-identical results.

The unit tests cover that guard, so the false pass cannot come back quietly: two arms on one
device is a failure, an empty or missing result is a failure, a nonzero exit is a failure, and a
difference in the operation stream is only excused by a recorded capability gap on one arm.
An earlier version of that rule excused any difference smaller than the skip count, which would
have excused a stream that diverged for any reason at all; it is gone.

The batch found two more of its own reporting faults, both fixed before the measured run and both
covered by a test:

- `fsync` was in the required operation list for the matched modes. `fsx` selects
  `op = rv % OP_MAX_FULL` and `OP_FSYNC == OP_MAX_FULL`, so no random run can ever record one, and
  the batch failed on it rather than passing quietly. The coverage moved to the `sync` mode's
  `-y`, the `fsync_readback` probe and the post-restart readback.
- a capability gap quoted whichever `fsx` disable line came first. Every `punch_hole` line also
  contains `KEEP_SIZE`, so the reason printed for `punch_hole` was the `KEEP_SIZE` mode. It now
  quotes the line that names the operation's own capability.

## Ownership

This lane owns the gate harness, its fixtures, its tests and this document.
It makes no production change.

- The fallocate `ENOTSUP` gap is in `crates/cowfs-fuse`, which is the FUSE conformance lane's
  surface (#45, slot 8). It is reported with the matrix as the reproduction and filed separately,
  not patched here.
- If a mounted run ever shows a torn or stale read, that is #45's reproduction and fix, not a
  gate fix.
- `docs/v1-daemon.md`'s known limits for the macOS NFS adapter and the namespace durability work
  in `crates/cowfs-daemon/src/import.rs` belong to their own lanes. Nothing here touches them.
- The shared daemon 15263, its store, its mounts and its sockets are not test fixtures. Every run
  here uses its own store, socket, mount and pid under
  `/home/moonscape/cowfs-ready-wave/task-g4/`.

## Reproducing

```sh
# on the machine that will host the mount
D=/home/moonscape/cowfs-ready-wave/task-g4
sh harness/build-cowfs.sh $D/src $D/target /home/moonscape/cowfs-ready-wave/linux-heavy.lock
sh harness/build-fsx.sh $D/fsx-work
sh harness/cowfs-mount.sh start $D/private
cowfs --socket $D/private/rt/control.sock snapshot create base
sh harness/locked-run.sh /home/moonscape/cowfs-ready-wave/linux-heavy.lock \
  python3 harness/run-fsx-gate.py \
    --native-root $D/native --cowfs-root $D/private/mnt/base \
    --fsx-bin $D/fsx-work/build/fsx --out $D/out/batch \
    --restart-cmd "sh $D/harness/cowfs-mount.sh restart $D/private"
python3 harness/fallocate-matrix.py $D/native $D/private/mnt/base
sh harness/cowfs-mount.sh stop $D/private
```

Unit tests, anywhere, with no mount:

```sh
python3 -m unittest discover -s bench/fsx-gate -p 'test_*.py'
```
