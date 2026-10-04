# Verification: full-stack daemon crash recovery with durable receipts

Issue: [#88](https://github.com/zeeshanhaque21/cowfs/issues/88).
Harness: `scripts/verify-daemon-crash.py`, tests `bench/test_daemon_crash.py`.
Finding: [#90](https://github.com/zeeshanhaque21/cowfs/issues/90).

## Verdict

Two verdicts, deliberately not collapsed.

| scope | verdict |
|---|---|
| the instrument: a bounded, runnable, fail-closed crash harness against the real daemon, CLI and mount | **holds**, with the defects the review found now fixed |
| success criterion 3 (zero data loss in crash-injection tests) and gate g6, for the `rename` + `fsync` boundary | **BLOCKED** |

**This run exits non-zero on purpose.** 6 of 29 executions fail, every one of them the same
boundary: a caller's `fsync` returned success, and the name it was supposed to make durable was
gone after the crash. That is a real product failure, not a harness artefact, and it is not
papered over with an expected-failure wrapper.

```
$ scripts/verify-daemon-crash.py --stage all --reps 2
verdict           : executed_with_failures (exit 1)
fresh acceptance  : yes
executed / reused : 29 / 0
passed / failed   : 23 / 6
```

## Provenance

The run is identified by a tuple, not by a branch name, and the harness refuses to reuse a
cached verdict unless all of it matches.

| | |
|---|---|
| git rev | `d071ce251e57021cd63ba0866f91731e92a77a3e` |
| harness digest | `30270e72e47b571652a495266371eb26793b535417ca33866b068be911126378` |
| `cowfs-daemon` | `4f29fab15f09ac2754c8ee0d4b7b9b0c515b620fbf10b89c6c010c99844085d8` |
| `cowfs` | `9567bded815291568a523c6d7a5772e9fd997d797da19cd9d628bee56ca0c5b5` |
| evidence | `bench/out/crash88/crash88-repair/` (gitignored) |
| transport | real in-process NFSv3 loopback, `cowfs-daemon --backend core` |

The harness does not exist at the base commit `ceb96c6`, and no row here claims it does.
The earlier revision of this report cited `ceb96c6` and was wrong.

## The receipt model, and why nothing is reclassified

Read off the source, not assumed:

- an NFS `WRITE` ack returns from `cowfs_core::op_write` with the bytes only in the node's
  in-memory state (`crates/cowfs-core/src/io.rs:100`);
- `cowfs_meta::db::Ack` defaults to `Applied` and nothing in the tree ever sets
  `Ack::Durable`, so a `cowfs snapshot create` returning 0 is applied, not durable;
- `os.fsync(fd)` on a file with dirty pages is the real receipt: the adapter maps `COMMIT` to
  `fsync(ino, false)` (`crates/cowfs-nfs/src/lib.rs:40`), reaching `op_fsync` ->
  `flush_snapshot` (blocks into the pack, then the metadata commit) and `meta.sync()`, whose
  `before_sync` hook is `cowfs_core::store_sync_hook` -> `Store::sync()` -> fsync of the pack,
  then `watermark.advance`.

Three kinds, enforced differently after the reopen:

| kind | meaning | after SIGKILL |
|---|---|---|
| `durable` | the caller's sync returned success | must be present and byte-identical; a miss **fails the case** |
| `applied` | genuinely un-fsynced work | presence or absence both recorded; absence is permitted by `docs/design.md` |
| `removed` | this harness deleted it | must be absent |

`Receipts` has no method that changes a receipt's kind. `bench/test_daemon_crash.py` asserts
its absence, so a reclassification cannot be reintroduced quietly. Every promise is written to
the ledger as a `receipt.issued` record when it is made, and this run's evidence contains
**zero** demotion records because the operation does not exist.

## Results, 29 executions

| | |
|---|---|
| executions | 29 (24 matrix + 4 sample + 1 native), matching 29 `case.terminal` records exactly |
| receipts issued | 73: 53 `durable`, 18 `applied`, 2 `removed` |
| `durable` matched | 47 of 53 |
| `durable` **missing after a successful fsync** | **6** |
| `applied` | 18 survived, 0 lost |
| `removed` absent | 2 of 2 |
| durably promised snapshot names present | 10 of 10 |
| `fsck` | 29 of 29 clean, 0 problems |
| aborts / harness errors | 0 / 0 |
| wall clock | 65 s total, 4.3 s slowest case |

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

The failure record carries the sibling listing, so it is visible that the rename reverted to
the pre-rename name rather than the file vanishing:

```json
{"name": "assert.durable_present", "ok": false, "path": "live/moved.bin",
 "parent_entries": ["orig.bin"],
 "detail": "caller's sync returned success and the bytes are gone"}
```

`fsck` is clean in every failing rep and the old name is intact, so this is loss of an
uncommitted name, not corruption.

## Finding: a successful POSIX `fsync` after `rename` buys nothing on this transport

`docs/design.md` lists atomic `rename` under full POSIX, and POSIX says a successful `fsync`
on the parent directory makes the new name durable. Here that call returns 0 and the name is
still lost, while the same bytes under the old name survive and `fsck` is clean.

What this is **not**: a broken rename path. The control case forces a real `COMMIT` by dirtying
a sibling file in the same snapshot, and the rename then survives 3 of 3. So a `COMMIT` that
arrives commits the queued `Op::Rename` (`crates/cowfs-core/src/ns.rs:557`) correctly.

What makes it a product problem rather than a documentation nit: the lost object is an
acknowledged namespace operation whose barrier the caller was told had succeeded, and there is
no supported way to obtain that barrier. `cowfs_ctl::Request` has no `sync`
(`crates/cowfs-ctl/src/types.rs:432`), and the only thing that works through the shipped macOS
transport is dirtying an unrelated file, which is not a contract a caller can rely on. Under
the full-POSIX criterion this boundary fails.

### Wire measurement, and its limits

The harness reads the kernel NFSv3 client `Commit` counter with `nfsstat`, which needs no root,
so the mechanism is measured rather than inferred. The parser pins the NFSv3 section, because
`nfsstat` also prints an NLM section with its own `Commit` column; a unit test asserts the two
are not confused.

| step | `Commit` delta | idle drift over the same window | attributable |
|---|---|---|---|
| `fsync(parent dir)` | 0, 0, 0, 1 | 0 to 21 | only 2 of 4 |
| `fsync(read-only fd)` | 0, 0 | 0 to 21 | only 1 of 2 |
| sibling `write` + `fsync` | 2, 3 | 3 to 21 | 0 of 2 |

`nfsstat -c` counters are **host-wide**, not per mount, and this machine was busy, so the idle
baseline drifted by up to 24 `COMMIT` in half a second. Each report therefore carries an
`attributable` flag, set only when the step's delta stands clear of the drift. Six of the eight
reports are not attributable and the harness makes no claim about them. The two that are show a
clean zero: `drift 0`, `delta 0`, so no `COMMIT` was attributable to those steps at all.

The numbers point the same way as the crash-side measurement, and the crash-side measurement
is what the verdict rests on. The wire probe is corroboration with a stated noise floor, not
the proof.

### Native control, same operations

The APFS control now performs the identical sequence: a durable write, a rename, a
parent-directory fsync, a read-only descriptor fsync, then the writer `SIGKILL`s itself.

| control | result |
|---|---|
| writer killed (`rc -9`) after rename + both fsyncs | the renamed file matched its source hash |
| clean restart (`rc 0`) | matched |

This is where cowfs diverges from native, and it is the reason the divergence matters: on APFS a
returned `fsync` keeps the name; on the shipped cowfs macOS transport it does not.

## Accounting and the resume contract

**Accounting.** The earlier revision keyed results on `case#rep` and ignored the phase, so the
sample and matrix runs overwrote each other and 30 executions printed as 25. Identity is now a
digest over phase, case, rep, argv scope, case config, git rev, the harness's own digest and
both binary digests, and the totals come from `case.terminal` records. This run: 29 planned, 29
`case.terminal`, 29 reported.

**Resume fails closed.** A cached verdict is reused only when the identity matches exactly, the
manifest is complete, the evidence file exists, and that evidence actually contains the receipts
and assertions the manifest claims. Anything else re-executes. Verified against the attacks the
review demonstrated, and each rejection reason is in the ledger:

| tampering | outcome |
|---|---|
| one hand-written `{"terminal": true, "key": "gc_crash#0"}` line | ignored, cannot suppress anything |
| the same line with `"name": "case.terminal"` and a matching key but no real manifest | rejected, `terminal record identity mismatch` |
| manifest `rev` rewritten to `deadbeef` | rejected, `identity mismatch` |
| manifest `daemon_sha256` zeroed | rejected, `identity mismatch` |
| `receipt.issued` lines stripped from the evidence | rejected, `receipt 'live/c.bin' absent from evidence` |
| a manifest whose every field claims `pass` but whose evidence has no receipts | rejected |

**A zero-execution run cannot be read as a pass.**

| exit | meaning |
|---|---|
| 0 | at least one case executed here, all executed cases passed |
| 1 | at least one case executed, at least one failed |
| 2 | **zero cases executed**: `verdict: cached_only`, `fresh_acceptance: false` |
| 3 | harness error |

`--accept-cached` is the explicit opt-in that turns 2 into 0, and it still prints
`fresh acceptance: NO - zero cases executed` and still writes `fresh_acceptance: false` into
`summary.json`. Both the human summary and the JSON refuse the claim.

**Nothing is wiped.** A re-execution lands in a new attempt directory
(`cases/<case>/<key>-aN`), so it never collides with an earlier store, which matters because a
case that creates snapshot `live` would otherwise fail on its own residue, and every earlier
attempt's evidence stays on disk.

## CI

`bench/test_daemon_crash.py`, 50 tests, discovered by the step CI already runs,
`python3 -m unittest discover -s bench`. No workflow change. They use synthetic ledgers, fake
fixtures and short-lived private processes: no daemon, no mount, no cargo, nothing under
`~/.cowfs`. Coverage is the nfsstat parser against recorded output, the immutability of the
receipt ledger, identity sensitivity to each bound field, every cache-rejection path above, the
exit contract, the probe label pairing, attempt-dir behaviour, and the refusal path, which
asserts the offered foreign `sleep` pid is still alive 300 ms after the refusal.

## Windows sampled, and windows not sampled

Sampled, with the kill 0 to 6 ms after the last receipt unless noted: after a completed `fsync`
of written data; after an un-fsynced write; after a queued `rename` with and without a real
`COMMIT`; after an `mmap` write with `msync` and `fsync`; after a fork committed by a later
`COMMIT`; after a `snapshot rm`; after a completed `gc` cycle (0.5 to 0.9 s); on an idle daemon.

Not sampled, each needing a seam this lane is not allowed to add:

- **power loss.** `SIGKILL` kills a process, not the kernel, so bytes already `write(2)`ed into
  a pack sit in the host page cache and survive any process death. That is also why every
  un-fsynced write survived here. Power-loss loss of un-fsynced pack bytes cannot be exercised
  this way at all.
- **mid-`gc` crash.** The kill lands after the `gc` ack; no public boundary exposes a point
  inside a collect cycle. The library seams (`cowfs_gc::Gc::set_between_lookup_and_walk`,
  `cowfs_core::fsops::set_fault`) exist for this and were deliberately not driven.
- **between the pack fsync and the watermark advance**, and between the watermark advance and
  the metadata commit. Both are internal orderings with no public edge.
- **concurrent writers**, and any writer that is not this harness.
- **`shutdown` as the receipt.** Exercised at teardown, not as a crash boundary.

## Superseded evidence, kept for provenance

The run behind the previous revision of this report produced 856 records, 0 failing, and 48 of
48 durable receipts matched. **It is not acceptance evidence and is excluded.** Four of its 52
durable receipts were reclassified after the fact by a `Receipts.downgrade()` call that had no
measurement behind it, and the demotion was never written to the ledger, so the "48 of 48" was
"48 of 48 among the receipts that had not been quietly reclassified". The raw evidence is left
untouched on disk rather than deleted. This report supersedes it.

The gc case in that run also reclaimed nothing; it still does not. 12,582,912 bytes of garbage
were made dead, the mark phase found 158 and 163 candidate blocks, and `freed_bytes` was 0,
because a small fixture lives in the open pack, which the collector cannot unlink. Real
reclamation with sealed packs is `docs/verification/gc-daemon-e2e.md`.

## Isolation

Every signal went to a pid this harness spawned, after checking the command line carries this
run's own `--store` and `--socket` and that the pid's start time still matches the one captured
at spawn. Unmounting only happens for a path the mount table lists exactly. Verified after the
run: the shared daemon on pid 15263 has the same pid and the same start time
(`Sat Oct 3 20:44:29 2026`) and is the only `cowfs-daemon` running; the only cowfs mount is the
shared `~/.cowfs/mnt`; no `cowfs-crash88-*` socket directory remains.

Budgets are declared before the run and raise rather than grow: 64 KiB per operation, 16
operations per case, 180 s per case, 90 s without progress. Only the gc case writes more than
64 KiB, and only enough to pass the collector's own 8 MiB floor.

## What is still needed

The source fix is **not** in this branch and is not mine to land. Issue #90 now carries the
correct scope: a durability barrier a caller can reach, or a narrowed capability claim in
`docs/design.md`. Either way, `rename_posix_durability` and `rename_posix_durability_ro` must
be re-run against the fixed tree and pass on their own, with no wrapper. This harness is the
instrument for that, and it currently reports the boundary as failing.