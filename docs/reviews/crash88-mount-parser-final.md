# PR 91 round 5: mount parser and the integrated binary

Reviewer: critic, native lease 12, exclusive. Nothing in this round touched source, tests, config or the lease.
Target head `7d9380aa345d682f52de45afb1013386af71785d` against the accepted head `2db2f7f0c5c3e39765f340cf700258cf19269257`.

The four earlier reports are untouched and byte-identical after the fast-forward: `crash88-final.md`, `crash88-repair-final.md`, `crash88-evidence-final.md`, `crash88-portability-final.md`.

## Verdicts

| axis | verdict |
|---|---|
| SOURCE, mount parser and teardown seam | **PASS with 2 report defects** |
| PROOF, semantics of the receipt/cache/verdict core | **PASS**, carried on byte identity, 48 of 52 bodies |
| g6 and success criterion 3 | **BLOCKED**, unchanged |
| merge | permitted only after the two text corrections below; the harness code itself raises no block |

The two defects are in `docs/verification/daemon-crash-acceptance.md`, not in the harness. The parser itself is correct and fails safe on every input I could construct.

## Carry: what is unchanged and what moved

Criterion used is byte identity of extracted function and class bodies, sha256 over each body, computed from `git show` of both revisions. Not an AST-normalised hash, and not a commit message.

| | |
|---|---|
| bodies byte-identical `2db2f7f` -> `7d9380a` | 48 |
| bodies changed | 4: `_mount_keys`, `parse_mount_entry`, `decode_mount_field`, `mount_state` |
| constants unchanged | `SCHEMA_VERSION = 3`, `MAX_ATTEMPTS = 64`, `MAX_OP_BYTES = 64 * 1024`, `MOUNT_CMD_CANDIDATES` |
| file digest | `d328d411b70e6304` -> `1c400c5edd34208e` |
| diff hunks | 6, confined to line 79 (`import os`) and lines 917-1023 |

Every body I carried is on the accepted proof path: `validate_cached`, `derive_verdict`, `_expected_from_assert_record`, `case_identity`, `attributable_for`, `kill_verified`, `run_case`, `main`, `reap_nowait`, `run_bounded`, `process_identity`, `pid_cmdline`, `pid_start_time`, `child_exited`, `verify_readback`, `verify_snapshot_names`, `verify_fsck`, `verify_no_torn_tree`, `write_file`, `fsync_dir`, `deterministic_body`, `read_records`, `sha256_file`, `digest_of`, `umount_binary`, `mount_command`, `is_our_mount`, `unmount_private`, all 14 case functions, and classes `Receipts`, `Receipt`, `CaseResult`, `PrivateDaemon`, `Recorder`, `Proc`, `Probe`, `Budget`.

Private PID, PGID and quarantine metadata are therefore carried, not re-derived: `kill_verified`, `process_identity`, `pid_cmdline`, `pid_start_time`, `child_exited` and `run_bounded` are byte-identical, so no new process-group or signal path entered this revision. No process group is used anywhere in the diff.

`unmount_private` is byte-identical too, so the UNKNOWN stop was reached through the existing tri-state return, not through a new teardown branch.

## P1: an unreadable entry no longer becomes a confident absence

Reproduced against the accepted implementation, executed rather than recalled. The old file was materialised from `git show 2db2f7f:scripts/verify-daemon-crash.py` into the lab directory and imported next to the working-tree one.

| input table | old `2db2f7f` | new `7d9380a` |
|---|---|---|
| our own entry truncated mid-line | `not_mounted`, "2 mount entries, none matching" | `unknown`, "1 entries read, 1 unparseable, none matching" |
| only a foreign entry truncated | `not_mounted` | `unknown` |
| exact match for us, plus a truncated neighbour | not exercised | `mounted` |
| our own entry written without an option group | not exercised | `unknown` |
| fully readable table, no match | `not_mounted` | `not_mounted` |
| empty output, blank table, non-zero exit, missing binary, reader timeout | not exercised | `unknown` in all five |

Teeth on the blocking path: with a truncated table, `unmount_private` issued exactly one command, the `mount` read, recorded `unmount.blocked_unknown_mount_state`, and never reached `umount`. The gate is a state, not a retry.

The direction is right. A positive identification of our exact path is not retracted by a neighbour that failed to parse, and a false absence, which is what lets cleanup proceed, is no longer reachable from a partially readable table.

## P2: escapes decode in one pass, exactly four sequences

| case | result |
|---|---|
| `\040` in a Linux point, key `/mnt/my mnt` | old `not_mounted`, new `mounted` |
| `\011`, `\012`, `\134` | each decodes to tab, newline, backslash |
| `\134040` | `/mnt/a\040b`, a backslash then `040`, never a space |
| `\999` | left literal |
| truncated `\04` | left literal, not consumed |
| unescaped space, macOS form | still matches |
| util-linux `type <fs>` suffix | stripped, so `/proc`, `/sys`, `/` match |
| macOS form with `mounted by` | matches |

No injection route: the decoded point is compared for exact equality against keys cached before the mount existed, so a decoded space or a decoded `..` inside a point cannot produce a prefix, child or parent match. Controls confirm a parent `/a`, a child `/a/mnt/x` and an infix `/mnt` are each a genuine absence, and a foreign mount is still refused with no `umount`.

The split is at the first `on`. That is the safe direction: a device containing `on` lengthens the parsed point rather than shortening it onto one of our keys. Constructed so the true point is foreign and ours appears only after a second `on`, the parsed point is `/theirs on /ours/mnt`, which is not equal to `/ours/mnt`, so the answer is `not_mounted`. Splitting at the last `on` would have returned `mounted` for a path we do not own.

Limitation, stated not fixed: on a macOS host, a target path that itself ends in ` type <word>` would have that tail stripped. No path this harness creates can, so it is a robustness note, not a defect.

## Controls run: 44 of 44

`bench/out/crash88-mount-parser-critic/parser_lab.py`, ignored lab directory, output at `parser_lab.out`, exit 0. Every case runs both implementations.

Three of my own controls were wrong before the code was, and I corrected the controls rather than the reading:

- I first wrote the "exact match survives an unreadable neighbour" fixture with an entry that had no option group, so the line was itself unparseable and the control failed for the wrong reason. Real entries carry a parenthesised group; rewritten, and the unparseable-neighbour case now passes, and a separate control pins that our own entry without a type is `unknown`, not `mounted`.
- I asserted the first-`on` split would lengthen a point in a case where it did not, because the util-linux `type` strip then landed on the right answer anyway. Rebuilt so the foreign point is the trap.
- Two reader-failure controls compared a returned tuple against a string and failed while the behaviour was correct. Fixed the assertion.

Also confirmed: no `os.walk`, `os.scandir`, `os.listdir`, `os.path.realpath` or `pathlib` line was added anywhere in the file, and neither implementation shells out for the table. Nothing walks a filesystem to decide whether a mount exists, which is the failure mode that would risk touching a dead path or traversing into the shared store.

Against this host's real table, all 15 lines parse, the shared mount at `/Users/zeeshanhaque/.cowfs/mnt` is identified as `mounted` without being touched, and an unrelated path reads `not_mounted`.

## Independent integrated run, at this head

Fresh, private store, private TMPDIR, real daemon, real CLI, real NFS loopback, SIGKILL, reopen. Evidence at `bench/out/crash88/m12-sample/`, log at `bench/out/crash88-mount-parser-critic/sample.log`.

| | |
|---|---|
| planned / accounted | 2 / 2, balanced |
| executed / reused | 2 / 0 |
| passed / failed | 1 / 1 |
| verdict / exit | `executed_with_failures` / 1 |
| rev recorded | `7d9380aa345d`, equals HEAD |
| `summary.harness_sha256` equals sha256 of the harness file | yes, `1c400c5edd34208e` |
| `summary.daemon_sha256` equals sha256 of the built daemon | yes |

`write_fsync` passed: four durable receipts at the `nfs_commit` boundary, four of four readback digests matching, `fsck_clean` with 0 problems, snapshot name present.

`rename_posix_durability` failed on `durable_present`, and this is the finding, not a harness defect:

- `readback.durable` `present=false`, `got=null`
- `assert.durable_present` `ok=false`, `parent_entries=["orig.bin"]`, the pre-rename name
- `wire.report fsync_parent_dir commit_delta=0` against an idle baseline drift of 24
- receipts `durable@nfs_commit` for `live/orig.bin` and `live/moved.bin`, seq 1, not downgraded
- `assert.fsck_clean` `n_problems=0`, `assert.snapshot_is_dir` passed
- teardown recorded `unmount.refused_not_our_mount` with `state=not_mounted`, then `unmount.after_kill` and `unmount.teardown` both clean, `abandoned=false`

That is the first real-run evidence for the tri-state inspection the previous round introduced. Both real unmounts read `not_mounted` after the fact, and the genuine absence still records the refusal. No `blocked_unknown` appeared, because the table was readable here.

The 29-execution matrix was not repeated. It remains the `90c9a8f` record and my round-3 29-run, both on the pre-merge binary, and this round's numbers must not be read as extending them.

## Defect 1, P1: the report's new central argument rests on an inference that does not hold

The report now says the integrated binary's digests differ from the `90c9a8f` run's, and concludes the six losses and the 46-of-52 match are properties of the pre-merge binary.

Digest inequality does not establish a source change. A dev-profile build of one fixed source tree is not byte-reproducible. Five builds of that tree, same toolchain rustc 1.99.0, no source edit between them, produced five daemon digests: `dce375b7`, `87428a84`, `c8411f6f`, `2144dc6b`, `ff34a900`. The binary embeds no absolute build path, so this is not a path artefact.

Consequences, both in the same document:

1. The conclusion happens to be true by a different route. `crates/cowfs-core/src/ns.rs` changed between `2db2f7f` and main, blob `8408832` to `5a1024c1`, and `crates/nfsserve/src/tcp.rs` changed too. That is the evidence for "the binary's inputs changed". The digest table is not, and it cannot be reproduced by a reader who builds the same tree.
2. The Provenance table presents the tuple as "the run's identity" and says the harness refuses to reuse a cached verdict unless all of it matches. Three of those five fields are reproducible: `rev` and `harness_sha256`, which is a source-file digest, and `schema`. `daemon_sha256` and `cli_sha256` are build-artifact digests that change on every rebuild of identical source, so they never attest to source, only to one build. The effect on the cache is fail-safe, over-rejection: my run recorded `reused: 0` and a second run of the same tree would also re-execute.

The one provenance claim that does hold, and I checked it because the report leans on it: the harness digest is genuinely reproducible across revisions. sha256 of the harness file is `047cf9750bcfa98d` at both `90c9a8f` and `1a2b4f5`, which is what the report says, and `d328d411` at `2db2f7f`, `1c400c5e` at `7d9380a`.

Correction is one paragraph: replace the digest table as proof with the `ns.rs` and `tcp.rs` blob change, and say in the Provenance table which fields identify source and which identify one build.

I could not reproduce the report's claimed integrated digests `eb48f336` and `e0cc963b` from this tree, and my five builds of that tree produced five digests, all different from both. The brief expected those digests. I am recording the discrepancy rather than adopting the numbers.

## Defect 2, P2: the test count is attributed to the wrong file

The report says `bench/test_daemon_crash.py`, now 154 tests. That file has 118. The 154 is the `python3 -m unittest discover -s bench` total, which also picks up `bench/test_gates.py` at 36. 118 plus 36 is 154, so the number under the CI command is right and the file attribution is wrong. The previous text said 87 for that file, which was also wrong; 106 at `2db2f7f`.

The count is the only thing I would rely on when re-running, and this is a provenance document. One word to fix.

## Test and lint, run under the CI command

| | |
|---|---|
| `python3 -m unittest discover -s bench` | `Ran 154 tests`, OK, `skipped=1`, exit 0 |
| `bench/test_daemon_crash.py` alone | 118 |
| `bench/test_gates.py` alone | 36 |
| skip reason | one, `no /proc on this platform`, at `bench/test_daemon_crash.py:1362` |
| `ruff check --select F,E9` on harness and tests | all checks passed |
| new test methods in `265fc3b` | 12, all parser and tri-state |

The 12 new methods cover the tri-state, exact-match precedence, the util-linux suffix, all four escapes, no-rescan of `\134040`, an unescaped host, exact-target comparison and an incomplete table running no `umount`. My 44 controls are independent of them; several were written before I read the method names and three failed on my own fixtures first.

## CI, read not polled

Run `37244249112` on branch `verify/full-stack-crash-88`, the head sha `7d9380a`. Three reads, no polling loop, no dispatch, no rerun: run plus jobs at about 10 minutes, the ubuntu job steps at about 11 minutes, then one confirming read of run plus the macOS job at about 16 minutes.

Final state: run `completed`, conclusion `success`. Jobs 3 of 3 `completed/success`: `linux-fuse`, `check (macos-latest)`, `check (ubuntu-latest)`. On ubuntu and on macOS the `Bench harness unit tests` step, step 8 of 11, is `completed/success`, alongside fmt, clippy with `-D warnings` and `cargo test --workspace`.

The earlier green run `9772ab6` is not this result and is not counted.

## PR and issue state, read once

PR 91: head `7d9380aa345d682f52de45afb1013386af71785d`, base `46b0f269d5bef4a2c204c25f5b3015da601d3beb`, `mergeable_state=clean`, `state=open`, `draft=false`. `main` `46b0f26` is an ancestor of the head, so the branch now merges forward, not sideways. The base advanced from `ceb96c6`.

GraphQL `closingIssuesReferences.nodes` is `[]`. The body opens "Related to #88" and disclaims g6 in its own words, which matches the state.

Issues: 88 open, 90 open, 93 closed, 95 closed, 96 open. The fix for #90 is issue #96 and it is not in this branch.

## g6 and success criterion 3: still BLOCKED

Unchanged from round 1 and not weakened by anything in this head.

The `rename` plus `fsync(parent dir)` and `rename` plus `fsync(read-only fd)` paths lose the name on every rep. The counter is NFSv3 Commit, read from `nfsstat`: zero across the rename window while the idle baseline drifted, on a passing `write` plus `fsync` sibling and against native APFS rename, which survives. fsck is clean, so this is not corruption, it is a false durability promise.

This round re-measured the boundary on the integrated binary and the promise is still broken there. #93 and #95 do not fix it and do not fix #90 either, which the report now says correctly. The receipt model is untouched and nothing was reclassified to make the run read better.

Still unproven and deliberately not attempted: real uninterruptible `umount` cwd pinning, power loss, a crash mid-`gc`, reclamation, and the merge-queue run against `main`. Also unexercised this round: a live Linux mount table. The Linux grammar is verified against recorded lines only.

No performance claim, no power-loss claim, no mid-`gc` claim, no reclamation claim is made anywhere in this review.

## Isolation

| | |
|---|---|
| shared pid 15263 | alive, same command, about 20h elapsed |
| daemons running under lease 12 | 0 |
| leaked `/private/tmp/cowfs-crash88-*` | 0 |
| signals sent to anything not mine | none |
| mounts created or removed | only inside the private store this harness made, and all cleaned |
| reports 1 to 4 | byte-identical after the fast-forward |
| new files this round | this document, plus `bench/out/crash88-mount-parser-critic/**`, both ignored or untracked |
| commits, pushes, merges, lease returns | none |

Every private daemon ran `start_new_session` with its own pgid. Before each signal and each unmount the harness verified pid, command line, start time, store path and socket path for the process it was about to act on; those checks are byte-identical to the accepted head, so no new targeting logic entered this revision.

The mount table was read three times, never written, and no mount was resolved through a dead path. `umount` was never invoked on this host by anything I ran.