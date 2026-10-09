# PR 102 final delivery review: `7b0c39a44a082736bcdc40fe6d60f7bb7a0730bf`

Critic for the ready-wave g4 fsx lane, final pass.
Target head `7b0c39a44a082736bcdc40fe6d60f7bb7a0730bf`, reviewed against my own prior head
`ff45b0a3c6ab1e6fcfd94bf0fdf733812a1fc38c`.
My previous report is `docs/reviews/mounted-fsx-g4-repair-final.md`,
sha256 `28ca5290f08d526ecfc020b0c8cf21e814f5d02d520478aa9dd2fb2409c1e03a`.

PR 102 is open, head is this SHA, `mergeable_state: clean`, no auto-merge, zero comments, zero reviews.
The delta is 7 commits, 9 files, 1754 insertions, 634 deletions.
No production crate changed and `.github/workflows/ci.yml` is untouched.
Issue 103 is open.

## Verdict

All nine residual findings from `ff45b0a` are closed. R1 through R9 each PASS, each with a
measurement rather than a reading.

Recommendation: merge the documentation and harness delta.
Two things the merge does not deliver, stated so nobody reads them into it: the full `fallocate`
family is still UNMEASURABLE because issue 103 is open, and macOS NFS is still unmeasured.

The exit-code migration is deliberate and correct: the runner moved to
`0 PASS, 1 FAIL, 2 UNMEASURABLE, 3 INVALID`, which is what `bench/compare.py` already used.
The historical run's recorded exit of 3 is left alone and labelled as the convention in force when
it ran. The raw log is not rewritten.

## Lease

Recovered, not re-acquired. `treehouse-state.json` for the ready-wave pool slot 14:
`lease_id 1246013cd5b485fe16d2479db0647c45`, `lease_holder cowfs-g4-critic`, leased since
`2026-10-04T17:13:24-07:00`, branch `review/mounted-fsx-g4`, worktree HEAD still `f816b5e`.
Branch unchanged, no checkout, no reset, no stash, no rebase.
I own only `bench/out/g4-residual-final-critic/**` in the lease and this document in the primary
checkout. The earlier `bench/out/fsx-g4-repair-critic/**`, `bench/out/ready-g4-critic/**` and
`bench/out/requirements19-critic/**` trees and all their artefacts were read only.

## F1 through F7, carried

Accepted by source-blob diff and the affected seam tests, as scoped for this pass. The enforcement
seams for all seven now live in classes that this delta owns and that I ran:
`ArmAttestation`, `DeviceDisambiguation`, `FirstDivergence`, `RestartLeg`, `ImmutableAttemptDir`,
`ToolPin`, `ByteCaps`, `PlannedBudget`, `UnreadableResult`, `ExitContract`, `DirectCommandLine`,
`Tabulate`.

111 tests, all passing, on both hosts, one module instance per host:

| host | command | result |
| --- | --- | --- |
| macOS, archived `7b0c39a` tree | `python3 -m unittest discover -s bench/fsx-gate -p 'test_*.py'` | 111 tests, OK, 1 skipped |
| macOS, same tree | `python3 -m unittest discover -s bench` | 147 tests, OK, 1 skipped |
| Linux `moonscape`, harness copied byte-identical | `python3 -m unittest test_run_fsx_gate` | 111 tests, OK |

The one skip is `RestartLeg.test_a_manifest_helper_reads_a_generation`, which needs `/proc` and is
absent on macOS. On the Linux host nothing skips.

No concrete regression found in any of the seven, so there is no BLOCK.

## R1 through R9

### R1, the evidence pointer named the wrong run, and that run was a FAIL: PASS

The document now names the reported run and the historical run as two separate places, and says
which is which:

| what | where |
| --- | --- |
| the run this document reports | `bench/out/ready-g4/repair-batch2/` |
| the earlier FAIL run | `bench/out/ready-g4/repair-batch-historical-fail/`, with `WHAT-THIS-IS.md` |

Both exist on the host as `task-g4/out/repair-batch2` and `task-g4/out/repair-batch`.
I read the historical one directly: its verdict is
`FAIL: 30 cases, 12 passed, 3 failed`, bytes cowfs 2915185 native 3065937, restart generation
1175979 to 1191050. The reported one is `UNMEASURABLE: 30 cases, 12 passed, 0 failed, 3 unmeasurable`,
bytes cowfs 3018150 native 2904582, restart 1191050 to 1209860. The two are now distinguished by
name and by a marker file, not left to be confused.

Every table in the document is produced by a committed tool from a run's own record rather than
typed:

```sh
python3 bench/fsx-gate/tabulate.py bench/out/ready-g4/repair-batch2/cases.jsonl --markdown
```

I ran that against the reported run's `cases.jsonl` on the host. All 15 rows reproduce: smoke seed 1,
matched seeds 1/2/3/5/8/13/21/34/55/89, sync seed 7 all stream-identical and PASS, full seeds 1/2/3
UNMEASURABLE with divergence at operation 1, 6 and 10, every digest and `st_dev` pair 2050/171 and
fstype pair ext4/fuse.cowfs matching the document.

### R2, a path that did not exist and a test count CI never reported: PASS

`run.log` now exists in the cited location, copied from the run's own stdout, and the run it names
is the reported one. Per-case seconds are in `bench/out/ready-g4/repair-batch2/run.log`; I read the
same content at `task-g4/out/repair-batch2.log` and every figure the document quotes is there.

The counts are now the counts CI reports, and the document names the run:

| measurement | result |
| --- | --- |
| gate module alone | 111 |
| isolated CI discovery on the branch tree | 147 |
| each CI job at this head | 391 |
| skipped | 1 |
| duplicated test names across CI discovery | 0 |
| `TestCase` classes defined by the bridge module | 0 |

CI at this exact head, one run, three jobs, all green:

| job | conclusion |
| --- | --- |
| `check (ubuntu-latest)` | success, `Ran 391 tests in 36.753s` |
| `check (macos-latest)` | success, `Ran 391 tests in 60.074s` |
| `linux-fuse` | success |

Run `37268779134`, `head_sha 7b0c39a44a082736bcdc40fe6d60f7bb7a0730bf`, `head_branch verify/fsx-g4`,
event `pull_request`, conclusion `success`. It is the only CI run for that SHA.

The 110 parseable gate ids in the job log are 110 distinct names, one per job, duplicated across the
two jobs rather than within one; the module's 111th id wraps onto a second line in the log. That
matches the local 111 exactly, so the bridge collects every gate test and adds none of its own.

### R3, two documents disagreed on how many controls ran: PASS

The document says five and names them. The shipped set is five and the names are right:
`5d` exit 3, `5f` exit 3, `16` exit 3, `19` exit 1, `17` exit 1, all marked ok in the reported
`task-g4/out/mutations.log`.

### R4, a test class count was one short: PASS

`FirstDivergence` has 7 tests, and the document now says 7. The full per-class breakdown at this
head, which I derived from the module and not from the document:

| class | tests | class | tests |
| --- | --- | --- | --- |
| CompareCase | 19 | ToolPin | 4 |
| PlannedBudget | 10 | ArmAttestation | 3 |
| ExitContract | 10 | Attribution | 3 |
| DeviceDisambiguation | 9 | ByteCaps | 3 |
| FirstDivergence | 7 | ImmutableAttemptDir | 3 |
| RestartLeg | 6 | MountIdentity | 3 |
| DirectCommandLine | 6 | Probe | 3 |
| CapabilityEvidence | 4 | Summary | 2 |
| Tabulate | 4 | FsxIdentity | 2 |
| UnreadableResult | 4 | ReadbackIsAFilesystemRead | 2 |
| | | ReadbackComparesAgainstTheRecord | 2 |
| | | SeparateProcessReadback | 1 |

147 isolated = 111 gate + 36 `test_gates`. 391 per CI job = those plus the main-branch suites the
merge commit carries.

### R5, the over-budget failure named the wrong arm: PASS

Now it names every arm that breached, with its own bytes and its own overage.
Three controls, each with a child that ignores its own `-l`, measured on my own mount:

| control | exit | text |
| --- | --- | --- |
| native only | 1 | `the native arm wrote 400000 bytes, over the declared per-arm budget of 262144 by 137856` |
| cowfs only | 1 | `the cowfs arm wrote 400000 bytes, over the declared per-arm budget of 262144 by 137856` |
| both arms | 1 | two separate failures, one per arm, each with its own figure |

The verdict record for the both-arms case carries
`over_budget: [{"arm":"native",...,"over_by":137856},{"arm":"cowfs",...,"over_by":137856}]`.
The earlier defect, where a native-only breach was reported as the cowfs arm with the cowfs figure,
is gone.

### R6, `planned_files` over-counted and the code comment contradicted the code: PASS

The plan is derived once, from the modes and seeds actually requested, with no in-place
multiplication. Measured directly against the archived runner:

| invocation | cases per arm | worst case per arm | partial |
| --- | --- | --- | --- |
| declared batch | 15 | 3932160 | false |
| `--seeds 2,3` | 8 | 2097152 | true |

8, not 30. A narrowed run reports partial coverage against the declared 15 rather than the batch's
figure.

The refusal happens before an arm is attested and before any child exists, and I confirmed the
artifacts rather than the prose. A per-arm total of 1000, exit 1:

| check | observed |
| --- | --- |
| record kinds in the evidence file | `verdict` only, one row |
| case rows | 0 |
| attempt directories created | 0 |
| bytes written per arm | native 0, cowfs 0 |
| caller file on the native arm | untouched |

Refusal text carries both numbers:
`this invocation would run 1 cases per arm, each at most 262144 bytes, so up to 262144 bytes per arm,
over the declared per-arm budget of 1000. The declared batch is 15 cases per arm and 3932160 bytes
per arm. Narrow the seeds, or raise the budget; nothing was run and nothing was deleted.`

The stale comment claiming the budget is "refused before it starts rather than reported after it
grew" is gone; the code now does what the comment used to claim, and the document describes the
actual behaviour.

### R7, the device lookup returned the first entry for a device: PASS

Mount selection is now longest-containing-path, with the ambiguity surfaced rather than resolved by
guessing:

| requirement | how the code meets it |
| --- | --- |
| longest containing path | `mountinfo_for_device(dev, path, rows)` picks the longest matching mountpoint among the entries on that device, and records `matched_by: "path"` |
| duplicate device | `mount_entries_for_device` returns every entry and never picks one silently |
| no containing entry | `ambiguous: true`, with `reason` and the full `candidates` list, and `fstype` left `None` |
| unreadable table | `unreadable: true` with a reason, kept distinct from ambiguity so nobody reads "several matched" into it |
| no fallback to a foreign mount | there is no branch that reaches another mount on the device, and none that reaches the `statfs` guess |
| no false clean arm | with `expect_fstypes` set, ambiguity gives `status: AMBIGUOUS_MOUNT`, an unreadable table gives `status: MOUNT_TABLE_UNREADABLE`, both `ok: false` |
| native arm | declares no expected type by default, so an unnameable device there is recorded and not fatal; `--expect-native-fstype` makes it fatal |

Nine `DeviceDisambiguation` tests cover this and pass.

### R8, the subject under test had no identity in the canonical document: PASS

The document now carries a full identity block, and it is truthful about what that identity is and
is not:

| | |
| --- | --- |
| serving process | pid 1209860, start time 6259868, from `/proc/1209860/stat` field 22 |
| `cowfs-daemon` sha256 | `769cf9e124b4d59d442146ec30075c7209643380a4566fd43110aa6e93d2e338`, 6178056 bytes |
| `cowfs` CLI sha256 | `a95c5b4b51aa066723ec9352b2e522e4f4b81bc689cb4d2f990b9ca3df9caf70`, 7036672 bytes |
| FUSE artifact | none separate, `cowfs-fuse` is a library linked into the daemon |
| source tree built | `task-g4/src`, 572 `.rs`, `.toml` and `.lock` files, tree digest `c25aec8b...` |
| compiler | `rustc 1.95.0 (59807616e 2026-04-14)` |
| build log | `task-g4/out/build-cowfs.log`, 3m08s, release profile |

Verified against the running host, read-only, without touching it:

| claim | my measurement |
| --- | --- |
| pid 1209860 start 6259868 | 1209860 / 6259868, unchanged for the whole review |
| daemon digest `769cf9e1` | `769cf9e124b4d59d` |
| CLI digest `a95c5b4b` | `a95c5b4b51aa0667` |
| 572 `.rs`/`.toml`/`.lock` files | 572 |
| `rustc 1.95.0 (59807616e 2026-04-14)` | exact match |
| build log 3m08s release | `Finished release profile [optimized] target(s) in 3m 08s` |
| read copy with no `.git` | no `.git` under `src`, top level is `Cargo.lock`, `Cargo.toml`, `crates`, no `target/` |

The disclosures are the part that matters and they are correct:

- **Digest identification only.** The document states the identification that holds is the binary
  digest, read from the running process's own `argv[0]`, and that this is what the independent
  review copied and compared.
- **Not a source-build attestation.** It states the source tree digest says which tree was built and
  is *not* a verified correspondence between that tree and those binaries, and that nothing checks
  one against the other after the fact.
- **No unverified commit claim.** It states the build tree has no `.git`, so no commit can be named
  for the binaries, and it names no commit.
- **Current main is not the subject.** It states the branch's own head is not the subject and is not
  claimed to be, and that main's head moves while the gate runs.
- **Exclusions disclosed.** The digest covers source files; the build output, the daemon binary and
  the CLI are separate named artefacts, and the absence of a separate FUSE artifact is stated.

One observation, not a defect. The named tree digest `c25aec8b...` is not independently
reproducible from the archive, because the method that produced it is not committed. My own
recomputation over the same 572 files, hashing the sorted per-file digests, gives
`ffb0c2f5d941a33cf9427d4e3c7fb0f6b74569529c438bd335a9b0b28be07281`, which is a different construction
and so a different value. Nothing hinges on it: the file count reproduces exactly, the identity that
holds is the binary digest, and the document already says the tree digest is not an attestation. If
the digest is meant to be checkable by a reader, the method belongs in the repo next to it.

### R9, two exit codes meant different things in two harnesses: PASS, by deliberate migration

The runner now declares
`EXIT_PASS, EXIT_FAIL, EXIT_UNMEASURABLE, EXIT_INVALID = 0, 1, 2, 3`, matching `bench/compare.py`.
What decides the code is the kind of the reason, set where the reason is produced and never parsed
back out of a message:

| kind | meaning | exit |
| --- | --- | --- |
| `unsupported` | an operation the filesystem does not have, so the arms did different work | 2 |
| `invalid` | input, provenance, tool pin, an arm's identity or the evidence itself is wrong | 3 |
| `divergence` | the arms really did different work and nothing explains it | 1 |

INVALID outranks FAIL, because a divergence reported next to bad provenance is not trustworthy.
FAIL outranks UNMEASURABLE, because a real divergence is a failure even when coverage is incomplete.

Unreadable evidence is now INVALID rather than an accidental FAIL. `sha256_file` returns
`(None, reason)` and every caller branches on it. I checked each one: an unreadable data file
produces `invalid("%s data file unreadable: %s")` and lands in `invalid`, while an empty data file
produces `divergence("%s data file is empty")` and lands in `failures`. No `TypeError` traceback, no
accidental exit 1.

All four codes preserved and reachable. Eleven process cases, measured through the process exit code
with the 7b0c39a harness on my own private mount, all eleven matching, all four codes seen:

| case | expected | observed | verdict line |
| --- | --- | --- | --- |
| pass, matched pair | 0 | 0 | `PASS: 2 cases, 1 passed, 0 failed, 0 unmeasurable` |
| unmeasurable, capability | 2 | 2 | `UNMEASURABLE full seed 1: ... part company at operation 1` |
| invalid, tool pin | 3 | 3 | `INVALID` |
| invalid, arm not a cowfs mount | 3 | 3 | `INVALID` |
| invalid, undeclared mode | 3 | 3 | `INVALID: no declared mode named no-such-mode` |
| fail, divergence at a read | 1 | 1 | `FAIL smoke: ...` |
| fail, divergence in a capability mode | 1 | 1 | `FAIL full: ...` |
| fail, plan over the cap | 1 | 1 | `FAIL: this invocation would run 1 cases per arm ...` |
| fail, over budget, native only | 1 | 1 | `FAIL the native arm wrote 400000 bytes ...` |
| fail, over budget, cowfs only | 1 | 1 | `FAIL the cowfs arm wrote 400000 bytes ...` |
| fail, over budget, both arms | 1 | 1 | `FAIL ...` with one failure per arm |

0 mismatches. `distinct_exits [0, 1, 2, 3]`.

The historical convention is labelled, not rewritten. The document's measured-run table carries both
figures side by side: exit 3 under the runner's convention at that revision, exit 2 under the
convention now in force, with the sentence that the earlier recorded exit of 3 is left as it is
because that was the convention in force when it ran. The raw log on the host is unmodified.

F1's fail-closed attestation still discriminates without a real mount. The three controls that need
no cowfs arm all refused correctly: a plain directory as the cowfs arm exit 3, a binary that is not
the manifest's exit 3, a mode the config does not declare exit 3. Nothing falls back to a clean
verdict when the mount is absent, a foreign, or unnameable.

## Budget, recomputed from the record

| figure | value |
| --- | --- |
| per-case maximum, reaches fsx as `-l` | 262144 |
| per-arm total, declared batch | 3932160 = 15 x 262144 |
| both arms, declared batch | 7864320 |
| written, reported run, cowfs | 3018150, under 3932160 |
| written, reported run, native | 2904582, under 3932160 |
| over-budget arms | none |
| verdict | UNMEASURABLE, unchanged |

The earlier per-arm figure was 15 x 2 x 262144, which multiplied the case count by two arms and
called the result a per-arm total. The correction changes the label and the declared budget, not the
verdict, because both measured arms fit under either reading. `max_bytes_written_both_arms` is now a
declared field rather than arithmetic in prose.

## Lint

`ruff check --select F,E9` over the lane's own files: all checks passed.
Files: `run-fsx-gate.py`, `mount-manifest.py`, `tabulate.py`, `exit-taxonomy.py`, `mutations.py`,
`test_run_fsx_gate.py`, `test_fsx_gate.py`.

## Merge tree against current main

Current `origin/main` is `252b93e981fe11b3ff8c811d25678f1c23caab75`, read explicitly and not from
`FETCH_HEAD`. This is not the SHA named in my brief, `c07aabce`; main has moved on, and I report the
one that is actually there.

`git merge-tree --write-tree origin/main 7b0c39a` returns a single tree, `dc5e3f24...`, exit 0, and
zero `CONFLICT` lines: a clean merge. `7b0c39a` is not yet an ancestor of main, and main is 81
commits ahead of the PR base `46b0f26`.

I did not merge and I make no claim about a combined runtime. The measured run happened on the
builder's private mount against the artifacts named in R8, not against a merge of main.

## Closing directives and issue state

| check | result |
| --- | --- |
| issue 103 | open |
| GraphQL `closingIssuesReferences` for PR 102 | empty |
| `closes`/`fixes`/`resolves` + number in the PR body | none |
| same, across all 15 commit messages in full, not just subjects | none |

The only `close` in the body is the prose "confirmed all seven closed on its own private mount",
which is a statement about the review, not a directive.
Nothing in the PR can close 103.

## Scope this delivery does not establish

| | |
| --- | --- |
| the whole `fallocate` family | UNMEASURABLE. 75 of 75 combinations answer `ENOTSUP` on the mount where 75 of 75 answer `ok` on `ext4`. Issue 103 is open. Not patched here, and correctly so: it is `crates/cowfs-fuse`. |
| macOS NFS | UNMEASURED. `ltp/fsx.c` includes `<linux/mman.h>` unconditionally, so there is no darwin build of the tool. A source fact, not a Darwin result. |
| full filesystem acceptance | not claimed anywhere in the delta. |
| durability | the restart leg is a clean reopen of a live store, not a durability acknowledgement, not crash injection, not power loss. The document says so. |
| fsync counts | none exist. `OP_FSYNC == OP_MAX_FULL` at line 138 and `op = rv % OP_MAX_FULL` at line 2411 in the pinned source. |
| throughput, the 1.5x criterion | no claim, none made. |

## Protected state, untouched

Borrowed builder daemon `task-g4/private`: pid 1209860, start time 6259868, before and after, with
no signal, no restart, no unmount and no write into its fixtures.
Its binaries were read-copied as inputs and compared by digest; nothing else of its was used.
The g5 mount and the builder's mount were present throughout and still are.
My own daemon was started on my own store, socket and mount, used, and stopped; the pid is gone, my
mount row is gone, and the signal was preceded by an identity check of that exact pid against my own
store path.
No install, no sudo, no sysctl, no reboot, no force, no reset, no stash, no rebase, no merge, no
lease return.
My own scratch scripts were removed after verifying their paths; my evidence directories and the
harness copy are kept as receipts: `attempt-7b0c39a` 39M, `attempt-ff45b0a` 24M, `harness-7b0c39a` 504K.

## Evidence

Canonical report: this file, `docs/reviews/pr102-final-delivery.md`.
Reviewed source archived under `bench/out/g4-residual-final-critic/source/`, 29 files, with
`logs/REVIEWED_SHA.txt` naming the exact commit and `logs/archived-source-sha256.txt` listing every
archived file.
Reviewed SHA: `7b0c39a44a082736bcdc40fe6d60f7bb7a0730bf`.

The harness I ran is byte-identical to my archive, verified on both sides:
`run-fsx-gate.py` `5a4c6edd408245c9`, `mount-manifest.py` `194a277d9f5fd2e1`,
`tabulate.py` `3ce5a15cdaa83f63`, `exit-taxonomy.py` `f4743cfcff086b76`,
`fsx-gate.json` `501ee24409507424`, `cowfs-mount.sh` `7425a0bb2c4c1c1d`.

Remote, under my own namespace:
`/home/moonscape/cowfs-ready-wave/task-g4-review/harness-7b0c39a/` is the reviewed harness.
`/home/moonscape/cowfs-ready-wave/task-g4-review/attempt-7b0c39a/` holds my store, socket, mount and
evidence, including `evidence/attest.json`, `evidence/exit-taxonomy.json`,
`evidence/tabulate-mine.md` and `evidence/tabulate-reported.md`, and the eleven control runs under
`out/taxonomy/`.

Evidence and claims are kept apart.
The raw records are gitignored and are what I measured from.
The document's claims I recomputed from those records, and the ones that disagreed are the nine above.

PR 102 stays open and unmerged.