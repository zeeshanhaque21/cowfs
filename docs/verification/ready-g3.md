# ready-g3: matched pjdfstest acceptance for a private real-Core mount

Gate g3 of `docs/ready-wave-dispatch.md`: pjdfstest no worse than native.
macOS adapter: **FAIL**.
Linux FUSE arm: **UNMEASURABLE**, not attempted.
No production source was patched by this lane.
Every number below is derived from the preserved raw records by `bench/pjdfstest.py`, and the
derivation is re-runnable.

## Two record sets, and which one says what

| record set | what it is | what it can support |
| --- | --- | --- |
| historical | `bench/out/ready-g3/run/20261005T004337Z`, the author's full 238-case run at head `025bde2`, 476 records, `cases.jsonl` sha256 `bb55fe4912a36302980c9a1b0f8214a44f2af83afb4cfa3f60b96f36db6a8259` | an ordinal-position differential, and 77 script-proven assertion pairs |
| repaired small run | `bench/out/ready-g3/run/20261005T022515Z`, 5 cases per arm at the repaired head, raw streams and hashes kept, `cases.jsonl` sha256 `1d64dabcca863b16162c94fc15ae52d4c4d4bfe4e71674a6fae070225b7a90ad` | a complete matched verdict with raw evidence for every case |

The historical records are preserved byte for byte.
Nothing in them was rewritten, and `reconciliation.json` sits beside them as the derived artifact.

## What the historical run measured

One pinned pjdfstest build, one case list, two arms of one host.

| | |
| --- | --- |
| Host | `Darwin 25.6.0 arm64`, Apple M3 Max |
| Native arm | a directory in the worktree, on the host's APFS volume |
| cowfs arm | a private `cowfs-daemon --backend core` store over the macOS NFS loopback |
| Mount | `localhost:/cowfs-cf052f1312b2c3ec668ebf09e03162ad on <run>/mnt (nfs, nodev, nosuid)` |
| Store | `meta.redb`, `virt.ino.b`, `packs/pack-00000000.cpk`: a real Core block store behind the adapter |
| Tool | `pjd/pjdfstest` at `85a8aea9e685999ef0540392fd80535f873d7ff7`, Apple clang 21.0.0, upstream `-Wall -Werror`, binary sha256 `5fa40986f39bb903...` |

| | native | cowfs |
| --- | --- | --- |
| Cases | 238 | 238 |
| Executed | 158 | 158 |
| Declined by the suite itself | 80 | 80 |
| Assertions | 8686 | 8686 |
| Passing | 2985 | 3140 |
| Failing | 5701 | 5546 |
| Privilege-gated assertions, all failing, none run | 1968 | 1981 |
| Non-privileged failures the native arm cannot adjudicate | 3733 | 3565 |
| Privilege-gated assertions that passed | 0 | 0 |
| Cases with a non-zero exit, a timeout or a `Bail out!` | 0 | 0 |

The privilege-gated assertions are a subset of the failures, not a second bucket beside them:
1968 of the native 5701 and 1981 of the cowfs 5546.
The native arm fails 43 percent of all assertions on plain APFS, and 3733 of those failures are
outside the privilege gate, so the native oracle cannot adjudicate them.
That is the same exclusion logic that removes the 1968, and it is a limit on the gate, not a
result.
Nothing in the failing-native, passing-cowfs direction is scored as a win.

## 687 is an ordinal differential, not a count of defects

The first version of this document read 687 as defects.
It is not, and the reviewer's independent audit is right about why.
The old comparator paired assertions by their position in the stream, so a case that took a
different path after an earlier failure shifted every later position and paired unrelated
assertions.

Re-derived from the preserved records, all figures independently reproduced:

| measurement | value |
| --- | --- |
| ordinal positions where native passed and cowfs failed, total | 700 |
| the same, outside the privilege gate | 687 |
| of those, pairs whose comparison text matches | 48, and all 48 are textless on both sides |
| of those, pairs that are structurally different | 639 |
| the other direction, ordinal positions where native failed and cowfs passed | 855, with 0 matching text |
| of those 855, `chown` or `lchown` calls | 714 |
| of those 855, the rest, all `lstat` or `mkdir` rows in the same ownership divergence | 141, namely 70 in `rename/09.t`, 60 in `rename/10.t`, 10 in `unlink/11.t`, 1 in `mkdir/10.t` |

The exclusive partition of the 687, first match wins, sums to 687:

| bucket | rows |
| --- | --- |
| A: the create itself answered `EIO` | 198, of which `mkfifo` 106 and `bind` 92 |
| B: rows in the 13 cases whose stderr says `pathconf returned -1`, excluding A | 66 |
| C: rows whose cowfs answer is `ENOENT` | 360 |
| D: textless rows | 48 |
| E: every other answer | 15 |

Withdrawn from the first version of this document, because they do not reproduce under any
reading: 617, 63 and 713.
The old text also said "three small separate divergences" while listing four checks, and put 684
and 683 in the body and the PR; those numbers are gone.

Bucket E reduces to the 13 fifo-loop rows this document already explained, plus the 2 named checks
below.
`open/22.t`, `mkdir/10.t`, `symlink/08.t`, `link/10.t`, `rename/13.t`, `rename/20.t` and
`rmdir/06.t` all loop over `for type in regular dir fifo block char socket symlink` and call the
suite's `create_file`, so on the mount the fifo, socket, block and char targets never exist and the
collision the case is testing never arises.
That is a consequence of bucket A, not a second defect class.

## 77 assertions are identified, and they all share one cause

Pairing by position is unsound.
Pairing by what the script proves is sound, and it is now what the harness does.
The pinned script makes its assertions in a fixed order unless it branches on an earlier outcome or
calls a helper that injects a variable number of assertions.
When neither is true and the script's assertion count equals the stream's plan, the k-th assertion
in the transcript is the k-th assertion the script makes, so position is identity and the pair is
established.

From the historical records:

| | |
| --- | --- |
| established regressions, script-proven | 77, of which 71 outside the privilege gate |
| candidate regressions, not established | 0 |
| assertions that cannot be paired at all | 10050 |
| of those, assertions with no operation text in a case whose script cannot prove a slot order | 5563 |

| case | established regressions |
| --- | --- |
| `mkfifo/00.t` | 22 |
| `mknod/00.t` | 22 |
| `unlink/00.t` | 30 |
| `open/17.t` | 3 |

Every one of the 77 is a non-regular create or a consequence of one: `mkfifo` answers `EIO`, then
`lstat` answers `ENOENT`, then `unlink` answers `ENOENT`.
`open/17.t` is the same mechanism stated as a POSIX rule: opening a fifo `O_WRONLY|O_NONBLOCK` with
no reader must answer `ENXIO`, and the fifo never existed.

Why the identity cannot be recovered everywhere: the suite prints no operation text on a pass, only
`ok <n>`.
So for a case whose script cannot prove its slot order, a passing assertion carries no operation at
all and no pair can be shown to be the same assertion.
Those are reported as unpairable, never guessed, and they are why the verdict is FAIL with an
explicit incomplete scope rather than FAIL with a tidy number.

### The four named residual checks

| check | native | mount | status |
| --- | --- | --- | --- |
| `rmdir/12.t` #4, `rmdir a/b/..` where `b` is empty | `ENOTEMPTY` or `EEXIST` | `EINVAL` | reproduced on the mount in the repaired small run, native control 6 of 6 |
| `unlink/14.t` #4, `nlink` after unlinking an open file | 0 | 1 | reproduced on the mount in the repaired small run, native control 7 of 7 |
| `ftruncate/12.t` #2, timestamp order after a truncate | passes | fails | no mechanism yet, hypothesis only |
| `truncate/12.t` #2, timestamp order after a truncate | passes | fails | no mechanism yet, hypothesis only |

The first two are real divergences in Core or adapter behaviour.
The last two are not claimed as anything yet.

## What the EIO attribution is, and is not

`crates/nfsserve/src/nfs_handlers.rs` answers `NFS3ERR_NOTSUPP` for every `MKNOD`,
`crates/nfsserve/PATCHES.md` records it, and `crates/cowfs-fuse` answers `ENOTSUP` for a `mknod`
that is not a regular file.
Attributing the 198 `EIO` rows to that policy is consistent with the source and is **inferred, not
proven**, for two reasons:

1. `crates/cowfs-vfs/src/error.rs` maps `Error::Corrupt` and `Error::Io` to the same `EIO`, so the
   errno alone does not separate unsupported from broken.
2. there is no errno translation table in the tree, so the `EIO` the client sees at a create is
   rendered by the macOS NFS client from the NFS status.
   That translation lives in XNU, which is not on this host, and no own RPC control run was made to
   observe the wire status directly.

The 360 `ENOENT` and 66 `pathconf` rows are unaffected: they are `ENOENT` and `-1`.

## The repaired small run

A complete matched run of five cases per arm on the repaired harness, both arms, every case keeping
its raw stream and its hash.

| | native | cowfs |
| --- | --- | --- |
| Cases | 5 | 5 |
| Executed / declined | 5 / 0 | 5 / 0 |
| Assertions | 88 | 88 |
| Passing / failing | 66 / 22 | 43 / 45 |
| Privilege-gated, all failing, none run | 6 | 6 |
| Cases with a non-zero exit, a timeout or a guard problem | 0 | 0 |

| | |
| --- | --- |
| Verdict | FAIL, exit 1 |
| Established regressions | 25, all in `mkfifo/00.t` (22) and `open/17.t` (3) |
| Ordinal differential for the same five cases | 27, of which 22 structurally different |
| Unpairable | 26, of which 24 assertions with no operation text |
| Arm separation | native `apfs` at `/`, `st_dev` 16777234; cowfs `nfs` at the mount, `st_dev` 436209625 |
| Daemon | pid 79659, registered before the mount wait, SIGTERM after its argv was re-verified, mount absence proven from a complete 14-line table |
| Measured cowfs build | head `025bde2f3ccea609f54edf67a9d86d66d28abeca`, `cowfs-daemon` sha256 `4804a16546a87679...`, `cowfs` sha256 `33055fed260adfc...` |

The same five cases were run twice under the repaired harness and produced the same counts.

One gap in that run's record, stated rather than hidden: `summary.json` for that run recorded the
cowfs arm's filesystem identity after teardown, so its `fstype` reads null.
The in-run identity was read while the mount was up and is in the run log, and the separation check
ran and passed on the two `st_dev` values.
A later repair records the in-run identity in the summary instead; that change is not exercised by
a third run, because the shared Mac lock is taken once per attempt by agreement.

## What the harness now refuses

A verdict is a conclusion about the filesystem, so anything that would make it a conclusion about
something else is refused before it is scored.
`verdict()` re-reads the run's own records, re-parses every raw stream, and refuses on: a missing
plan line, a plan that does not match the assertions emitted, duplicate assertion ids, ids that are
not contiguous from 1, a `Bail out!`, a truncated or malformed line, a non-zero child exit, a
timeout, a record filed under the wrong case, a raw stream whose hash moved, a record set that
mixes cases with and without a raw stream, and a synthetic fixture.
The guards are inside `verdict()`, not only in the runner, so a library caller cannot skip them.
A record set that keeps no raw streams is labelled legacy and reported once rather than refused per
case, which is how the historical set is scored.

An unmeasurable run is UNMEASURABLE and a malformed one is INVALID.
Neither is a pass, and an established divergence is reported as FAIL even when part of the scope is
unpairable, because that is a result and the unpairable part is a limit.
Exit status: 0 PASS, 1 FAIL, 2 UNMEASURABLE, 3 INVALID.

Two more things the old harness got wrong and no longer does:

- `mount_listed()` was a boolean over an unchecked `/sbin/mount`, so a failed or truncated read read
  as "not mounted" and could have orphaned a mount. It is tri-state now: an exact decoded match is
  MOUNTED, a complete parseable table with no match is NOT_MOUNTED, and a table that could not be
  read whole is UNKNOWN, which blocks any unmount, walk or deletion.
- the native arm's filesystem was recorded with `df -T`, which macOS does not support, so the
  record was empty and nothing asserted the arms were on different filesystems.
  Identity now comes from the mount table plus `st_dev`, and equal `st_dev` is refused before any
  case runs.

Tool integrity is enforced, not just recorded: the checkout must be clean at the pinned commit,
`pjdfstest.c` and every case script must hash to the pinned commit's own blob rather than to a
caller-supplied value, and a mismatch is INVALID before any case runs.
Every child is registered with its pid, start time and argv before anything waits on it, and a
signal is refused unless that pid still reads as the process this harness started.
No process group, no `pkill`, no mount walk.

## Remaining scope

- **The Linux FUSE arm is UNMEASURABLE.** `moonscape` has `/dev/fuse` and `fusermount3`, so the arm
  is reachable and the gap is a cross-build plus the shared Linux lock, not an absent host.
  Starting it is a separate authorisation; this lane did not.
- **Privilege-gated assertions.** 1968 native and 1981 cowfs assertions need root.
  None ran, none passed, and they are excluded from the verdict rather than scored.
- **The native oracle is weak.** 3733 native and 3565 cowfs non-privileged assertions fail on the
  native arm too, so neither arm can adjudicate them.
- **80 cases per arm are declined by the suite**, each with a reason in the pinned source:
  the FreeBSD-only granular permission batteries, `chflags`, `lchmod`, `posix_fallocate`,
  `utimensat`, `rename_ctime` outside EXT4/UFS/ZFS, and the NFSv4 ACL cases.
- **11 host capability macros are absent**, so that POSIX surface is untested here:
  `posix_fallocate`, `bindat`, `connectat`, `lpathconf`, `chflagsat`, `lchflagsat`,
  `HAS_NFSV4_ACL_SUPPORT`, and the `st_atim`, `st_ctim`, `st_mtim`, `st_birthtim` spellings.
  The probes are honest: a macro is defined only when a program taking the address of the function
  compiles and links, which is what `AC_CHECK_FUNCS` does.
- **No performance claim.** This is a correctness gate.
  The per-case durations were recorded while other workers held the machine, so they are not
  evidence about speed, and nothing here is a soak, a capacity or a power-loss result.

Gate g3 does not close on this evidence.

## Ownership

No production source was changed here, because every candidate site belongs to another lane.

| finding | owner |
| --- | --- |
| fifo, socket and device-node creation unsupported on both adapters | #19 NFS server requirements, with a Core create-with-type path behind it |
| `pathconf` answers -1 on the mount | Core or adapter gap, `pathconf` comes from the file system |
| `rmdir a/b/..` answers `EINVAL` | Core or adapter, reported for triage |
| `nlink` stays 1 after unlinking an open file | Core or adapter, reported for triage |
| `chown` and `lchown` answer success and change nothing | #43, as a Vfs trait contract question: `crates/cowfs-vfs/src/types.rs` defines `SetAttr` with no uid or gid, so ownership is fixed by design |

The `chown` reading is qualitative and verified from the source.
It is not a security finding: nothing is granted, and the divergence is that a caller is told
success and then reads back an answer it did not ask for.

## Reproducing

```sh
# Re-derive the historical verdict from the preserved records. No mount, no daemon, no build.
python3 bench/pjdfstest.py --reconcile bench/out/ready-g3/run/20261005T004337Z

# A small matched run. The whole invocation holds the shared Mac lock.
rtk proxy python3 -c 'import fcntl,os,subprocess,sys,time; p=sys.argv[1]; os.makedirs(os.path.dirname(p),exist_ok=True); f=open(p,"a"); until=time.monotonic()+600
while True:
 try: fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB); break
 except BlockingIOError:
  if time.monotonic()>until: print("resource lane busy; blocked",file=sys.stderr); sys.exit(75)
  time.sleep(.25)
sys.exit(subprocess.run(sys.argv[2:]).returncode)' /Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/mac-heavy.lock \
  python3 bench/pjdfstest.py --tests mkfifo/00.t,open/17.t,mkdir/00.t,rmdir/12.t,unlink/14.t

# The comparator's own checks, all synthetic fixtures.
python3 bench/test_pjdfstest.py
```

The curated repair evidence, with per-number provenance, is in
`docs/verification/evidence/pjdfstest-g3-repair.md`.