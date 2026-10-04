# Ready g5: xfstests-generic, matched native and cowfs

Task: isolated Linux xfstests acceptance for gate g5 (`progress/plan.json`, "xfstests-generic no worse than native").
Lease: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/11/cowfs`, branch `verify/xfstests-g5`, base `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.
Everything on the Pi stayed under `/home/moonscape/cowfs-ready-wave/task-g5/`.

## Verdict

**UNMEASURABLE, prerequisite block.** No generic case can run on this host, and the reason is the suite's own startup gate, not a harness decision.
Every case sources `common/rc`, which sources `common/config`, which calls `_fatal` (an exit) when `$here/ltp/fsstress` or `$here/ltp/fsx` is missing.
Those two are C programs the tree builds from `ltp/*.c`, and every build path for them goes through autoconf-generated `include/config.h` and `include/builddefs`.
This tree ships neither, and `autoconf`, `automake`, `libtool` and `m4` are all absent from the host, so the tree's build cannot be run without installing packages.
Installing packages is out of scope for this lane, so the gate stops here rather than inventing a pass.

G5 therefore stays **OPEN**.
What is delivered is the harness that makes the answer reproducible, the reviewed allowlist it will run, and the exact evidence above.

## What ran, with real exit codes

Every number below is a real process exit code, captured from the child process itself and never from a pipeline.

| step | command | rc | result |
|---|---|---|---|
| suite build | `make` in the xfstests tree | not attempted | `include/builddefs` absent, no `configure`, no autoconf |
| preflight | `xfstests_gate.py preflight --xfstests ref/xfstests` | 2 | UNMEASURABLE, two blocking entries |
| preflight probe | the suite's own `generic/010`, run by the harness | 1 | `fsstress not found or executable` |
| one real allowlisted case, native arm | `generic/005`, `./tests/generic/005` from the tree root | 1 | `fsstress not found or executable` |
| one real allowlisted case, cowfs arm | `generic/005`, same argv, `TEST_DIR` inside the private cowfs mount | 1 | `fsstress not found or executable` |
| classify | `xfstests_gate.py classify --xfstests ref/xfstests` | 0 | 802 cases classified, allowlist drift none |
| harness tests | `python3 -m unittest discover -s bench` | 0 | 69 tests (36 pre-existing, 37 new) |

Both arms refused the same case for the same recorded reason, which is the point: the refusal is the suite's, and it is identical on a native ext4 directory and on the private cowfs mount.

## Provenance

| thing | value |
|---|---|
| xfstests remote | `https://git.kernel.org/pub/scm/fs/xfs/xfstests-dev.git` |
| commit | `3e1ee800e52a0f53d7d9a7809be1ffc80ec1788f`, committed 2026-08-28T18:11:59+08:00 |
| clone | `git clone --depth 1`, into `task-g5/ref/xfstests`, never into the repo |
| tree state | clean (`git status --porcelain` empty at classify time) |
| allowlist sha | `5eab2e8a473b26613e279266bfb1812426e2a8bfbd256aef2eae9920cb2c6671` |
| host | `Linux moonscapenas 6.12.109+rpt-rpi-2712`, aarch64, Debian, 4 cores, 16 GiB |
| cowfs binary | `cargo build -p cowfs-cli` from this lease, dev profile, at base `46b0f26`, `target/debug/cowfs` |
| cowfs daemon | pid 899604, argv recorded, started 2026-10-04 16:15:54, store `task-g5/store`, mount `task-g5/mnt`, socket `task-g5/run/control.sock` |
| mount | `findmnt` reports `task-g5/mnt fuse.cowfs cowfs` |
| native arm | `/home/moonscape/cowfs-ready-wave/task-g5/native`, ext4 on `/dev/sda2` |
| cowfs arm | `/home/moonscape/cowfs-ready-wave/task-g5/mnt/wt`, a writable imported snapshot inside the private mount |

Both arms were proven usable before the case attempt, with a real write, read back and unlink, rc 0 on each:

```
native  fstype=ext4       write-read-unlink rc=0 out='hello'
cowfs   fstype=fuse.cowfs write-read-unlink rc=0 out='hello'
```

## The prerequisite gate, item by item

`common/config` checks these in order and exits on the first miss.
A non-root `PATH` does not include `/usr/sbin`, so the first pass of this table is misleading: `mkfs` and `xfs_io` are installed and simply invisible.
With `/usr/sbin:/sbin` on `PATH`, eight of the ten are present.

| gate entry | status | detail |
|---|---|---|
| `common/config:114 mkfs` | present | `/usr/sbin/mkfs` (dpkg `e2fsprogs` 1.47.0-2+b2) |
| `common/config:117 mount` | present | `/usr/bin/mount` |
| `common/config:120 umount` | present | `/usr/bin/umount` |
| `common/config:129 perl` | present | `/usr/bin/perl` |
| `common/config:132 awk` | present | `/usr/bin/awk` |
| `common/config:135 sed` | present | `/usr/bin/sed` |
| `common/config:143 df` | present | `/usr/bin/df` |
| `common/config:147 xfs_io` | present | `/usr/sbin/xfs_io` (dpkg `xfsprogs` 6.1.0-1) |
| `common/config:123 ltp/fsstress` | **absent** | not built, and not buildable here |
| `common/config:126 ltp/fsx` | **absent** | not built, and not buildable here |

Capabilities this lane refused to use, and why it did not need them:

- no root: `init_rc` only mounts when `$TEST_DEV` has no filesystem type, and setting `TEST_DIR == TEST_DEV` to the arm directory skips that entirely.
  This is the shape `README.fuse` uses for a non-device `TEST_DEV`, so it is upstream-sanctioned rather than a workaround.
- no block scratch: `SCRATCH_DEV` and `SCRATCH_MNT` are set empty, and every case that needs them is refused before it runs.
- no device format: no `mkfs` call is reachable on either arm, because `FSTYP` is empty on both.
  Every fstype-specific block in `common/config` wants `mkfs.<fstype>`, and `FSTYP=ext4` on the native arm alone would have demanded `mkfs.ext4`.
  Empty on both arms is also the only setting that keeps them comparable.
- no global setup, no shared mounts, no other worker's fixtures: the only mount involved is this lane's own, and the lock was used for the mounted workload.

## Bypasses considered and declined

Two ways around the missing helpers were available and both were rejected, because both manufacture a pass rather than measure one.

1. Put any executable at `ltp/fsstress` and `ltp/fsx`, for example a copy of `/bin/true`.
   `common/config` only tests `-x`.
   Every case that then calls `$FSSTRESS_PROG` or `$FSX_PROG` would compare a no-op against its expectations and exit 0.
   That is precisely the failure this gate exists to catch, and the harness's own log check exists because I hit it: an early probe of `generic/002` exited 0 while printing `unary operator expected` thirteen times, having asserted nothing.
2. Hand-write `include/config.h` and `include/builddefs` so `ltp/fsstress.c` and `ltp/fsx.c` compile.
   An empty `config.h` says "no optional features", which is false about this host, and the resulting helpers would quietly differ from the suite's own build in ways no local check could confirm.

## The harness

`bench/xfstests_gate.py`, with `bench/xfstests-allowlist.txt` as the reviewed input and `bench/test_xfstests_gate.py` as its test.

Four subcommands: `preflight`, `classify`, `run`, `report`.
Exit codes match `bench/compare.py`: 0 PASS, 1 FAIL, 2 UNMEASURABLE, 3 INVALID.

Three properties make a future run a measurement rather than a claim.

- **The suite answers, not the harness.** `preflight` checks each `common/config` entry by name, then runs a real case and treats the suite's own exit code as the answer.
  A missing prerequisite prints UNMEASURABLE and exits 2. There is no code path from a missing prerequisite to PASS.
- **Every case is classified before anything runs.** Each of the 802 `tests/generic/*` is read and refused with a recorded reason when it formats, mounts, loops, repartitions, needs root or a second user, names an absolute path in a destructive command, reaches a helper binary, reads source this gate has not read, or asks for more than 1 GiB or a soak runtime.
  Only the six reviewed ids may run, and `classify` recomputes the set and exits 3 on drift, so an upstream change cannot quietly widen the gate.
- **Matched arms, exact codes, immutable fixtures.** Both arms run the same id with the same generated environment, each in a per-case directory that must not already exist and must be empty, and the exit code comes from the child process.
  Each case appends and flushes one JSONL line, so an interruption keeps every finished case.
  A case whose log carries a skip signature (`command not found`, a missing helper under `src/` or `ltp/`, a malformed shell comparison, the suite's own `notrun`) is INVALID, never a pass.

The static classification needed three tightening passes, each driven by reading real sources rather than by a failure:

1. the first pass missed `$here/src/...`, because the suite reaches helpers through `$here`, not `$SRC_DIR`.
2. the second missed `_test_cycle_mount`, because `\bmount\b` does not match inside an underscore-joined name, and missed soak cases that run a million operations.
3. the third missed `_user_do` and `mknod`, which need root, so cases 005, 123 and 184 were wrongly safe until their sources were read.

Each of those is now a unit test.

## Safety classification of all 802 cases

| verdict | count | meaning |
|---|---|---|
| NEEDS_SCRATCH | 511 | needs a scratch device or `SCRATCH_MNT` |
| NEEDS_DEVICE | 93 | mkfs, mount, loop, device node, repartition, or a filesystem repair tool |
| NEEDS_HELPER | 88 | reaches a helper binary this host cannot build |
| UNREAD_SOURCE | 40 | sources `common/*` this gate has not read |
| UNSAFE | 26 | names an absolute path in a destructive command |
| NEEDS_ROOT | 26 | sudo, `chown`/`chgrp`, `mknod`, `_runas`, `_user_do`, user creation |
| NEEDS_LONG | 11 | soak or long group, or a six-figure operation count |
| SAFE | **6** | 005, 236, 245, 309, 360, 755 |
| NEEDS_BIG_SPACE | 1 | more than 1 GiB |

Per-case records with the line number and the reason: `bench/out/ready-g5/linux/classify.jsonl` (ignored, local).

The six, and what each asserts:

| id | assertion | note |
|---|---|---|
| 005 | `touch` through a 25-deep symlink chain returns ELOOP | relative `rm` only |
| 236 | a hard link bumps the target inode ctime | sleeps 1s |
| 245 | `mv` of a directory onto an existing name | renames inside `$TEST_DIR` |
| 309 | `mv` into a directory bumps that directory's mtime and ctime | real `status` increments, so a pass is a real assertion |
| 360 | `readlink` of a 1019-byte target, md5 of the result | needs perl, present |
| 755 | unlinking a hard link bumps the target inode ctime | weak: it echoes on a mismatch and never raises `status` |

Known limits of this list, stated rather than hidden:

- 6 of 802 is 0.7 per cent of the group. Even with the prerequisite block lifted, g5 needs a much larger unprivileged subset before it means anything, and the 511 scratch-device cases need a capability this lane does not have.
- A static scan cannot see a helper a case reaches indirectly at run time. The per-case log check is what catches that, and it makes the case INVALID rather than passing it.
- 755 cannot fail by its own logic, so its pass is weaker evidence than the other five. It is listed rather than hidden.

## Declined scope

I did not build a substitute conformance suite, and I did not present the harness's own synthetic tests as xfstests results.
The arm write-read-unlink probe above is arm preparation evidence, not a filesystem result.
No xfstests assertion ran on cowfs, so there is nothing here that says cowfs is or is not xfstests-conformant.

## Evidence, all ignored and local

| file | what |
|---|---|
| `bench/out/ready-g5/linux/preflight-run1.jsonl` | host, PATH, git provenance, all ten gate entries, the probe record and its reason |
| `bench/out/ready-g5/linux/classify.jsonl` | 802 records, one per case, with verdict, line number and reason |
| `bench/out/ready-g5/linux/case005.jsonl` | both arms of case 005: rc, timed_out, wall, log path |
| `bench/out/ready-g5/linux/case005-native.log` | the native arm's real output |
| `bench/out/ready-g5/linux/case005-cowfs.log` | the cowfs arm's real output |
| `bench/out/ready-g5/case005-attempt.py` | the script that produced the two arm records |

Raw artifacts stay on the Pi under `task-g5/out/`.

## Next safe acceptance conditions

In order, each one sufficient to move the gate forward and none of them assumed here.

1. **Make the suite runnable, with authority to install packages.** On this host that means `autoconf`, `automake`, `libtool` and `m4`, then `make` at the tree root to produce `include/builddefs`, `include/config.h` and `ltp/fsstress`, `ltp/fsx`.
   A container with the distro's xfstests build dependencies is the cleaner route and leaves the host alone.
   Alternatively build and install the suite once into a private prefix from the same pinned sha, so `here` still resolves inside the pinned tree.
2. **Keep `/usr/sbin` on `PATH` for the run.** Eight of the ten gate entries are installed and hidden from a user shell otherwise. The harness records the `PATH` it used in every run's meta record.
3. **Re-run the harness, not a hand-written invocation.** `xfstests_gate.py run --xfstests DIR --native-root DIR --cowfs-root DIR --cases 005,236,245,309,360,755`, with `PATH` including `/usr/sbin`, and the native root a private ext4 directory and the cowfs root a writable snapshot in the private mount.
4. **Then widen the allowlist deliberately**, one review pass per batch, because the 6 that survive now are the residue after three tightening passes and more will survive a careful read.
   The `UNREAD_SOURCE` 40 and `NEEDS_HELPER` 88 are the largest honest growth, and each needs its sources read.
5. **For the 511 scratch-device cases, get a capability decision, not a workaround.** A loop-backed scratch device needs root; a container with `CAP_SYS_ADMIN` and its own device namespace would do it without touching the host.
   Until then those cases stay refused, and the coverage line in the report stays honest about it.
6. **Only claim g5 when `--require-full` passes.** That flag exits 2 unless every one of the 802 generic cases ran, which is the flag that makes "g5 no worse than native" mean what it says.

## Dependencies on other lanes

None of the work touched another lane's code, and no cowfs defect was found, so there is nothing to hand to #45, #42, #19 or #43.
The comparison rules in `bench/compare.py` were not modified; this gate does not read them.