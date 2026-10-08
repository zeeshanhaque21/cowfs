# Verification: full-stack daemon crash recovery with durable receipts

Issue: [#88](https://github.com/zeeshanhaque21/cowfs/issues/88).
Harness: `scripts/verify-daemon-crash.py`, tests `bench/test_daemon_crash.py`.
Finding: [#90](https://github.com/zeeshanhaque21/cowfs/issues/90), closed, repaired by PR #96.
Reviews: `docs/reviews/crash88-final.md`, `docs/reviews/crash88-repair-final.md`.
Post-#96 status analysis: `docs/reviews/issue88-status-20261008.md`.
The reviews predate #96, so no independent reviewer has seen the post-#96 result below.

## Verdict, post-#96, 2026-10-08

Measured at main `04bbdd0ec430eecd606dc55acd64d98f7d3a4611`, which contains PR #96 (merge `951045f`).
Harness sha256 `1c400c5edd34208eaf1d1c0a205cea4156e35ef534a553186aef36289aee3e10`.
Build artifacts, not reproducible: `cowfs-daemon` `7ea65e2eabd608fd3c5e1638ce8c9d60f7d7af4f57aac868cd28c13c9d163023`, `cowfs` `46aa94a1522f06c360c44ce3b81b3c1e0307e9d3456838bac83b97483d470b77`.
Transport and host: real in-process NFSv3 loopback, `cowfs-daemon --backend core`, macOS Darwin 25.6.0, local run and not a CI run.
Evidence is gitignored: `bench/out/crash88-post96/s88a` and `bench/out/crash88-post96/m88a` in the primary checkout.

Sample: `python3 scripts/verify-daemon-crash.py --stage sample --reps 1 --run-id s88a`.
Result: 2 executed, 0 reused, 2 passed, 0 failed, accounting balanced.
Cases: `write_fsync` and `rename_posix_durability`.

Matrix: `python3 scripts/verify-daemon-crash.py --stage all --reps 2 --run-id m88a`.
Result: `executed_all_passed`, exit 0, fresh acceptance yes.
Executed 29 (28 cowfs and 1 native), reused 0, accounting 29 planned and 29 accounted and balanced.
Passed 29, failed 0, in about 3m05s.
Receipts issued: 72, of which 52 durable, 18 applied and 2 removed.
Every one of the 52 durable receipts matched the sha256 read back through a fresh daemon on the same store.
fsck was clean in 28 of 28 cowfs cases.
Before #96 the same matrix was 23 passed and 6 failed, all six at the `rename` plus `fsync` boundary (history below).

| scope | verdict |
|---|---|
| the instrument: a bounded, runnable, fail-closed crash harness against the real daemon, CLI and mount | **holds** |
| zero data loss for the sampled process-crash windows (SIGKILL) on this Mac: 2 matrix reps per cowfs case, 2 sample reps of two cases, 1 native | **met** |
| `rename` + `fsync` boundary, formerly blocked by #90 | **met**, 4 of 4 `rename_posix_durability` and 2 of 2 `rename_posix_durability_ro` |
| gate g6 for power loss, mid-GC crash and internal fsync orderings | **NOT covered** |

This is a finite matrix of 2 reps, scoped evidence and not a proof over all crash windows.

### Not covered

Power loss is not tested.
SIGKILL kills a process, not the kernel, and the host page cache survives, so fsync is never really exercised.
A mid-GC crash is not tested.
The kill lands after the gc ack, and the gc case frees 0 bytes (`freed_bytes` 0 with 167 and 164 candidate blocks in the two reps) because a small fixture lives in the open pack.
Internal orderings are not tested: pack fsync to watermark advance, and watermark advance to metadata commit.
They have no public boundary to drive.
The native control runs on APFS, where a process kill cannot expose a missing fsync, so it validates the recipe and readback only.
Concurrent writers and `shutdown` as a crash boundary are not sampled either.

## Superseded verdict, before #96, kept as history

Everything from here to "Superseded evidence" describes the pre-#96 run at `90c9a8f` unless a section says otherwise.
Its counts and its "BLOCKED" verdict are superseded by the section above.

| scope | verdict |
|---|---|
| the instrument: a bounded, runnable, fail-closed crash harness against the real daemon, CLI and mount | **holds** |
| success criterion 3 (zero data loss in crash-injection tests) and gate g6, for the `rename` + `fsync` boundary | **BLOCKED** (then) |

**The run exits non-zero on purpose.** 6 of 29 executions fail, every one the same boundary: a
caller's `fsync` returned success and the name it was supposed to make durable was gone after
the crash. That is a product failure, not a harness artefact, and it is not wrapped in an
expected-failure handler.

```
$ scripts/verify-daemon-crash.py --stage all --reps 2
verdict           : executed_with_failures (exit 1)
fresh acceptance  : yes
executed / reused : 29 / 0
accounting        : 29 planned, 29 accounted, balanced=True
passed / failed   : 23 / 6
```

## Provenance

The run is identified by a tuple, and the harness refuses to reuse a cached verdict unless all of it
matches. The fields are not all the same kind of thing, and conflating them was a defect an
independent reviewer found in an earlier revision of this section, so the distinction is stated here.

**Source identity, reproducible.** These identify the inputs, and a reader who checks out the same
revision gets the same values:

| | |
|---|---|
| git rev the run was executed at | `90c9a8f8eed9ac5d21fa098d97ee020e23c6a059` |
| harness digest, sha256 of `scripts/verify-daemon-crash.py` | `047cf9750bcfa98d7659ee0d4ecd90f2e2b3e325cb5a95b38473f50beb772840` |

**Build-artifact identity, not reproducible.** A dev-profile build of one fixed source tree is not
byte-reproducible, so these identify *one particular build* and nothing about the source it came
from:

| | |
|---|---|
| `cowfs-daemon`, this run's artifact | `4f29fab15f09ac2754c8ee0d4b7b9b0c515b620fbf10b89c6c010c99844085d8` |
| `cowfs`, this run's artifact | `9567bded815291568a523c6d7a5772e9fd997d797da19cd9d628bee56ca0c5b5` |

Measured, not assumed: rebuilding `cowfs-daemon` three times from unchanged source on this tree, with
no edit between builds and no absolute build path embedded, produced three distinct digests. An
independent reviewer got five distinct digests from five builds of the same tree, and could not
reproduce this report's integrated digests from that tree at all. Both observations are the same
fact: a differing digest is evidence that two *artifacts* differ, and is not evidence about source.

The cache consequence is fail-safe and worth stating: because the two artifact digests are part of the
match key, a rebuilt binary invalidates a cached verdict and the case re-executes. That is
over-rejection, not under-rejection. No verdict has ever been reused across differing inputs.

Also part of the record, neither of them source identity:

| | |
|---|---|
| evidence | `bench/out/crash88/accept-final/` (gitignored, so not readable from the repository) |
| transport | real in-process NFSv3 loopback, `cowfs-daemon --backend core` |

The harness digest is the field that actually pins the instrument, and it is reproducible: it reads
`047cf9750bcfa98d` at both `90c9a8f` and `1a2b4f5`, `d328d411` at `2db2f7f` and `1c400c5e` at
`7d9380a`. An independent reviewer reproduced every count below at `1a2b4f5`, whose harness digest
is identical, because that revision only touched this document.

The harness does not exist at the base commit `ceb96c6`, and no row here claims it does. An
earlier revision of this report cited `ceb96c6` and was wrong.

### Pre-#96 (`90c9a8f`): the binary's source inputs changed after that run, so the pre-#96 counts above are not re-asserted for current main

This branch has since merged main at `46b0f269d5bef4a2c204c25f5b3015da601d3beb`, which carries #93
and #95. The 29-execution run above was measured at `90c9a8f`, **before** both.

The evidence that the inputs changed is the tracked source, not the artifact digests. An earlier
revision of this document argued it from differing digests, which does not follow for the reason
given above. The source evidence:

| tracked file | at `90c9a8f` | at main `46b0f26` | changed by |
|---|---|---|---|
| `crates/cowfs-core/src/ns.rs` | blob `84088323fb5798c129d1f3454bce88d60ca0bfbc` | blob `5a1024c115d51806131c6f6cc9ff99f5f20e6588` | `2d00743`, #94 |
| `crates/nfsserve/src/tcp.rs` | blob `c92d50e07fd4532fa423fd89cd17b2e018d13542` | blob `97e3ecd7363995d6b178509dbefd417ec7524184` | `af1d113`, #93 |

Five tracked `.rs` files differ between the two revisions in total: those two, plus
`crates/cowfs-core/tests/elide_dentry.rs`, `crates/cowfs-nfs/src/mount.rs` and
`crates/cowfs-nfs/tests/resource_bounds.rs`. The `ns.rs` change is a correctness fix to the dentry
cache: an elided create now marks the entry dirty with the queued `seq` instead of `0`, so a
queued unlink the cache has not seen cannot be lost. The `tcp.rs` change makes `EMFILE` from `accept`
recoverable rather than fatal.

Both are in the shipped code paths this harness exercises: `ns.rs` is the namespace dentry cache
behind the metadata layer, and `tcp.rs` is the NFS listener. So the binary the run above measured
was built from genuinely different source.

Therefore the six losses, the 46-of-52 durability match and every other count above are properties of
the **pre-merge source**. They are carried forward as that run's record and nothing more. Re-running
all 29 cases purely to restate unchanged numbers was not done: eleven other workers hold the shared
heavy lane, and the brief for this revision scopes the re-measurement. What is needed instead is a
scoped re-measurement of the failing boundary on the integrated source, which is the next section.

The integrated build's digests, `eb48f336` for the daemon and `e0cc963b` for the CLI, are recorded
below as the artifacts that particular run used. They are build-specific, they are not reproducible,
and they are not offered as evidence of anything about source.

### Pre-#96 (`90c9a8f`): scoped re-measurement on the integrated binary

Executed on the merged tree, one sample per case, real daemon, real CLI, real NFS loopback, private
store, SIGKILL and reopen:

| | |
|---|---|
| executed / reused | 2 / 0 |
| planned / accounted | 2 / 2, balanced |
| passed / failed | 1 / 1 |
| `write_fsync` | passed |
| `rename_posix_durability` | **failed** on `durable_present`: the caller's `fsync` returned success and the bytes are gone |
| `fsck_clean`, `snapshot_is_dir` | passed |
| run exit | 1, `executed_with_failures` |
| receipt | one `durable` receipt, `nfs_commit` boundary, seq 1, on `live/moved.bin` |
| teardown | `unmount.after_kill` and `unmount.teardown` both `not_mounted`, `abandoned: false` |
| harness digest, source identity | `1c400c5edd34208eaf1d1c0a205cea4156e35ef534a553186aef36289aee3e10` |
| `cowfs-daemon` artifact, build-specific | `eb48f336d56c7632e15a12481a6e4cee5a2eadf88f104fcd80db98db69c39e0a` |
| `cowfs` artifact, build-specific | `e0cc963bb66d897f4b0567ebe8c3966f748f81cdead5e6a447fddd6c7936029d` |
| evidence | `bench/out/crash88/integrated-sample/` (gitignored) |

The two artifact digests identify the binaries that particular run executed. They are not
reproducible from the source and are not evidence of it; the source identity for this run is the
harness digest and the merged revision.

The finding is therefore **unchanged on the integrated source**: a successful POSIX
parent-directory `fsync` after `rename` still emits no COMMIT on this client, and the promised name
is still lost. That is the same defect issue #90 owns, and it is still not fixed here. Receipt
classification is unchanged: nothing was downgraded to make the run read better.

This is a deliberate scope boundary. Neither #93 nor #95 touches the `fsync`-after-`rename` COMMIT
path: #94 changes the dentry cache's dirty marking on an elided create, and #93 changes accept-loop
error handling. Re-running the full 29-case matrix on the merged source would restate numbers this
revision has no reason to restate, while the shared heavy lane is held by eleven other workers.

The teardown records are the first real-run evidence for the tri-state mount inspection. Both real
unmounts returned `not_mounted` after the fact with no abandonment, and the pre-existing
`unmount.refused_not_our_mount` still records a genuine absence when nothing is mounted. No
`unmount.blocked_unknown_mount_state` appeared, because the table was readable on this host.

What this run does **not** establish: the 29-case matrix on the merged source. Only the failing
boundary and its passing neighbour were re-measured. The other counts above remain the `90c9a8f`
run's and are labelled as such.

## The receipt model, and why nothing is reclassified

Read off the source:

- an NFS `WRITE` ack returns from `cowfs_core::op_write` with the bytes only in the node's
  in-memory state (`crates/cowfs-core/src/io.rs:100`);
- `cowfs_meta::db::Ack` defaults to `Applied` and nothing in the tree ever sets
  `Ack::Durable`, so a `cowfs snapshot create` returning 0 is applied, not durable;
- `os.fsync(fd)` on a file with dirty pages is the real receipt: the adapter maps `COMMIT` to
  `fsync(ino, false)` (`crates/cowfs-nfs/src/lib.rs:40`), reaching `op_fsync` ->
  `flush_snapshot` (blocks into the pack, then the metadata commit) and `meta.sync()`, whose
  `before_sync` hook is `cowfs_core::store_sync_hook` -> `Store::sync()` -> fsync of the pack,
  then `watermark.advance`.

| kind | meaning | after SIGKILL |
|---|---|---|
| `durable` | the caller's sync returned success | must be present and byte-identical; a miss **fails the case** |
| `applied` | genuinely un-fsynced work | presence or absence both recorded; absence is permitted by `docs/design.md` |
| `removed` | this harness deleted it | must be absent |

`Receipts` has no method that changes a receipt's kind, and a unit test asserts its absence. This
run's evidence contains zero demotion records, because the operation does not exist. Every
promise is written to the ledger when made (`receipt.issued`, 72 of them), every rename is
recorded (`receipt.repath`, 8), every durable promise gets the state it actually resolved to
(`readback.durable`, 52, one per durable receipt), and every case records its verdict before its
terminal record (`case.verdict`, 29).

## Pre-#96 (`90c9a8f`): results, 29 executions

| | |
|---|---|
| planned | 29 = 28 cowfs + 1 native control, and `accounting_balances: true` |
| terminals | 29 `case.terminal`, 29 `case.verdict`, 29 distinct attempt directories |
| receipts issued | 72: 52 `durable`, 18 `applied`, 2 `removed` |
| `durable` matched | 46 of 52 |
| `durable` **missing after a successful fsync** | **6** |
| `applied` | 18 survived, 0 lost |
| `removed` absent | 2 of 2 |
| durably promised snapshot names present | 10 of 10 |
| `fsck` | 28 of 28 clean, 0 problems |
| aborts / harness errors | 0 / 0 |
| wall clock | 122 s total, 6.4 s slowest case |

### Per boundary

| boundary | level | outcome |
|---|---|---|
| `write` + `fsync` | durable | matched, every rep |
| `write`, no `fsync`, kill immediately | applied | all survived |
| `write` + `fsync` plus a sibling `write` + `fsync` | durable | matched |
| **`rename` + `fsync(parent dir)`** | durable | **LOST, 3 of 3** |
| **`rename` + `fsync(read-only fd)`** | durable | **LOST, 3 of 3** |
| `rename` + sibling `write` + `fsync` (control) | durable | survived, 3 of 3 |
| `mmap` write, `msync`, `fsync` | durable | matched |
| `snapshot create` fork, committed by a later `COMMIT` | durable | both trees listed, matched |
| `snapshot rm` of a fork | n/a | base's durable bytes intact |
| `gc` cycle, then crash | durable | survivor matched before and after |
| idle daemon | n/a | reopened clean |

The failure record carries the sibling listing, so loss of a name is distinguishable from the
file vanishing:

```json
{"name": "assert.durable_present", "ok": false, "path": "live/moved.bin",
 "parent_entries": ["orig.bin"],
 "detail": "caller's sync returned success and the bytes are gone"}
```

`fsck` is clean in every failing rep and the old name is intact, so this is loss of an
uncommitted name, not corruption.

## Pre-#96 (`90c9a8f`) finding, fixed by #96: a successful POSIX `fsync` after `rename` bought nothing on this transport

`docs/design.md` lists atomic `rename` under full POSIX, and POSIX says a successful `fsync` on
the parent directory makes the new name durable. Here that call returns 0 and the name is still
lost, while the same bytes under the old name survive.

This is not a broken rename path: the control case forces a real `COMMIT` by dirtying a sibling
file in the same snapshot, and the rename then survives 3 of 3. A `COMMIT` that arrives commits
the queued `Op::Rename` (`crates/cowfs-core/src/ns.rs:557`) correctly.

It is a product problem because the lost object is an acknowledged namespace operation whose
barrier the caller was told had succeeded, and there is no supported way to obtain that barrier.
`cowfs_ctl::Request` has no `sync` (`crates/cowfs-ctl/src/types.rs:432`), and the only thing
that works through the shipped macOS transport is dirtying an unrelated file, which is not a
contract a caller can rely on.

### Wire measurement, and its limits

The harness reads the kernel NFSv3 client `Commit` counter with `nfsstat`, which needs no root,
so the mechanism is measured rather than inferred. The parser pins the NFSv3 section, because
`nfsstat` also prints an NLM section with its own `Commit` column; a unit test asserts the two
are not confused.

| step | `Commit` delta | `attributable` |
|---|---|---|
| `fsync(parent dir)` | 0, 0, 0, 0 | false, false, false, false |
| `fsync(read-only fd)` | 0, 0 | false, false |
| sibling `write` + `fsync` | 2, 2 | **true, true** |

`attributable` means "a positive `COMMIT` signal that stands clear of the host's own activity".
The rule is stated once, in `Probe.attributable_for`: a delta of zero or less is never positive
attribution, because a zero delta *observes* no activity rather than evidencing it, and a
negative delta means the counter was reset, which invalidates the reading. With a zero idle
drift any positive delta is attributable; otherwise the signal must reach `3 * drift`. The whole
table is a unit test, because the previous expression got it wrong by operator precedence and
reported `0/0` as attributable.

The delta is a raw difference of two cumulative readings and is **not** background-subtracted;
each report says so and carries the idle drift beside it.

Two honest limits. First, `nfsstat -c` counters are host-wide, not per mount, so on a busy
machine the drift can exceed a step's delta and the step is then reported as unattributable
rather than as "no `COMMIT`"; the earlier revision's run on a busy host had 6 of 8 reports
unattributable, and this one on a quiet host had none. Second, and this run's own numbers show
it: the six POSIX fsync steps each read a delta of exactly 0 on a host whose idle drift was
also 0. That is a clean *observation of no `COMMIT`* for those steps. It is not a proof that no
packet crossed the wire, and the report does not claim it is. The verdict rests on the
crash-side measurement; the wire probe is corroboration with a stated noise floor.

### Native control, same operations

The APFS control performs the identical sequence: a durable write, a rename, a
parent-directory fsync, a read-only descriptor fsync, then the writer `SIGKILL`s itself.

| control | result |
|---|---|
| writer killed (`rc -9`) after rename + both fsyncs | the renamed file matched its source hash |
| clean restart (`rc 0`) | matched |

This is where cowfs diverges from native, and the reason it matters: on APFS a returned `fsync`
keeps the name; on the shipped cowfs macOS transport it does not.

## Accounting, and the 28-versus-29 question

The earlier revision reported "25 of 25" for 30 executions, because results were keyed on
`case#rep` and the phase was ignored, so the sample and matrix runs overwrote each other. Then a
reviewer found a second discrepancy: 28 planned against 29 terminals.

Both are now closed by enumeration rather than by arithmetic. The plan is written out as a
`plan.enumerated` record listing every entry, the native control is counted in it, and
`planned_total` is compared against `executed + reused + unknown_case` with the result recorded as
`accounting_balances`. This run: **29 planned = 28 cowfs + 1 native, 29 accounted, balanced**.
The native control is a planned execution, not an extra; the earlier asymmetry was that it wrote
a terminal record without a matching `case.begin`, which is fixed.

## Resume, and the cached-verdict laundering that was fixed

A cached verdict is reused only when the identity matches exactly, the manifest is complete, the
evidence file exists, and that evidence agrees with the manifest on every relevant fact. The
review demonstrated a real failing verdict being edited into a cached pass; that no longer works,
because the outcome is re-derived from the ledger rather than read from the manifest:

- every case writes `case.verdict` (outcome, failures, per-assertion values, receipt state)
  **before** its terminal record;
- every durable receipt gets a `readback.durable` record carrying the observed `present` and
  `got`, and every rename a `receipt.repath` event;
- `validate_cached` compares receipts on `kind`, `path`, `sha256`, `size` and `boundary`, and
  accepts a renamed path only if the ledger recorded the rename from a path that was genuinely
  issued;
- the manifest's assertions must match `case.verdict` exactly, values included;
- the outcome is recomputed from `readback.durable` and from each assertion's own fields
  (`want`/`got`, `present`, `n_problems`, `rc`, the recorded parent listing), **ignoring every
  `ok` flag**. A flag that contradicts its own fields rejects outright, tracked separately from the
  derived failures so deduplication cannot hide it.

Verified rejections, each with a recorded reason:

| tampering | reason |
|---|---|
| the reviewer's exact laundering (outcome, failures, manifest flags and ledger flags all flipped together) | `manifest assertions differ from the ledger verdict` |
| manifest outcome `fail` to `pass` | `manifest outcome 'pass' but the ledger derives 'fail'` |
| ledger assertion flag flipped only | `assertion flags contradict their own fields: ['durable_present']` |
| `readback.durable` rewritten to matched | `manifest outcome 'pass' but the ledger derives 'fail'` |
| receipt `kind` weakened `durable` to `applied`, both files | `receipt 'live/moved.bin' field 'kind' differs` |
| manifest receipt path altered | `receipt 'live/moved.bin' field 'path' differs` |
| one-line `{"terminal": true, "key": ...}` | cannot suppress anything; an unnamed line is not a terminal record |
| manifest `rev` `deadbeef`, or a zeroed binary or harness digest | `identity mismatch` |
| `receipt.issued` lines stripped | `receipt 'live/a.bin' absent from evidence` |
| `case.verdict` removed | `no case.verdict in evidence` |

**This is artifact consistency, not authentication.** There is no signature and none is
claimed: anyone who can rewrite `manifest.json` and `records.jsonl` coherently can still forge a
verdict. What is guaranteed is that the laundering the review performed, and its relatives, are
rejected, and that the outcome is derived from semantic facts rather than from a self-reported
flag.

**A cached failure stays a failure.**

| exit | meaning |
|---|---|
| 0 | at least one case executed here, all executed cases passed |
| 1 | at least one case executed and one failed, **or** zero executed and a cached verdict failed |
| 2 | zero cases executed and every cached verdict passed: `verdict: cached_only`, `fresh_acceptance: false` |
| 3 | harness error |

A zero-execution run whose cached verdicts include a failure is `cached_only_with_failures` and
exits 1, never 0, including under `--accept-cached`. A cached-only run that passed exits 2 by
default, and `--accept-cached` turns that into 0 while both the human summary and `summary.json`
still say `fresh_acceptance: false`.

**Nothing is wiped.** A re-execution lands in a new attempt directory
(`cases/<case>/<key>-aN`), so it never collides with an earlier store, which matters because a
case that creates snapshot `live` would otherwise fail on its own residue, and every earlier
attempt's evidence stays on disk.

## A defect found by running rather than reading

A full matrix run exceeded 40 minutes and had to be killed, and the harness could not be
restarted because even `ls` on the wedged case directory hung. Two causes, both in the teardown
that runs immediately after a `SIGKILL`, which is exactly when the mount is stale:

- `is_our_mount` called `os.path.realpath` on the mount path on every poll. Resolving a path that
  is a dead NFS mount can block indefinitely. The resolved form is now computed once, before the
  mount exists and the call is safe, and cached; `is_our_mount` never resolves anything itself.
- `umount` ran under `subprocess.run(timeout=...)`, which is not an enforceable bound: it signals
  the child and then blocks in `wait()`, and a command in uninterruptible sleep never dies. A
  bounded runner now polls, kills the child's process group on expiry and returns without
  reaping, recording `abandoned: true`.

Verified: the wedged case's stale mount unmounted by hand in 1.6 s once `realpath` was avoided,
and the full matrix then completed 29 of 29 with no orphan daemon and no leftover mount.

### What actually contains a survivor, and what is not claimed

The group signal is exactly as narrow as the contract allows and no wider. It is the child's own
process group, created with `start_new_session`, which cannot be this harness's group, the
reviewing shell's group, the shared daemon's group, or an unrelated sibling. That is the whole of
its reach.

It does **not** contain a process that starts its own session. A grandchild launched that way
survives the group signal and can keep writing after `run_bounded` has returned, which was
measured rather than assumed, in `bench/out/crash88-portability-repair/quarantine.py`. An
`umount` in uninterruptible sleep is the same shape: still running, still holding its descriptors,
against a mount path, after the harness has moved on.

So the containment is **not** the kill. It is the path allocation: `next_case_dir` always hands
out a fresh attempt directory, and nothing in the harness ever reuses or removes an attempt path,
a store, or a mount. A survivor can only ever hold a path nobody will use again. The quarantine
control records this distinction explicitly, and records `claims_all_children_killed: false` and
`claims_cleanup_complete: false`.

When a command is abandoned, `run_bounded` now carries the identity on the exception and into the
ledger: pid, pgid, the argv this harness owned, and the outcome of a non-blocking reap. The
`umount` record says `abandoned: true`, names `abandoned_process`, and its `cleanup_claim` field
states that the record does not claim the mount was removed. An operator can find the holder from
the evidence alone. Reaping uses `waitpid(WNOHANG)` and never waits on a live child, so the
bounded return cannot be undone by a reap.

Three things remain unproven, in both directions, and are not claimed:

- whether an uninterruptible `umount` keeps its cwd pinned to the mount path. Proving it needs a
  real long-lived `D` state on a shared host, which this work will not create.
- that every descendant is killed. One that leaves the group is not, by construction.
- that cleanup completed after an abandonment. The record says it did not claim that.

A related hazard, stated rather than fixed: the harness starts each daemon with
`start_new_session`, so a harness killed by a tool timeout leaves those daemons orphaned, exactly
as `environment-traps` describes in the other direction. Each is recorded in its case ledger as
`daemon.spawned` with pid, start time and command line, so an orphan is attributable to a run.
During this work 25 orphaned daemons were found on the machine; all 25 belonged to another
worker's tree, none were mine, and none were touched.

## CI

The CI step runs `python3 -m unittest discover -s bench`, which discovers **154 tests** at this
revision. That total is not one file:

| | |
|---|---|
| `python3 -m unittest discover -s bench` | 154 |
| `bench/test_daemon_crash.py` | 118 |
| `bench/test_gates.py` | 36 |
| skips | 1, `no /proc on this platform`, at `bench/test_daemon_crash.py:1362` |

An earlier revision of this document attributed 154 to `bench/test_daemon_crash.py`. That file has
118; the other 36 are `test_gates.py`, which this work did not add and does not own. The distinction
matters because 154 is the number to re-run, while 118 is the number attributable to this harness.

Locally on this Mac the run is `OK (skipped=1)`, exit 0. The skip is the only platform-dependent case
and it is a genuine skip, not a silent pass. No workflow change. Synthetic ledgers, fake fixtures and
short-lived private processes only: no daemon, no mount, no cargo, nothing under `~/.cowfs`.

The counter reader is injectable, so the available path is tested deterministically with an
explicit reading list and runs identically on Linux and macOS. The `nfsstat`-missing path is a
separate class asserting `available: false`, `commit_delta: None` and an explicit error. Nothing
is skipped or conditionally dropped, and a reader that raises is treated as unavailable rather
than as a zero.

The mount-table read is portable for the same reason. `/sbin/mount` is no longer hardcoded: the
binary is found with `shutil.which("mount")` and falls back to the known locations, `umount` is
discovered the same way, and each is executed directly from its discovered path rather than
through a shell. Inspection is tri-state, `mounted` / `not_mounted` / `unknown`, and a missing
binary, an unreadable table, a non-zero exit, a timeout, empty output or output with no
parenthesised mount entry all yield `unknown`. `unknown` is not `not_mounted`: it blocks cleanup
and records `unmount.blocked_unknown_mount_state`, because a table that could not be read is
never permission to remove, reuse or unmount anything. The whole matrix is a unit test with
fixture output, including a foreign path that must never match.

One test in the previous revision called the real mount runner instead of mocking it, which is
what kept the ubuntu job red: `/sbin/mount` does not exist on that runner and the resulting
`FileNotFoundError` escaped. It now mocks the runner, as its siblings do, and a missing binary is
handled by the runtime rather than by the test.

### The two defects the next review found, and the policy they forced

A line carrying `on` but no parenthesised type was previously skipped, so a table holding one such
line beside valid ones still reported a confident `not_mounted`. That is the dangerous direction: a
truncated copy of our own entry is then indistinguishable from a foreign one, and a false absence is
what lets cleanup proceed. The policy now is:

- an exact match on a fully parsed line is `mounted`. That is a positive identification, and a
  neighbour that failed to parse cannot retract it.
- no match, plus any unparseable entry, is `unknown`, which blocks cleanup and runs no `umount`.
- a fully readable table with no match is still `not_mounted`.

Empty output, a non-zero exit, a reader failure, a timeout and a missing binary all remain `unknown`.
No mount walk and no `realpath` fallback was added; the only resolution is the key cached before the
mount existed.

Separately, mount(8) escapes were never decoded, so on a host that escapes them a path containing a
space read as absent. `decode_mount_field` handles exactly the four documented sequences, `\040`
`\011` `\012` `\134`, in one left-to-right pass with no rescan, so `\134040` is a literal backslash
followed by `040` and never a space. It is deliberately not `unicode_escape`, which would interpret
backslash sequences a path may legitimately contain; an unrecognised sequence stays literal. The
decoded point is compared for exact equality against the cached key, so a prefix, a child and a
parent of the real path are each still a genuine absence, and a host that prints a literal space
still matches. `parse_mount_entry` covers the two forms in scope, with or without a util-linux
`type <fs>` suffix, and splits at the first `on` because the device is a single token in every form
this harness produces; a device containing `on` would lengthen the point rather than shorten it
into a false match.

Both were reproduced before the fix. The teeth fixture records 6 of 12 cases holding on the old tree
and 12 of 12 on the fixed tree, at `bench/out/crash88-portability-repair/teeth-OLD.txt` and
`teeth-NEW.txt`.

Coverage: the nfsstat parser against recorded output including the NLM column; ledger
immutability; identity sensitivity per bound field; every cache-rejection path above; the
laundering mutations; the `attributable` truth table; assertion expectation directions; the
exit contract; probe label pairing; attempt-directory behaviour; the refusal path, which asserts
the offered foreign `sleep` pid is still alive 300 ms after the refusal; and the stale-mount
guards, including a test that a stuck child cannot block the bounded runner.

## Windows sampled, and windows not sampled

Sampled, with the kill 0 to 6 ms after the last receipt unless noted: after a completed `fsync` of
written data; after an un-fsynced write; after a queued `rename` with and without a real
`COMMIT`; after an `mmap` write with `msync` and `fsync`; after a fork committed by a later
`COMMIT`; after a `snapshot rm`; after a completed `gc` cycle (about 0.5 to 0.9 s); on an idle
daemon.

Not sampled, each needing a seam this lane is not allowed to add:

- **power loss.** `SIGKILL` kills a process, not the kernel, so bytes already `write(2)`ed into a
  pack sit in the host page cache and survive any process death. That is also why every
  un-fsynced write survived here. Power-loss loss of un-fsynced pack bytes cannot be exercised
  this way at all.
- **mid-`gc` crash.** The kill lands after the `gc` ack; no public boundary exposes a point
  inside a collect cycle. The library seams (`cowfs_gc::Gc::set_between_lookup_and_walk`,
  `cowfs_core::fsops::set_fault`) exist for this and were deliberately not driven.
- **between the pack fsync and the watermark advance**, and between the watermark advance and the
  metadata commit. Both are internal orderings with no public edge.
- **concurrent writers**, and any writer that is not this harness.
- **`shutdown` as the receipt.** Exercised at teardown, not as a crash boundary.

## Superseded evidence, kept for provenance

Three earlier runs are excluded from acceptance and left on disk rather than deleted.

The first, behind the second revision of this report, produced 856 records, 0 failing, and 48 of
48 durable receipts matched. It is not acceptance evidence: 4 of its 52 durable receipts had been
reclassified after the fact by a `Receipts.downgrade()` call with no measurement behind it, and
the demotion was never written to the ledger, so the figure was really "48 of 48 among the
receipts not quietly reclassified".

The second, at `d071ce2`, is the run the repair review reproduced and is the basis for the 29
executions, 23 passed and 6 failed figures above.

The third, `bench/out/crash88/crash88-final-repair`, is the run that wedged on the stale mount.
Its first 10 cases completed in 27 s and its evidence is preserved, including the incomplete
directory, so the hang is inspectable.

The gc case in all of them reclaims nothing: 12,582,912 bytes of garbage were made dead, the mark
phase found 155 and 163 candidate blocks in this run, and `freed_bytes` was 0, because a small
fixture lives in the open pack, which the collector cannot unlink. Real reclamation with sealed
packs is `docs/verification/gc-daemon-e2e.md`.

## Isolation

Every signal went to a pid this harness spawned, after checking the command line carries this
run's own `--store` and `--socket` and that the pid's start time still matches the one captured at
spawn. Unmounting only happens for a path the mount table lists exactly, with the resolved form
cached before the mount exists. Verified after the run: the shared daemon on pid 15263 has the
same pid and the same start time (`Sat Oct 3 20:44:29 2026`) and is the only `cowfs-daemon` this
lane owns; the only cowfs mount is the shared `~/.cowfs/mnt`; no `cowfs-crash88-*` socket
directory remains. No shared store was collected or scanned, no lease returned, no runner
touched, no workflow dispatched or rerun.

Budgets are declared before the run and raise rather than grow: 64 KiB per operation, 16
operations per case, 180 s per case, 90 s without progress, and a hard bounded runner for every
mount and unmount. Only the gc case writes more than 64 KiB, and only enough to pass the
collector's own 8 MiB floor.

## What is still needed

Pre-#96 text, superseded: issue #90 is closed by PR #96, and `rename_posix_durability` and `rename_posix_durability_ro` pass on main `04bbdd0` with no wrapper.

Still open, as listed under "Not covered": power loss, mid-GC crash and the internal fsync orderings.
An independent review of the post-#96 result has not been done.
