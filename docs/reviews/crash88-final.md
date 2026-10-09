# Independent review of PR 91 (issue 88), critic 12

Reviewed tree `94998b2c5cb9eb878f08cf38fdd706d03f027538`, branch `review/full-stack-crash-88`, base `ceb96c67033cbf97f79267d6af7db3fa204d77d1`.
The PR adds exactly two files: `scripts/verify-daemon-crash.py` (1579 lines) and `docs/verification/daemon-crash-acceptance.md` (272 lines).
No source, test or CI file changed.
I changed no tracked file. My artifacts are `bench/out/crash88-critic/**` and the harness runs `bench/out/crash88/critic12-*/**`, all gitignored.

## Verdicts, kept separate

| scope | verdict |
|---|---|
| Harness and proof scope: is there a real, bounded, runnable, fail-closed crash harness against the real daemon, CLI and mount, with honest scoping of what it did not sample | **PASS with defects** |
| Success criterion 3 (zero data loss in crash-injection tests) for the rename + `fsync` boundary, and gate 6 (g6) | **BLOCK** |

The first verdict is about the instrument.
The second is about the product promise the instrument declined to test.
They must not be collapsed into one "crash gate PASS".

## What I ran, and from what

Binaries built by me from the reviewed tree: `cargo build -p cowfs-cli -p cowfs-daemon`.
`cowfs-daemon` sha256 `f797bb276dcdd57526b2d46dd189abb19606a59013dc1dacc6eed00565eb3b89`, `cowfs` sha256 `de4e4ac3e01ce57295670d0b726d109f480a427dbef58e6a52a831a4e61cc356`, macOS 26.6.2 build 25G83 (Darwin 25.6.0), APFS, NFSv3 loopback on 127.0.0.1.
The harness's own `source.identity` record agrees on `rev=94998b2c5cb9eb878f08cf38fdd706d03f027538`.
The reviewed report cites different digests (`4f29fab1...`, `9567bded...`) for a different tree, see "Provenance" below.

Order of work: small sample first, then targeted cases, then the full matrix, then the independent reproduction.

| run | command | result |
|---|---|---|
| `critic12-sample` | `--stage sample --reps 1` | 3/3, 94 records, 0 failing, 5 durable receipts all matched, fsck clean x3 |
| `critic12-exitcheck` | same, exit status read without a pipe | exit 0 |
| `critic12-rename` | `--only rename,rename_filefsync,rename_writefsync,write_fsync --reps 2` | 8/8, 218 records, 0 failing |
| `critic12-all` | `--stage all --reps 1` | "13/13 passed", 15 case executions |
| `critic12-full` | `--stage all --reps 2` | "25/25 passed", 856 records, 30 case executions, 80 s wall clock |
| `critic12-fabricated` | `--stage matrix --reps 1` with one hand-written record | "12/12 passed", `gc_crash` never ran |

Independent reproduction, `bench/out/crash88-critic/repro.py` and `unit_checks.py`: variants x3, native x3, wire counters, an 8 point no-sync delay sweep, and 22 fail-closed controls.

Isolation held throughout: the shared daemon is still pid 15263 started `Sat Oct  3 20:44:29 2026`, it is the only `cowfs-daemon` process, the only NFS mount is the shared `~/.cowfs/mnt`, and no `cowfs-crash88-*` or `cowfs-critic12-*` socket directory leaked.

## Harness and proof scope: PASS with defects

What holds up.

- The harness drives the real daemon, the real control CLI and a real NFSv3 loopback mount over a private store, SIGKILLs a fixture-owned pid, then reopens the same store with a fresh daemon and a fresh mount.
- `kill_verified` cannot signal a foreign pid: it requires the daemon basename plus this run's own `--store` and `--socket` in the command line, which the shared daemon's command line does not contain. I confirmed the refusal path raises without signalling, and I added the probe the harness lacks: the offered `sleep` pid is still alive 300 ms after the refusal.
- `is_our_mount` matches the exact `" on <realpath> "` in the mount table, so an unmount cannot reach another agent's mount.
- The daemon under test is started with `start_new_session=True`, so a tool timeout cannot SIGTERM the group into the daemon. `prepare_platform` sweeps only mounts under this run's own mount prefix whose server no longer answers, so it cannot touch the shared mount.
- Evidence is appended, flushed and `fsync`ed per record. A torn last line is tolerated: I injected one and the prior records still loaded with the step counter continuing.
- The readback, fsck and snapshot-name checks genuinely fail closed. I fed them wrong hashes, wrong sizes, missing files, a reported fsck problem and a missing durably promised snapshot name. All five were rejected; only an `applied` item's absence is tolerated, as documented.
- `Receipts.repath` matches by path, so a rename re-files the intended receipt and not the last one.
- Budgets are declared before the run and raise rather than grow.
- The report is honest about `SIGKILL` not being power loss, about the gc case establishing no reclamation, and about which crash windows were never sampled. Those three limitations are correctly labelled as limitations, not as passes.

What is defective.

1. **Result accounting undercounts executions.** `critic12-full` printed "25/25 passed" while 30 case executions ran (30 `case.terminal` records). `go()` keys results on `"%s#%d" % (case_name, rep)` and ignores the phase, so the matrix result for `write_fsync#0`, `write_nofsync#0` and `kill_control#0` overwrites the sample result for the same keys. The report repeats the error: "25 of 25 executions passed: 3 sample cases and 12 matrix configurations at 2 reps each, plus 2 native controls" is 29 or 30 executions depending on how you count, never 25. Fix: count `case.terminal` records and label skipped executions as skipped.
2. **Resume is fail-open.** A skipped case is recorded as `results[key] = True`. Re-running `critic12-all` executed nothing (zero `daemon.ready`) and still printed "13/13 passed" with exit 0. A single hand-written line in `records.jsonl` (`{"terminal": true, "key": "gc_crash#0"}`) suppressed the entire gc case while the run reported "12/12 passed". A resume key carries no source revision and no binary digest: I confirmed a record carrying `rev=deadbeef` and an all-zero digest suppresses the same case in this tree. A resume therefore reuses cached verdicts across different binaries, which is exactly the false pass a crash gate must not produce. Fix: bind the key to `rev` plus both binary digests, count skips separately from passes, and re-execute anything whose identity does not match.
3. **CI does not run any of this.** `.github/workflows/ci.yml` runs `python3 -m unittest discover -s bench -v`, and the only discoverable module is `bench/test_gates.py`. The harness lives in `scripts/`, so the new code has zero CI coverage, and it has no unit tests at all. Issue 88 requires a committed runnable harness and independent review, not a CI wiring change, so this is not an acceptance failure, but a 1579 line crash gate with no automated test is a standing risk.
4. **The kill count cited as isolation evidence is misleading.** The report cites "`kill.verified_target`, 60 occurrences" as proof that every signal was verified. Sixty is 30 intentional kills plus 30 teardown kills; it does not correspond to 60 crash boundaries. The claim is true but the number does not mean what it appears to.
5. **The receipt demotion is never written to the evidence ledger.** `Receipts.downgrade` rewrites the receipt in memory only. I searched the run ledger for any `not_durable` boundary or demotion reason across `critic12-full` and `critic12-rename`: none. The report's sentence "the reason is stored in the receipt rather than hidden" is false with respect to the evidence file the report cites; `records.jsonl` shows `receipt.durable` for `live/orig.bin` and a separate `case.rename` with `level=applied`, and nothing that connects them or records that a receipt was reclassified.

## g6 and the rename + `fsync` boundary: BLOCK

### Measured, on the reviewed tree, with wire evidence

The builder's central claim is that on this client a directory `fsync` after a rename, and an `fsync` of a read-only descriptor, never put a `COMMIT` on the wire.
The harness never observes the wire: it contains no `nfsstat`, `tcpdump`, `dtrace`, `fs_usage` or `NFSPROC` reference (I checked the source), so the claim is inferred from a rename coming back with the old name.
I measured it directly instead, using the kernel NFSv3 client RPC counters that need no root.

Positive and negative controls in one daemon, one mount, no kill (`bench/out/crash88-critic/wire1`):

| step | RPC counter delta |
|---|---|
| idle, 0.5 s, do-nothing baseline | `Access +100, Getattr +33, Fsstat +2, RdirPlus +9, Setattr +2` from other mounts, no `Commit` |
| `write` then `os.fsync(fd)` | **`Commit +2`, `Write +2`, `Create +2`** |
| `os.rename` | `Rename +2` |
| `os.fsync(dirfd)` on the parent | **no `Commit`**, `Getattr +1` |
| `os.fsync(fd)` on a read-only fd of the renamed file | **no `Commit`**, `Getattr +1` |
| `write` then `os.fsync(fd)` again | **`Commit +2`, `Write +2`** |

So the mechanism is confirmed as measurement, not inference: the macOS NFSv3 client emits `COMMIT` only when it has dirty pages for that inode, so the two POSIX-habit syncs cross no wire at all.
Issue 90's reading of the mechanism is correct.

### Measured, crash side, same tree

`repro.py`, three reps per variant, private store per variant, pid, command line, store, socket and start time verified immediately before each `SIGKILL`, kill gap recorded:

| after `os.rename` | rename after fresh reopen | kill gap | fsck |
|---|---|---|---|
| `os.fsync(dirfd)` on the parent | **lost 3/3**, reverted to `orig.bin` | 197 / 217 / 245 ms | 0 problems |
| `os.fsync(fd)` on a read-only fd | **lost 3/3**, reverted to `orig.bin` | 159 / 134 / 183 ms | 0 problems |
| `write` + `fsync` of a sibling file | **survived 3/3**, `moved.bin` present with matching sha256 | 201 / 174 / 184 ms | 0 problems |
| native APFS, writer SIGKILLed after `os.rename` + `os.fsync(dirfd)` | **survived 3/3** | n/a | n/a |
| native APFS, writer SIGKILLed after `os.rename` + read-only fd fsync | **survived 3/3** | n/a | n/a |

Bytes were intact under whichever name survived, in every rep.
The daemon is not broken in the sense issue 90 claims: a `COMMIT` that does arrive commits the queued rename, and the sibling control proves it.
That part of issue 90 is right.

### The loss window, which the report does not measure

The harness kills 0 to 6 ms after the receipt.
At that instant no namespace operation could have committed for any reason, so the two demoted cases do not, on their own, distinguish "the `fsync` was useless" from "the kill was too early".
The sibling case is the discriminator and it does pass, which is to the builder's credit.
But the size of the window matters for the POSIX verdict, so I swept it with no `fsync` at all (`sweep1`, `sweep2`):

| kill gap after the rename, no fsync at all | rename after reopen |
|---|---|
| 144 ms | lost |
| 340 ms | lost |
| 720 ms | lost |
| 920 ms | lost |
| 1128 ms | survived |
| 1277 ms | survived |
| 1325 ms | survived |
| 2128 ms | survived |

The flip sits between 920 ms and 1128 ms, so the rename survives a kill only by accident of the daemon's background flusher, roughly a second after the caller was told the `fsync` succeeded.
That is later than the 500 ms `flush_interval` alone would predict (`Inner::tick` at `crates/cowfs-core/src/inner.rs:1024` commits a pending queue once `q.age()` reaches `flush_interval`, and the tick period is `flush_interval / 2`, so a queued rename should commit inside roughly 750 ms).
I did not instrument the daemon, so I am reporting the measured bracket and flagging that the extra delay is unattributed rather than guessing which queue or commit stage adds it.
The only attribution I can support from source is that a namespace commit goes `flush_snapshot` then `commit_batch`, which sets `unsynced`, and the metadata write becomes durable on a later `meta.sync()` (`Inner::sync_all`, `Inner::fsync_snapshot`, `crates/cowfs-core/src/inner.rs`).

### Why the demotion does not honestly label a client quirk

`case_rename` and `case_rename_filefsync` write the file with `do_fsync=True`, which calls `receipts.durable(...)` and records `receipt.durable`, and then unconditionally call `receipts.downgrade(...)`.
I confirmed by source inspection that the downgrade has no measurement gate: the case body contains no wire observation of any kind, so these two cases cannot fail no matter what the transport does.
Their results are recorded as `readback.applied_lost_permitted` with `ok=True` when the rename is lost and as `readback.applied_survived` with `ok=True` when it survives.
That is a case that cannot fail, and it is being used as gate evidence.

Four reasons the level-A relabelling masks a real failure rather than describing an unsupported client behaviour honestly.

1. `docs/design.md:51` lists "Full POSIX: `mmap`, atomic `rename`, ..." as settled scope. POSIX says a successful `fsync` on the parent directory after `rename` makes the new name durable. Here that call returns 0 and the rename is still lost up to about a second later. Under the full-POSIX criterion this boundary fails, whatever the client did or did not send.
2. The level-A justification is `docs/design.md`'s "bounded loss of recent writes on a crash". The lost object here is not a recent write and not data still in daemon memory; it is an acknowledged namespace operation whose durability barrier the caller was told had succeeded. The harness itself recorded the receipt as level B a few lines earlier.
3. No supported way to obtain the durability exists. `crates/cowfs-ctl/src/types.rs:432` has no `sync` request, confirmed again here, so the control plane offers no barrier; and the only way to make a rename durable through the shipped macOS transport is to dirty an unrelated file, which is not a contract any caller can rely on. When the client cannot produce the barrier, the product needs a server-side mutation, a supported flush seam, or a documented capability limit. A doc note does not create the barrier, so while the surface is presented as full POSIX the acceptance for this boundary is blocked, not satisfied.
4. The evidence does not record the relabelling, so no reviewer reading `records.jsonl` can see that 4 of the 52 durable receipts in `critic12-full` were demoted after the fact, and cannot detect a demotion that was wrong. My run: 52 `receipt.durable`, 48 `readback.durable_match`, 4 demoted, all of them the two rename variants at 2 reps.

The honest form of this finding is "a successful POSIX `fsync` after `rename` provides no durability on the macOS NFS transport, and the harness cannot currently certify or refute that boundary because the case that exercises it is defined so that it passes either way".

## Issue 90 scope assessment

Issue 90's suggested scope is documentation only, and its "not in scope" section says "Confirmed by measurement that nothing here is broken in the daemon".
That is a builder's opinion about a defect it found, not a standing contract, and on the evidence above it is too narrow in two ways.

- "Nothing is broken in the daemon" is supported only in the narrow sense that a `COMMIT` that arrives commits the rename. The user-visible promise is still broken: `fsync` returns success and the rename is lost, on the only macOS transport the project ships.
- A documentation-only change cannot make the criterion pass. Either the transport gets a durable barrier for namespace-only operations, or the project states a capability limit and the full-POSIX claim in `docs/design.md` is narrowed. Both are larger than a doc edit, and the first one is a source change.

The measurement table and the mechanism in issue 90 are sound and worth keeping.
The scope classification is the part that needs to change.

## Minimal responsible fix, for a follow-up issue

Not implemented here, and not my paths to change.

1. Add a durability barrier that a caller can actually reach, cheapest first: a `sync` request in `cowfs_ctl::Request`, or server-side write-through of namespace mutations (`rename`, `create`, `rm`, `mkdir`) so they commit and `meta.sync()` before the ack. A macOS `mount_nfs` option that forces `COMMIT` is worth measuring first, because if one exists the whole fix is a mount flag.
2. Add a harness case that asserts the POSIX outcome, `rename_posix_durability`, and let it fail until 1 lands. If the team decides the limitation is acceptable for v1, the case must be recorded as `expected_unsupported` with the acceptance explicitly marked blocked, and the "no daemon broken" claim dropped.
3. Record any reclassification as an immutable ledger event, `receipt.demoted`, carrying the reason and the observation that justified it, or stop reclassifying at all.
4. Measure `COMMIT` directly with the `nfsstat` client counters in the rename cases instead of inferring it from a lost rename. It needs no root, and it makes the finding reproducible from the ledger alone.
5. Add `rename` plus parent-directory `fsync` to the native APFS control. The current native baseline writes two files and never renames, so it cannot show the one place cowfs diverges from native.
6. Fix the accounting and the resume: count executions, report skips as skips, and key the resume on `rev` plus both binary digests.
7. Move or mirror the fail-closed checks into a module `python3 -m unittest discover -s bench` can see, so CI runs them.

## Provenance of the cited evidence

The report's "What ran against" table says the run was against tree `ceb96c67033cbf97f79267d6af7db3fa204d77d1` on branch `verify/full-stack-crash-88`, base `ceb96c6`.
That cannot be right: the harness does not exist at `ceb96c6`.
`git show ceb96c6:scripts/verify-daemon-crash.py` fails, and `git log ceb96c6..94998b2` shows the file added in `f95a764` and modified in `94998b2`.
The only trees that can produce this evidence are `f95a764` or `94998b2`, and the report was refreshed in `94998b2` specifically to match a re-run.
The tree cell should read `94998b2c5cb9eb878f08cf38fdd706d03f027538`.

The cited evidence path, `bench/out/crash88/accept-88-final/records.jsonl`, is gitignored and absent from the tree, so no reviewer can re-read the numbers the report is built on.
I reproduced them instead, and they hold: my `critic12-full` run produced 856 records with 0 failing, 48 durable receipts matched, fsck clean on 30 reopened stores, 26 applied surviving and 6 lost, which matches the report's figures including the 48/26/6 split and the 30 reopened stores.
The report's tree row is the only provenance error.

## What I did not verify

- Power-loss behaviour. `SIGKILL` cannot exercise it and neither can my runs.
- Any crash window the harness lists as unsampled: mid-`gc`, between the pack `fsync` and the watermark advance, between the watermark advance and the metadata commit, concurrent writers, `shutdown` as the receipt.
- Reclamation. The gc case frees 0 bytes by design; my run shows `candidate_blocks` 158 and 156 with `freed_blocks` 0, matching the report's shape and its stated reason.
- The builder's cited evidence file `bench/out/crash88/accept-88-final/records.jsonl` does not exist in the tree, since `/bench/out/` is gitignored and no artifact directory was present. I reproduced the numbers instead, and my run matches them closely: 856 records, 0 failing, 48 durable receipts matched, 26 applied survived, 6 applied lost, fsck clean on 30 reopened stores.
- Whether the missing 6 applied losses are exactly the 4 rename receipts plus the 2 gc garbage sets, as the report says. In `critic12-full` the applied losses number 6 and the demoted rename receipts number 4, with the gc garbage sets accounting for the remainder by construction, but I did not audit each path individually.
- Whether `nfsstat` counters are per-mount or host-wide. They are host-wide, which is why I recorded an idle baseline with nonzero `Access` and `Getattr` deltas from other mounts, and why the deltas I rely on are `Commit`, which was zero outside the write-plus-fsync steps.
- The 2 MiB per-op and concurrency budgets beyond what the code declares. I did not drive the daemon to those limits.

## Verdict summary

The harness is a real instrument and the safety discipline around signalling is sound, which is why the first verdict is a pass.
The rename-plus-`fsync` boundary is a measured product-visible failure of the full-POSIX promise, the case that exercises it is written so it cannot fail, the demotion is invisible in the evidence, and no caller-reachable barrier exists.
g6 and success criterion 3 are blocked for that boundary.
PR 91 should not be merged as a completed crash gate.
It is worth merging as the instrument plus an honest finding, once the report stops calling a returning-but-useless `fsync` a documentation defect and the rename cases stop passing unconditionally.