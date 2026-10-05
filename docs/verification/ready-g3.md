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
| receipted small run | `bench/out/ready-g3/run/20261005T025742Z`, 5 cases per arm at this head, raw streams and hashes kept, `identity.json` written before teardown, `cases.jsonl` sha256 `eb2ff1245baa4aa306b2a6458cdde6e32abced1120987283898d5c03ff51ecdc` | a complete matched verdict with a live identity receipt and raw evidence for every case |
| superseded small runs | `20261005T022439Z` and `20261005T022515Z`, raw records preserved untouched | nothing about the filesystem: both recorded `cowfs_fs` as unusable, with `stat failed`, so a machine verdict on either is INVALID |

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
| Arm separation | from `identity.json`, written while the mount was up: native `apfs` at `/`, `st_dev` 16777234, source `/dev/disk3s1s1`; cowfs `nfs` at the run's own mount, `st_dev` 436209661, source `localhost:/cowfs-de4387245f4a6633cfc77c43fa2d25bd`, `problem` null |
| Daemon | pid 79659, registered before the mount wait, SIGTERM after its argv was re-verified, mount absence proven from a complete 14-line table |
| Measured cowfs build | head `025bde2f3ccea609f54edf67a9d86d66d28abeca`, `cowfs-daemon` sha256 `4804a16546a87679...`, `cowfs` sha256 `33055fed260adfc...` |

The same five cases produced the same counts on every run of the repaired harness.

**Withdrawn: the `st_dev` 436209625 that the previous revision of this document reported for the
cowfs arm.** It is in no preserved file.
Both earlier runs recorded `cowfs_fs` with `fstype`, `mountpoint`, `st_dev`, `source` all null and
`problem` set to a `stat failed` line, because the identity was read after the mount was torn down.
A device number that exists only in a chat message is not a receipt, and it was the single number
that was supposed to prove the two arms are on different filesystems.
The raw records of both runs are preserved byte for byte and are not rewritten; they simply cannot
carry a verdict, and reconciling either now returns INVALID 3.
The receipted run above is the replacement, and the independent review's own five-case sample
carries the same receipt with `st_dev` 436209639 (`docs/reviews/pjdfstest-g3-repair-final.md`).
NFS `st_dev` is assigned per mount, so the three numbers differing in the last digits is expected
and means nothing on its own; what matters is that each is inside its own run's receipt.

## Disclosure: an independent reviewer regenerated three derived files in this lane's tree

Running `--reconcile` against this lane's preserved run directories wrote `reconciliation.json`
into `20261005T004337Z`, `20261005T022515Z` and `20261005T025742Z`, overwriting files that were
already there.
That was the reviewer's error to make and this lane's error to have made possible: the command wrote
into the directory it was reading.

What is intact, verified by hash and unchanged on disk:

| input | sha256 |
| --- | --- |
| `20261005T004337Z/cases.jsonl` | `bb55fe4912a36302...` |
| `20261005T022515Z/cases.jsonl` | `1d64dabcca863b16...` |
| `20261005T025742Z/cases.jsonl` | `eb2ff1245baa4aa3...` |
| `20261005T025742Z/identity.json` | `31d92ea0d2bd89d7...` |
| all ten raw streams of the receipted run | present, hashes match their records |
| pinned `pjdfstest.c` and built binary | `a6c354f2c42015a1...`, `5fa40986f39bb903...` |

What was lost is derived: three `reconciliation.json` files whose content now reflects a later head.
Their bytes at the time they were written are not recoverable, and they are not reconstructed here
as though they were.
The files present now are the reviewer's regenerations, kept as they are, and every analysis this
lane produces from now on carries its own revision and input hashes so a reader can tell which
reading is in front of them.

The classifier numbers are unaffected, because they derive from the intact `cases.jsonl`.

`--reconcile` no longer writes into the evidence it reads.
The default destination is refused the moment anything is there, an explicit `--output` must sit
outside the run directory it reads and must not exist, and the write itself is staged and then
`link`ed into place, which is the one creation call that refuses an existing name atomically, so
there is no window between checking and writing for a second writer to be overwritten.
A staged file that cannot be linked is kept as evidence of the failure.
Every analysis carries an `analysis` block: this analyser's sha256, the source head, the pinned
tool commit, and the sha256 of every input it read.
Named tests cover the sentinel case, the unchanged input run, the fresh isolated output, an output
pointed at `cases.jsonl`, an output pointed at a raw stream, an existing explicit output, and a run
without an identity receipt.
Those tests run against a fixture copied into this lane's own
`bench/out/ready-g3/reconcile-safety/**`, never against a preserved run.

## A reconciliation without the pinned scripts returned PASS

The previous repair fixed the identity guard and the writer.
It left a hole that made the gate unsound in the one direction that matters, found by the
independent review and reproduced here before anything was changed.

Pairing is proved from the upstream case scripts.
Without them every assertion falls through to the text route, which needs operation text the suite
does not print on a pass, so a run with no scripts pairs almost nothing, finds no regression, and
reaches PASS.
A reader would take that for a clean bill of health on a filesystem nobody tested.

`bench/out/pjdfstest-fresh-clone-spike/` holds the spike, the copied run it reads and both of its
recordings.
One input, three readings, the boundary instrumented before anything is classified:

| reading | profiles built | established | unpairable | exit |
| --- | --- | --- | --- | --- |
| with the pinned scripts | 5 of 5 compared cases | 25 | 26 | 1 FAIL |
| without them, a checkout with no cache | 0 of 5 | 0 | 170 | 0 PASS, the bug |
| without them, after the repair | 0 of 5 | 0 | 170 | 3 INVALID |

Calling `verdict()` directly with `tests_root=None` returned `PASS exit 0` with 170 unpairable and
no kind of reason but coverage, which is the same hole seen from inside.
The number that matters is the exit, not the count: 170 unpairable on its own is a limitation, and
170 unpairable with nothing else present was a pass.

The repair treats the pinned scripts as a prerequisite, checked before anything is classified:

- a run whose compared cases have no verified script, or no verified source at all, is INVALID 3
- a source that is not the pinned commit is INVALID 3, whether it is a modified checkout or a
  curated closure whose bytes moved
- coverage that is genuinely unpairable is still disclosed, and only disclosed, on top of a
  classification that actually ran

A record set that reaches beyond a curated closure therefore cannot be scored from it: the gate
requires a profile for every compared case, and the closure proves five.

## Every refusal is typed, and no refusal follows a symlink

The writer was already exclusive and staged.
It was not closed against the ways a filesystem says no.
Each of these is now an INTEGRITY refusal with a message and no traceback, and each is a named
check that runs the CLI as a child process:

| condition | refusal |
| --- | --- |
| the default destination already exists | 3, not one byte of the input changes |
| an explicit destination inside the run it reads | 3, `cases.jsonl` unchanged |
| an explicit destination naming a raw stream | 3, that stream unchanged |
| an explicit destination that is a symlink, live or dangling | 3, the link is still a link and its target is untouched |
| a destination whose parent does not exist | 3, `could not be staged` |
| a destination whose parent is a file | 3, `could not be staged` |
| a destination in a directory without write permission | 3, nothing written, no staging file left |
| a `link` that fails for any other reason | 3, and the staged copy is kept as the evidence |
| a destination that already exists, explicitly | 3, that file unchanged |

The destination's parent is resolved and its last component is not, so a symlink named as the
output cannot redirect an analysis onto a foreign path.
Publication is a `link`, which is the one creation call that refuses an existing name atomically,
so there is no window between checking and writing for a second writer to be lost in.
Only this run's own staging file is ever removed, and only after the bytes are published.

## What a receipt can and cannot claim about its own code

The provenance block used to carry a `source_head` read from the nearest git checkout.
That is not where the analysis came from.
It recorded this lease's HEAD while the analyser was some commits ahead, so it attributed the result
to code that did not produce it.

- `analyser_sha256` is authoritative: the sha256 of the script that actually ran.
- `analyser_revision` is a commit only when the blob HEAD records for this path is the same object
  as the script on disk, and `UNKNOWN` otherwise: an unrelated checkout, an edited script, or no
  repository at all.
- `ambient_checkout` carries the checkout with its head and says in the block itself that it is not
  the origin of the analysis.
- The `source_head` field is gone, and a named check fails if it ever comes back.

A reconciliation is a fresh reading of preserved records, so it also carries the sha256 of every
input it read, which is what makes two readings of the same run distinguishable.

## The checks no longer need a developer machine

Seven of the previous checks were collected but skipped unless the gitignored tool cache happened to
exist, which meant the writer was untested in a clean tree and a green run did not say so.
They now build their own copy of a tracked fixture and none of them can skip for want of a cache.

`bench/pjdfstest-fixture/` holds the record set of the receipted run `20261005T025742Z`, byte for
byte: the ten transcripts, the identity receipt, and the ten case records whose only edit is the
raw path, made relative so the fixture is portable.
Each record's `raw_sha256` still verifies.
It also holds the five upstream case scripts those records name, and the harness pins each of them
by the sha256 it has in the pinned commit, so a closure whose bytes moved is refused rather than
trusted.

What the fixture is not, stated in its own README: it is not new filesystem evidence, it is not
acceptance, and it is not a substitute for the live receipt.
The transcripts were captured once, on 2026-10-05, and the live run directory remains the source of
truth.

Counts from this repair:

- 56 checks in `bench/test_pjdfstest.py`, none skipped, ruff and py_compile clean
- 92 checks from `python3 -m unittest discover -s bench` in a clean copy of the tracked files with
  no `bench/out` and no `.git`: all pass, no skips. 56 are this lane's and 36 belong to
  `bench/test_gates.py`, which another lane owns
- 17 checks in the writer and classification class, run as written, reversed, in three seeded
  shuffles, and each alone in its own process: no failures and no order coupling
- the two classification checks fail against the previous source in a tree with no cache, one with
  exit 0 and one with `PASS`, so they test the gate rather than the machine it ran on

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
Reasons are typed, so the exit status follows from what went wrong rather than from wording:

| kind | means | exit |
| --- | --- | --- |
| INTEGRITY | the run's own records or provenance cannot be trusted: malformed or truncated stream, a raw hash that moved, a synthetic fixture, a case-integrity failure, a mixed record format, tool-source drift, a missing or invalid runtime identity | 3 INVALID |
| CAPABILITY | the tool, a prerequisite or a capability is absent, so nothing ran | 2 UNMEASURABLE |
| DIVERGENCE | an established assertion passes on one arm and fails on the other | 1 FAIL |
| COVERAGE | a limit on what the transcript can conclude: unpairable assertions, identity unrecoverable | disclosed, never an exit on its own |

So exit 0 PASS, 1 FAIL, 2 UNMEASURABLE, 3 INVALID, integrity outranks everything, and partial
coverage never turns a real divergence into a pass or into an unmeasurable.
Both the verdict function and the command line are covered: the test suite runs the module as a
child process and reads its real exit status, so a refusal proven only by a predicate would fail
that test.

Two more things the old harness got wrong and no longer does:

- `mount_listed()` was a boolean over an unchecked `/sbin/mount`, so a failed or truncated read read
  as "not mounted" and could have orphaned a mount. It is tri-state now: an exact decoded match is
  MOUNTED, a complete parseable table with no match is NOT_MOUNTED, and a table that could not be
  read whole is UNKNOWN, which blocks any unmount, walk or deletion.
- the native arm's filesystem was recorded with `df -T`, which macOS does not support, so the
  record was empty and nothing asserted the arms were on different filesystems.
  Identity now comes from the mount table plus `st_dev`, and a run is refused before any child
  process exists unless **both** arms report a positive integer `st_dev`, a filesystem type, a
  mount point and no `problem`, the cowfs mount point is the path this run asked the daemon for,
  and the two devices differ.
  An earlier version compared the two devices only when both were present, so two null devices
  compared equal and passed; that is closed, and the negatives are named tests in which the spawn
  callback is never reached.
  The guard runs before any **case** child process exists, not before any child: the cowfs arm's
  filesystem only exists once the mount is up, so the daemon and its snapshot are already serving
  when the identity is taken. What is guaranteed is that no case runs and no receipt is trusted on
  an unplaceable arm.
  The validated identity is written to `identity.json` while the mount is up, so it survives
  teardown, and it is passed to `verdict()` explicitly rather than read back from a summary that
  does not exist yet.

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
# The pinned scripts are a prerequisite, so they are named; the default is only a convenience.
python3 bench/pjdfstest.py --reconcile bench/out/ready-g3/run/20261005T004337Z \
  --tool bench/out/ready-g3/tool/pjdfstest --output /tmp/g3-004337.json

# A small matched run. The whole invocation holds the shared Mac lock.
rtk proxy python3 -c 'import fcntl,os,subprocess,sys,time; p=sys.argv[1]; os.makedirs(os.path.dirname(p),exist_ok=True); f=open(p,"a"); until=time.monotonic()+600
while True:
 try: fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB); break
 except BlockingIOError:
  if time.monotonic()>until: print("resource lane busy; blocked",file=sys.stderr); sys.exit(75)
  time.sleep(.25)
sys.exit(subprocess.run(sys.argv[2:]).returncode)' /Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/mac-heavy.lock \
  python3 bench/pjdfstest.py --tests mkfifo/00.t,open/17.t,mkdir/00.t,rmdir/12.t,unlink/14.t

# The comparator's own checks. 56 of them, none of which needs the tool cache or a mount.
python3 bench/test_pjdfstest.py
python3 -m unittest discover -s bench

# The bounded spike that found the false PASS, in both directions.
SPIKE_WITHOUT_TOOL_EXIT=3 python3 bench/out/pjdfstest-fresh-clone-spike/spike.py
```

The curated repair evidence, with per-number provenance, is in
`docs/verification/evidence/pjdfstest-g3-repair.md`.
The spike that found the false PASS is written up in
`docs/verification/evidence/pjdfstest-g3-spike.md`.