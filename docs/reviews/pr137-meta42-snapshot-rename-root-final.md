# PR #137 root repair review: the source fix is correct and complete, the old red is real, and the runtime gate is still pending

Reviewer lane: `.treehouse-build-train/.treehouse/cowfs-7c1bf8/6/cowfs`, held slot 6.
Lease verified before any write: branch `review/gc-root-mark-retention-82`, HEAD `b4b55ab`, working tree showing only the six pre-existing untracked `docs/reviews/*.md` files from other lanes, no process of mine running.
Matched the expected state, so the lane was used.

Head reviewed: `c2afa0e12c09d7a46d0a604c5ef7493b81e30adb`.
Base reviewed: `93cfef94457a989d031cb6b0a475ac4edbdb85ef`.
Remote main, read through the API and not fetched: `b486d4541bc47b273a5bbd222b95c24fed05c36d`, which is the merge commit of PR #136, merged `2026-10-05T23:02:36Z`.
Red head the fix sits on top of: `9d1e5ef66d55798da08780296a05d6601468611f`.
Prior critic report this answers: `docs/reviews/pr137-meta42-snapshot-rename-final.md`, sha256 `471a07310e793016763a1f77e4e195bfeca89dcf1613d84fe50b238a6355b0ec`, preserved immutable and committed byte for byte on this head.

Review date: 2026-10-05.
Artifacts: `bench/out/meta42-snapshot-rename-root-final-critic/logs/**`, 0.28 MiB, two independently fetched CI logs.
This document: canonical PRIMARY copy, `docs/reviews/pr137-meta42-snapshot-rename-root-final.md`.

## Verdict

SCOPED SOURCE PASS on the metadata API, with the runtime gate PENDING.

I found no remaining defect in the rename path at this head.
The root fix is the correct pattern, it is applied in the one place that needed it, and I checked the stale-root variants around it rather than only the reported one.
My three earlier blocking findings are resolved: the source defect, the inert fixture, and the clippy failure.

The old red is real and I verified it from the actual logs rather than from the receipt.
The fix commit did not weaken the test that produced it: that test is byte-identical between the failing head and this one.

CI on `c2afa0e` had not completed when I read it, one snapshot and no polling.
No compiled, linted or executed result is claimed for this head by me.
The verdict is a source PASS with the runtime gate explicitly open, not a merge recommendation.

## Budget: read-only, and not waived

The lane's `bench/out`, where build artifacts belong under this project's rules, measures 35.881 GiB physical against the 8 GiB cap.
So this review is source and CI receipts only: no cargo invocation, no archive, no target directory, no private probe, no heavy job, no deletion, no pruning, no move, no offload.
The cache-cleanup approval referenced in the assignment is scoped to the READY5 lane and does not extend to this review, so nothing was deleted and no cap was waived.

The MAIN primary checkout's own `bench/out` is 4.8 GiB and is under the cap, and I did not treat that as authorization: documents go in the MAIN checkout, build output goes in the lease, and the gate is the lease's tree.

Both lanes' numbers stay on the record and neither is waived: 20.862 GiB in the author's lane per the root-repair receipt line 120, 35.881 GiB in mine.

## The old red, verified from the logs myself

Run `37376415288`, `head_sha` `9d1e5ef66d55798da08780296a05d6601468611f`, created `2026-10-05T21:31:34Z`, conclusion `failure`.
I fetched both failing job logs myself into my own artifact directory and read the failure in them.

`check (ubuntu-latest)`, job `111986390388`, started `21:31:45Z`, completed `21:39:08Z`, failure.
`check (macos-latest)`, job `111986390434`, started `21:31:42Z`, completed `21:39:57Z`, failure.
`linux-fuse`, job `111986389988`, started `21:31:40Z`, completed `21:36:40Z`, success.

Both failing logs carry the same failure:

```
test a_rename_commits_a_pending_tree_change_and_leaves_the_row_readable ... FAILED
thread 'a_rename_commits_a_pending_tree_change_and_leaves_the_row_readable' panicked
  at crates/cowfs-meta/tests/snapshot_rename.rs:372:5:
assertion `left != right` failed: the committed row must carry the flushed root, not the pre-flush one
test result: FAILED. 7 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
```

That is my Finding 1 reproduced as a real red, on two platforms, on the code that contained it: the durable row carried the pre-flush root.
My copies are raw API archives with timestamps and escape sequences, so their sha256 values differ from the author's post-strip copies by design; `1952408994d7a286156a293178c88e0c82eff9b266875c3741ebf23a1e2d4661` for Ubuntu and `bddc3e6904484ed1ac0a523a7f00aa14db8804e6fb738b2da7e01e1be24c4c51` for macOS.

What the red does not prove, and I say so independently of the receipt:
the assertion at `372:5` aborts before the lookup and before `check()`, so no missing-node error and no `check()` failure was observed in either log.
Those remain source predictions.
The receipt at lines 44 to 47 states the same limit and I agree with it rather than crediting it as new.

## The fix, and every variant around it

`crates/cowfs-meta/src/db.rs`, from `924c16b` to `c2afa0e`, is seven added lines and nothing else:

```rust
let e = s.snaps.get(id).ok_or(Error::NoSuchSnapshot)?;
// The dirty flush above may have moved the root before session publication.
let root = new_roots
    .iter()
    .find(|(i, _)| i == id)
    .map(|(_, r)| *r)
    .unwrap_or(e.info.root);
let mut info = e.info.clone();
info.root = root;
info.name = (*name).to_string();
snaps.insert(id.0, encode_snap(&info).as_slice())?;
names.remove(e.info.name.as_str())?;
names.insert(*name, id.0)?;
```

Line numbers below are this head.

The ordering it depends on is right: the flush loop at `db.rs:531` to `546` runs and fills `new_roots` before the `match &extra` block opens at `547`, and `new_roots` is declared at `519` inside the `guard` closure, so it is rebuilt on every commit and cannot carry an entry from an earlier one.
The lookup is by exact `SnapshotId` equality, so no two snapshots can alias each other's root.

Variants I checked, since one is not enough:

- **Rename target dirty.** `new_roots` holds `(id, R_new)`, so the row gets `R_new`, which is the same value the loop wrote at `db.rs:543`. `settle()` at `619` then drops `R_old`, and no row points at it. Correct.
- **Rename target clean.** No entry in `new_roots`, so the fallback at `610` yields `e.info.root`, which is already the durable row's value. The clean case is bit-for-bit the old behaviour.
- **Rename target dirty but the root is unchanged.** The loop skips its own insert at `536` when `root == e.info.root` and still pushes the pair, so the lookup returns that same value. No refcount delta, no row change. Correct.
- **Other snapshots dirty in the same commit.** The loop writes their rows with their own new roots at `543`; the rename arm only writes the target's key; `db.rs:637` to `642` brings every session entry's root forward and resets its tree. A dirty neighbour is untouched by the rename and correctly updated by the flush.
- **Forked shared old root.** `refs[R_old]` is above one, so `drop_ref` only decrements and the old tree is not freed. The fork's row keeps `R_old` and the renamed row moves to `R_new`; both agree with the counts.
- **Half names.** Still one write transaction opened at `517` with the only commit at `627`, so `db.rs:614`, `615` and `616` land together or not at all.
- **Session and table agreeing afterwards.** `db.rs:637` sets `info.root` and resets the tree; `db.rs:667` to `678` sets the new name after the commit. The session no longer diverges from the row, which was the second half of my original finding.

`SnapshotInfo` fields: the id is the table key, and `created` and `parent` come from the session entry unchanged, so a rename cannot move the id, the creation time, the parent or the generation. Only `root` and `name` are written, which is what the API contract says.
The inode reservation floor, the reap queue and `next_snapshot` are untouched: the arm writes only `SNAPSHOTS` and `SNAP_NAMES`, and `db.rs:625` is the existing unconditional `ino_reserved` write that every commit makes.

Refusals, identical-name semantics and the hook are untouched by this delta and still correct at this head: the destination check at `db.rs:497` to `500`, the missing id at `492` to `494`, the identical-name `Ok(None)` at `505` to `506` which returns before `run_hook` at `511`, and `run_hook` before `begin_write` at `517` so `before_sync` remains strictly pre-publication.
The receipt keeps declining any universal post-commit rollback claim and that remains the right claim to make.

## The test that produced the red was not weakened

`a_rename_commits_a_pending_tree_change_and_leaves_the_row_readable` is byte-identical between the failing head `9d1e5ef` and this head `c2afa0e`: 74 lines, sha256 prefix `09122d93f3918541800a2cf6` in both.
So the fixture, the assertion, its operands and its message are the same ones the red was pinned to, and the fix was not made to pass a weaker test.

The only edit to existing test lines is at `snapshot_rename.rs:583`, one `assert_eq!` collapsed by rustfmt, 211 lines after the pinned assertion, plus a one-line shortening of the `db.rs` comment.
The `new_roots` lookup, the `.map`, the `.unwrap_or(e.info.root)` fallback, `info.root = root` and `info.name` are untouched by the formatting commit, which is what the format-correction receipt claims and what the diff confirms.

The fixture split is the right repair for my Finding 2: `opts_pending()` at line 47 leaves `sync_every_ops` at the default 256 with `Ack::Applied` and `background: false`, and the doc comment at lines 44 to 46 explains in the source why the pending case cannot share `opts()`.
That is the same mechanism I traced, stated in the file.

## The two new cases are real discriminators

Both use `opts_pending()`, so both exercise a genuinely dirty tree.

`a_rename_keeps_real_file_bytes_readable_after_a_drop_and_reopen`, line 498.
512 KiB of deterministic irregular bytes go through a real `cowfs_store::Store` via `ingest_bytes`, and the refs are attached through the public `Tx::set_content`, so the tree points at real blocks.
Because `MAX_CHUNK_LEN` is 256 KiB at `crates/cowfs-store/src/chunk.rs:8`, a 512 KiB body cannot be one block, so the readback is genuinely multi-block; the `chunks.len() > 1` assertion at line 520 is a weaker bound than the structural guarantee but not a vacuous one.
After the rename, a drop of everything and a reopen, it asserts the chunk count is unchanged, that the chunk lengths still cover the body exactly, that no id is the empty-block placeholder, that every block is still in the store via `store.contains`, that each block is the same `BlockId` and not a rewritten one, and finally that concatenating `store.get` over the refs reproduces the body byte for byte, then asserts `check()`.

I verified the APIs it depends on exist and are public rather than taking the receipt's word: `Store::open` at `cowfs-store/src/store.rs:334`, `ingest_bytes` at `1386`, `contains` at `1290`, `get` at `1224`, `sync` at `1295`, `BlockId::of` at `lib.rs:39`, `ChunkRef` with public `id` and `len` at `lib.rs:68`, `Tx::set_content` at `cowfs-meta/src/tx.rs:444`, `Snapshot::chunks` at `db.rs:1748` and `Meta::check` at `db.rs:1596`.
`cowfs-store` is already a normal dependency of `cowfs-meta`, so no manifest change is needed and the delta contains none.
This case is the one my prior review said was owed, and it is written against real bytes rather than a metadata fingerprint.

`a_rename_of_a_dirty_snapshot_keeps_a_forked_old_root_live_and_the_renamed_one_fresh`, line 606.
It uses the existing `Snapshot::fork` seam at `db.rs:1699`, asserts the fork starts on the base root, dirties only the base, renames the base, and then after a drop and reopen asserts three things: the fork still resolves the old root, still has its own content, and does **not** gain the write that was pending on the base; the renamed base does carry that pending write; and `check()` passes.

That is exactly the variant my prior review called untested, and it is the interesting one: with a shared root the transaction only decrements, so a stale row would show up as stale content rather than as a missing node.
The assertion at line 658 to `661` is the discriminator, and on the old code it would fail because the renamed base's row would still be on the pre-flush root, which does not contain the pending entry.
No assertion was removed, weakened or retargeted in either case.

## My own check, and its exact limit

Standalone formatter only, no cargo and no build:

```
rustfmt --edition 2021 --check  crates/cowfs-meta/src/db.rs                  -> exit 0, zero diff
rustfmt --edition 2021 --check  crates/cowfs-meta/tests/snapshot_rename.rs  -> exit 0, zero diff
```

Both read from the head's blobs through `git cat-file`, at the workspace edition `2021` declared in the root `Cargo.toml` line 8, with the installed `rustfmt 1.10.0-stable (b940084d7e 2026-09-28)`.
That is a real formatter exit code on the two owned files, and it is the whole of what it proves: rustfmt is not a compiler, a linter or a test runner.
CI installs its own stable toolchain, so a formatting difference there would be toolchain skew rather than a claim failure.

I also scanned the final test file statically for the class of lint that produced the earlier clippy failure, an unused `let` binding: every tuple binding is referenced at least once in its own test, and the only single-use binding is a `_` placeholder, which does not warn.
That is a source scan, not a clippy run, and I do not claim clippy passes.

## CI on this head: one snapshot, nothing completed

`headRefOid` `c2afa0e12c09d7a46d0a604c5ef7493b81e30adb`.

| Check | Status | Conclusion |
| --- | --- | --- |
| `check (ubuntu-latest)` | IN_PROGRESS | null |
| `check (macos-latest)` | IN_PROGRESS | null |
| `linux-fuse` | IN_PROGRESS | null |

All three started `2026-10-05T23:11:24Z` or later and none had completed.
`mergeStateStatus` is `UNSTABLE`.
Nothing was polled, rerun, dispatched or reconfigured, and no workflow or runner setting was touched.
So the compiled, linted and executed status of this head is unknown to me, and this review cannot stand in for that read.

Other PR state, same snapshot: `baseRefOid` `93cfef94457a989d031cb6b0a475ac4edbdb85ef`, `state` OPEN, `isDraft` true, `mergeable` MERGEABLE, `reviewDecision` null, `closingIssuesReferences` empty.
Issue #42 is `open` with `state_reason` `reopened`, so the empty reference list is not trusted on its own, exactly as the format-correction receipt line 98 says.

## Merge position, and why main has moved under it

`git merge-tree --write-tree 93cfef9 c2afa0e` returns a single tree id `0991a63aabb50d8d0bfaa04fdf9e81c99af2d47c` and no conflict list: the PR merges cleanly into its stated base.
No checkout and no working tree change were involved.

Remote main is now `b486d454`, the #136 merge, so the PR's base is behind main and merging will be a merge rather than a fast-forward.
I checked overlap at file level instead of fetching: #136 touched `crates/cowfs-core/src/inner.rs`, `crates/cowfs-core/tests/operation_time.rs`, `crates/cowfs-meta/src/tx.rs`, `crates/cowfs-meta/tests/operation_time.rs`, `docs/v1-core.md` and its own evidence and review documents.
None of those is `crates/cowfs-meta/src/db.rs` or `crates/cowfs-meta/tests/snapshot_rename.rs`, so the two changes do not touch the same files.
I did not fetch `b486`, so I make no merge-tree claim against it.

## Source carry and commit history

The whole delta from base to head is seven paths.
Production code is `crates/cowfs-meta/src/db.rs` at 93 insertions and no deletions, which is the original API plus the seven-line root fix and nothing else.
No `Cargo.toml` and no `Cargo.lock` line moves, so no dependency version changed and no schema changed.
No `cowfs-core`, `cowfs-store`, `cowfs-vfs` or `cowfs-meta/src/tx.rs` change, so there is still no consumer of the new API and no behaviour change outside `cowfs-meta`.

Blob shas at `c2afa0e`:

| Path | Blob at `c2afa0e` | Blob at `924c16b` | Blob at base |
| --- | --- | --- | --- |
| `crates/cowfs-meta/src/db.rs` | `64190d30f47fb438bf37005d7488e3e0886c6227`, sha256 `da41c8e70c4b7a7e7f9116b11f6476acc21f6d92216f863e5416dd9212843cae` | `22a6015ed311e1eedff08e3475e89c6790a21a1b` | `b08ed7f7e204a60625124b13415f471ec82800b8` |
| `crates/cowfs-meta/tests/snapshot_rename.rs` | `7d00efcb5f5c0d6377027dfbd5f1bb512cf1c1c2`, sha256 `ded55c9fd5d592f6f499128b1b15fd7cc7b5d140c22aafe0f18b37c8b530d156`, 670 lines, ten tests | `8ef1fcf4ab92ed0fe9f12ba9c1ffa89871e699d9` | absent, new file |
| `docs/verification/evidence/meta42-snapshot-rename-root-repair.md` | sha256 `91aeec0e68d2c2b85daef8a1e61c70afb8de6642a9a12570a65ac6e45848f3da` |
| `docs/verification/evidence/meta42-snapshot-rename-format-correction.md` | sha256 `9a8fb4efdb9acc1c97356a1a50fb4a214da21d7e7d1e54da03d477c58b756f7a` |
| `docs/verification/evidence/meta42-snapshot-rename-regression-correction.md` | sha256 `a390aaf4b84bcd6a36117b854faa54710543e4c32c20e868185ff08a390be811` |
| `docs/verification/evidence/meta42-snapshot-rename.md` | sha256 `93c4b1317b7d9445ced3f7d1316e159b3cca472cbee6172c0a81e2b9a8883c0b` |
| `docs/reviews/pr137-meta42-snapshot-rename-final.md` | sha256 `471a07310e793016763a1f77e4e195bfeca89dcf1613d84fe50b238a6355b0ec` |

Every one of those six documents is byte-exact between the MAIN primary checkout and the blob inside the head, verified by comparing `shasum -a 256` on the file against `git cat-file -p` piped to the same digest.
The retracted counts in `a390aaf4` stay retracted and the receipt that carried them is preserved unedited.
My own prior report `471a0731` is committed on this branch unmodified.

Commit range from base, oldest first: `bf37fdf` the API, `924c16b` the original receipt, `9d1e5ef` the fixture and clippy correction, `c66befa` the root fix, `c2afa0e` the formatting correction.

## Closing-keyword hazard, reported and not rewritten

The PR body and title are clean.
The title ends `(#42)` with no keyword in front of it, and the body's only reference form is `Refs #42, request 1.`, which is not a closing form.
`closingIssuesReferences` is empty, which is the authoritative signal for the body as it stands.

The hazard is in the commit messages of the new range, and I flag it because it is prospective rather than current:

- `9d1e5ef` subject: `test(meta): fix the #42 rename regression fixture and the clippy lint (#42)`.
  `fix` and `#42` are on the same line, with `(#42)` trailing.
- `c66befa` subject: `fix(meta): resolve a renamed snapshot's root through new_roots (#42)`.
  Same shape, with `fix(meta):` immediately after the keyword.

Neither is a merge commit, and GitHub only derives closing references from commit messages pushed to the default branch, so nothing is closing today.
The risk is that whoever squash-merges pastes one of these subjects into the merge message.
`9d1e5ef` is the sharper of the two because `fix` sits directly in front of a `#42` reference.
Rewriting published history is the wrong fix and I am not asking for it; the report is the deliverable.

The other keyword-adjacent mention is in `meta42-snapshot-rename-format-correction.md:98`, which describes an earlier commit that closed #42 through a negated phrase.
That is documentation prose in a file, not a PR body or a commit subject, so it cannot close anything, and #42 is `reopened` with `state_reason` `reopened` as the API confirms.

## Limitations, stated plainly

- **No compiled, linted or executed result is claimed for `c2afa0e`.** CI had not completed when I read it. This is a source PASS with the runtime gate open.
- The missing-node and `check()` consequences past line 372 are still unobserved in any run, on this head or the red one.
- My only executed checks are the two `rustfmt --check` invocations and the reading of CI logs; the unused-binding result is a static scan, not a clippy run.
- The variant analysis above is source reasoning. It is derived from the exact chain and it is checkable, but I did not execute any of it.
- I did not fetch remote main, so there is no merge-tree result against `b486d454`; the overlap check is at file level only.
- The byte-readback case depends on `store.get` returning whole blocks and the concatenation reproducing the body exactly. That is unexecuted and is owed to CI.
- Budget remains blocked for this lane, so nothing heavier can be added without a coordinator decision.
- `no-mistakes` is uninitialized in this lane and was not initialized. Browser unverified.
- Misakanet is local-only here and was not consulted; no local memory store was reachable in this lane.

## Scope discipline

Issue #42, request 1 only: the metadata rename API and the root fix for it.
No new issue, no new feature, no audit matrix, no new task.
Issues #125, #127 and #128 remain parked and untouched.
No production edit, no test edit, no source patch by me anywhere.
No checkout, branch change or commit in the lease.
No lease acquired, returned, reset, stashed, pruned or destroyed.
No signal, restart, sudo, install, unmount or store operation.
Nothing deleted: `471a0731`, `803ea5c5`, `ccc5eafc` and `4c3450f5` and all their artifact trees are intact, as is every other lane's cache and the `ready-40` tree the author recorded as protected.
The shared daemon PID 15263 with start time `Sat Oct 3 20:44:29 2026`, the Linux host `9879298996041209860`, and every store, mount and job were never contacted, and no mount was traversed.
Concurrent lanes untouched: READY3's PR #138 hole-flag critic, READY1's PR #139 cache critic, READY5 idle at `c2afa0e`, READY6 idle at `ad6`.
MAIN checkout modifications belonging to other lanes, `docs/v1-core.md` and `progress/`, were not read for content and not touched.
Files I own for this review: this document and the two logs under `bench/out/meta42-snapshot-rename-root-final-critic/logs/`, which is gitignored by `.gitignore:10`.

## What remains before this is a full PASS

1. Green CI on `c2afa0e` on all three checks, which has to show the ten tests compiling, clippy clean under `-D warnings`, and the pinned red turned into a pass.
2. The byte-readback and forked-shared-root cases executing rather than being owed.
3. A merge decision against the new main `b486d454`, since the base is behind it.

Nothing was fixed, merged, marked ready or closed.
PR #137 stays a draft, issue #42 stays open, and no new task was created.