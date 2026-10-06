# Correction receipt for the #42 request 1 consumer integration

Answers the two merge conditions raised by `docs/reviews/pr141-meta42-core-atomic-rename-final.md`, sha256 `fe6515c318a58d6640fcaf2040b94245b8d43507aa6f4528a43018c024ff3f50`, 169 lines, which is left untouched.

That review is the source authority for the source verdict and for CI.
This receipt corrects one term it uses, records the one code change made in response, and states exactly what is and is not proven.

| what | value |
|---|---|
| head this receipt describes | `c220cc37e4ab71d9c048b49e0856d9672be6d633` |
| independent review | `docs/reviews/pr141-meta42-core-atomic-rename-final.md`, sha256 `fe6515c318a58d6640fcaf2040b94245b978...` truncated in this table on purpose; the full digest is the one above |
| author record, immutable | `docs/verification/evidence/meta42-core-atomic-rename.md`, sha256 `b9290d6e8949baafd363f4a2f7369607e6070863aa88170e7923be17d2d81479`, not rewritten |
| base | `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`, the tip of `main` and the #137 merge |
| scope | issue #42 request 1, the `Core` consumer side only |

Every reference in this document is to a commit that exists at the time of writing.
No head is named that is not reachable, and this receipt makes no claim about the commit that will carry it, because that commit does not exist yet and naming it would be a self-cycle.

## The one code change: a test identifier, nothing else

`crates/cowfs-core/tests/core_atomic_rename.rs`, one case renamed:

- from `promote_base_still_replaces_its_target`
- to `promote_base_replaces_its_target_with_the_source_content`

and its one-line description above `#[test]` reworded to match.

Nothing else in that file changed, and nothing changed anywhere else.
The proof is a normalisation, not a claim: replacing the identifier surface of the two versions with a single placeholder and hashing the remainder gives `67ce83e7caebe8a2f4bcf3bbe983a47fc9dfbdcfa259455d13e813bb716571f4` on **both** sides.
Taking only the case body and normalising the identifier gives `3ccbacf870f24f5a53262c1c8cce055c9a761abdeab5b44c826d831471d47f67` on **both** sides.
The count of `assert` occurrences in the file is 46 before and 46 after.
`rustfmt --check --edition 2021` on that one file exits 0.
That is a formatting proof, not a runtime proof.

### Why the case was recorded as failing on the old consumer

It was not the rename change, and the old log says so.

`bench/out/meta42-core-atomic-rename/logs/red.log` records, for that case:

```
thread 'promote_base_still_replaces_its_target' (43872129) panicked at crates/cowfs-core/tests/common/mod.rs:65:32:
getattr: Stale
```

`common/mod.rs:65` is `fs.getattr(ino).expect("getattr")` inside the shared `read_all` helper.
The body that produced that panic read the **target snapshot's original file inode**, the one `with_file("base", ...)` had created.
A promotion replaces the target, so that inode is destroyed by the very call under test, and `getattr` answers `Error::Stale`.
That is true on **both** consumers, because promotion is unchanged by this branch: the diff over `crates/cowfs-core/src/swap.rs` shows only a removed parameter and corrected documentation, and `promote_base` still calls the swap.

The body now reads the source's file `h` by name out of the replaced target, and asserts the target's own file `f` is gone, so it never asks about a destroyed inode.
The case therefore does not depend on the rename change, and the reviewer's alternative, making the promotion assertion specific enough to pass on the old consumer too, is satisfied by the body that was already corrected before the review.

The setup of the case contains **zero** calls to `rename_snapshot`.
It is `with_file("base", ...)`, `create_snapshot("src")`, one file in `src`, one `sync`, then `promote_base("src", "base")`.
So the case is not rename-then-promote and is not rename-coupled in its setup; the honest name describes the promotion property alone.

### What is not proven about that case

The fixture that produced the recorded RED was untracked and has since been edited, so the exact bytes that ran against the old consumer are **not** independently recoverable.
The only retained binding is the log itself: the nine test names it printed, the panic site `common/mod.rs:65:32`, and the message `getattr: Stale`.
That this case passes on the old consumer with its current body is therefore **unverified by this lane**, stated as unverified rather than argued.
It is not claimed, and it is not used as evidence for anything.

No fresh archive, target directory or binary was created to establish it, and none would have fitted.
Cargo artefact names are not used as content identity anywhere in this receipt.

## The term correction

The review describes the removed `SnapCtx.name` field as a lock-free read that becomes unsound once a rename can move a name.
The precise statement is narrower, and the correction is to the term, not to the verdict.

The field was written **once**, at construction in `add_snap`, and was never mutated anywhere in the tree.
There was therefore no shared mutation and no concurrent unsynchronised write to it, and no Rust memory-safety or data-race unsoundness is demonstrated by the history.
What the old code had was a **stale reported name**: `Inner::health` read `sc.name.clone()` with no lock, and after a rename commits that read can report a lane under the name the snapshot has already given up, so a reported name can disagree with the registry.
That is a reported-name consistency defect in a diagnostic path, not a memory-safety defect, and it is not a soundness bug in the Rust sense.

The fix does not add a lock around a `String`.
It removes the duplicate so there is nothing to keep in step, and the registry becomes the single authority: `by_name` is the only name index, every write to it happens under `self.snaps.wr()`, and `health` takes its names from `by_name` under `self.snaps.rd()` before it touches any per-snapshot lock.
No shared `String` mutation exists now, so there is no writer to order against a reader.

## The 9 versus 10 count, reconciled from the log rather than from a table

The old-consumer run printed **nine** test names.
The tenth case, `a_rename_that_fails_at_the_commit_changes_nothing`, is **absent from that log**, which is consistent with it having been written after the old run.

So the old consumer ran this fixture with nine cases, and the final fixture has ten.
Two of the nine passed on the old consumer and seven failed, and the two that passed are the refusal case and the error-ordering case, which is the intended result: the existing contract is unchanged by this branch.

The author table in the immutable record has the same nine rows.
Its tenth row reads "added with the change", which agrees with the log.
No count is inferred from a passing `b5e6f78` CI run: that run is that tree's own green and proves nothing about this fixture's behaviour on it, and it is not cited as evidence here.

One row of that table is **wrong and is corrected by this receipt**: the `promote_base_still_replaces_its_target` row records a failure on the old consumer that, as shown above, was caused by a since-corrected assertion body rather than by the rename change.
The old consumer's result for the current body of that case is unverified.
The immutable record is not rewritten; this row is superseded.

## The three retargeted `critic2b` cases, named exactly

All three are in one file, `crates/cowfs-core/tests/critic2b.rs`.
Two were renamed and one kept its name.

| before | after | what it now pins |
|---|---|---|
| `a_refused_rename_leaves_the_mount_exactly_as_it_was` | `a_refused_promotion_leaves_the_mount_exactly_as_it_was` | a promotion refused because the snapshot it would destroy has an open handle leaves the name set unchanged, both snapshots' content intact, no `swap-old` intent file, and the next open does not complete it |
| `a_step_three_refusal_removes_the_intent_and_staging_snapshot` | same name | the same case, now driven through `promote_base` under `set_swap_fault(3)`, asserting no `swap-base` intent survives and no hidden staging snapshot remains |
| `every_pre_removal_refusal_stays_refused_after_reopen` | same name | the same range, now on `promote_base`, asserting the refusal does not complete at the next open |

The count is three cases, in one file, of which two were renamed.
A statement that two cases were retargeted undercounts it.

### The eight obligations, and where each lives now

| obligation | carried by |
|---|---|
| refusal leaves no name changed and no leftover staging state | `core_atomic_rename.rs::a_refused_rename_changes_no_name_and_leaves_no_leftovers` |
| missing source reported before an invalid target name | `core_atomic_rename.rs::a_missing_source_is_reported_before_an_invalid_target_name` |
| a pending dirty write survives the rename | `core_atomic_rename.rs::a_dirty_write_before_the_rename_is_visible_after_it` |
| a hook failure at the commit changes nothing | `core_atomic_rename.rs::a_rename_that_fails_at_the_commit_changes_nothing` |
| an open handle stays usable | `core_atomic_rename.rs::a_handle_open_across_the_rename_still_reads_and_writes` |
| packed inode identity | `core_atomic_rename.rs::a_rename_keeps_the_id_the_root_and_the_file_numbers` |
| reopen identity and exact bytes | `core_atomic_rename.rs::the_stored_bytes_survive_a_drop_and_a_reopen_under_the_same_id` |
| other snapshots untouched | `core_atomic_rename.rs::a_rename_moves_one_name_and_changes_no_other_snapshot` |

All eight are present.
The promotion step-1-to-5 matrix remains covered twice, by `swap.rs::promote_base_survives_a_failure_at_every_step` and by `critic2b.rs::a_fault_at_every_step_of_a_swap_leaves_old_or_new_and_nothing_in_between`, both unchanged.

## The reopen case and the virtual inode, kept as it stands

`the_stored_bytes_survive_a_drop_and_a_reopen_under_the_same_id` asserts that a fresh session does **not** reuse an earlier session's virtual inode number.
That `assert_ne!` is deliberate and is a statement about the durable reservation, not an incidental detail: meta's allocator starts at the committed `virt.ino` floor, so a number from an earlier session cannot be reissued, and nothing in this branch touches that file.

The case therefore resolves the file **by name** after the reopen and then compares the exact bytes.
What it pins is: same snapshot id, same packed root inode, file still reachable under the new name, exact stored bytes.
It does **not** pin that a virtual inode number is stable across a restart, and it must not, because nothing promises that.

Nothing in this case was changed by this correction.

## The post-commit fallible step, recorded and not "fixed"

`entry(&info)` is the last statement of `rename_snapshot` and is the only step after the point of no return that can return an error, because `pack` can fail when an id exceeds the packed layout's field width.
The review names it as a non-blocking ordering weakness.

It is unreachable by the existing invariant rather than by a check: `info.id` is the id of a snapshot that was already in `Inner.snaps`, so `add_snap` had already packed it once when it was registered, and `pack` is a pure function of that id.
No extra branch, guard or error path was added for it, because none was asked for and none is needed to make the invariant true.
It is recorded, not defended against.

## Production source identity after this correction

The four production files and the retargeted test file are **byte-identical to the reviewed head** `c220cc37e4ab71d9c048b49e0856d9672be6d633`.
`git diff c220cc3 -- ` over `lib.rs`, `queue.rs`, `swap.rs`, `io.rs` and `critic2b.rs` is empty.

Content digests at the reviewed head, by file content and never by file name:

| file | sha256 |
|---|---|
| `crates/cowfs-core/src/lib.rs` | `c9fa761aadfa066888fe4bf24ef2a47f18110b89677dea6e748a1bdd99a52d5d` |
| `crates/cowfs-core/src/queue.rs` | `8bba9c1a0bf1c5208af18ab35c43fb04ec495b6a4faefa0a827d52930d233efe` |
| `crates/cowfs-core/src/swap.rs` | `a9bd79ccce6f1830c9e97ea1de375304ab94c1e34a872554e134398652b15dfc` |
| `crates/cowfs-core/src/io.rs` | `377caf686ec566c74285120e2c47463d0aa426ea04761ee76b913dfd6f8987be` |
| `crates/cowfs-core/tests/critic2b.rs` | `4d389b7032e71c129a9e2f0782eca371cbfd2937119bf85bd1d66dd8f7cde6a9` |

The fixture changes in this correction only, from `5bdfe20d6b14e166e410b73b78131a960ee825b9db751b584a6d0860e91fae43` to `aa3c13345bfe0dc43833271ef9cc750a13f7daf7af802dace3a2202f2ce7c045`.

`io.rs` is listed because the neighbouring clock work owns it, and it is untouched: the diff of `crates/cowfs-core/src/io.rs`, `crates/cowfs-meta/`, `crates/cowfs-store/` and `crates/cowfs-fuse/` against `main` is empty.
No inode allocator, alias policy, store, metadata, daemon or hole-flag file was touched, here or at the reviewed head.

## Runtime status

No cargo run, build, test, clippy, archive, target directory or probe was made for this correction.
A test identifier and two documents do not need one, and `bench/out` was not touched: it stands at 7,450,168 KiB against the 8,388,608 KiB cap, with no cleanup, no offload, no prune and no waiver.

So no gate result is restated here.
The eighteen gate results in the immutable record belong to the reviewed head and are not carried forward as a fresh green for this correction.
The only check run is `rustfmt --check` on the one owned fixture file, which is a formatting proof and not a runtime one.

CI is read once at the final head, without polling, rerun, dispatch, trigger, workflow or runner change, and its state is reported as read rather than as a pass.

Issue #42 stays open.
Nothing here claims the whole-file snapshot rename family, a POSIX rename matrix, performance, or a crash-injection matrix, and nothing here claims request 1 is finished: the consumer integration is one part of request 1 and the runtime gate is open until CI is green.

Not available in this lane, stated rather than assumed: `no-mistakes` is not initialised here, so no such gate ran.
MisakaNet is local-only and was not consulted.
Browser rendering of any document is UNVERIFIED, so no rendering is claimed.
The pre-correction fixture bytes are unrecoverable, as stated above.