# pjdfstest g3 repair: independent review

Reviewer lane: read-only review of PR #106 at head `d004caacf42d0859d61018c7aa119cf4c529cc35`, compared against the head I reviewed previously, `025bde2f3ccea609f54edf67a9d86d66d28abeca`.
Lease `a53321161c0b6660c9124671c6c6654c`, branch `review/pjdfstest-g3`, fetched over HTTPS by exact SHA.
No production source edited, no commit, no push, no merge, no lease return, no workflow dispatch, no rerun, no poll.

| lane | verdict |
| --- | --- |
| identity classifier, route 1 "script slot" | **PASS**, manually proved for all 36 cases where it fires |
| guards, process registry, tri-state mount, exit taxonomy | **PASS** with named gaps, no path from a bad run to a PASS |
| own small matched sample, 5 cases, real mount | **PASS**, reproduces every claimed number and supplies the receipt the builder's artifacts lack |
| canonical write-up and evidence doc | **BLOCKED** on three counts, listed below |
| macOS NFS against private real-Core mount | **FAIL**, independently reproduced by me |
| Linux FUSE arm | **UNMEASURABLE**, not attempted, still missing coverage |

## What the repair gets right, and what I could not break

The comparator no longer pairs by stream position.
It pairs by what the pinned script proves, and it says which of its two routes produced each pair.
That is the correct shape for this problem, because the old 687 was an ordinal differential across two control flows that diverge the moment `chown` stops answering `EPERM`.

I re-ran the new classifier myself, importing the new module and feeding it the historical 476 records plus script profiles read from the pinned checkout.
It reproduces the claim exactly: **77 established regressions, 71 outside the privilege gate, 0 candidates, 10050 unpairable, 5563 of those with no operation text**, distributed `unlink/00.t` 30, `mkfifo/00.t` 22, `mknod/00.t` 22, `open/17.t` 3.
Every figure in the evidence doc's accounting table that comes from the historical record set also reproduces.

Route 1 fires for exactly **36 of 238** cases.
I did not accept that on the strength of the regexes.
I enumerated every assertion site, loop, conditional, local function definition and sourced file in all 36 scripts, then closed the transitive chain by hand.

- All 36 have **zero `for` loops, zero `while` loops and zero locally defined functions**.
- All 36 source exactly one file, `${dir}/../misc.sh`.
- The only function in `misc.sh` that calls `expect`, `test_check`, `jexpect` or `create_file` is **`create_file`**, and `script_profile` blocks on the string `create_file`.
- `misc.sh` emits **no** assertion at source time; its top level is variable setup, two search loops, and one `echo "not ok - could not source configuration file"` failure path, which `parse_tap` would catch as a malformed pre-plan line and refuse.
- `namegen` emits a name, not an assertion.
- `require` calls `supported` and then `quick_exit`, and emits nothing.

So for those 36 the chain is script to `misc.sh` to nothing, with the only assertion-emitting helper deliberately excluded, and the site count equals the emitted count on both arms.
Route 1 is sound for all 36, and I can say that from the scripts rather than from the function's name.
Under-counting a site would break the count equality and fall through to route 2, so the parser errs in the safe direction.

The counterexamples the task asked about are covered by named tests, and I ran them: a removed result does not repair a pair, an added result does not shift the next identity, a repeated operation without a literal loop is unpairable, a text duplicate is ambiguous rather than paired, generated names do not change identity while argument order is not provable from names, error text is never part of an identity, textless assertions are never paired, a stream that contradicts the script slot is refused.
Guards: bail out plus non-zero child, duplicate and non-contiguous ids, plan mismatch, wrong case name, truncated stream, synthetic never conformance, legacy flagged.
Mount: exact decoded match is MOUNTED, a prefix is not a match, an unreadable table is UNKNOWN and not absence, exit taxonomy fixed.
**25 tests, all pass, exit 0**, run as a pair in isolation so the old module could not shadow the new one.

`FAIL` takes priority over `UNMEASURABLE` in `verdict()`, so a genuine filesystem failure is never downgraded by partial coverage.
That was the right call and it is the opposite of the trap.

## Three blockers

### 1. The arm-separation number in both documents is in no artifact

The canonical write-up and the evidence doc both state, for the repaired small run:

> native `apfs` at `/`, `st_dev` 16777234; cowfs `nfs` at the mount, `st_dev` 436209625

The artifact they cite, `run/20261005T022515Z/summary.json`, records:

> `cowfs_fs`: `fstype` null, `mountpoint` null, `st_dev` null, `source` null, `problem`: `stat failed: [Errno 2] No such file or directory: .../run/20261005T022515Z/mnt/pjd`

`436209625` appears in **no** preserved file under the builder's `bench/out` tree.
The same all-null `cowfs_fs` is in the other repaired run, `20261005T022439Z`.
So the receipt says the cowfs arm's filesystem was never identified, and the documents report a measurement of it anyway.
The value is in the plausible NFS range and may well come from an unpreserved attempt, but as written it is unreceipted, and it is the single number that proves the two arms are different filesystems.

The underlying code fix is real and does work.
`arm_fs` is captured at `main()` before `run_arm` and written into the summary after teardown from the captured value, which is the correct shape, and `fs_identity` works: pointed at the live protected mount it returns `fstype nfs`, `st_dev 436207620`, `source localhost:/cowfs-6952209e...`, `problem null`.
The two preserved runs simply predate the fix.
That is exactly the situation where a receipt document should not be citing the run as post-fix evidence.

### 2. The separation gate fails open on an unidentified cowfs arm

`main()` refuses only when `native_fs["st_dev"] is not None and native_fs["st_dev"] == cowfs_fs["st_dev"]`.
When the cowfs arm cannot be identified, `st_dev` is `None`, the comparison is `16777234 == None`, which is `False`, and the run proceeds and is scored.
I confirmed this by evaluating the exact expression.

This matters because it is the one guard whose failure mode is invisible: the run produces a complete, plausible, FAIL-or-PASS summary with no statement that an arm was unattributable.
The task asked for "cowfs metadata unknown safe". It is not safe.
The fix is one clause: refuse when either `st_dev` is `None`, or when `fs_identity` reported a `problem`, not only when the two are equal.

`verdict()`'s own copy of the check is doubly inert: it requires both to be non-`None`, and on a fresh run `summary.json` does not exist yet when `verdict()` runs, because `main()` writes it afterwards.
It only does anything under `--reconcile`.

### 3. `ruff check` is 2 findings at this head, not 0

The evidence doc says "17 findings at the reviewed head, 0 at this head".
Same tool, same invocation, ruff 0.16.2: the old head is 17, the new head is **2**, both `EXE001`, "Shebang is present but file is not executable", one per file.
Fifteen of the seventeen were genuinely fixed.
The number is wrong by two and both are one `chmod` away.

## Two more corrections, smaller

**CI at this exact head is not green.** `ubuntu-latest` and `linux-fuse` pass; `macos-latest` is **pending**.
One pending check, not three.
The head is therefore not yet demonstrated green, and nothing here should be read as claiming otherwise.

**The exit taxonomy is coarser than advertised.** `INVALID` and exit 3 are reachable only from four setup refusals in `main()`: tool-source problems, no tests selected, a control socket over 103 bytes, and equal `st_dev`.
Every guard and integrity failure that `verdict()` detects, including a moved raw-stream hash, a malformed record, a truncated stream and a synthetic fixture, lands in `unmeasurable` and exits 2.
That is safe, because nothing in that set can produce a PASS, and the evidence doc's own test names the behaviour correctly.
But the document lists those refusals directly above a four-state table, which reads as if integrity failures exit 3.
An integrity failure and an unmeasurable run are different things and the code does not distinguish them.

## What the identity work does not buy, stated plainly

All 77 established regressions share one cause: a non-regular create answers `EIO`, or is a consequence of one.
The write-up says this and it is correct.
So the repair raises confidence in the count without adding a single new defect class.

The two divergences that are not that cause, `rmdir/12.t` #4 and `unlink/14.t` #4, are **not** in the established set.
They fall into the 26 unpairable, for a reason worth stating because it is the honest limit of the method:

- `unlink/14.t`: the native arm **passes**, and the suite prints no text on a pass, so the native side has no operation text to pair against the cowfs side's `not ok`. Its script has no blockers but 6 sites against 7 emitted assertions, so the count equality blocks route 1. The shortfall comes from a line-continued `expect` whose continuation the site parser drops.
- `rmdir/12.t`: blocked by the word `if`, which occurs **inside its own `desc=` string**, not as control flow. The script is straight-line with 6 sites and 6 assertions and would otherwise qualify.

Over-blocking is the safe direction, so neither is a soundness problem.
But it costs coverage on precisely the two cases worth triaging, and both causes are cheap to fix: strip quoted strings before matching `RESULT_DEPENDENT_RE`, and fold a line-continued assertion into one complete site.

These two are nonetheless established well enough for triage, and I checked them myself rather than trusting the document.
Both parent cases are short, six and seven assertions, with no loops, so there is no repeat-ordering question, and the assertion text names the same operation on both sides.
From **my own** raw streams: `rmdir/12.t` #4 native `ok`, mount `not ok 4 - tried 'rmdir <a>/<b>/..', expected ENOTEMPTY|EEXIST, got EINVAL`; `unlink/14.t` #4 native `ok`, mount `not ok 4 - tried 'open f O_RDONLY : unlink f : fstat 0 nlink', expected 0, got 1`.
Both match the pinned script's fourth `expect` line.
I also re-read the pinned source: `nfs_handlers.rs:135` returns `NFS3ERR_NOTSUPP` for every `NFSPROC3_MKNOD`, `PATCHES.md` lines 21 and 28 agree, `cowfs-fuse/src/fs.rs:613` and `convert.rs:51` answer `ENOTSUP` for a non-regular `mknod`, and `types.rs:103` gives `SetAttr` no uid or gid.
`error.rs:74` still maps `Corrupt` and `Io` to `EIO`, so the 198 `EIO` rows remain attributed **by inference**, as the new document correctly states, and the `NFS3ERR_NOTSUPP` to `EIO` client translation stays unproven because it lives in XNU.
No fake proof was manufactured for either, and both documents now say so.

## My own sample run, and why it mattered

The task required an own-mounted sample before any larger batch.
The shared lock was free, load was high, and lease 16 had no `target/release`, so I did **not** trigger the cold 405 MB release build.
Instead I read-copied the builder's release binaries, after confirming their sha256 against the manifest recorded in the builder's own summary: `cowfs-daemon` `4804a165...` and `cowfs` `33055fed...`, both matching.
`target/` is gitignored in the lease.
The tool checkout was the verified pinned one at `85a8aea9...`, and the harness independently re-verified all 238 case scripts against the pinned git blobs.

My first attempt was **refused by the harness itself** for a 137-byte control socket against the 103-byte `sun_path` limit, and I verified it left nothing behind: no socket directory, no mount, no daemon, no run directory.
That is the guard working, before any side effect.

I then ran 5 cases per arm, `mkfifo/00.t`, `open/17.t`, `mkdir/00.t`, `rmdir/12.t`, `unlink/14.t`, foreground, one bounded 600 s wait under the shared lock, own fresh mount and private store.

Result, and I re-verified every number by re-parsing my own raw streams from disk rather than reading the manifest:

| | native | cowfs |
| --- | --- | --- |
| cases | 5 | 5 |
| assertions | 88 | 88 |
| passing | 66 | 43 |
| failing | 22 | 45 |
| child exit non-zero | 0 | 0 |
| raw streams present and hash-matching | 5 | 5 |
| re-parse mismatches against the record | 0 | 0 |
| guard problems | 0 | 0 |

Established regressions **25**, `mkfifo/00.t` 22 and `open/17.t` 3, exactly as claimed.
Unpairable **26**, of which **24** with no operation text. Candidates **0**.
State **FAIL**, exit 1. Teardown clean: SIGTERM to the single registered pid, daemon exited 0, mount verified absent from a complete 14-line table, lease socket directory removed, zero mounts and zero daemons of mine left.
The three `open/17.t` regressions are the `mkfifo` `EIO` cascade, consistent with the shared-cause claim.

And the receipt the builder's runs lack, **mine carries**:

> `native_fs`: `fstype apfs`, `mountpoint /`, `st_dev 16777234`, `source /dev/disk3s1s1`
> `cowfs_fs`: `fstype nfs`, `mountpoint <run>/mnt`, `st_dev 436209639`, `source localhost:/cowfs-d3c41ca6...`, `problem null`

So the in-run capture fix works, and my `436209639` differs from the documented `436209625` in the last digit.
NFS `st_dev` is not stable across mounts, so that difference is not evidence of anything on its own; the point is only that the documented figure is absent from every receipt while mine is present in mine.
`cases_sha256` in my receipt carries **238** per-script blob hashes, so the anti-forgery manifest does persist, under `cases_sha256` rather than `cases`.
`cowfs_head` is `025bde2...`, because the repair touches only `bench/`, so the Core backend measured is the same build the builder measured.

One thing a manifest cannot do is prove a child actually ran.
I did not try to argue it could.
The evidence is structural instead: 10 separate `Popen` children with direct exit codes, one immutable directory each, 10 raw streams on disk whose hashes match and which I re-parsed to the same assertions, and a tool source verified blob-by-blob against the pinned commit before any case ran.
A forged manifest would have to forge all of that consistently.

## Regression audit, restated against my own numbers

Everything I derived independently from the raw records at the old head still holds, and the repair adopts it.
Native 5701 = 1968 privilege-gated + 3733 unadjudicated; cowfs 5546 = 1981 + 3565; **no** privilege-gated assertion passed on either arm.
The 1981 versus 1968 asymmetry is itself a consequence of the `chown` divergence changing which branch a case takes.
Ordinal totals: 700 regressions, 687 outside the privilege gate, and of those only 48 pair matching comparison text, all 48 textless, leaving 639 structurally different.
855 looser, **0** pairing matching text, of which **714** are `chown`/`lchown` and **141** are `lstat` uid rows, namely `rename/09.t` 70, `rename/10.t` 60, `unlink/11.t` 10, `mkdir/10.t` 1.
Partition summing exactly to 687: **198** `EIO` creates, **66** `pathconf`, **360** `ENOENT` cascades, **48** textless, **15** every other answer.
617, 63 and 713 do not reproduce under any reading, and both documents now withdraw them explicitly; the phrase that listed four checks under "three" is gone, and "Every regression is accounted for" is gone with it.
The native oracle still fails 43 percent of all assertions on plain APFS, and 3733 of those are outside the verdict by the same logic that excludes the privileged ones.
The new write-up discloses that, which the old one did not.

Gross suite failure is not in dispute: many pjdfstest cases do not apply to macOS or to an unprivileged run.
That does not soften the verdict, because the 25 established regressions in my run are matched, same-case, same-operation, script-proven pairs.
It does bound the coverage, and g3 cannot close on it.

## Scope still missing, unchanged

- **Linux FUSE arm unmeasured.** `moonscape` has `/dev/fuse` and `fusermount3`, so the host is reachable and the arm is available; the gap is a cross-build plus the shared Linux lock.
- 80 suite-declined cases per arm, each with an explicit reason in the pinned source. I confirmed the mechanism end to end: a `require` in the case consults `supported`, and on a host without the feature it calls `quick_exit`. Explanatory, never a blanket unsupported marker.
- 1968 native and 1981 cowfs privilege-gated assertions, none run.
- 11 absent host capability macros, all confirmed absent from the generated `config.h`, and the probes are genuine compile-and-link with no stubbed macro.
- 3733 native and 3565 cowfs non-privileged assertions the native oracle cannot adjudicate.

## Process safety, verified rather than assumed

`Registry` records pid, argv, registration time and the `ps` line **before** any wait.
Before signalling it re-checks that the pid is alive, that its start time and argv are unchanged, and that it carries this run's store or socket path, and it refuses to signal when any of that fails.
Hung cases are handled one child at a time, and a child is killed only if its own argv shows it inside this case's directory, otherwise it is left alone and the refusal is recorded.
No process group, no `pkill`, no pattern kill anywhere.

`mount_table` is genuinely tri-state: a non-zero exit, empty output, or a missing trailing newline all become UNKNOWN, and UNKNOWN blocks any unmount, walk or deletion.
`start_daemon` raises on UNKNOWN instead of proceeding, and `stop_daemon` reports UNKNOWN rather than claiming the mount is gone.
I exercised `mount_state` and `fs_identity` against the live protected mount at `~/.cowfs/mnt` read-only: exact decoded match, `nfs`, correct source and `st_dev`.
Teardown is name-scoped: only `c.sock` and `c.sock.lock` are unlinked, only after the socket is confirmed gone, and the parent directory only if it is then empty.
That replaces the old `shutil.rmtree(repo/"rt", ignore_errors=True)`, which swallowed failures and would have deleted a concurrent run's socket.
All waits are bounded: 180 s to serve, 120 s for the snapshot, 60 s for the mount, 120 s for exit, 600 s per case.

One residual: a teardown that ends UNKNOWN is reported in a log line but does not affect the verdict, so a run whose mount state could not be confirmed afterwards is still scored.
That is the safe direction for filesystem claims but it should be visible in the summary rather than only in the log.

I preserved everything: the historical record set still hashes to `bb55fe49...`, all 32 leases remain held, pid 15263 and the `~/.cowfs/mnt` mount are untouched, and the g4, g5, 987929D and 899604 mounts are as I found them.
I wrote only under my own lease `bench/out/ready-g3-repair-critic/**` plus the two gitignored release binaries I copied in, and this document.

## Ownership and the closing-reference trap

`closingIssuesReferences` on PR #106 returns `totalCount 0` with an empty node list, so merging closes nothing.
The repair adds one commit, `fix(g3): pair pjdfstest assertions by script-proven identity, not by stream position`.
`fix(g3):` is a conventional-commit prefix with no issue reference, so it is not a GitHub closing keyword; the added commit messages contain no `close`, `fixes`, `resolves` or `#N` at all.
The bare `#107` to `#110` bullets remain neutral references.
I did not find the historical pattern the task warned about, a negated "does not close" phrase, in the current body.
No acceptance issue is at risk from this PR.

Issue ownership is unchanged and correct: non-regular creation belongs to the NFS server-requirements lane #19 with a Core create-with-type path behind it, `SetAttr` carrying no uid or gid is a Vfs trait contract question for #43, and #42 and #43 are open.
#107, #108 and #110 have had their titles corrected, #109 is narrowed to the two named controls.
I filed nothing, fixed nothing in production, and delegated to no builder.

## Required before merge

1. Replace `st_dev` 436209625 in both documents with a receipted value, or cite my run at `docs/reviews/pjdfstest-g3-repair-final.md` and drop the unreceipted figure. Better: re-run the 5 cases at this head so the canonical run directory itself carries a populated `cowfs_fs`.
2. Refuse to score when either arm's `fs_identity` reports a `problem` or a `None` `st_dev`, not only when the two are equal. This is the one guard that currently fails open.
3. Correct "0 ruff findings at this head" to 2, both `EXE001`.
4. Wait for the `macos-latest` check, or state plainly that the head is unverified on that leg.
5. Optionally strip quoted strings before `RESULT_DEPENDENT_RE` and fold line-continued assertions into one site, which would let `rmdir/12.t` and `unlink/14.t` qualify for route 1 and move the two triage-relevant divergences out of the unpairable set.
6. Decide whether integrity failures should exit 3 rather than 2, and make the document match the code either way.
7. Keep g3 open for the Linux arm, the 80 declined cases, the 3969 privilege-gated assertions, the 11 absent macros and the weak native oracle.