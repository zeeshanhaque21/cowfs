# PR #139 publication delta and CI attribution audit: the four corrections land, the `04d2fb5` runtime gate is closed by CI on the merge ref, and this head's own gate is still open

Reviewer lane: `.treehouse-build-train/.treehouse/cowfs-7c1bf8/6/cowfs`, held slot 6, distinct from the READY6 author.
Lease verified before any write: branch `review/gc-root-mark-retention-82`, HEAD `b4b55ab`, working tree showing only the six pre-existing untracked `docs/reviews/*.md` files from other lanes, no process of mine running.

Head reviewed: `5e89c3abde3cceebf7f930784fe7af3f45a7c166`.
Head I reviewed last round: `04d2fb554563df467b3f14a712f994c5d363ca19`.
PR base per the API: `b486d4541bc47b273a5bbd222b95c24fed05c36d`.
Current main, fetched into the local object store: `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`, unchanged since my last round.

New record read as the object under audit: `docs/verification/evidence/meta42-cache-write-clock-gap-publication-correction.md`, sha256 `5b33ed93092a5a6db4be460bd991ed24748430a70483e4f04800898b11e755d1`, identical in the MAIN primary checkout and in the blob inside the head.
My prior report `docs/reviews/pr139-meta42-cache-write-clock-gap-final.md`, sha256 `661ead7050c648d5d3a53c75167daf0844924d69debd173decabb6a1fdfbd51d`, is published on this branch byte for byte.

Review date: 2026-10-06.
Artifacts: `bench/out/pr139-publication-final-critic/logs/**` inside the lease, two CI logs, 722,025 bytes.
This document: canonical PRIMARY copy, `docs/reviews/pr139-meta42-cache-write-clock-publication-final.md`.

## Verdict

SCOPED PASS on the publication delta, with this head's runtime gate PENDING.

The delta is documentation only and I verified that by full blob OID rather than by the diff summary: every code and configuration path is byte-identical between the two heads.
My four wording findings are all corrected, each in the way I asked for and each without inventing a benchmark to settle it.
The self-attribution discipline is the strongest thing in the record: it separates the author's local measurements from CI's, corrects its own earlier over-claim about the integrated tree, and declines to let a green `linux-fuse` job stand in for a runtime gate it does not provide.

The runtime gate I left open at `661ead70` is now closed, and closed by evidence I read myself.
Both workspace job logs for `04d2fb5` record the merge ref `a78e287`, which is `04d2fb5` merged into `cf67e8a6`, and both record the named clock test `ok`, `cowfs-core --lib` at 30 passed 0 failed 0 ignored, `caches` 2, `locks` 2, `operation_time` 4 and 6, and `critic2b` 27 passed with 1 ignored, with zero failures and zero error lines on either platform.
That is CI compiling and testing the branch integrated into current main, which is exactly the integration neither the author nor I could afford locally.

This head's own CI has not completed, so I record the attribution the author chose, which is the correct one: the outcome carries as a matter of source identity, this is still a different commit, and its own CI is read once and reported on its own terms rather than inherited.

One new accuracy defect, non-blocking: the record cites a commit id that does not exist in this repository.

## Budget: read-only, nothing run

The lane's `bench/out` measured 35.882 GiB against the 8 GiB cap before and after, the difference being two CI logs totalling 722,025 bytes.
No cargo invocation, no build, no archive, no target directory, no private probe, no test execution, no deletion, pruning, move, offload or waiver.
Free space was 272.8 GiB and is not the gate.
I did not use the MAIN checkout's `bench/out` or any other lane's artifact tree as a way around that.
The only reads outside this repository's git objects were the two CI logs, which I retrieved from the API myself rather than from another lane's stored copies.

## The delta is documentation only, by full blob OID

`git diff --stat` between the two heads names two documents and reports 623 insertions.
I did not rely on that. I compared the full object id of every code and configuration path the task named, unabbreviated:

| Path | Full blob OID at both heads |
| --- | --- |
| `crates/cowfs-core/src/io.rs` | `f131185ce3c474b92748e3be192dea54e8e207b7` |
| `crates/cowfs-core/src/inner.rs` | `09219472a79701e3f1f4d9623b41967030281bb1` |
| `crates/cowfs-meta/src/tx.rs` | `7372c24c31298d64487ae048cd7f73501a457e02` |
| `crates/cowfs-core/tests/caches.rs` | `af9fddcd4ad292baa335ca791d2e99fb91cb8155` |
| `crates/cowfs-core/tests/locks.rs` | `856b4d8df1f99ef639cee092cdfb90490edef671` |
| `crates/cowfs-core/tests/operation_time.rs` | `dc6ce88ef11b63cc6d9a8345861a0dd3fbf322e1` |
| `crates/cowfs-core/tests/critic.rs` | `d1a9406d4763fbbe1460a52c333e6b98e495d057` |
| `crates/cowfs-core/tests/critic2b.rs` | `7b86e02d36345f3488335b08c8e79fbda01f3f90` |
| `crates/cowfs-core/Cargo.toml` | `fac6c29cdcdf976ff1ca29f080620a2a77dfaf44` |
| `crates/cowfs-meta/Cargo.toml` | `26eacd6f74cd684edfb0c14b069c779b6cefe5f3` |
| `Cargo.toml` | `55adec658854db42e509b922286b7756977a806e` |
| `Cargo.lock` | `64ccfc6ec32fdd5cf943fe981cfc6d05bb71dcf5` |

Every row identical, so there is no physical clock change, no new public feature, no dependency movement, no feature flag and no other clock site touched.
The two added documents are my published review and the new record.

## History count, measured rather than taken

The task flagged a reported count of three against a list of two.
The actual range `04d2fb5..5e89c3a` is **two** commits:

```
081209740a5f447172269afa10dc41d5b40b839b  docs(verification): publish the gap review, and correct four claims in my own record
5e89c3abde3cceebf7f930784fe7af3f45a7c166  docs(verification): attribute CI's merge-ref runtime evidence separately from local work
```

Both are documentation-only, so the code-identity conclusion is unaffected by the discrepancy.

## The four corrections, checked against the source

**Correction 1, the vDSO claim.** Withdrawn and replaced with the defensible statement, that the production build makes the same single call to `Timestamp::now` and no more.
I agree with the replacement: the reported evidence is a symbol name plus the absence of TLS and atomic references, and none of that establishes a clock route.

**Correction 2, `#[inline]`.** Corrected to a request the optimiser may decline, with the observation that the author's own evidence was symbol-level and that a debug build may keep the call.
It also drops the zero-cost, no-call-instruction and wall-time claims explicitly, and states that no benchmark was invented, in either direction.
That is the right call and it matches what I asked for.

**Correction 3, the `old` arm.** This is the one I could verify hardest, and it resolves in the author's favour on the substance.
The record now gives a table placing each arm's reading: `new` under the lock through `clock_read(&st)`, `gap` between the park and the lock, `old` above the park, with three distinct `io.rs` digests, and the separate claim that the `ClockGate` region and `mod clock_order_tests` are identical in all three.

I reconstructed both mutants myself, read-only, from the `04d2fb5` blob in memory, moving only the one clock line, and hashed the results.
The whole-file digests match the record's published values **exactly**:

| Arm | My independent reconstruction | Record |
| --- | --- | --- |
| `new` | `41b5a38ecdc6a64f19ccb16f3f9cfdd618b7fe34ad65eb335c8539b80bea1961` | `41b5a38ecdc6a64f…` |
| `gap` | `3a791b9a11f7d20c7475cba636a7c0824c7bb325ec093a68af04dabf94c68cb1` | `3a791b9a11f7d20c…` |
| `old` | `1bfbb7330e3f26a2ce26b575a2df6d93f794f5d895e71913fd83a710fb5fe63a` | `1bfbb7330e3f26a2…` |

And the placements I measured are the record's: `gap` reads at line 90 with the park at 89 and the lock at 92, `old` reads at line 88 with the park at 90 and the lock at 92.
So `old` really is the above-park revert and the original "byte-identical" was the error, and both mutants really are different files at different positions.
The identity-across-arms claim also reproduces: my `ClockGate`-region and `mod clock_order_tests` digests are byte-identical across all three of my reconstructions.

One limit on that: the record publishes those two region digests as 16-hex prefixes and does not state its region boundaries, so I could not reproduce those two specific values.
My boundaries were the doc comment before `struct ClockGate` through the closing brace of `park_if_armed`, ending at line 537, and `mod clock_order_tests` through end of file, 117 lines from line 590.
The identity claim, which is what the table is for, is confirmed; the specific prefixes are boundary-dependent and are not independently reproducible as printed.
Publishing the boundaries would make them checkable.

**Correction 4, the citations.** Corrected, and I verified every row against the source at `04d2fb5`:

| Line | Function | What the record claims | Verified |
| --- | --- | --- | --- |
| 172 | `op_setattr` | read before `node.st.wr()` at 179 | yes, 179 is `let mut st = node.st.wr();` |
| 391 | `op_setxattr` | read after `sc.ns.lk()` at 390, before the node lock | yes, 390 is `let _ns = sc.ns.lk();` |
| 440 | `op_removexattr` | read after `ns.lk()` at 439, before the node lock at 442 | yes, 439 is `let ns = sc.ns.lk();` and 442 is `let mut st = n.st.wr();` |
| 560 | `clock_read`, `not(test)` twin | the helper | yes |
| 573 | `clock_read`, `test` twin | the helper | yes |

Neither 168 nor 436 resolves to a clock read, `op_setxattr` was indeed missing, and `ns.rs` is correctly left un-measured with no line number asserted.

The two further notes are also taken: the compile-error sentence is split into a typed compile-refusal half and a runtime bare-bypass half, with neither presented as an ownership probe, and the merge-tree figure is explained as having been computed from an input tree that still contained the record as an uncommitted file.
I reproduced the author's corrected figure exactly: `git merge-tree --write-tree cf67e8a6 04d2fb5` returns `5cf4e415e464f07d3a164c2205bf4786a8e8ae4b` with one line of output and no conflicts.

## One new defect: a cited commit id that does not exist

Record line 245 states that the production change "landed at `cd1dca8b7b5bd331e8e553e7874c0852458a1c15` and is untouched since".

That object is not in this repository.
`git cat-file -t` on it fails, and no commit reachable from any ref begins with `cd1dca8b`.

The commit that actually landed the production clock change on this branch is `e9dc1066a5601bde3bcca439d8df71e03c1972ea`, titled `fix(core): read a write's clock after it takes the node lock (issue #42 cache-write clock)`, and it is the oldest of the three commits in the range that touch `io.rs`:

| Full OID | Subject |
| --- | --- |
| `e9dc1066a5601bde3bcca439d8df71e03c1972ea` | the production fix, the clock read moved under the node lock |
| `d2c5337b0032250778228a0c8b72331d2850112f` | the first permanent test |
| `8b90129f60fd3110345ace06afa71e2af356af59` | the guard-requiring helper and the counter, closing the gap |

So the substantive statement in that paragraph is true and I verified the code is unchanged, but the citation is wrong and a reader who tries to resolve it will fail.
It is a one-token correction.

A second, smaller completeness note: the record's CI table lists `operation_time` as 4 passed.
The logs show **two** `operation_time` test binaries on both platforms, at 4 passed and 6 passed.
Neither is a failure and neither contradicts anything, but the table undercounts a target it claims to summarise.

## CI on the head I reviewed last round: verified closed

Run `37393266584`, branch head `04d2fb5`.
Both workspace job logs, read in full, record the same checkout line:

```
HEAD is now at a78e287 Merge 04d2fb554563df467b3f14a712f994c5d363ca19 into cf67e8a6b2f8d346485fdf1c71d24283da0b43a0
```

That matters more than a green tick: CI compiled and tested the branch **integrated into current main**, on the merge ref `a78e287`, which is the integration the author could not afford locally and which I reported as unverified last round.
The record says exactly this and attributes it to CI rather than to itself, and it uses that attribution to correct its own earlier claim in `508698d0` that no compile of the integrated tree had been run.
That self-correction is the right one and I endorse it.

Verified from the logs, per platform:

| What | macOS `112043107530` | Ubuntu `112043107562` |
| --- | --- | --- |
| merge ref checked out | `a78e287` | `a78e287` |
| branch head inside it | `04d2fb5` | `04d2fb5` |
| `io::clock_order_tests::ctime_does_not_move_backwards_when_the_first_writer_applies_last` | **ok** | **ok** |
| `cowfs-core` lib binary | 30 passed, 0 failed, 0 ignored | 30 passed, 0 failed, 0 ignored |
| `tests/caches.rs` | 2 passed, 0 failed, 0 ignored | 2 passed, 0 failed, 0 ignored |
| `tests/locks.rs` | 2 passed, 0 failed, 0 ignored, 60.45s | 2 passed, 0 failed, 0 ignored, 60.28s |
| `tests/operation_time.rs` | 4 passed and 6 passed, 0 ignored | 4 passed and 6 passed, 0 ignored |
| `tests/critic2b.rs` | 27 passed, 0 failed, **1 ignored** | 27 passed, 0 failed, **1 ignored** |
| `test result: FAILED`, `error:`, `warning:`, `##[error]` | 0 | 0 |

The named clock test appears as its own line ending `ok`, so it ran rather than being filtered out, and it is inside the 30.
The 1 ignored in `critic2b` is reported as ignored on both platforms, not folded into a pass count.
The `locks` durations of just over 60 seconds are consistent with the fixture's bounded 60-second wait actually being exercised rather than short-circuited.

My independently retrieved copies: sha256 `c6c8cc723f08af6fcc770919922b61c7111718010ed53409f6591867b0d0a88c` for Ubuntu at 366,376 bytes and `40757e399d86f70b5dfe2374aa03e37a443f9521cdefddb65b52abbc53512bd2` for macOS at 355,649 bytes.

The `linux-fuse` job in that run is green and carries no `cowfs-core` evidence, exactly as I reported last round and as the record now states in its own table.
I did not treat it as a gate then and I do not now.

## CI on this head: one snapshot, PENDING

Run `37394945153`, `head_sha` `5e89c3abde3cceebf7f930784fe7af3f45a7c166`, `head_branch` `fix/cache-write-clock-42`, created `2026-10-06T00:37:05Z`, `status` `in_progress`.

| Job | Id | Status |
| --- | --- | --- |
| `linux-fuse` | `112048562492` | in_progress |
| `check (macos-latest)` | `112048562687` | in_progress |
| `check (ubuntu-latest)` | `112048562707` | in_progress |

PR #139 reports `mergeStateStatus` UNSTABLE, `isDraft` false, `state` OPEN, `closingIssuesReferences` empty.
Nothing was polled, rerun, dispatched or reconfigured, and no workflow, runner or job setting was touched, and no new trigger was created.

On the attribution the record asks for, I agree with it and would not word it differently.
The test outcome carries from `04d2fb5` to this head as a matter of source identity, because `io.rs` and every test blob are byte-identical.
Source identity does not guarantee an identical runtime context or harness, and here the honest statement is that the configuration is the same and strong, but this head is a different commit whose own CI has not been read.
The record says it is "still a different commit, and its own CI is read once and reported on its own terms, not inherited from `04d2fb5`", and that is the correct discipline.

So: the `04d2fb5` runtime gate is closed by independently verified CI, and this head's gate is open.

## Integration against current main, read-only

`origin/main` was fetched into the tracked remote-tracking ref rather than left in `FETCH_HEAD`, resolved to `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`, verified present locally as a commit, and matched by the API.
`HEAD` and the checked-out branch are unchanged: branch `main`, HEAD `93cfef94457a989d031cb6b0a475ac4edbdb85ef`.

`git merge-tree --write-tree cf67e8a6b2f8d346485fdf1c71d24283da0b43a0 5e89c3abde3cceebf7f930784fe7af3f45a7c166` exits 0 with a single tree and no conflict list, tree `12f97b3d82c44c4412b9192e2b6a0d97673351a0`, and I read that tree:

| Property | Result |
| --- | --- |
| `clock_read` definitions | 2, the two twins |
| merged `crates/cowfs-core/src/io.rs` identical to this head's | yes |
| merged `crates/cowfs-core/tests/caches.rs` identical to **main's** | yes |
| hole test `live_blocks_filters_holes_and_yields_only_stored_blocks` present | yes |
| my published report present in the merged tree | yes, `661ead70…` byte for byte |
| merged `Cargo.lock` identical to this head's | yes |

Main has not touched `io.rs` relative to this branch, so the source locks are unchanged by the merge, and main's hole-flag `caches` variant survives whole.
This is a statement about a tree and nothing more: I compiled nothing from it and ran nothing in it, and I make no claim that it builds or that its tests pass.
For this head, CI on the merge ref will be the first thing that actually compiles the integration, and it has not finished.

## Closing forms, and one the author has now fixed

`closingIssuesReferences` is empty and the API reports issue #42 `open` with `state_reason` `reopened`, last updated `2026-10-05T23:04:24Z`.
The timeline shows `closed` at `2026-10-05T23:02:38Z` and `reopened` at `2026-10-05T23:03:29Z`, both before this branch's current commits, which is exactly what the record asserts.

The body has been improved since my last review and the hazard I flagged is gone.
The negated form I reported, `This is not a replay fix and touches no replay code. **#42 stays open.**`, has been replaced by `This is not a replay fix and touches no replay code. The umbrella issue stays open, and this branch does not decide any of its other requests.`, which carries no issue token on the keyword line at all, and the closing reference at the foot is now the neutral `Refs: #42`.
A keyword-and-reference scan of the current body finds one such line, `Depends on #136:`, where the keyword is part of the branch path `fix/deferred-operation-time-42` rather than prose.

The body also carries a coordinator warning about the residual history hazard: `e9dc106`'s subject pairs a closing keyword with an issue number on one line, published shared history is not rewritten to edit a message, an empty `closing_issues_references` has already been observed in this repository alongside an issue that was shut anyway, and the issue's state must therefore be read from the API immediately after any merge and reopened if the platform shut it.
I concur with that, and with the conclusion that it is the coordinator's action and not a rewrite.

## What this does not claim

- **No local execution of any kind.** No cargo, build, archive, target, probe or test.
- **This head's CI is not green and its gate is open.** Three jobs in progress at my one snapshot.
- The three-arm mutant results, and the object-level symbol table, are the author's local measurements. I re-read those archives' effect by reconstructing the mutants in memory and matching all three whole-file digests exactly, which is independent verification of the archives' identity and of Correction 3, not a re-execution of the runs.
- No zero-cost, no-call-instruction, no wall-time and no clock-route claim, and no benchmark was invented in either direction.
- The two 16-hex region digests in the record are not independently reproducible as printed, because their boundaries are unstated; the identity claim they support is reproduced.
- No compile of the merged tree, and no claim that the integrated tree builds or its tests pass.
- No global clock monotonicity; `Timestamp::now()` is the host wall clock and can step backwards.
- `op_setattr`, `op_setxattr` and `op_removexattr` are untouched and unreproduced, and their status is unchanged by the citation fix.
- No performance, timing acceptance, threshold, `SIGKILL` or power-loss claim.
- `no-mistakes` is uninitialized in this lane and was not initialized. Browser UNVERIFIED, and `chromium` is not installed on this host.
- Misakanet is local-only here and was not consulted; no local memory store was reachable in this lane.

## Scope discipline

Issue #42, the cache-layer `ctime` obligation, this publication delta and CI attribution only.
No new issue, no new feature, no audit matrix, no new task.
Issues #125, #127 and #128 remain parked and untouched.
No production edit, no test edit, no fixture edit, no commit, no push, no merge, no ready flag, no issue closure.
No checkout, no reset, no stash, no branch change.
The one repository-state mutation is the explicit fetch of `origin/main` into `refs/remotes/origin/main`, which was authorised and moved no branch and no `HEAD`.
No lease acquired, returned, reset, stashed, pruned or destroyed, and no lease action of any kind.
No signal, restart, sudo, install, unmount or store operation, and no deletion of any kind.
The shared daemon PID 15263 with start time `Sat Oct 3 20:44:29 2026`, the Linux host `9879298996041209860`, and every store, mount and job were never contacted, and no mount was traversed.
Concurrent lanes untouched: READY1's PR #141 final fixture work in its own lane, READY5's PR #140 lint and docs worker owning `cowfs-meta` only, READY5's Core critic idle at `fe65`, READY3's PR #141 author idle at `4b`, READY6 idle at `5e89c3a`.
`crates/cowfs-core/src/io.rs` was read only through `git cat-file` at committed heads, never from a working tree.
Preserved immutable and re-verified unchanged: `508698d0`, `75a2310b`, `b5e1f19c`, `72d37cc4`, `a8bc5337`, and my own `661ead70`, `a552af90`, `9341cc4f`, `471a0731`, `803ea5c5`, `ccc5eafc`, `4c3450f5`.
Files I own for this review: this document and `bench/out/pr139-publication-final-critic/logs/**` in the lease, which `.gitignore` excludes.

## What remains

1. A read of run `37394945153` after `check (ubuntu-latest)` and `check (macos-latest)` complete, showing the merge ref for this head, the named clock test `ok`, `cowfs-core --lib` at 30, `caches`, `locks`, `operation_time` and `critic2b` with its ignore reported as ignored, and clippy and fmt clean.
2. Two one-token corrections in `5b33ed93`: replace `cd1dca8b7b5bd331e8e553e7874c0852458a1c15` with `e9dc1066a5601bde3bcca439d8df71e03c1972ea`, and add the second `operation_time` binary to the CI table.
3. Optional: state the two region boundaries in the record so its `ClockGate` and `mod clock_order_tests` digests are reproducible as printed.
4. The `bench/out` budget decision, still over cap and still blocking every lane that wants to execute.
5. The coordinator's post-merge check of issue #42 from the API rather than from the refs list, because of the `e9dc106` history hazard and the repository's own precedent for an empty list alongside a closed issue.

Nothing was fixed, merged, marked ready or closed.
PR #139 stays open, issue #42 stays open, and no new task was created.