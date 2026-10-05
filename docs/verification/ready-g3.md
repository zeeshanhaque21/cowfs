# ready-g3: matched pjdfstest acceptance for a private real-Core mount

Gate g3 of `docs/ready-wave-dispatch.md`: pjdfstest no worse than native.
Verdict on this host and this adapter: **FAIL**, with every regression traced to two adapter or
Core behaviours that are not the harness's to change.
No production source was patched for this lane.

The harness is `bench/pjdfstest.py`; its own checks are `bench/test_pjdfstest.py`.
Raw per-case output is ignored under `bench/out/ready-g3/**` in the worktree that ran it.

## What was measured

One pinned pjdfstest build, one test list, two arms of one host.

| | |
| --- | --- |
| Host | `Darwin 25.6.0 arm64`, Apple M3 Max |
| Native arm | a directory in the worktree, on the host's own APFS volume |
| cowfs arm | a private `cowfs-daemon --backend core` store, mounted over the macOS NFS loopback |
| Mount | `localhost:/cowfs-cf052f1312b2c3ec668ebf09e03162ad on <run>/mnt (nfs, nodev, nosuid, mounted by zeeshanhaque)` |
| Daemon | pid 97710, started 2026-10-04T17:43:40-0700, private store and private socket, SIGTERM at teardown with the mount verified gone |
| Run | `bench/out/ready-g3/run/20261005T004337Z` |
| Harness exit | 1 (verdict FAIL) |

The two arms differ only in the path: the same suite binary, the same 238 case files, the same
unprivileged uid, the same host, one after the other with fresh directories per case.
Nothing inside a test knows which arm it is on.

## Tool provenance

Neither this Mac nor the Linux host packages pjdfstest, and neither has autoconf or automake, so
the upstream `autoreconf && configure && make` path cannot run here.
The harness clones the pinned commit, generates `config.h` by compiling one probe per `HAVE_*`
macro, and builds the suite with the system compiler under upstream's `-Wall -Werror`.
No package was installed.

| | |
| --- | --- |
| Source | `https://github.com/pjd/pjdfstest.git` at `85a8aea9e685999ef0540392fd80535f873d7ff7` |
| `pjdfstest.c` sha256 | `a6c354f2c42015a1d2d538ea4276091278a1e4955bb6a10d0ec9b9838f24a23e` |
| generated `config.h` sha256 | `493cda00f566b96eae2c032c26bfb22dcc000b0b44c7f46c1726ba31957a5a45` |
| binary sha256 | `5fa40986f39bb9033ef2fc15a7b110f3405541ff97344ca34756fceb090cdb71` |
| compiler | Apple clang 21.0.0 (clang-2100.3.34.2) |
| features detected | 29 macros, listed in `summary.json` |

A macro whose probe fails to compile is left undefined, which is what autoconf's `AC_CHECK_FUNCS`
would do.
The suite's own `misc.sh` finds the binary by walking up for `./pjdfstest`, so it is built into the
source root exactly where `make` puts it.
Not detected on this host, and therefore not exercised by any case: `posix_fallocate`,
`bindat`, `connectat`, `lpathconf`, `chflagsat`, `lchflagsat`, `ACL_TYPE_NFS4` (so the NFSv4 ACL
assertions never run), and the `st_atim`/`st_ctim`/`st_mtim`/`st_birthtim` spellings (the
`timespec` and `st_birthtime` spellings are present and were used).

## Counts

Both arms ran the same 238 cases.
All 476 case executions returned exit code 0, none timed out, none produced empty TAP output, and
no case exists on only one arm.

| | native | cowfs |
| --- | --- | --- |
| Cases | 238 | 238 |
| Executed | 158 | 158 |
| Skipped by the suite itself | 80 | 80 |
| Assertions produced | 8686 | 8686 |
| Assertions passing | 2985 | 3140 |
| Assertions failing | 5701 | 5546 |
| Assertions needing privilege this run has not | 1968 | 1981 |
| Cases with a non-zero exit code | 0 | 0 |
| Cases that timed out | 0 | 0 |

The 80 skipped cases are the suite declining to run here, identically on both arms: the FreeBSD-only
granular permission batteries, `chflags`, `lchmod`, `posix_fallocate`, `utimensat`, `rename_ctime`
outside EXT4/UFS/ZFS, and the NFSv4 ACL cases.
They are counted, not silently dropped.

The 1968 and 1981 privileged assertions are `-u`/`-g` and `mknod` cases.
Running them needs root, which this gate does not have, so they are counted and excluded from the
regression verdict instead of being reported as failures.
They are the whole of the remaining POSIX surface that this run does not speak to.

## Verdict

`FAIL`: 687 assertions pass on the native arm and fail on the cowfs mount, outside the
privilege-gated set.
A further 855 go the other way, and they are reported rather than counted as a win: 713 of them
are `chown`/`lchown` calls that native answers `EPERM` and cowfs answers 0.
`crates/cowfs-vfs/src/types.rs` documents that choice: everything is owned by the mounter and
`SetAttr` carries no uid or gid, so `chown` is accepted and ignored by adapters.
A caller that chowns and then reads back ownership gets a success and a different answer than it
asked for, which is a contract question for the Vfs trait owner rather than a harness question.
It also changes which branch a pjdfstest case takes, which is why `rename/09.t` and `rename/10.t`
show 1900 and 1616 native failures against 1630 and 1364 on cowfs: that is the chown difference
reshuffling a root-gated cascade, not an improvement.

Every regression is accounted for:

| cause | assertions | evidence |
| --- | --- | --- |
| Creating a fifo, socket or device node is not supported | 617 | `mkfifo ... expected 0, got EIO` (103), `bind ... got EIO` (92), `mknod` fifo `lstat expected fifo got ENOENT`, plus the `ENOENT`, `EEXIST`, `ENOTDIR`, `unlink EPERM` and time-order assertions that follow in the same tests |
| `pathconf` answers -1 on the mount | 63 | case stderr `pathconf returned -1` in 13 cases; the suite cannot derive `NAME_MAX`/`PATH_MAX`, so the case fails wholesale |
| `rmdir a/b/..` answers `EINVAL` | 1 | `rmdir/12.t` #4, native answers `ENOTEMPTY` or `EEXIST` |
| `nlink` stays 1 after unlinking an open file | 1 | `unlink/14.t` #4, `open : unlink : fstat 0 nlink` expected 0, got 1 |
| timestamp-order checks after a successful call | 2 | `ftruncate/12.t` #2 and `truncate/12.t` #2, `test_check` assertions with no text, not yet reduced to a mechanism |

Where the non-regular create fails, the follow-on assertions are cascades, not independent defects.
`open/22.t` is the clearest case: the suite creates a fifo, expects `O_CREAT|O_EXCL` to answer
`EEXIST`, and on cowfs the fifo was never created, so the open succeeds and the next `unlink`
fails `EPERM` on what is now a directory.

### Where each cause lives

- **Non-regular creation.** `crates/nfsserve/PATCHES.md` states that MKNOD answers
  `NFS3ERR_NOTSUPP`, and `crates/cowfs-fuse/src/fs.rs` answers `ENOTSUP` for any `mknod` that is not
  a regular file (`convert::mknod_is_regular`), which `crates/cowfs-fuse/src/lib.rs` lists under
  "Not supported". This is the NFS server-requirements lane (#19) with a Core create-with-type
  path behind it, not the acceptance harness.
- **`pathconf`.** `crates/nfsserve/PATCHES.md` records that `fsstat` and `pathconf` come from the
  file system, so a `pathconf` that answers -1 is a Core or adapter gap rather than a missing NFS
  procedure.
- **`rmdir a/b/..` and `nlink` after unlink.** Core or adapter behaviour, reported for triage.

Nothing here was patched, because every candidate site is production filesystem code owned by
another lane.

## Remaining scope

- **The Linux FUSE arm is unmeasured.** This verdict covers the shipped macOS adapter only.
  `moonscape` has `/dev/fuse` and `fusermount3`, so the arm is available, but it needs a
  cross-build of the workspace on that host and the shared Linux resource lock.
  g3 stays open until both arms have a matched verdict.
- **Privilege-gated assertions.** 1968 native and 1981 cowfs assertions need root and are counted,
  not run.
- **Host capability gaps.** The absent macros listed above, and the 80 suite-declined cases.
- **No performance claim.** This is a correctness gate. The per-case durations in `cases.jsonl` were
  recorded while other workers held the machine, so they are not evidence of anything about speed.

## Reproducing

```sh
# From the worktree that owns this gate. The whole invocation holds the shared Mac lock.
rtk proxy python3 -c 'import fcntl,os,subprocess,sys,time; p=sys.argv[1]; os.makedirs(os.path.dirname(p),exist_ok=True); f=open(p,"a"); until=time.monotonic()+600
while True:
 try: fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB); break
 except BlockingIOError:
  if time.monotonic()>until: print("resource lane busy; blocked",file=sys.stderr); sys.exit(75)
  time.sleep(.25)
sys.exit(subprocess.run(sys.argv[2:]).returncode)' /Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/mac-heavy.lock \
  python3 bench/pjdfstest.py
```

A single case can be scoped with `--tests rename/13.t` or `--groups link`, and
`--case-timeout` bounds one case.
The control socket is created at the repository root under a short name because `sun_path` holds
103 bytes and a treehouse lease path alone is 88 of them.
Exit status is 0 only when both arms completed with nothing the native arm passes failing on cowfs.