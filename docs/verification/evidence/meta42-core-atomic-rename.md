# Issue #42 request 1, the consumer side: `Core::rename_snapshot` moves a name atomically

Scope: the `Core` consumer integration for request 1.
`cowfs-meta`'s `Meta::rename_snapshot` is a dependency and is not touched here.
Issue #42 stays open; requests 2, 3, 4 and 5 are untouched by this change.

| what | value |
|---|---|
| branch | `fix/core-atomic-snapshot-rename-42`, cut from the reviewed dependency head |
| dependency, and first base of this branch | `b5e6f785eeee62f4a60401cf3ebbd113693d3872` (`fix/meta-snapshot-rename-42`, PR #137) |
| head | `f4ce53ac29b43674b4349455d956eb7f7bd0f719`, `main` merged in, see "The merge" |
| PR | https://github.com/zeeshanhaque21/cowfs/pull/139 |
| lease | `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/3/cowfs` |
| accepted metadata review | `docs/reviews/pr137-meta42-snapshot-rename-runtime-final.md`, sha256 `a552af900706a3eb5565db245c810c9f46a9ba5adff6991e30bf7ad74b6a2c0a` |
| accepted metadata root review | `docs/reviews/pr137-meta42-snapshot-rename-root-final.md`, sha256 `9341cc4f8e5724846bae85a131d13bf3bb661d8755570fbe4836d226f28063d2` |
| accepted residual, immutable | `docs/verification/evidence/meta42-residual-verification.md`, sha256 `fbc6a078137b0fab370638d27dcaf64ff3ad283de37d8e7e970e4b10faac53ba` |
| toolchain | `rustc 1.99.0 (b940084d7 2026-09-28)`, macOS |

PR #137 was a dependency and was **not** merged while this work was written.
It merged during the work, as `cf67e8a`, and `main` now contains `b5e6f78`.
`main` was brought into this branch rather than rebased onto, and the whole scoped suite was
re-run against the merged tree. The dependency is therefore satisfied by `main` and the claim
is stated as a fact rather than as an assumption.
The branch base is that exact commit, fetched by name into `refs/remotes/origin/fix/meta-snapshot-rename-42`, never by `FETCH_HEAD`.

## The merge

While this was in flight, `main` moved: #136, #137 and #138 all merged, so `main` went from `b486d45` to `cf67e8a`.
Two of those touch this work.

`#138` corrected the `Core::live_blocks` doc comment in the same file this change edits, at the line the stale-comment review had flagged.
The two changes are disjoint: `live_blocks` is a different method from `rename_snapshot`.
A read-only merge-tree of this head against `main` was clean, and the merge was then made with `--no-ff` so both lines of history are kept rather than rebased away.
After the merge, the corrected comment from `main` is present verbatim in the merged file, and `git diff main HEAD -- crates/cowfs-core/src/lib.rs` contains only this change's own 45 insertions and 11 deletions, so nothing of `main`'s is reverted.
The whole scoped suite was then re-run against the merged tree, which now also carries the hole flag from #138 and the clock work from #136: 18 gates, all green.

The branch adds two commits over `main`: the change, and the merge.

## What was wrong, in the words of the tree

`Core::rename_snapshot` did not rename anything.
It called `swap_snapshot(old, Some(old), new)`, and the swap's job is to *replace* a snapshot: fork the source into a reserved staging name, write an intent file, unregister the victim, fork the staging snapshot into the target name, remove the staging snapshot, remove the intent file.

Two forks means two new snapshot ids.
The old name's snapshot is the victim, so it is unregistered, and `unregister` refuses while a handle is open.

So before this change a rename:

- changed the snapshot id,
- changed every inode number inside it, because the inodes are packed with the snapshot id,
- invalidated every handle already open on it, because the snapshot it referred to was destroyed,
- wrote an intent file and created a reserved staging snapshot to do it.

Measured on the old consumer, from the regression fixture: a rename of the snapshot with id `1` returned an entry with id `3`, and across a drop and reopen the packed root inode moved from `1099511627777` to `1099511627778`.

## The change

`crates/cowfs-core/src/lib.rs`, `Core::rename_snapshot`, and nothing else in that file but the two name readers described below.

The order is the existing contract, in the existing order:

1. `snap_by_name(old)` first, so a missing source is reported before an invalid target name,
2. `validate_snapshot_name(new)`,
3. the same name twice is refused, with the message callers already match on,
4. `check_new_name_except(new, Some(old))`, which is the case-folding and normalising alias rule, with the name being given up excluded so it cannot collide with itself,
5. `Meta::rename_snapshot(id, new)`, one metadata transaction that keeps the row, the tree and the id,
6. every fallible read of the new state happens *before* the live registry is touched,
7. only then is `Snaps.by_name` moved from the old key to the new one, under the registry write lock, and `root_time` bumped because the mount root's listing changed.

Because nothing is written before step 5 and nothing can fail after step 5, there is no third state: a refused rename adds no name, removes no name and half-moves nothing, and a successful one cannot fail afterwards.

`promote_base` is untouched in behaviour.
It still calls `swap_snapshot`, because a promotion has to *destroy* the target it replaces and that is not one transaction.
The only change to `swap.rs` is that `swap_snapshot` lost its `rename_from` parameter, which is now provably always `None` since rename no longer comes through it, plus the module and method docs, which said "cowfs-meta has no atomic rename of a snapshot".

## The one change outside the rename method, and why it was necessary

`SnapCtx` carried a second copy of the snapshot's name, in a plain `String` field, while `Snaps.by_name` already held name to id.

Only two places read it: `Inner::unregister`, which removed the `by_name` entry by that name, and `Inner::health`, which reported it as a lane label and did so **without holding any lock**.
A rename that updates the field would race with that unlocked read, so the duplicate had to go rather than be written.

`by_name` is now the single authority:

- `unregister` removes the entry **by id**, which is what it actually means and cannot go stale,
- `health` resolves a name from `by_name` under the registry read lock before it takes any per-snapshot lock, so no new lock nesting is introduced.

The field and its constructor argument are gone from `crates/cowfs-core/src/queue.rs`.
Nothing else in the tree read it: `swap.rs` and `stress.rs` both read `SnapshotInfo.name`, a different type.

## What was deliberately not done

- **No inode allocator change.** The virtual inode and alias policy is untouched. `crate::ino::pack` and the durable `virt.ino` reservation still own every number handed out. A rename does not reach them, which is exactly why the inode numbers survive.
- **No `io.rs`, no cache clock, no store, no hole flag, no metadata API, no database, no `tx.rs`, no daemon change.** `cowfs-meta` is a dependency and is byte-identical.
- **No gate work.** The reference gate exists so a collector can hold the reference side still. A rename creates and releases no block reference and touches no tree, so it needs no gate and takes none. It is not a GC root change.
- **No promotion or crash-recovery redesign.** The intent-file protocol, the staging name rules and the promotion fault matrix are unchanged and still runtime-covered.
- **No new public fault API.** The failure injection below uses the existing `Core::open_with_meta` seam.

## Proof

One fixture, `crates/cowfs-core/tests/core_atomic_rename.rs`, 10 cases, public `Core` and `Vfs` surface only, so one source runs against both consumers.

The RED is real and it is the identity claim, not a compile error.
On the unpatched consumer at `b5e6f78`, the same source gives **2 passed, 7 failed, exit 101**:

| case | old consumer | patched consumer |
|---|---|---|
| `a_rename_keeps_the_id_the_root_and_the_file_numbers` | FAIL, id went 1 then 3 | pass |
| `a_handle_open_across_the_rename_still_reads_and_writes` | FAIL | pass |
| `a_dirty_write_before_the_rename_is_visible_after_it` | FAIL, id went 1 then 3 | pass |
| `a_refused_rename_changes_no_name_and_leaves_no_leftovers` | pass | pass |
| `a_missing_source_is_reported_before_an_invalid_target_name` | pass | pass |
| `the_stored_bytes_survive_a_drop_and_a_reopen_under_the_same_id` | FAIL, root inode 1099511627777 then 1099511627778 | pass |
| `a_rename_moves_one_name_and_changes_no_other_snapshot` | FAIL | pass |
| `promote_base_still_replaces_its_target` | FAIL | pass |
| `two_renames_in_a_row_keep_the_same_id` | FAIL, id reached 5 | pass |
| `a_rename_that_fails_at_the_commit_changes_nothing` | added with the change | pass |

The two that pass on both are the refusal and error-ordering cases, which is the point: the existing contract is unchanged.

What the fixture pins, in the order the brief asked for it:

- snapshot id, packed root inode and file inode all unchanged across a rename,
- a handle opened before the rename still reads and writes the same file through the same inode afterwards,
- a dirty write queued before the rename is visible after it, through the same inode, and still exact after a sync,
- refusal on a held name, an invalid name, a missing source and the same name twice, with no name added or removed and content untouched,
- missing source reported before an invalid target name,
- exact stored bytes across a sync, a drop and a reopen, under the same id, with the old name gone,
- one snapshot renamed, every other snapshot's entry and content untouched,
- promotion still replaces its target, including that the replaced target's own file is gone by design,
- two renames in a row from the same id,
- a failure at the metadata commit, armed through `Core::open_with_meta` by wrapping the `before_sync` hook `Core::open` already installs: the call returns a control error, no name moves, the new name does not exist, and the same rename succeeds once the fault is disarmed.

Two cases were wrong on the first run and were my error, not the change's, and are recorded here rather than dropped:
`promote_base_still_replaces_its_target` originally asserted the target's own file survived a promotion, which promotion never promised, and `the_stored_bytes_survive_a_drop_and_a_reopen_under_the_same_id` compared `SnapshotEntry::ino`, the packed metadata inode, against the virtual alias a view hands out, and then expected a fresh session to reuse the previous session's virtual number, which the durable reservation exists to prevent.
Both now assert what is actually true.

### Everything that was run

Against the **merged** tree, which is the tree that ships, 18 gates all exit 0:

| gate | result |
|---|---|
| `cowfs-core --test core_atomic_rename` | 10 passed, 0 failed |
| `cowfs-core --test swap` | 3 passed, 0 failed |
| `cowfs-core --test critic2b` | 27 passed, 0 failed, 1 ignored |
| `cowfs-core --test core` | 20 passed, 0 failed |
| `cowfs-core --test names_ino` | 4 passed, 0 failed |
| `cowfs-core --test caches` | 2 passed, 0 failed |
| `cowfs-core --test durability` | 6 passed, 0 failed |
| `cowfs-core --test alias` | 3 passed, 0 failed, 1 ignored |
| `cowfs-core --test fsck` | 3 passed, 0 failed |
| `cowfs-core --test invariant` | 4 passed, 0 failed |
| `cowfs-core --test locks` | 2 passed, 0 failed |
| `cowfs-core --test hole_walk`, from #138 | 3 passed, 0 failed |
| `cowfs-meta --test snapshot_rename` | 10 passed, 0 failed |
| `cowfs-meta --test hole_flag`, from #138 | 14 passed, 0 failed |
| `cowfs-meta --test critic` | 12 passed, 0 failed |
| `cowfs-gc --test mark` | 11 passed, 0 failed |
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy -p cowfs-core --lib --test core_atomic_rename --test swap --test critic2b -- -D warnings` | exit 0 |

`hole_walk`, `hole_flag` and `cowfs-gc --test mark` are run because the merged tree carries #138 and this change shares `cowfs-core`, not as a claim about #138's own review scope.

| target | result |
|---|---|
| `cowfs-core --test core_atomic_rename` | 10 passed, 0 failed |
| `cowfs-core --test swap` | 3 passed, 0 failed |
| `cowfs-core --test critic2b` | 27 passed, 0 failed, 1 ignored |
| `cowfs-core --test core` | 20 passed, 0 failed |
| `cowfs-core --test names_ino` | 4 passed, 0 failed |
| `cowfs-core --test caches` | 2 passed, 0 failed |
| `cowfs-core --test durability` | 6 passed, 0 failed |
| `cowfs-core --test alias` | 3 passed, 0 failed, 1 ignored |
| `cowfs-core --test fsck` | 3 passed, 0 failed |
| `cowfs-core --test invariant` | 4 passed, 0 failed |
| `cowfs-core --test locks` | 2 passed, 0 failed |
| `cowfs-meta --test snapshot_rename` | 10 passed, 0 failed |

The same scope was run once before the merge, on the pre-merge tree, with identical results for every gate that does not depend on #138; that run is what produced the RED and the first GREEN above.

### Three existing `critic2b` cases re-pointed, and why

Three cases in `crates/cowfs-core/tests/critic2b.rs` asserted that a **rename** goes through the swap, and they failed once it stopped doing so.
Leaving three red tests is not shippable, and deleting them would drop real coverage, so each was re-pointed at the path that still uses the swap, which is `promote_base`, keeping the property each was written to pin:

| case | was | now |
|---|---|---|
| `a_refused_rename_leaves_the_mount_exactly_as_it_was` | a rename refuses while a handle is open on the renamed snapshot | `a_refused_promotion_leaves_the_mount_exactly_as_it_was`: a promotion refuses while a handle is open on the snapshot it would destroy, and leaves the mount, both contents and the absence of an intent file exactly as they were, before and after a reopen |
| `a_step_three_refusal_removes_the_intent_and_staging_snapshot` | a step-3 swap fault refuses a rename | the same, on `promote_base`, still asserting no intent file and no hidden staging snapshot survives |
| `every_pre_removal_refusal_stays_refused_after_reopen` | swap faults 1 to 3 refuse a rename | the same range, on `promote_base`, still asserting the refusal does not complete at the next open |

Nothing is lost.
The full promotion fault matrix over steps 1 to 5 is covered twice already, by `swap.rs::promote_base_survives_a_failure_at_every_step` and by `critic2b.rs::a_fault_at_every_step_of_a_swap_leaves_old_or_new_and_nothing_in_between`, and both passed unchanged.
The rename refusal contract is covered by the new fixture and by the existing `swap.rs::rename_snapshot_is_failure_safe` and `names_ino.rs::snapshot_names_follow_the_cli_rules`, both unchanged and both passing.

`critic2b.rs` is a reviewer-authored regression file.
It is inside the owned test surface for this change and the three edits are confined to the three cases named above, but the coordinator should know the file was touched.

## Source bindings

sha256, at the head this record describes, of every file the result rests on.

| file | sha256 |
|---|---|
| `crates/cowfs-core/src/lib.rs` | `c9fa761aadfa066888fe4bf24ef2a47f18110b89677dea6e748a1bdd99a52d5d` |
| `crates/cowfs-core/src/queue.rs` | `8bba9c1a0bf1c5208af18ab35c43fb04ec495b6a4faefa0a827d52930d233efe` |
| `crates/cowfs-core/src/swap.rs` | `a9bd79ccce6f1830c9e97ea1de375304ab94c1e34a872554e134398652b15dfc` |
| `crates/cowfs-core/tests/critic2b.rs` | `4d389b7032e71c129a9e2f0782eca371cbfd2937119bf85bd1d66dd8f7cde6a9` |
| `crates/cowfs-core/tests/core_atomic_rename.rs` | `5bdfe20d6b14e166e410b73b78131a960ee825b9db751b584a6d0860e91fae43` |
| `crates/cowfs-meta/src/db.rs`, the dependency, unmodified | `da41c8e70c4b7a7e7f9116b11f6476acc21f6d92216f863e5416dd9212843cae` |
| `crates/cowfs-store/src/lib.rs`, from #138, unmodified here | `4613a829bc5bd384e731c16fcdf726e200ea5624736cf1b5afcc342d95672fba` |

`lib.rs` and `critic2b.rs` digests differ from the pre-merge values because `main` corrected the `live_blocks` comment in the first and because rustfmt reflowed the re-pointed cases in the second.
`queue.rs`, `swap.rs` and the fixture are byte-identical across the merge, which is the evidence that this change did not disturb anything of `main`'s.

Digests are of file content, not of anything derived from a file name.
The branch base is a full commit id and was fetched by ref name.

## Budget

`bench/out` held 6,370,416 KiB before this work began, against an 8 GiB cap, leaving 2,018,192 KiB.
Free floor 291 GiB against a 20 GiB floor.
One target directory was built, `bench/out/meta42-core-atomic-rename/target-red`, and reused across the red and green runs by building the old consumer first and then patching the same tree in place, so the dependencies compiled once rather than once per arm.
Peak `bench/out` 7,450,168 KiB after the merged-tree rebuild, still under the cap, checked before every gate of every batch.
Every batch ran under one foreground `mac-heavy.lock` acquisition, appended and flushed per gate, and checked the cap before each gate.
No cleanup, no deletion, no move, no offload and no cap waiver.

## What this does not claim

No whole-file snapshot rename family, no POSIX rename matrix, no performance or timing claim, no crash-injection matrix, no GC physical reclamation claim, and no statement that issue #42 is finished.
Requests 2, 3, 4 and 5 are untouched and issue #42 stays open.

The promotion and crash-recovery fault matrix is unchanged and remains covered where it was, and none of it is a requirement of the rename mechanism.

Not run, and not claimed: `no-mistakes` is not initialized in this repository, `.no-mistakes` and `.claude` are both absent.
MisakaNet is local-only and was not consulted.
The full 91-suite run was not performed; the scope was the rename fixture, the swap and critic suites that exercise the swap protocol, the core, name, cache, durability, alias, fsck, invariant and lock suites, and the dependency's own rename fixture, plus `fmt` and `clippy`.