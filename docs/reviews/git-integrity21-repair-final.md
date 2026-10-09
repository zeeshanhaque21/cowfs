# Independent final review: PR #104 repair, head `016769e`

Reviewer: independent critic, ready-wave slot 15, lease `68c9beb4122cda0dad992ca97575045`.
Reviewed head: `016769e7f4076a5c0fc712a65932c546048052f7`, verified exact.
Reviewed delta: `6df8b9fcfd6f9d54a6af0c8e424f3851fc6ecc71..016769e7f4076a5c0fc712a65932c546048052f7`, five commits.
Supersedes nothing: my prior report `docs/reviews/git-integrity21-final.md`, sha256 `30752fb48f9499382e46685c944c36b0159ef54b56613b93693490bf56c988a8`, is preserved unchanged, with its negative-control labs.

Method note: my worktree HEAD stayed at `6df8b9f` throughout.
I did not check out, reset, stash or modify the branch.
The new head was reviewed through `git archive` into `bench/out/ready-21-final-critic/src-016769e/`, and both reviewed files hash-match their git blobs at `016769e`:

| File | sha256 | matches blob |
| --- | --- | --- |
| `scripts/verify-git-index-integrity.py` | `3f8897fe95a69af375520c925d7a2e846c778ae9c0479d9d5eaf7578d397b422` | yes |
| `scripts/test_verify_git_index_integrity.py` | `138560bbc08edb034db9c2fc47f0bfda71d36b4f493599eed2c18887cd239f2b` | yes |

## Verdict

**F2, F3, F4, F5: PASS, each verified by execution and not by reading a claim.**
**F1: fixed and independently re-confirmed empty.**
**Exact-head CI: green on all three jobs.**
**Issue #21: OPEN. No merge performed.**

The five-commit delta touches four files and nothing else:

```
docs/verification/evidence/git-integrity21-repair.md   (new)
docs/verification/ready-21.md
scripts/test_verify_git_index_integrity.py             (new)
scripts/verify-git-index-integrity.py
```

`git diff --stat 6df8b9f..016769e -- crates/ .github/` is empty.
No production code, no workflow edit, nothing belonging to any other lane.

## The parser lineage, resolved

This is the part that needed a real answer rather than a restatement, so here is the evidence.

The repair evidence says the older harness had a parser bug, reporting `before: total 17 unparsed 17` against this host's real mount table, and attributes it to `split_mount_line` taking the fstype from the source head plus an off-by-one that rejected a one-character mountpoint.

**That function does not exist in the original source.**

```
git show 6df8b9f:scripts/verify-git-index-integrity.py | grep -c split_mount_line   ->  0
```

`split_mount_line`, `MountTable` and `unescape_mount_path` were all introduced by the repair.
The original `mount_line_for` was a plain substring test, `if f" on {mp} " in line`.

So the catastrophic parser, the one that refused every line and would have made the harness unable to reach a verdict at all, **did not exist in the 42/42 harness**. The five commits are where the parser was born, and it was born already correct.

I checked the fstype extraction at every committed repair revision:

| Revision | `split_mount_line` refs | fstype source | bracket test |
| --- | --- | --- | --- |
| `6df8b9f` | 0 | none, substring era | none |
| `1a71efe` | 5 | options list, `line[open_paren+2:close]` | `open_paren < on + 5` |
| `ff57b70` | 5 | options list | `open_paren < on + 5` |
| `8053f03` | 5 | options list | `open_paren < on + 5` |
| `26701f5` | 5 | options list | `open_paren < on + 5` |
| `016769e` | 5 | options list | `open_paren < on + 5` |

Identical at all five. The broken variant exists in **no committed revision**, so the `unparsed 17` figure describes an uncommitted working state that published history cannot reproduce.

Now the part that actually decides whether the historical 42/42 proof survives. I ran both matchers against this host's live mount table, read-only:

| Property | original substring matcher | new parsed matcher |
| --- | --- | --- |
| real mount lines matched back to their own mountpoint | 14 of 14 | 14 of 14 |
| disagreements between the two | 0 | 0 |
| `map auto_home on /System/Volumes/Data/home` (space in source) | HIT | HIT, source `map auto_home` recovered |
| live cowfs NFS mount found by exact path | HIT | HIT |
| new parser unparsed lines | n/a | 0 of 14 |

**The original matcher was not broken for matching.** It found every real mount line, including the awkward `autofs` line whose source contains a space and the live NFS export.
What the original could not do, and what I filed as F3 in my prior report, was distinguish a genuine absence from a reader failure: it returned `None` for both, and downstream code read `None` as "not mounted".
That is a fail-open semantics defect, not a parse defect.

Consequence for the historical proof: the 42/42 run used the substring matcher, which I have now shown works on this host's real mount table, and `daemon.start()` only returned `ok` when that matcher found a line.
The historical mounted result is therefore **not invalidated** by the parser episode.

Two honest caveats.
First, the builder's framing that the older harness "had parser bug" is imprecise and, read literally, wrong; the precise statement is that the older harness had a fail-open absence defect, and the catastrophic parser existed only in an uncommitted intermediate state.
Second, I could not reconcile the `17`/`17` figure itself, because the state it measures is not in history, and I did not read another lane's raw fixture directory to chase it.
The historical 42/42 numbers are carried as the author's measurement, not re-derived by me.
My own fresh-head run is the proof I stand behind, and it is reported below.

## F2 PASS: target directory and real window bounds

The documented commands now work as written, and the bounds are enforced rather than described.

| Check | Result |
| --- | --- |
| build command exports `CARGO_TARGET_DIR` to the path the script reads | yes, in the doc and in the script's own prerequisite message |
| `--ops 29` | rc **3**, `outside 30..44` |
| `--ops 20` | rc **3**, `outside 30..44` |
| `--ops 45` | rc **3**, `outside 30..44` |
| `--ops 60` | rc **3**, `outside 30..44` |
| `--ops 30` | accepted, proceeds to the prerequisite check |
| `--ops 44` | accepted, proceeds to the prerequisite check |
| `build_ops()` length | 44, and `ops_bounds` is recorded as `[30, 44]` against a `documented` band of `[30, 60]` |

All four rejections print the real ceiling and name `build_ops()` as the authority.
The bounds check runs **before** the binary check, which is why I could observe rc 3 with no binaries present at all.
That ordering is correct: a bad request should not be reported as a missing prerequisite.
The silent-slice hazard my prior report described is gone, and the exit code is a distinct 3 rather than the generic 2.

## F3 PASS: tri-state mount table and quarantine

`MountTable` is tri-state over `mount`(8), and the state machine is the right shape:

- `PRESENT` requires an exact parsed match, and wins even when neighbouring lines are unreadable, because the target itself parsed.
- `ABSENT` is returned only when the reader exited 0, the output was non-empty, and every line parsed.
- `UNKNOWN` covers reader failure, timeout, empty output, and any unparseable line.

A reader is injectable, so the tri-state is unit-testable against synthetic failures without touching a real mount or a real NFS server, which is what the test file does.

Teardown keeps process state and mount state separate, and my own run exercised the branch that used to be the bug:

```
seq 83  stopped=true  why="argv guard cleared"                  mount_state=absent  clean=true
seq 91  stopped=true  why="argv guard cleared"                  mount_state=absent  clean=true
seq 94  stopped=true  why="pid already gone before this stop"   mount_state=absent  clean=true
```

The third record is the important one. In the original, that branch returned `stopped: True` straight from a `ProcessLookupError` without ever consulting the mount table.
Now the mount state is still read through the tri-state reader and recorded as `absent`, independently of the process having vanished.
`clean` is computed from both, so a dead pid with a live mount would report `clean: false` and `quarantine` rather than a tidy stop.
The unit tests cover all three rows of that table, plus the recycled-pid and never-started cases.

Escape handling is single-pass by construction: the decoder substitutes from a four-entry table in one `re.sub`, so `\040040` decodes to a space followed by the literal text `040` and never double-decodes.
The exact-match change also closes a real prefix hole: `/tmp/mnt-a` no longer matches a mountpoint of `/tmp/mnt-ab`, which the substring reader would have accepted.

## F4 PASS: mount attestation with device identity and negative controls

The attestation fails closed and records evidence for every condition, and my run satisfies all of them:

| Condition | My run |
| --- | --- |
| exact parsed mount line, not a substring hit | `localhost:/cowfs-9be24e19a620cb62ca0cea8db954abb7 on .../mnt-a (nfs, ...)` |
| that line's fstype is `nfs` | `fstypes: ["nfs"]` |
| mount arm `st_dev` differs from native arm | native `16777234`, mount `436209618` |
| mount arm `st_dev` differs from the local scratch root | scratch `16777234`, equal to native, different from mount |
| serving process alive, owned, `--backend core` | pid 28541, `daemon_owned: true`, `backend_declared: core` |
| fresh write reads back its own bytes | attestation `pass: true`, `why: []` |
| table read cleanly | `reader_rc: 0`, `line_count: 15`, `unparsed_lines: 0` |

The scratch-root comparison is the fix that matters, and the evidence doc explains why the first attempt was wrong in a way I agree with: comparing the mount arm against the mountpoint compares the export against itself, because the mountpoint is legitimately on the NFS device.
That first attempt failed closed and rejected a correct run, which is the right failure direction.
Comparing against the mountpoint's parent, the local scratch directory the harness itself created, is the actual silent-fallback shape.

Both negative controls held, so the detector is not vacuous:

| Control | `attested_pass` | control `pass` | reasons |
| --- | --- | --- | --- |
| two local dirs, no mount line, no daemon | false | true | 7 |
| mount arm is a local dir while native arm is a real repo | false | true | 7 |

The first control's reasons include `mount arm st_dev 16777234 equals the local scratch root`, `no live owned private daemon is serving the mountpoint`, and `serving daemon argv does not carry --backend core`, which is precisely the set of ways this gate could have been a no-op.

One thing worth stating plainly for anyone reusing this receipt: the NFS device number is per-mount and therefore not a portable identity. Mine is `436209618` where the builder recorded `436209587`.
The invariant is that mount differs from **both** native and scratch, not that it equals a particular number.

## F5 PASS: combined idx gate, with nested exit codes read correctly

The doc no longer claims `show-index` validates the magic and the trailing SHA-1, and the measured table it now carries matches my own independent measurement on git 2.56.0 and Apple's 2.54.0 exactly.

The per-operation `idx.check` is now `idx_integrity`, requiring `show-index` **and** `verify-pack` **and** a present sibling `.pack`.
My run's log shows the nested structure and the per-kind exit codes read from where they actually live:

```json
{"idx": "pack-db32fdf73557c688725ee578620169f88c05b08e.idx", "pack_present": true, "pack_bytes": 862989,
 "show_index": {"rc": 0, "stdout_lines": 18}, "verify_pack": {"rc": 0, "secs": 0.022}, "why_fail": null}
```

Both arms report `idx.show-index` pass, `idx.verify-pack` pass and `idx_integrity` pass, count 1 each.
An empty pack directory is a fail rather than a vacuous pass.

The four bugs the earlier revisions fixed are all confirmed live in my own run rather than only in unit tests:

- **Per-shape op records.** 60 op records, 30 native and 30 mount. Op 2, the fixture-write shape that used to raise `KeyError`, is recorded on both arms as `{"bytes": 16, "sha256": "ba33767185...", "wrote": "README.md"}` with rc 0.
- **Nested exit codes.** Read from the per-kind sub-object, not the outer record.
- **Window construction.** `requested_ops 30`, `declared_ops 30`, `available_ops 44`, and `window_planned_eq_executed true` means planned and executed are compared as separate recorded quantities, so a shortened window cannot satisfy its own check.
- **Bootstrapping.** Construction and execution were checked at 30, 42 and 44 without running the full 42-op window unnecessarily.

## My own bounded run on a real private Core NFS mount

I ran the harness myself at this head, on a real private Core NFS export, with a matched APFS control.
Binaries were reused from the predecessor's release artifacts rather than rebuilt, copied read-only into my own artifact folder and pinned by hash, which is why no cold 405 MB build was needed this time:

| Binary | sha256 | matches documented manifest |
| --- | --- | --- |
| `cowfs` | `4da060426e96b555e80fd36b48fcdc1270fd3345cf29c731795ea48dcc148667` | yes |
| `cowfs-daemon` | `6273f72478025d49911898d20a67f8bacdc375f2544b34fe6887d2e8e11abfa1` | yes |

Provenance recorded by the run itself: git `2.56.0`, python `3.12.2`, `macOS-26.6.2-arm64-arm-64bit`.
Attempt `bench/out/ready-21-final-critic/run/20261004T192045-attempt`, 96 log records.
One bounded foreground run, exit **0**.

```
verdict NOT REPRODUCED: no git index or pack-index corruption in the declared window on either arm
requested_ops 30   declared_ops 30   available_ops 44
window_planned_eq_executed true          window_no_failed_op true
mount_attested true                       mount_attestation_negative_controls true
idx_integrity_native true                 idx_integrity_mount true
checks_native true                        checks_mount true
pack_compare true                         history_compare true
worktree_compare true                     reopen true
shared_daemon_untouched true
```

All 14 verdict parts true. Window 30 of 30 executed and 0 skipped on **both** arms, with no failed ops.

| Item | Native | Mount |
| --- | --- | --- |
| idx sha256, both arms | `c0cef4476687747f2a8a5b16b710b54f624017b27a6ee3ffc9fa2523e703c165` | identical |
| commits | 5 | 5, lists equal, HEAD equal |
| HEAD | `bcc3023c66690bae8c64d1f03dca826ace526457` | identical |
| tracked files compared | 7 paths, all equal | no one-sided paths |
| device | `16777234` | `436209618` |

The idx hash reproduces the builder's documented value exactly, and the reopen re-read it again through a fresh daemon on the same store.

Reopen detail: daemon A stopped under the argv guard, daemon B on the **same store** with a new mount and socket, argv carrying `--backend core`.
`cowfs fsck` pass, `git fsck --full` rc 0, 7 tracked files re-read with 0 bad, and `idx_show-index`, `idx_verify-pack` and `idx_idx_integrity` all pass with count 1.
Cookie probe at 64 entries: pass, 0 remaining on both arms, no divergence.
Shared daemon 15263 identical before and after on pid, lstart and argv.

Signal discipline in my own run: exactly 2 SIGTERMs, both to daemons this run spawned, each with full spawn argv, store, socket, mount and `--backend core` recorded, each authorized by `argv guard cleared`.
No group signal, no `pkill`, no global cleanup, and no `umount` anywhere in the harness.

## CI at the exact head, read once

| Item | Value |
| --- | --- |
| head checked | `016769e7f4076a5c0fc712a65932c546048052f7` |
| `check (ubuntu-latest)` | success |
| `check (macos-latest)` | success |
| `linux-fuse` | success |
| mergeable | `MERGEABLE` |
| mergeStateStatus | `CLEAN`, was `UNSTABLE` at `6df8b9f` |
| closingIssuesReferences | `[]` |
| issue 21 | `OPEN` |

The previously failing model test was verified by reading the actual run log, not inferred from the diff.
From the ubuntu job log of run `37254072529`:

```
test model::tests::oracle_catches_broken_backends ... ok
```

Toolchain and host as recorded in that log: `rustc 1.99.0 (b940084d7 2026-09-28)`, `host: x86_64-unknown-linux-gnu`.
Failure markers across the three job logs: zero in the ubuntu job, zero in the macos job, and the 4 matches in `linux-fuse` are `git config` lines and a Node.js deprecation warning, with the check-run concluding `success`.

So the earlier failure is genuinely resolved at this head, and nothing needed waiving.
I also confirmed the causal boundary rather than assuming it: this PR changes neither `crates/` nor `.github/`, so the model repair came from elsewhere in the tree, consistent with the #95 and ready-#42 lanes owning that code.
I did not patch `cowfs-vfs-test`, the Core model, or any workflow, and I did not dispatch, rerun or poll anything.

## Remaining gaps, none of them a merge blocker

**CI still does not run this harness.** `ci.yml` line 25 is `python3 -m unittest discover -s bench -v`, and that discovery root is `bench/`.
`scripts/` is never imported, executed or linted by any job.
Green CI therefore still says nothing about the 42-op or 30-op window, and the harness's own suite is not exercised in CI either.
Wiring it up needs a macOS runner with a private Core NFS mount, so it is not a one-line change.
I did not edit the workflow, as instructed.

**Minor hygiene: the socket directory is not removed at teardown.** `sock_dir` is cleared in `start()` only, at line 793-794, and never in teardown.
My run left `/private/tmp/cowfs-r21-a-28428` and `-b-28428` behind.
I verified both are inert: mode `0700`, containing only a 0-byte `d.sock.lock`, no entry from `lsof`, and both daemon pids 28541 and 32221 confirmed gone.
I left them in place deliberately, because `start()` already handles reuse by `rmtree`-ing the directory, and deleting under a machine running 32 leases carries more risk than the debris does.
Other `cowfs-r21-*` directories in `/private/tmp` belong to other lanes and I did not touch them.

**No overclaiming in the repair.** The evidence doc names what it deliberately did not do, and it does not claim the 805 MiB soak, the 16-cell grid, performance, crash or durability results.
The zero-run scan remains outside `verdict_parts`, so F6 stays correctly ungated.
My own receipt makes no such claim either: one 30-op run with 64 cookie entries is a bounded observation, not a soak.

## Bottom line

Every finding from my prior report is genuinely fixed, and for the first time the oracles come with proof that they can fail: the parser is tri-state and unit-tested against synthetic reader failures, the attestation has two negative controls that both refuse, the window bounds reject instead of slicing, and the idx gate is combined rather than resting on `show-index` alone.
I verified that by execution at the exact head, including my own end-to-end run on a real private Core NFS mount that reproduced the builder's idx hash and device invariant independently.

The one analytical correction worth carrying forward: the catastrophic mount parser was **not** in the 42/42 harness and never existed in a committed revision.
It was introduced and fixed inside the repair itself.
The original harness had a fail-open absence defect, which is a weaker and different thing, and the original substring matcher does in fact find every real mount line on this host, which is why the historical mounted proof stands.

**Issue #21 stays OPEN.** Its bullet 2 is unreproduced and unfixed; bullet 3 belongs to slot 1.
The current Core backend did not reproduce bullet 2 in a bounded window, and the harness that would catch a recurrence now exists, is tested, and fails closed.
Neither this review nor merging #104 should close it.

No commit, push, merge or lease return was performed.
My branch HEAD is still `6df8b9fcfd6f9d54a6af0c8e424f3851fc6ecc71` on `review/git-integrity-21`, and my prior report and labs are byte-identical to before.
The shared daemon 15263 was never signalled, mounted to, or restarted.