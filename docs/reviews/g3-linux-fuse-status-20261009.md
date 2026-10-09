# g3 Linux FUSE arm status, 2026-10-09

Status: measured. Gate g3 is NOT closed on Linux: one NEW divergence, see below.

## Scope

Gate g3 asks that pjdfstest on cowfs be no worse than native.
The macOS NFS arm was measured in `docs/reviews/g3-status-20261009b.md`.
This document covers the Linux FUSE arm, measured on the cachyos box (`zeeshan@100.122.64.51`), unprivileged (uid 1000, no sudo).

## Build and identity

Source: origin/main `fb64023efd6beaa53692a33019e24b7dc587f9be`, shipped as a git bundle to `/mnt/docs/Projects/cowfs-g3-linux/src`, `git status --short` empty.
Release build on the box: `cargo build --release -p cowfs-daemon -p cowfs-cli` (cargo 1.99.0, gcc 16.2.1 for pjdfstest).
`cowfs-daemon` sha256 `f0e55b2e01783edf3f3b787ecdf1fb24cfc15abbadd485f32be9577753375132`.
`cowfs` CLI sha256 `54156ae3b2305b3fc10eae5f8efc83160e12ae8720eb7373c7126b3e89fed12e`.
The harness then ran from commit `db7b961` (the mount-table fix below, on top of `fb64023`, bench files only).
`identity.json` records `cowfs_head` as `db7b961` because it reads HEAD at run time; the daemon bytes are the `fb64023` build, and `db7b961` changes no crate.
pjdfstest pinned `85a8aea9e685999ef0540392fd80535f873d7ff7`, cloned from GitHub on the box, 21 config features detected.

## The harness could not run on Linux as is

`bench/pjdfstest.py` parsed `/sbin/mount` only in the macOS shape `src on /mp (fstype, opts)`.
Linux prints `src on /mp type fuse.cowfs (rw,nosuid,...)`.
Reproduced on the box against a live FUSE mount owned by another agent (read only): `mount_entries` returned the mount point `/mnt/docs/Projects/cowfs-g45-torn/mnt type fuse.cowfs`, `mount_state` answered `NOT_MOUNTED` for a mounted path, and `fs_identity` found no entry for the native arm.
So `start_daemon` would wait 180 s and fail, and no case could run.
Fix: `mount_entries` splits a trailing ` type <fs>` off the mount point and puts the type first in the options, so the first option is the type on both OSes.
Failing-first test `LinuxMountTable` in `bench/test_pjdfstest.py`, built from verbatim box lines: 2 of 3 failed before the fix, 113 of 113 tests pass after it on the box.
After the fix the same live probe reads `MOUNTED`, and the native arm resolves to `/mnt/docs` `btrfs`.
Cosmetic: `mount_table_line` in the receipt is rebuilt from the parsed fields, so on Linux it reads `cowfs on <mp> (fuse.cowfs,rw,...)`, not the raw line.

## Root-only cases

The box gives no root.
pjdfstest marks a whole case with `requires_root` in the script (only `chflags/01.t` uses it), which prints `not ok N not root` on both arms.
Individual assertions that need another uid or gid (`-u`, `-g`) or `mknod` fail with EPERM on both arms when unprivileged.
The harness marks an assertion `root_required` when the script calls `requires_root` or the assertion text matches `-u`, `-g`, `mknod` or `not root`, and reports them separately from the non-root failures.
`unshare -Ur` would make `id -u` read 0, but the FUSE mount has no `allow_other` and other uids are unmapped, so `-u`/`-g` assertions would still fail on both arms as an environment artefact; it was not used.

## Suite branching per arm

The pinned `tests/conf` sets `fs` from `df -PT .`, so the native arm reads `BTRFS` and the cowfs arm `FUSE.COWFS`.
Most `${os}:${fs}` branches name FreeBSD, SunOS or Darwin, but `rename/24.t` branches on `btrfs|BTRFS` and `utimensat/09.t` has `todo Linux` rows, so the arms are not guaranteed identical paths.
Plan counts match on all 238 cases in the full run, and only `rename/24.t` differs in outcome because of this (see the better positions).

## Receipts

| run | scope | state | evidence (box, scratch) |
| --- | --- | --- | --- |
| `20261009T113349Z` | sample: `open/17.t`, `unlink/14.t` | harness PASS exit 0, COVERAGE reasons only | `/mnt/docs/Projects/cowfs-g3-linux/run/20261009T113349Z`, `cases.jsonl` sha256 `76ab6f18fdebb680bc289e1173248a759264233478a40178ad9ae98e25649d29` |

## Sample (before the full run)

`identity.json`: `validated: true`, no problems.
Native on `btrfs` `/mnt/docs`, `st_dev` 58; cowfs on `fuse.cowfs` at the run's own `mnt`, `st_dev` 108.
PATH_MAX shape `equal`: both arms answer `PC_PATH_MAX` 4096 and `PC_NAME_MAX` 255, so no overlay was injected.
`open/17.t`: native 3 ok / 0 not ok, cowfs 3 ok / 0 not ok.
`unlink/14.t`: native 7 ok / 0 not ok, cowfs 7 ok / 0 not ok.
The two accepted-list entries (`open/17.t` #2 for #204, `unlink/14.t` #4 for #109) are reported stale on Linux: neither fails here.
Mount and daemon absent after the run (`mount | grep cowfs-g3-linux` empty).

## Full run

Run `20261009T113404Z`, all 238 cases, one pass, launched detached (`setsid nohup nice -n 10`), load average 3.0 on 16 cores at start (another agent's FUSE stress was running in its own directory).
`identity.json` `validated: true`, same placement as the sample (native `btrfs` dev 58, cowfs `fuse.cowfs` dev 108 at the run's own `mnt`), PATH_MAX shape `equal`, no overlay.
`cases.jsonl` sha256 `6a5a7076ccc352f803bd73733953bbc7033bebfad6f011e00d7fa9946b791983`.

| | native | cowfs |
| --- | --- | --- |
| Cases | 238 | 238 |
| Executed / declined by suite | 167 / 71 | 167 / 71 |
| Assertions | 8798 | 8798 |
| Passing | 3072 | 3074 |
| Failing | 5726 | 5724 |
| Root-required (harness mark), none passed | 1948 | 1950 |
| Non-root failures | 3778 | 3774 |
| Non-zero rc, timeout, bail out | 0 | 0 |

Plan counts and assertion counts match per case on all 238 cases.
Only two cases differ in ok / not ok counts:

| case | native ok / not ok | cowfs ok / not ok |
| --- | --- | --- |
| `mknod/08.t` | 29 / 6 | 27 / 8 |
| `rename/24.t` | 9 / 4 | 13 / 0 |

The other 236 cases have identical ok / not ok counts on both arms.
Established regressions 0, candidates 0, unpairable 5687 (coverage, disclosed).
Ordinal differential: worse 2, better 4.

## Harness verdict

With the checked-in accepted list: FAIL, exit 1.
Reasons: DIVERGENCE `mknod/08.t` #19 and #24 not covered by an accepted divergence; COVERAGE 5687 unpairable; COVERAGE both list entries (`open/17.t` #2 #204, `unlink/14.t` #4 #109) listed but not worse in this run.
Without the list (`verdict(..., accepted=[])` on the same run directory): FAIL, exit 1, the same DIVERGENCE reason, no stale-entry reason.
The list changes nothing on Linux: neither of its entries fails here.

## Every worse position, classified

| position | class | evidence |
| --- | --- | --- |
| `mknod/08.t` #19 | NEW (not #108, #109 or #204) | cowfs `tried 'mknod <n0> f 0644 0 0', expected EEXIST, got 0`; native ok |
| `mknod/08.t` #24 | NEW, same mechanism | identical text in the char-device iteration |

Both are downstream of positions #18 and #23, which fail on both arms but differently.
`mknod/08.t` loops `create_file <type>` then `mknod b`, `mknod c 0 0`, `mknod f`, then unlink, for each type.
Unprivileged, `create_file block` and `create_file char` fail with EPERM on both arms, so the name does not exist.
Native then answers `0` to `mknod <n0> c 0644 0 0` (it creates a 0:0 char device, the overlayfs whiteout, which Linux lets an unprivileged user create), so the following `mknod f` gets EEXIST and passes.
cowfs answers EPERM to `mknod c 0 0`, the name stays absent, and `mknod f` creates a regular file, so #19 and #24 fail.

Reproduced by hand on the box, kernel `7.2.8-2-cachyos`, uid 1000, a fresh private daemon (same binary) on `/mnt/docs/Projects/cowfs-g3-linux/repro/mnt`, inside a `pjd` snapshot:

| operation | native btrfs | cowfs FUSE |
| --- | --- | --- |
| `pjdfstest mknod wo c 0644 0 0` | `0`, `stat` reads `character special file 0:0` | `EPERM`, no file |
| then `pjdfstest mknod wo f 0644 0 0` | `EEXIST` | `0` |
| coreutils `mknod wo2 c 0 0` | `0` | `Operation not permitted` |
| `pjdfstest mknod d12 c 0644 1 2` | `EPERM` | `EPERM` |
| `pjdfstest mkfifo ff 0644` | `0` | `0` |

Mechanism, read from source, consistent with the reproduction but not traced on the wire: `crates/cowfs-fuse/src/fs.rs` `mknod` refuses every device node when `req.uid() != 0` with EPERM ("The kernel already demands CAP_MKNOD for a device; this is the second line").
The Linux kernel exempts the 0:0 whiteout char device from the CAP_MKNOD check, so the request reaches the daemon and the second line refuses what the kernel allowed.
The check came in with `c373387` (slice C of #107).
Not filed, per the task; listed here for a decision.
The harness marks both rows `root_required` because their text matches `mknod`, but the native arm shows the operation succeeds unprivileged, so it is a real divergence and the verdict correctly counts it.

## The 4 better positions

`rename/24.t` #4, #5, #8, #9 fail on native only, each tagged `# TODO Btrfs uses CoW; link count semantics differ from POSIX.`
The pinned script branches on `${fs}` (`btrfs|BTRFS`), so the native arm runs TODO-marked `lstat ... nlink` expectations that btrfs fails, and cowfs passes them.
This is a native-oracle limitation, not a cowfs gain.

## Classification of earlier known divergences on Linux

- #204 (`open/17.t` #2, fifo write-open EACCES): does not reproduce, `open/17.t` is 3/3 on both arms in the sample and the full run.
- #109 symptom 2 (`unlink/14.t` #4, nlink after unlink of an open file): does not reproduce, 7/7 on both arms. Its mechanism is macOS NFS silly-rename and has no FUSE counterpart.
- #108 (pathconf PATH_MAX): does not reproduce, both arms answer 4096 and no ENAMETOOLONG row is worse.

## Cleanup

After each run and after the hand reproduction, `mount | grep -c cowfs-g3-linux` read 0 and no process with `cowfs-g3-linux` in its arguments remained.
No other agent's mount or process was touched (the `cowfs-g45-torn` mount and `/tmp/.tmp*/mnt` mounts were only read).
Scratch stays in `/mnt/docs/Projects/cowfs-g3-linux` (source clone, build, runs, repro store).

## Code change

PR on branch `fix/g3-linux-mount-parse` (commit `db7b961`): Linux mount-table parsing in `bench/pjdfstest.py` plus the `LinuxMountTable` test.
On the Mac, the same 113 tests pass and the live macOS mount table (16 lines) parses identically under the old and new `mount_entries`.

## Verdict for g3, Linux arm

Measured, not closed.
One NEW divergence (`mknod/08.t` #19, #24, unprivileged whiteout mknod refused by the FUSE adapter) fails the gate on Linux.
Everything else is equal or explained: 236 of 238 cases have identical counts, and the 4 better rows are a btrfs TODO in the native oracle.

## Limits

- One full pass, no repeat, so run-to-run variance is unmeasured.
- Unprivileged only: 1948 to 1950 root-required assertions fail on both arms and say nothing about cowfs.
- 5687 assertions are unpairable by script identity; the per-case count comparison above is the coarser check that covers them.
- The FUSE mechanism is read from source and matches the reproduction; the FUSE request itself was not logged.
- The accepted list is not per-platform, so on Linux its two macOS entries always read stale.
