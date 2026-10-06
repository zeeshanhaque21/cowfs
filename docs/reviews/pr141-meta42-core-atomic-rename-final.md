# PR #141 review: SCOPED PASS on the source, with one real weakness found in the new tests and the runtime gate still open

Reviewer lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, held slot 5, READY5.
Reviewer is not the author. The author worked a different held slot on branch `fix/meta-atomic-rename-consumer`; nothing of another lane was edited, reverted or checked out.

Head reviewed: `c220cc37e4ab71d9c048b49e0856d9672be6d633`.
Main reviewed and merged in: `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`, which is the PR #137 merge commit and the current tip of `main`.
Merge base of the two is `cf67e8a6` itself: `git merge-base --is-ancestor audit/main audit/c220` returns true, so `main` really is an ancestor and the reviewed tree is the merged tree, not a rebase.
`git merge-tree --write-tree` on the two refs exits 0 with tree `5dda52476386a44d40c4ed5a7973235aef3c4459`, so a merge into `main` is clean. Both refs were fetched explicitly by name into `refs/remotes/audit/`, so nothing here rests on a `FETCH_HEAD` assumption.

Author report: `docs/verification/evidence/meta42-core-atomic-rename.md`, full sha256 `b9290d6e8949baafd363f4a2f7369607e6070863aa88170e7923be17d2d81479`, 17912 bytes, in the MAIN primary checkout. I derived that digest myself rather than trusting the quoted prefix, and the prefix matches. That document is left untouched.

Review date: 2026-10-06.
This document: canonical PRIMARY copy, `docs/reviews/pr141-meta42-core-atomic-rename-final.md`.

## Verdict

**SCOPED PASS on the source, conditional on CI.**

The consumer change is the right shape, it reuses the metadata rename exactly as intended, it preserves the identity and refusal contracts, and the ownership deviation the author declared is correct and necessary rather than opportunistic.
One weakness is real and is recorded below: two of the new tests assert less than their names promise, and one of them is the test that carries the reopen-and-bytes obligation.

**The runtime gate is open.** CI run `37393711065` on the head is `in_progress`, so nothing here is a pass claim about execution.
No test was run in this lane: local cargo is blocked because this lane's `bench/out` is 20.863 GiB against an 8 GiB cap with an 11.839 GiB floor, so all runtime evidence below is the author's, cited, and this review's contribution is source analysis plus the actual CI record.

## What the change actually is

`Core::rename_snapshot` at `crates/cowfs-core/src/lib.rs:352-382` no longer calls the swap. It resolves the source, validates the target name, refuses the same-name case, checks the target against the registry with the old name excluded, calls `self.inner.meta.rename_snapshot(id, new)`, reads the info back, and only then updates the live registry:

```rust
let sc = self.inner.snap_by_name(old)?;
validate_snapshot_name(new)?;
if old == new {
    return Err(ControlError::InvalidName(
        "source and target are the same snapshot",
    ));
}
// the name this snapshot is giving up cannot collide with itself
self.inner.check_new_name_except(new, Some(old))?;
let id = SnapshotId(sc.id);
self.inner
    .meta
    .rename_snapshot(id, new)
    .map_err(control_meta)?;
```

That is what request 1 asked for: a name move that keeps the snapshot's identity, not a promotion-style replacement. `promote_base` still goes through `swap_snapshot`, and the swap protocol, its intent records, its recovery and its step-1-to-5 failure matrix are untouched.

## Property by property, from the source

**Snapshot identity preserved.** The metadata call takes a `SnapshotId` and moves only the name; `crates/cowfs-meta/src/db.rs:1552` in the merged tree is the reviewed `Meta::rename_snapshot`, unchanged by this PR, and `git diff main..head -- crates/cowfs-meta/` is **empty**. Nothing in the consumer re-creates a snapshot, so id, root and inode numbers survive by construction rather than by test luck.

**Refusal ordering preserved.** `snap_by_name(old)` runs first, so a missing source is reported before an invalid target name, and `check_new_name_except` is the case-folding `name_key` rule at `lib.rs:703-720`, unchanged, with the old name excluded so a snapshot cannot collide with itself. The staging-name refusal is still inside it.

**Same-name contract preserved.** The explicit `old == new` guard returns `InvalidName`. Note this is a refusal, not the metadata layer's no-op: `Meta::rename_snapshot` treats a same-name rename as a no-op, so without this guard Core would have quietly succeeded where it used to fail. The author kept the Core-side contract rather than inheriting the meta one, which is the correct choice.

**Registry agreement before and after commit.** Every fallible step is before the registry write: the metadata rename, the `snapshot_by_id`, and `.info()`. After that the registry is brought in line under `snaps.wr()`, then `root_time` is refreshed so the synthetic root's mtime changes. Nothing fallible remains that could leave the registry describing a name metadata has already given up, except `entry(&info)` on the last line, which is examined below.

**One genuine ordering weakness, non-blocking.** `entry(&info)` is called *after* the registry update and can return `Err` from `pack(info.id.0, ROOT_INO)`. That would mean metadata and the registry both hold the new name while the caller sees an error. It is not reachable in practice, because `pack` fails only when the id exceeds the packed layout's field width and `info.id` is an id that already existed in the registry, so it packed once already. Worth naming because it is the only step after the point of no return, not because it is a live bug.

**Durability stated accurately.** The author does not claim a universal rollback. The fault test `a_rename_that_fails_at_the_commit_changes_nothing` arms the existing `Core::open_with_meta` `before_sync` hook rather than adding fault API, and asserts the failure surfaces as a control error and the old name still works after the fault is disarmed. That is a pre-commit failure guarantee, and the receipt describes it as such.

**Virtual inode and alias policy untouched.** `git diff main..head` over `crates/cowfs-meta/`, `crates/cowfs-store/`, `crates/cowfs-fuse/` and `crates/cowfs-core/src/io.rs` is **0 files**. `crate::ino::pack` and the durable `virt.ino` reservation still own every number, and a rename no longer reaches them, which is exactly why the numbers now survive. No allocator transition is attempted and none is implied.

**Promote path unchanged in behaviour.** `swap_snapshot` lost only its `rename_from: Option<&str>` parameter and its now-false documentation; the single caller passes `None`'s absence, and `swap.rs`'s recovery, staging-name reservation and intent handling are untouched. Module docs were corrected to stop claiming meta has no atomic rename, which was true before #137 and false now.

**Merged tree preserves the neighbouring merges.** `set_now` is present in `cowfs-meta/src/tx.rs` (#136 timestamps), 32 hole references are present in `cowfs-store/src/lib.rs` (#138), and the reviewed `Meta::rename_snapshot` is present. Total diff against `main` is 6 files, 721 insertions, 47 deletions.

## The ownership deviation, audited

The author removed the duplicated `name: String` from `SnapCtx` and made the registry the single authority. That is the correct call, and it was necessary rather than cosmetic.

The deviation is sound because of what it eliminates. On `main`, `Inner::health` built its `LaneHealth` from `sc.name.clone()` read straight off the `SnapCtx`, with no lock at all, and that field was a plain `String` written once at construction. Once a rename can move a name, a lock-free read of a cached name is unsound: `health` could report a lane under the old name after the rename committed, and there was no lock that made the pair of reads consistent. With the field gone, `health` at `lib.rs:853-856` snapshots the names from the registry under `self.snaps.rd()` before iterating lanes, so name and id come from one locked read and a concurrent rename either lands before the snapshot or after it.

Importers audited, all of them:

- `SnapCtx::new` has exactly **one** caller at head, `Inner::add_snap` at `lib.rs:673`, which on `main` passed `info.name.clone()` and now passes nothing. No other construction site exists.
- No reader of the removed field survives: `git grep 'sc\.name\|ctx\.name'` over `crates/cowfs-core/` at head returns **nothing**.
- `unregister` at `lib.rs:746-749` dropped `s.by_name.remove(&sc.name)` and became `s.by_name.retain(|_, id| *id != sc.id)`, removing by id rather than by a name it can no longer read. That is strictly more robust: a snapshot removed under a name that has since moved is still removed from the index.
- `by_name` now has exactly four write sites, all under `self.snaps.wr()`: the rename at `377-378`, `add_snap` at `676`, and the `unregister` retain at `749`. There is no unlocked writer anywhere.
- Readers are consistent: `snap_by_name_raw` at `690-691`, `check_new_name_except` at `710-711`, `root_readdir` in `ns.rs:143-149`, `root_lookup` in `ns.rs:78-80`, `health` at `854-855`, and the alias seam at `854`. Every one takes `snaps.rd()` and drops it before doing anything else.

Lock order in the rename path is `meta` first, then `snaps.wr()`, then `root_time`, each released before the next is taken, with `drop(s)` before `root_time` at line 379. No path holds `snaps` while waiting for meta's writer lock, which is the liveness property `snapshot_lock_probe` exists to check, and that probe is unchanged.

Root namespace agreement: `root_readdir` reads `by_name` under `rd()`, so a renamed snapshot appears under the new name and the old name disappears, both from the same locked read that `root_lookup` uses. There is no separate dent cache for snapshot names to go stale, and no `ino` reference is invalidated because no inode is created or dropped.

I approve this seam specifically. It is the minimum needed to make the rename sound, it is confined to the name-index lifecycle, and it is not a general refactor: no unrelated function was restructured, and the `by_name` readers other than `health` are unchanged from `main`.

## The retargeted critic tests, and the two lost obligations

Two `critic2b` cases asserted that a rename goes through the swap protocol, which is no longer true, so both were retargeted to `promote_base` rather than deleted. The properties each now pins:

`a_refused_rename_leaves_the_mount_exactly_as_it_was` became `a_refused_promotion_leaves_the_mount_exactly_as_it_was`: a promotion refused because the victim snapshot has an open handle leaves the name set unchanged, both snapshots' content intact, no `swap-old` intent file, and the next open does not complete it.

`a_step_three_refusal_removes_the_intent_and_staging_snapshot` now drives `promote_base` through `set_swap_fault(3)`, asserts no `swap-base` intent survives, and that hidden staging state is gone.

That is the right retarget, and the promotion step-1-to-5 failure matrix is untouched, so it still runs twice.

**But retargeting a test to green is not proof, so here is the audit of what the rename-specific tests now carry.** The new file `crates/cowfs-core/tests/core_atomic_rename.rs` has 10 cases on the public surface only, and I checked each obligation the retargeted tests used to own:

| obligation | now covered by |
|---|---|
| refusal leaves no name changed, no leftover staging | `a_refused_rename_changes_no_name_and_leaves_no_leftovers` |
| missing source reported before invalid target | `a_missing_source_is_reported_before_an_invalid_target_name` |
| pending dirty write survives the rename | `a_dirty_write_before_the_rename_is_visible_after_it` |
| hook failure at commit changes nothing | `a_rename_that_fails_at_the_commit_changes_nothing` |
| open handle stays usable | `a_handle_open_across_the_rename_still_reads_and_writes` |
| packed inode identity | `a_rename_keeps_the_id_the_root_and_the_file_numbers` |
| reopen identity and bytes | `the_stored_bytes_survive_a_drop_and_a_reopen_under_the_same_id` |
| other snapshots untouched | `a_rename_moves_one_name_and_changes_no_other_snapshot` |

Nothing was dropped. I looked specifically for an assertion weakened into a skip and found none in these cases: the identity assertions are real `assert_eq!` on the id, the packed root inode and the file inode, and the refusal case asserts a real `ControlError::InvalidName`.

## Two weak tests, found and named

This is the part I would change, and it is why the pass is scoped.

**1. `promote_base_still_replaces_its_target` is not really testing what its name says.** The author corrected two of its own fixture assertions, and I verified both corrections are right: promotion never promised the replaced target's own file would survive, and comparing `SnapshotEntry::ino` against the virtual alias a view hands out was comparing two different numbering schemes. Both were genuine bugs in the fixture, not in the code.

However, the resulting test asserts that promotion still replaces its target, and the author's own RED table records this case as **FAIL on the old consumer and pass on the patched one**. A promotion test should not be sensitive to the rename change at all. Either it is incidentally sensitive through shared setup, or the case is doing double duty. Either way its name over-promises, and a reader will assume it pins promotion semantics when what it mostly pins is that the shared fixture still works. It should be renamed to what it actually asserts, or its promotion assertion should be made specific enough that the old consumer would also pass it.

**2. `the_stored_bytes_survive_a_drop_and_a_reopen_under_the_same_id` inverts its own alias obligation, and the inversion is correct.** The fixture previously asserted a fresh session *reuses* an earlier session's virtual inode number, which is exactly what the durable reservation exists to prevent. The author fixed it to `assert_ne!` with the message "a new session must not reuse an earlier session's virtual inode".

That is the right contract and I confirm it against the source: meta's allocator starts `next` at the durable `reserved` floor, so a fresh session cannot reissue a number below a committed floor, and `virt.ino` is untouched by this PR. So a virtual inode number differing across a reopen is expected, and asserting equality would have been asserting a bug.

But the consequence is that the test resolves the file **by name** after the reopen rather than by the number captured before it, and then compares the bytes. So it pins "same id, same packed root inode, file still reachable under the new name, exact bytes", which is the right property for *this* change. It does **not** pin that a virtual inode number is stable across a restart, and it should not, because nothing promises that. The report and the fixture comment both say so plainly, and I checked that the comment at lines 226-228 states the reasoning rather than implying stability.

I am recording this as a weakness rather than a defect because the assertion is correct and honestly labelled. The risk is a future reader treating `assert_ne!` as an incidental detail rather than a deliberate statement about the alias policy.

## The 9 versus 10 discrepancy, labelled rather than assumed

The brief flagged that the old-consumer count and the final fixture case count disagree.
They are reconciled by the author's own table, and the reconciliation is checkable rather than assumed: the tenth case, `a_rename_that_fails_at_the_commit_changes_nothing`, is marked "added with the change". So the old consumer ran the same source **with 9 cases**, giving 2 passed and 7 failed, and the final source has **10**. The two that pass on both are the refusal and the error-ordering case, which is the intended point: the existing contract is unchanged.

**Source binding is asserted, not proven by me.** The author reports the old-consumer run at `b5e6f78` with the same fixture source, and I have not executed anything, so I cannot independently confirm that the file compiled at `b5e6f78` was byte-identical to the committed one.
What I can confirm is that `b5e6f78`'s own CI run `37389525793` completed `success`, so that tree built and tested green in its own right, and that the reported failures are identity assertions rather than compile or refusal failures, which is consistent with one source against two consumers.
The reported RED values are concrete and falsifiable: a rename of snapshot id `1` returned an entry with id `3`, and across a reopen the packed root inode moved from `1099511627777` to `1099511627778`. Both are what the swap-based implementation must do, since it forks twice and repacks.

On executable identity, per the standing rule: **no filename is used as a content hash anywhere in this review.** Cargo artifact names such as `core_atomic_rename-<hex>` are per-unit metadata and prove nothing about source identity; I corrected a body claim of that shape on PR #137 for exactly this reason. The author's report is likewise not credited with filename-as-hash, and its bindings are by full commit sha and by tree contents.

## Actual CI, at the time of writing

One run for the head, and it is not finished:

| run | head | status | conclusion | created |
|---|---|---|---|---|
| `37393711065` | `c220cc37` | `in_progress` | none yet | `2026-10-06T00:23:02Z` |

For comparison, the old consumer base `b5e6f78` has run `37389525793` `completed` `success`, which is that tree's own green, not evidence about this change.

So the merged-tree gates are **pending**, and I am not recording the author's eighteen gate results as mine.
If this run goes red, the attribution is checkable: a compile or `fmt`/`clippy` failure is this change's, since it edits Core source and tests; a test failure in `core_atomic_rename` or `critic2b` is this change's; anything in `cowfs-meta`, `cowfs-store` or the FUSE suite would have to be examined against `main`, which is green.

## What I did not do

- No local cargo, build, test, clippy, archive, target directory, probe, offload, cleanup, prune, move or cap waiver. `bench/out` is untouched.
- I did not run the author's prebuilt binaries. Source and the CI record are sufficient for a source review, and running them would need budget permission I do not have.
- No source or test edit, no commit, no push, no new issue, no task, no merge.
- No checkout or branch change in my own lane: `fix/meta-inode-reservation-42` is still at `f5f7bbc8af72e1ffd257e87c3193a7fe0ebe8b9e`, clean, matching its remote.
- The author's report `b9290d6e…` and my earlier receipts `4ef55a3c…`, `a552af90…`, `9341cc4f…`, `91aeec0e…`, `b66ea61b…` are all untouched.
- No workflow, runner, dispatch, rerun or trigger change. One CI snapshot only, no polling.
- Browser unverified, so no images are linked or claimed. `no-mistakes` is uninitialized in this lane. Misakanet is local-only here and was not consulted.

## Conditions for merge

1. CI run on `c220cc3` green on all three jobs, with `cowfs-core --test core_atomic_rename` actually executing 10 cases.
2. Either rename `promote_base_still_replaces_its_target` to what it asserts, or make its promotion assertion specific enough to pass on the old consumer too. A test whose sensitivity to this change is unexplained should not gate a merge as-is.
3. The two weak-test notes stay recorded, particularly that the alias `assert_ne!` is a deliberate statement about the durable-reservation contract rather than an incidental detail.

None of these requires a code change to the consumer itself, which is the part I am satisfied with.