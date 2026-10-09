# Gate g5 harness redesign: a real xfstests differential

Issue: 101.
Decision: Zee, 2026-10-09.
Status: design, implemented in `bench/g5_diff.py`, `bench/g5_root.sh` and `bench/test_g5_diff.py`.
Evidence of the hand recipe this turns into code: `docs/reviews/g5-status-20261009.md` and `docs/verification/evidence/g5-20261009/`.

## Why the old arm model is replaced

The PR 100 harness (`bench/xfstests_gate.py`) gave each arm a plain directory and an empty `FSTYP`.
The pinned xfstests rejects that: `check` needs root, `TEST_DEV` must be a device or a network filesystem, and `TEST_DIR` must be the exact mount target of `TEST_DEV`.
So `check` never ran end to end under the old harness.
The hand recipe on the cachyos box did run it, 6 of 6 on both arms.
This design makes that recipe the harness.
The old `run` subcommand now refuses with exit 3 and points here, unless `COWFS_G5_OLD_ARM_MODEL=1` is set for its unit tests.
Its pin and suite-grammar code is reused by the new module.

## Roles of the files

`bench/g5_root.sh` is the only code that needs root.
It does loop, mkfs, mount, umount, namespace and daemon work, and runs the suite's `check`.
It is one script, shipped to the box once and run through a single `sudo` call.
It writes plain text evidence per case and never judges it.
`bench/g5_diff.py` needs no root and no mounts.
It parses that evidence, classifies it, applies the validity rules and writes the receipt.
`bench/g5_box.sh` ships the script to the box, feeds the password once on stdin, runs the root script detached, and polls and fetches.
`bench/test_g5_diff.py` covers the parser, the classifier and the validity rules with synthetic and real captured text.

## Getting root

Only the root script runs under `sudo`, as one `sudo -S -p ""` pipeline with the password read inside the same shell invocation.
The password is never in argv, a file, a log or a receipt.
Everything the script produces is chowned back to the invoking user (`SUDO_USER`) before it exits.
The report step runs unprivileged.
`check` refuses non-root with `QA must be run as root`.
`preflight` now reports that reason instead of an empty one.

## The native arm

Each case gets a fresh sparse image, attached with `losetup`, formatted with `mkfs.ext4` and mounted at `<out>/native/mnt`.
The environment is `FSTYP=ext4 TEST_DEV=/dev/loopN TEST_DIR=<that mount>`.
A case that unmounts and remounts `TEST_DIR` uses the real `mount` and `umount`, so a real ext4 cycle happens.
The loop device and mount are torn down after each case, and a trap tears them down on any exit.
A fresh filesystem per case matches the cowfs arm, where each case also starts empty.

## The cowfs arm

The root script starts one root `cowfs-daemon --backend core` from the release build on a fresh store under the output directory.
It records the binary path, its sha256 and the build profile, because the debug build times out on generic/074 and 127 and those would read as false regressions.
Its main mount is a read-only namespace of snapshots, so a case never runs on it.
Each case gets a fresh empty snapshot.
The case then runs in its own private mount namespace (`unshare -m --propagation private`).
Inside it, the snapshot is bind-mounted at `<out>/cowfs/mnt`, and the daemon's main mount is lazily detached in that namespace only.
That leaves exactly one mount with source `cowfs`, which `findmnt -S` in the suite's `_check_if_dev_already_mounted` requires.
The environment is `FSTYP=fuse TEST_DEV=cowfs TEST_DIR=<that bind mount>`.
Host mounts are untouched.
The snapshot is removed after the case, and the namespace dies with the case, so a case that destroys its mount only fails itself.

## Unique mount identity

Every cowfs mount reports the fsname `cowfs`, so the source string alone cannot tell two mounts apart.
Uniqueness is therefore established by construction and then measured.
By construction: one namespace per case, one visible `cowfs` source in it.
Measured: the root script records, inside the namespace and before `check` starts, the fstype, source, target and root of `TEST_DIR`, the snapshot name, and the count of mounts whose source is `TEST_DEV`.
The mount root must be `/<that case's snapshot>`, which proves the case ran on its own fresh snapshot.
The report requires that count to be exactly 1 on both arms, and the fstype to be `ext4` on the native arm and `fuse*` on the cowfs arm.
A daemon option for a per-mount fsname already exists in `cowfs-fuse` (`fsname=`) but is not reachable from the control CLI, so it is a follow-up, not a dependency.

## The mount helper

The suite resolves `mount` and `umount` with `type -P` while loading `common/config`, and overwrites `MOUNT_PROG` unconditionally.
So the helper is two scripts in a directory placed first on `PATH`, not an environment override.
On the cowfs arm only, `umount <TEST_DEV or TEST_DIR>` runs `mount --move <TEST_DIR> <stash>`, and `mount ... <TEST_DEV> <TEST_DIR>` runs `mount --move <stash> <TEST_DIR>`.
Every other invocation is passed to the real tools.
Each call is appended to a `mountcycle.log` for the case.
This is an emulated cycle: the data stays in the daemon and the FUSE session stays up.
The shim logs every call that names `TEST_DEV` or `TEST_DIR`, whether or not anything moved, with the caller read from `/proc/$PPID/cmdline`.
So a cowfs case with a cycle gets the status `PASS_EMULATED`, never `PASS`, and acceptance does not accept it.
The suite's own wrap-up runs in `check` and unmounts `TEST_DEV` once after every case, so a call whose caller is `check` is not a cycle.
Any other caller counts, and so does a line the report cannot parse.
This closes a hole: a case that unmounts and never restores leaves the wrap-up with nothing to unmount, and the log then holds one line, so counting lines alone would have read it as clean.
After each case, outside its namespace, the root script lists the bare `mnt` and `stash` directories.
Anything in them means the case ran on the host directory, and the record is invalid.
The six reviewed ids are checked this way and none cycles.
While the mount is moved to the stash, `findmnt -S cowfs` still finds it, at the stash path.
A case that checks mounts in that window may fail, and the receipt shows the cycle count next to such a failure.

## Scratch

`SCRATCH_DEV` and `SCRATCH_MNT` are empty on both arms.
A second `cowfs` source would break the one-source rule above, and FUSE has no mkfs.
Cases that need a scratch device end as `not run: requires SCRATCH_DEV` on both arms.
They are classified `harness`, are symmetric, and are listed in the receipt as unmeasured, never as passes.
A scratch arm is a follow-up.

## Per-case receipt

Per arm and per case, the root script writes `console.txt` (the suite's own stdout and stderr), `rc`, `identity.txt` and `mountcycle.log`.
The report turns each into one record with a status:

- `PASS_EMULATED`: a cowfs `PASS` with at least one emulated cycle.
- `PASS`: the case line has no bracket, `Ran:` names exactly this case once, `Passed all 1 tests`, no `Not run`, rc 0.
- `FAIL`: `Failures:` or `Failed 1 of 1 tests`, or an output mismatch or nonzero exit line.
- `NOT_RUN`: a `[not run] reason` line and `Not run: <case>`.
  The suite also prints `Passed all 1 tests` here, so the bracket is what decides it, never the summary.
- `TIMEOUT`: rc 124 from `timeout`.
- `NO_RESULT`: nothing the grammar accepts.
  This is a harness fault and makes the run INVALID.

Each `NOT_RUN` carries its reason and a class:

- `inherent_fuse`: block device or block size assumptions that cannot hold on FUSE.
- `missing_feature`: a probe by the suite failed on a capability cowfs could provide (`fpunch`, `fzero`, `falloc`, `fcollapse`, `fiemap`, `exchangerange`, renameat2, O_TMPFILE, chattr flags, creation time).
- `by_fstype`: the suite decided from the filesystem name, not by probing cowfs (reflink, dedupe, ACL text).
  It is evidence that the suite skips FUSE, not that cowfs lacks the feature.
- `harness`: the test rig lacks something on both arms (`SCRATCH_DEV`, `src/locktest` not built, `dbench`).
- `unclassified`: any other reason.
  It is reported by name so it is looked at, never silently bucketed.

A case is `worse` only when native is `PASS` and cowfs is `FAIL` or `TIMEOUT`.
Native `PASS` with cowfs `NOT_RUN` is a `gap`, listed by class, and is never counted as a cowfs pass.
A skip on both arms is `both_not_run`, symmetric, and never a gap.
Classification is per arm and per reason string, and every captured reason string is a unit-test fixture.

## Validity rules

A run is INVALID, with the broken rule named, when any of these fails.
A PASS that skips a rule does not exist.

1. The xfstests tree sha, the `check` bytes and the bytes of every reviewed case equal the reviewed pin, and the tree is clean.
   The root script captures these before the first case (`meta.txt`), because the unprivileged report runs later and cannot prove what the tree was during the run.
   It also removes a stray root-owned `tmp.*` from the tree first, and logs that in `cleanup.txt`.
2. Both arms have a record for every requested id, no id twice, and the id lists are identical.
3. In each `Ran:` line the suite names exactly the requested case, once.
4. The header `FSTYP` of the console equals the arm (`ext4` or `fuse`).
5. The recorded identity of the native arm is fstype `ext4` on a `/dev/loop*` source.
   This is what stops a native arm that is secretly cowfs.
6. The recorded identity of the cowfs arm is a `fuse*` fstype with source `cowfs`.
   This is what stops a cowfs arm that is secretly native.
7. Exactly one mount has the arm's source in the namespace the case ran in.
8. The two arms have different `TEST_DIR`.
   The fstype difference is enforced by rule 4 and rules 5 and 6.
9. No record is `NO_RESULT`.
10. The bare mount and stash directories are empty after every cowfs case.
11. A negative control ran and was judged `FAIL` with the `Read-only file system` error in its console: `generic/005` on a read-only remount of a fresh snapshot.
    A report without a failing control cannot tell a pass from a rubber stamp.

On top of validity, the verdict is:

- `UNMEASURABLE` (exit 2) when either arm has no clean `PASS` at all, which is the empty or all-skipped run on native or on cowfs.
- `FAIL` (exit 1) when the `worse` list is not empty.
- `PASS` (exit 0) only in acceptance mode, only on the reviewed id set from the allowlist, only on a release daemon whose sha256 equals the one given to the report and whose build revision (a 40-hex git revision, from `COWFS_REV` or a `BUILD_REV` file next to the binaries) is recorded, and only when every case is `PASS` on both arms.
  In acceptance mode every other valid result exits 2 or 1, never 0.
- `DIAGNOSTIC` (exit 0) for any other id set that is valid.
  It prints the counts and lists and never says PASS.

Exit codes are the same set `bench/compare.py` and the old gate use.

## What a run looks like

1. `rsync` the tree of the harness files to the box workspace.
2. One `sudo` pipeline runs `g5_root.sh` for the native arm, the cowfs arm and the control.
3. The output directory is read back and `g5_diff.py report` produces `receipt.json` and `receipt.md`.
4. A leftover check (`mount`, `losetup -a`, anchored `pgrep`) must come back empty.
   The root script also records its own teardown result in the output directory.

## First results, 2026-10-09, cachyos box

The evidence is in `docs/verification/evidence/g5-20261009-redesign/`.
The cowfs daemon was the release build with the sha256 in `*-meta.txt`; its source revision was not recorded, because the box copy is not a git checkout.
The sample is the six reviewed ids: native 6 of 6 and cowfs 6 of 6, control failed correctly.
It is labelled diagnostic, not acceptance evidence, because the daemon's source revision was not recorded and acceptance now refuses that.
The wide run is the 155 diagnostic ids: valid, verdict FAIL by the rules above, so diagnostic and not acceptance.
Native took about 14 minutes with a fresh filesystem per case, and cowfs about 21 minutes on the release build.
generic/247 unmounts `TEST_DIR` and never restores it, and the shim log caught it as `PASS_EMULATED`.
generic/127 fails on cowfs twice in a row on the release build (one `fsx` line missing from the output), where the hand run recorded a pass.
The hand recipe passed it on the same binary, but a rerun with the shim disabled (`NOSHIM=1`) also failed, so the shim is not the cause.
Its cause is not diagnosed here.

## Where work stopped (2026-10-09)

Stopped by request, with the harness, report, unit tests and two box runs (sample, wide) done.
Remaining before this can close g5: a critic review, the `xfstests_gate.py preflight` reason fix verified on the box (it has no unit test yet), a lint check of `bench/`, and a diagnosis of generic/127.
The 6-id sample is repeatable, the wide run is not part of CI.
`wide-receipt` was produced by an intermediate `g5_root.sh` (before the teardown wait and `NOSHIM` edits), which only affects teardown counting and a diagnostic switch.

## Re-run required (critic block on PR 200)

The wide list and the sample must be re-run on the final script, with `COWFS_REV` (or `BUILD_REV`) set from `git rev-parse HEAD` of the tree that built the release daemon, and `--daemon-sha256` given to the report.
The consoles of the eight worse cases and of generic/127 must then be copied into the evidence directory.
Neither was done in this round, to avoid the cost.
Existing receipts are diagnostic evidence only.
`BUILD_REV` is written at build time, for example: `git archive <sha>` to the box, build there, then `echo <sha> > target/release/BUILD_REV`.
Acceptance on the box is not possible until that rebuild happens, because the current `src` there is not a git checkout.
The `--daemon-sha256` value must come from that build step; copying it from `meta.txt` would make the check circular.
A `NOSHIM` run is never acceptable, since it logs no mount cycles.
generic/127 on cowfs failed three times, each with rc 1 and an output mismatch, never a harness timeout (`TMO` was 900).
The missing `All 100000 operations completed A-OK!` lines differed: two in the wide run, one in each of the two single-case reruns.
The second rerun had the shim disabled (`NOSHIM=1`) and failed the same way.
Its cause is undiagnosed.
The consoles are in `consoles/`, produced by the intermediate script, together with those of the eight worse cases.
`g5_box.sh start` now checks that the box is idle, that the workspace is under `/mnt/docs`, and runs `sudo -v` once before anything else.

Preflight was run once on the box, unprivileged and read-only.
Its reason is no longer empty: `the suite refused a probe case with exit 1: common/config: TEST_DEV (...) is not a block device or a network filesystem`.
That is the old arm model's own refusal, which `check` raises before the root check; the root refusal text is covered by a unit test.

## Not done here

- Scratch arm for both sides.
- A reachable per-mount fsname, which would make the namespace trick unnecessary.
- A real FUSE remount for cowfs cycling.
- `src/locktest` on kernel 7.2 headers: those cases are `harness` on both arms.
- Repinning and widening the reviewed set beyond six ids.
