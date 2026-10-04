# Verification: full-stack daemon crash recovery with durable receipts

Issue: [#88](https://github.com/zeeshanhaque21/cowfs/issues/88).
Harness: `scripts/verify-daemon-crash.py`, tests `bench/test_daemon_crash.py`.
Finding: [#90](https://github.com/zeeshanhaque21/cowfs/issues/90).
Reviews: `docs/reviews/crash88-final.md`, `docs/reviews/crash88-repair-final.md`.

## Verdict

| scope | verdict |
|---|---|
| the instrument: a bounded, runnable, fail-closed crash harness against the real daemon, CLI and mount | **holds** |
| success criterion 3 (zero data loss in crash-injection tests) and gate g6, for the `rename` + `fsync` boundary | **BLOCKED** |

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

The run is identified by a tuple. The harness refuses to reuse a cached verdict unless all of it
matches, so these fields are the run's identity rather than decoration.

| | |
|---|---|
| git rev | `90c9a8f8eed9ac5d21fa098d97ee020e23c6a059` (this branch's head) |
| harness digest | `047cf9750bcfa98d7659ee0d4ecd90f2e2b3e325cb5a95b38473f50beb772840` |
| `cowfs-daemon` | `4f29fab15f09ac2754c8ee0d4b7b9b0c515b620fbf10b89c6c010c99844085d8` |
| `cowfs` | `9567bded815291568a523c6d7a5772e9fd997d797da19cd9d628bee56ca0c5b5` |
| evidence | `bench/out/crash88/accept-final/` (gitignored) |
| transport | real in-process NFSv3 loopback, `cowfs-daemon --backend core` |

The harness does not exist at the base commit `ceb96c6`, and no row here claims it does. The
revision of this report that cited `ceb96c6` was wrong.

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

## Results, 29 executions

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

## Finding: a successful POSIX `fsync` after `rename` buys nothing on this transport

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

A related hazard, stated rather than fixed: the harness starts each daemon with
`start_new_session`, so a harness killed by a tool timeout leaves those daemons orphaned, exactly
as `environment-traps` describes in the other direction. Each is recorded in its case ledger as
`daemon.spawned` with pid, start time and command line, so an orphan is attributable to a run.
During this work 25 orphaned daemons were found on the machine; all 25 belonged to another
worker's tree and none were touched.

## CI

`bench/test_daemon_crash.py`, 87 tests, discovered by the step CI already runs,
`python3 -m unittest discover -s bench`. No workflow change. Synthetic ledgers, fake fixtures and
short-lived private processes only: no daemon, no mount, no cargo, nothing under `~/.cowfs`.

The counter reader is injectable, so the available path is tested deterministically with an
explicit reading list and runs identically on Linux and macOS. The `nfsstat`-missing path is a
separate class asserting `available: false`, `commit_delta: None` and an explicit error. Nothing
is skipped or conditionally dropped, and a reader that raises is treated as unavailable rather
than as a zero. This was the ubuntu job's only failure and it is gone.

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

The source fix is **not** in this branch and is not mine to land. Issue #90 carries the corrected
scope: a durability barrier a caller can reach, or a narrowed capability claim in
`docs/design.md`. Either way, `rename_posix_durability` and `rename_posix_durability_ro` must be
re-run against the fixed tree and pass on their own, with no wrapper. This harness is the
instrument for that and currently reports the boundary as failing.

Independent review of this revision is wanted.