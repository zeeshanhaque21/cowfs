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
| config header | `bench/fsx-gate/fsx-config-linux.h`, sha256 recorded in `identity.json` |
| compiler | `cc (Debian 12.2.0-14+deb12u1) 12.2.0`, aarch64 |
| binary sha256 (Linux) | recorded in `identity.json` and in every run's meta record |
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

Compiled in and compiled out, read from the binary's own usage text rather than assumed:
`-F -H -z -Y -C -I -u` are present, so the fallocate family is compiled in; `-A` and `-U` are
absent, so no libaio and no liburing; `-E` is absent, so no `copy_file_range`; extended
attributes are not compiled in. None of those absences is a cowfs claim.

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

See "Measured run" below.

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
| `test_run_fsx_gate.py` | 48 unit tests over the parsing, the probes and the verdict |

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
