# Final CI gate verification for the #42 cache-layer work: PR #139 head `5e89c3a` and PR #141 head `4b3daed` both green on both platforms, both clean into main, disjoint files

Reviewer lane: `.treehouse-build-train/.treehouse/cowfs-7c1bf8/6/cowfs`, held slot 6.
Lease verified before and after every write: branch `review/gc-root-mark-retention-82`, HEAD `b4b55abfe9ab2d8d6f5fc42403bb1eb8b1c02d41`, only the six pre-existing untracked `docs/reviews/*.md` files from other lanes, no process of mine running.

Review date: 2026-10-06.
Mode: read-only. Nothing compiled, nothing executed locally, nothing merged.
This document is the canonical PRIMARY copy at `docs/reviews/meta42-final-ci-gates.md`.

## Verdict

PASS on both final gates, with two items the coordinator must carry, neither of which is a code defect.

PR #139 at head `5e89c3abde3cceebf7f930784fe7af3f45a7c166`, run `37394945153`: all three jobs completed success, both workspace jobs compiled and tested the branch integrated into current main on merge ref `e2fbf6b`, the named clock test reports `ok` on both platforms, and every target of interest reports 0 failed.
PR #141 at head `4b3daed988faf2dcd63d16a8c2b9aa6140b1d5a4`, run `37394623027`: all three jobs completed success, both workspace jobs on merge ref `d6d37bb`, `core_atomic_rename` 10 passed 0 failed 0 ignored on both platforms, every promotion case `ok` on both, and the promotion-matrix negative control green on both.

I verified the combined tree without a checkout.
The two PRs edit a disjoint set of files, and merging PR #141 into the projected result of PR #139 into main exits 0 with no conflicts, carrying both changes' production blobs exactly.

The two items I carry forward:
PR #141 is a **draft**, so its green does not authorise a merge and its head may still move.
PR #139 carries the `e9dc1066a560` commit-subject closing hazard in shared history, and issue #42 must be read back from the API after the merge rather than trusted to the refs list.

## Budget: read-only, nothing run

The lane's `bench/out` measured 35.883 GiB against the 8 GiB cap before this round and the same after, the difference being six CI logs totalling 2,356,208 bytes.
No cargo invocation, no build, no archive, no target directory, no private probe, no test execution, no deletion, pruning, move, offload or waiver.
Free space was 271.2 GiB and is not the gate.
I did not use the MAIN checkout's `bench/out` or any other lane's artifact tree as a way around the cap.
Logs went into the already-owned `bench/out/pr139-publication-final-critic/logs/`, which `.gitignore` excludes.

## Tooling note, and one method correction I owe the reader

The coordinator reported that context-mode's batch executor failed on a disk I/O error.
I did not retry it.
Every derivation below was done with bounded `rtk proxy python3` one-liners against the log files already on disk, and every count printed was computed in that sandbox rather than read into the conversation raw.
No result in this document is quoted from a previous agent's summary.

I owe a correction about method.
The raw jobs archive returned by `gh api .../jobs/N/logs` interleaves cargo's stderr `Running <target> (target/debug/deps/<bin>)` lines against the test binaries' stdout, so the `Running` line for one binary is frequently printed *inside* the block belonging to its neighbour.
Keying a result onto the nearest preceding `Running` line therefore produces confident, wrong attributions.
I hit exactly that: on one pass I read `critic2b` as 3 passed / 4 ignored, `core_atomic_rename` as 20, and `caches` as 6 on macOS.
All three were wrong.

The sound anchors are `running N tests` and the `test result:` line that closes it, which are self-validating because `passed + failed + ignored` must equal `N`, with target identity taken from the per-test names inside the window.
Every window in all four workspace logs closes, and all but two satisfy that sum identity.
The two exceptions are the same artefact in all four logs, an interleaved block that repeats `1 passed; 12 filtered out` eight times, which is a windowing artefact of that block and not a test outcome.
I re-derived all counts that way and the corrected figures are the ones reported below.

The corrected, verified numbers are:
`critic2b` is 28 tests, 27 passed, 0 failed, 1 ignored, on all four workspace jobs.
The target with 7 tests and 3 passed / 4 ignored is `critic`, a different target, and it is present on both platforms in both PRs.
`core_atomic_rename` is 10 tests on both platforms.
`caches` is 2 tests on both platforms.
`cowfs_core` lib is 30 tests on both platforms for PR #139 and 29 for PR #141, the difference being exactly the one clock-order test PR #139 adds.

## Run identities and merge refs

| Run | Head | Created | Conclusion |
| --- | --- | --- | --- |
| `37394945153` | `5e89c3abde3cceebf7f930784fe7af3f45a7c166` | `2026-10-06T00:37:05Z` | `completed` / `success` |
| `37394623027` | `4b3daed988faf2dcd63d16a8c2b9aa6140b1d5a4` | `2026-10-06T00:33:22Z` | `completed` / `success` |

Both workspace jobs and both FUSE jobs record the merge ref, which is the integration point that matters and not a branch-tip build.

PR #139, merge ref `e2fbf6b`:
`HEAD is now at e2fbf6b Merge 5e89c3abde3cceebf7f930784fe7af3f45a7c166 into cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`, recorded identically on Ubuntu `112048562707`, macOS `112048562687` and FUSE `112048562492`.

PR #141, merge ref `d6d37bb`:
`HEAD is now at d6d37bb Merge 4b3daed988faf2dcd63d16a8c2b9aa6140b1d5a4 into cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`, recorded identically on Ubuntu `112047533370`, macOS `112047533548` and FUSE `112047533596`.

Job completion times:

| Job | Id | Completed |
| --- | --- | --- |
| `linux-fuse`, 139 | `112048562492` | `2026-10-06T00:40:49Z` |
| `check (ubuntu-latest)`, 139 | `112048562707` | `2026-10-06T00:49:40Z` |
| `check (macos-latest)`, 139 | `112048562687` | `2026-10-06T00:52:38Z` |
| `linux-fuse`, 141 | `112047533596` | `2026-10-06T00:38:25Z` |
| `check (ubuntu-latest)`, 141 | `112047533370` | `2026-10-06T00:43:27Z` |
| `check (macos-latest)`, 141 | `112047533548` | `2026-10-06T00:49:57Z` |

## PR #139 final gate, run `37394945153`

Identical on Ubuntu `112048562707` and macOS `112048562687`, which is the outcome I expected and the one I verified rather than assumed.

| Target | Tests run | passed | failed | ignored |
| --- | --- | --- | --- | --- |
| `cowfs_core` lib unit binary | 30 | 30 | 0 | 0 |
| `core` integration | 20 | 20 | 0 | 0 |
| `critic` | 7 | 3 | 0 | 4 |
| `critic2b` | 28 | 27 | 0 | **1** |
| `caches` | 2 | 2 | 0 | 0 |
| `locks` | 2 | 2 | 0 | 0 |
| `operation_time`, core crate | 4 | 4 | 0 | 0 |
| `operation_time`, meta crate | 6 | 6 | 0 | 0 |
| `snapshot_rename` | 10 | 10 | 0 | 0 |

The named fixture the whole branch exists to pin appears as its own line on both platforms:
`test io::clock_order_tests::ctime_does_not_move_backwards_when_the_first_writer_applies_last ... ok`.
It ran, it was not filtered, and it sits inside the 30.

The `critic2b` ignore is reported as 1 ignored on both platforms and is not folded into a pass count.
The 4 ignores in `critic` are a separate pre-existing target and are likewise reported as ignored.

Steps, all actually executed in each workspace job:
`cargo fmt --all --check` ran and produced no output.
`cargo clippy --workspace --all-targets -- -D warnings` ran, emitted zero diagnostic lines of any kind, and closed with `Finished \`dev\` profile [unoptimized + debuginfo] target(s)`.
`cargo test --workspace` ran, 134 test windows, every one closed.
The bench step `python3 -m unittest discover -s bench -v` ran and reported `Ran 484 tests` with `OK (skipped=10)` on Ubuntu and `OK (skipped=13)` on macOS.
The differing skip counts are the same step on two platforms and are pre-existing, not a difference between the branch and main.

Strict failure scan across the whole log, both platforms:
`test result: FAILED` 0, lines beginning `error` or `error[` 0, lines beginning `warning` 0, `##[error]` 0, `Process completed with exit code [1-9]` 0.
Exactly one `##[warning]` exists per log and it is the runner's own notice that `actions/checkout@v4` targets deprecated Node.js 20 and is forced onto Node.js 24.
It comes from the action, not from this repository's build.

## PR #141 final gate, run `37394623027`

Also identical on both platforms.

| Target | Tests run | passed | failed | ignored |
| --- | --- | --- | --- | --- |
| `cowfs_core` lib unit binary | **29** | 29 | 0 | 0 |
| `core` integration | 20 | 20 | 0 | 0 |
| `core_atomic_rename` | **10** | 10 | 0 | 0 |
| `critic` | 7 | 3 | 0 | 4 |
| `critic2b` | 28 | 27 | 0 | **1** |
| `caches` | 2 | 2 | 0 | 0 |
| `locks` | 2 | 2 | 0 | 0 |
| `operation_time`, core crate | 4 | 4 | 0 | 0 |
| `operation_time`, meta crate | 6 | 6 | 0 | 0 |
| `snapshot_rename` | 10 | 10 | 0 | 0 |

The lib binary is 29 here against PR #139's 30, and the difference is exactly PR #139's clock-order test, which is the expected relationship between a branch that adds the test and one that does not.
It is positive evidence that the two runs are not the same tree, and it confirms the counts are read from the trees the merge refs actually name.

Every promotion case reports `ok` on both platforms:

| Test | Ubuntu | macOS |
| --- | --- | --- |
| `promote_base_replaces_its_target_with_the_source_content` | ok | ok |
| `a_refused_promotion_leaves_the_mount_exactly_as_it_was` | ok | ok |
| `promote_base_cannot_create_a_name_key_collision` | ok | ok |
| `a_damaged_block_does_not_stop_a_promote` | ok | ok |
| `promote_base_survives_a_failure_at_every_step` | ok | ok |
| `a_promoted_base_with_no_provenance_reports_itself_unknown_not_fresh` | ok | ok |
| `a_promoted_snapshot_reports_itself_as_a_base_and_keeps_doing_so` | ok | ok |
| `a_promote_is_idempotent_and_reports_a_base` | ok | ok |
| `base_refresh_records_the_ref_and_promotion_is_idempotent` | ok | ok |
| `the_matrix_catches_a_broken_ordering` with `should panic` | ok | ok |

`the_matrix_catches_a_broken_ordering ... should panic ... ok` is the promotion-matrix negative control, the test that fails if the property matrix stops catching a broken ordering.
It reports `ok` on all four workspace jobs, and it is present and green in PR #139's runs too, so it is not new to PR #141 and PR #141 did not weaken it.

Steps and the strict failure scan are identical in shape to PR #139's: fmt ran clean, clippy ran with `-D warnings` and zero diagnostics, `cargo test --workspace` ran with every window closed, and the bench step reported `Ran 484 tests` with `OK (skipped=10)` on Ubuntu and `OK (skipped=13)` on macOS.
`test result: FAILED` 0, `error` 0, `^warning` 0, `##[error]` 0, nonzero exit 0, and the same single runner-level Node.js deprecation notice.

## The FUSE jobs, and what they do not prove

Both FUSE jobs completed success and both checked out the correct merge ref.
Their content is byte-for-byte the same set of targets in both runs:
`cowfs_fuse` 48 passed 0 failed, `coherence` 4 passed, `mount` 40 passed, `regress` 11 passed, `conformance` 0 passed, and three `native` comparison runs each reporting 1 passed.

`cowfs_core` appears nowhere in either FUSE log.
The FUSE job therefore carries no `cowfs-core` evidence, and since its content is identical across the two PRs it distinguishes nothing between them.
I have not treated it as a gate in either direction, and neither PR's verdict depends on it.

Each FUSE log carries four `##[warning]` annotations, three of them the project's own:
`xattr_on_directory_and_symlink`, annotated known failing because Linux denies user xattrs on symlinks.
`concurrent_readers_and_writers_of_one_file`, annotated as a known native-kernel limit because the page cache serves buffered reads.
`statfs_free_after_unlink`, annotated as gated because the kernel sends `FORGET` asynchronously so reclaim is not visible in the same run.
The fourth is the same Node.js deprecation notice.
All three are pre-existing, identical on both heads, and the job still concludes success.
They are worth the coordinator's awareness as standing project annotations, not as anything these two PRs introduced.

## Heads, main, and issue #42 all as pinned

Verified unchanged from my prior rounds:

PR #139 head `5e89c3abde3cceebf7f930784fe7af3f45a7c166`, and `crates/cowfs-core/src/io.rs` at that head is still `f131185ce3c474b92748e3be192dea54e8e207b7`, the same object I reviewed.
PR #141 head `4b3daed988faf2dcd63d16a8c2b9aa6140b1d5a4`, with `crates/cowfs-core/tests/core_atomic_rename.rs` at `d40971829e646cd83427d293baac7c9f94a029d9`.
Main `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`, with the local `refs/remotes/origin/main` and the API agreeing exactly.
Issue #42 `open`, `state_reason` `reopened`.

PR state, one read:

| PR | Head | Base | Draft | State | Merge |
| --- | --- | --- | --- | --- | --- |
| #139 | `5e89c3a` | `b486d4541bc47b273a5bbd222b95c24fed05c36d` | false | OPEN | MERGEABLE / CLEAN |
| #141 | `4b3daed9` | `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0` | **true** | OPEN | MERGEABLE / CLEAN |

Both have an empty `closingIssuesReferences` list.
PR #141's draft state is the material one: a draft cannot be merged as it stands, and its green therefore covers the head as read, not a mergeable proposal.

## Combined integration, read-only, no checkout

Three `merge-tree --write-tree` runs, each exiting 0 with a single line of output and no conflict list.

| Inputs | Tree |
| --- | --- |
| `cf67e8a6` + `5e89c3a` | `12f97b3d82c44c4412b9192e2b6a0d97673351a0` |
| `cf67e8a6` + `4b3daed` | `bc6e81dd35490afccef3869b355b1b757ef1d633` |
| projected-139 merge + `4b3daed` | `6ff6b446a026277f6344d09e190e310f64d96594` |

`git merge-tree` refuses a tree as its first argument, so to get the third row I created virtual commit `a1350bf52ec40a15565674a5236e9029ba6550f5`, tree `12f97b3d…`, parent `cf67e8a6`, with the message `virtual: 139 projected merge, read-only audit, no ref`.
It is unreachable: `git for-each-ref --contains` lists no ref, and `HEAD` is still `main` at `93cfef94457a989d031cb6b0a475ac4edbdb85ef`.
No branch, tag or remote-tracking ref was moved or created, no index or working tree was touched, and the object is prunable by ordinary garbage collection.

The combined tree `6ff6b446…` carries both changes exactly:

| Property | Result |
| --- | --- |
| `clock_read` definitions in `io.rs` | 2, the two twins |
| `io.rs` identical to PR #139 head's | yes |
| `lib.rs` identical to PR #141 head's | yes |
| `queue.rs` identical to PR #141 head's | yes |
| `swap.rs` identical to PR #141 head's | yes |
| `core_atomic_rename.rs` identical to PR #141 head's | yes |
| `critic2b.rs` identical to PR #141 head's | yes |
| `caches.rs` identical to **main's** | yes |
| my published gap review present | `661ead7050c648d5` |
| the author's publication correction present | `5b33ed93092a5a6` |

The file-level evidence agrees with the merge result.
PR #139's diff touches `crates/cowfs-core/src/io.rs` plus seven documents.
PR #141's diff touches `crates/cowfs-core/src/lib.rs`, `crates/cowfs-core/src/queue.rs`, `crates/cowfs-core/src/swap.rs`, `crates/cowfs-core/tests/core_atomic_rename.rs`, `crates/cowfs-core/tests/critic2b.rs` plus three documents.
The intersection of the two file lists is **empty**.
Neither PR touches the other's production file, and neither touches `caches.rs`, so main's hole-flag `caches` variant survives the combination whole.

This is a statement about trees only.
I compiled nothing from any of the three merged trees and ran nothing in them, and I make no claim that they build or that their tests pass.
CI compiled and tested each branch integrated into main on `e2fbf6b` and `d6d37bb` respectively.
The one combination nobody has compiled is all three together, `6ff6b446…`, and given the empty file intersection and the two clean merge results I expect it to be fine, but that is an expectation and not a result.

## The closing-history hazard, unchanged and still live

I scanned every commit subject in both PR ranges for a line pairing a closing keyword with an issue reference.

PR #141 has none.
Its four subjects are `test(core): name the promotion case for the property it actually pins`, `docs(verification): record the #42 request 1 consumer integration against...`, `merge: bring main in so the reviewed tree is the one that ships`, and `fix(core): Core::rename_snapshot moves a name and keeps the snapshot`.
The keyword in the last is `fix`, and the line carries no issue number.

PR #139 has exactly one:
`e9dc1066a5601bde3bcca439d8df71e03c1972ea  fix(core): read a write's clock after it takes the node lock (issue #42 cache-write clock)`.
`fix` and `#42` share that line, so a merge subject that carries or the platform's own heuristics may act on it.

That history is published and must not be rewritten to edit a message.
The author's PR body now carries a coordinator warning about exactly this, notes that an empty `closing_issues_references` has already been observed in this repository alongside an issue that was shut anyway, and asks for the issue's state to be read from the API immediately after any merge and reopened if the platform shut it.
I concur.
Issue #42 is open and reopened as of this read, and the empty refs list on both PRs is not evidence that a merge would be safe on that front.

## What this does not claim

- **No local execution of any kind.** No cargo, build, archive, target, probe, test, or lint.
- These are CI's results. I read the logs; I did not produce them.
- The three merged trees, including `6ff6b446…`, have never been compiled by anyone I can see.
- PR #141's gate covers the head as read. It is a draft and its head may move, and its green must be re-read if it does.
- `linux-fuse` provides no `cowfs-core` evidence and was not used as a gate.
- The 4 ignores in `critic`, the 1 ignore in `critic2b`, the 10 or 13 bench skips and the 3 annotated FUSE warnings are all pre-existing and all reported as ignored or annotated, never as passes.
- The two interleaved `1 passed; 12 filtered out` repeats in the tail of each workspace log are a windowing artefact of that block, not a test outcome. Every window closes and the jobs conclude success.
- No global clock monotonicity. `Timestamp::now()` is the host wall clock and can step backwards.
- `op_setattr`, `op_setxattr` and `op_removexattr` remain untouched and unreproduced, as recorded in `661ead70` and unchanged by anything here.
- No performance, timing acceptance, threshold, or power-loss claim.
- The `bench/out` cap remains over budget and unapproved, so no lane can still execute locally.
- `no-mistakes` is uninitialized in this lane and was not initialized. Browser UNVERIFIED.
- Misakanet is local-only here and was not consulted.

## Scope discipline

Final CI gate evidence for PR #139 and PR #141 only.
No receipt rewritten, no production edit, no test edit, no commit, no push, no merge, no draft flag changed, no new issue, no new task, no readiness change.
No checkout, no reset, no stash, no branch change in either the MAIN checkout or the lease.
MAIN's dirty tree was left exactly as found: three tracked modifications belonging to other agents (`docs/v1-core.md`, `progress/index.html`, `progress/plan.json`) and their untracked documents, none of them mine, none of them touched.
The single repository-state mutation was creating the unreachable virtual commit `a1350bf5…` for the combined `merge-tree`, which moves no ref.
No lease acquired, returned, reset, pruned or destroyed.
No signal, restart, sudo, install, unmount, store operation or mount traversal.
All 32 leases, the shared daemon PID 15263 with start time `Sat Oct 3 20:44:29 2026`, the Linux host `9879298996041209860`, and every store, mount and job were untouched.
Other agents' work was not modified: READY1, READY3, READY5's `cowfs-meta` worker, READY5's Core critic, READY6, and the PR #139 and #141 authors.
My earlier reports remain immutable and unedited: `661ead70`, `2b04d305`, `a552af90`, `9341cc4f`, `471a0731`, `803ea5c5`, `ccc5eafc`, `4c3450f5`.
The author's records remain immutable: `5b33ed93`, `508698d0`, `75a2310b`, `b5e1f19c`, `72d37cc4`.

## Log provenance

Retrieved by me from the API into the owned directory, with digests:

| File | Bytes | sha256 |
| --- | --- | --- |
| `final-139-ubuntu-112048562707.log` | 366,370 | `2241ee5504aaba9925156f7c2ac9117692d678af4fa5f73a4231916d177df51d` |
| `final-139-macos-112048562687.log` | 354,843 | `ca1e9ef886fa6e63437e2a0f865134da2032a103c5f5bb233bb1189f79d012d9` |
| `final-139-fuse-112048562492.log` | 94,614 | `24d039bbd3f3f9846bf243a632ad26d6dd2b2cc5ab9d5549a0ff2b9d41ba631c` |
| `final-141-ubuntu-112047533370.log` | 367,593 | `d0d09751ee404ef783068f368574731f7b525cb6e5538e95e17f1bbfccda7bef` |
| `final-141-macos-112047533548.log` | 356,228 | `30535433548add9495ef6ba65924ce9664334c6f1637831bd436767075a647d3` |
| `final-141-fuse-112047533596.log` | 94,535 | `c70d569e6d101e780d618856d4897df871d573b33e9b97d98a2e1052868fa4f9` |

The two prior-round logs `c6c8cc723f08af6fcc770919922b61c7111718010ed53409f6591867b0d0a88c` (Ubuntu, 366,376 bytes) and `40757e399d86f70b5dfe2374aa03e37a443f9521cdefddb65b52abbc53512bd2` (macOS, 355,649 bytes) remain in place from run `37393266584`.
All eight are the raw API archives, whose digests differ by design from a `gh-axi run view --log` rendering because of timestamps and escaping.

## What remains

1. The coordinator's pinned merge decision for PR #139, carrying the `e9dc1066a560` subject-shape warning and an API read-back of issue #42 immediately after the merge, reopening it if the platform shut it.
2. PR #141 must leave draft before it can be merged, and its CI re-read if the head moves after that. Its base is already `cf67e8a6`, so it does not depend on PR #139 landing first.
3. Optionally, the combined tree `6ff6b446…` gets one CI run if the coordinator wants the both-merged-together configuration actually compiled rather than argued from an empty file intersection.
4. The `bench/out` budget decision, still over cap and still blocking every lane that wants to execute locally.
5. The two one-token corrections still outstanding in `5b33ed93` from `2b04d305`: the non-existent commit id `cd1dca8b7b5bd331e8e553e7874c0852458a1c15`, which should read `e9dc1066a5601bde3bcca439d8df71e03c1972ea`, and the missing second `operation_time` binary in the CI table. The second is now confirmed as 6 passed in addition to 4, on both platforms.

Nothing was fixed, merged, marked ready, drafted or closed.
Both PRs stay open, issue #42 stays open, and no new task was created.