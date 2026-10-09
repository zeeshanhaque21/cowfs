# PR 141 runtime regression binding: final independent review

Reviewed head: `4b3daed988faf2dcd63d16a8c2b9aa6140b1d5a4`, branch `fix/core-atomic-snapshot-rename-42`.
Base of the delta: `c220cc37e4ab71d9c048b49e0856d9672be6d633`.
Current `main`: `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`, which does **not** contain the Core rename change and
is therefore a genuine pre-change consumer baseline.
Author: READY3. Reviewer: READY1, Core read-only, disjoint from the author's files.

**Verdict: the delta is exactly what it claims, and the shipped fixture genuinely discriminates the
change. One fixture-scope weakness is recorded, and it does not affect the binding.**
Not merged by this review, not fixed by it.

This review edited no source, no test and no manifest, made no commit, pushed nothing, merged nothing,
and filed no issue.

## Digests, verified before anything was trusted

| document | sha256 | verified where |
| --- | --- | --- |
| `docs/reviews/pr141-meta42-core-atomic-rename-final.md` | `fe6515c318a58d6640fcaf2040b94245b8d43507aa6f4528a43018c024ff3f50` | canonical PRIMARY and the blob committed at `4b3daed9` |
| `docs/verification/evidence/meta42-core-atomic-rename-final-correction.md` | `53b679303314f54596c55e911ce4061fbfc5fa48dfecb6cf3fa3f8814144de14` | canonical PRIMARY and the blob committed at `4b3daed9` |

Both are unchanged. The pre-correction fixture bytes are, as the correction itself states,
unrecoverable, and I make **no** claim that any fixture byte-identity held before the correction.

## The delta is identifier and comment only, proven mechanically

One commit, `test(core): name the promotion case for the property it actually pins`.

The only code change is in `crates/cowfs-core/tests/core_atomic_rename.rs`: a doc comment rewritten to
say what the case pins, and the case renamed from `promote_base_still_replaces_its_target` to
`promote_base_replaces_its_target_with_the_source_content`.

I did not take that on trust. Normalising the file by stripping comments and mapping both identifiers to
one token:

```
normalised lines: old=321 new=321  identical=True
assert/expect lines: old=53 new=53 identical=True
cases: old=10 new=10
only in old: ['promote_base_still_replaces_its_target']
only in new: ['promote_base_replaces_its_target_with_the_source_content']
```

All 53 assertion-bearing lines are byte-identical, so **no assertion was relaxed, reordered or removed**,
and no fixture body changed.
Files changed in the delta: the one test file plus two documents.
**No production file changed**, so `lib.rs`, `queue.rs`, `swap.rs` and `io.rs` are untouched by the delta
and the rename fix from `c220cc37` stands exactly as reviewed.

The name is also more accurate than the old one. "Still replaces its target" described promotion as a
contrast to rename; the new name states the property the case actually asserts, that the target ends up
holding the source's content.

## Runtime binding: OLD fails, NEW passes, on the byte-identical final fixture

**Fixture binding.** The final shipped fixture is sha256
`aa3c13345bfe0dc43833271ef9cc750a13f7daf7af802dace3a2202f2ce7c045`.
The OLD arm carries that exact file, verified by sha256 and by `cmp`, so both arms ran the **same
fixture bytes**; no arm has a locally edited, relaxed or synthesised case.

**Arm composition, stated exactly.**

| arm | source | tracked files | deviation |
| --- | --- | ---: | --- |
| NEW | `4b3daed9` | 667 | none: every file byte-identical to the commit |
| OLD | `cf67e8a6` (`main`) | 663 | **one added file**, `crates/cowfs-core/tests/core_atomic_rename.rs`, byte-identical to the final shipped fixture |

`crates/cowfs-core/tests/common/mod.rs` is identical between the two arms
(`994960978dc0e06908371037a4ab7035cf2d1c22`), so the fixture's `mod common` needed no transplant.
That file is **absent from `main`**, so on the OLD arm it is an addition, not a modification, and I
corrected my own first reading of this before reporting it.
No Core production file was edited in either arm.

**The OLD baseline is genuinely old.** `main`'s `rename_snapshot` still delegates to the staging fork:

```rust
// main cf67e8a6, lib.rs:350-353
pub fn rename_snapshot(&self, old: &str, new: &str) -> Result<SnapshotEntry, ControlError> {
    self.inner.snap_by_name(old)?;
    self.swap_snapshot(old, Some(old), new)
}
```

against the atomic `self.inner.meta.rename_snapshot(id, new)` on the PR head, so the arms differ in
exactly the property under review.

**Executables are distinct, proven by content rather than by filename.** Both arms produce the same
binary name `core_atomic_rename-6a5e234102253ae6` and different contents, stored per arm under
`bench/out/pr141-final-runtime-critic/bin/`:

| arm | sha256 |
| --- | --- |
| NEW | `2ca014bf155668660e2dc038304a271793b0367f30f4c062418b4a1b3de9d70c` |
| OLD | `d3f6ec206aeb2ec5519f56f7e02b48df2d554c8e65648c6c197955303839d44a` |

### A false pass I produced and then caught

Worth recording at length, because it is the exact trap this task warns about.

My first OLD run used **one shared `CARGO_TARGET_DIR`** for both arms, expecting cargo to rebuild from
the second source directory.
It reported **10 passed, 0 failed** on OLD, which contradicted the code: the old fork allocates a new
snapshot id, so the identity assertions cannot hold.

The cause was visible only because I checked instead of reporting: the shared target directory held a
single `core_atomic_rename` binary and a single `libcowfs_core` rlib, both from the NEW build.
The OLD arm had silently reused the NEW artifacts, so that "OLD passes" result was meaningless.

Re-run with a **separate** isolated `CARGO_TARGET_DIR` for OLD, the real result appeared.
Had I taken the first result at face value I would have reported the fixture as non-discriminating and
reached the opposite conclusion about the branch.

### The discriminating case, OLD fail and NEW pass

`a_rename_keeps_the_id_the_root_and_the_file_numbers` captures the entry **before** the rename and
asserts the id, the root inode and the file inode are unchanged afterwards.

| arm | exit | result |
| --- | ---: | --- |
| OLD, isolated target | **101** | `the snapshot id must not change`, **left: 3, right: 1** |
| NEW | **0** | 1 passed |

Left 3 against right 1 is the staging fork allocating a fresh snapshot id where the pre-rename id was 1,
which is precisely the claim being made.
This is an identity failure, not a backend fault, not a lint, and not a fixture failure.

### Whole fixture, both arms

| arm | exit | result |
| --- | ---: | --- |
| OLD, isolated target | **101** | **4 passed, 6 failed** |
| NEW | **0** | **10 passed, 0 failed** |

The six OLD failures, all identity assertions, none a fixture or infrastructure failure:

| case | OLD failure |
| --- | --- |
| `a_rename_keeps_the_id_the_root_and_the_file_numbers` | `the snapshot id must not change`, left 3, right 1 |
| `a_dirty_write_before_the_rename_is_visible_after_it` | `the snapshot id must not change`, left 3, right 1 |
| `two_renames_in_a_row_keep_the_same_id` | `the id survives both renames`, left 5 |
| `a_rename_moves_one_name_and_changes_no_other_snapshot` | left 4, right 1 |
| `a_rename_that_fails_at_the_commit_changes_nothing` | left 3, right 1 |
| `a_handle_open_across_the_rename_still_reads_and_writes` | panicked at `core_atomic_rename.rs:101` |

The four OLD passes are controls that should not discriminate, and they do not:

- `a_missing_source_is_reported_before_an_invalid_target_name`, about error ordering.
- `a_refused_rename_changes_no_name_and_leaves_no_leftovers`, about the refusal path.
- **`promote_base_replaces_its_target_with_the_source_content`**, the case the delta renamed.
  It **passes on both arms**, which is exactly what the correction document `53b6793` argues: promotion
  still goes through the staging swap and is untouched by the rename change, so it is a positive
  control rather than a rename claim. This independently confirms the correction's central point and is
  the strongest single piece of evidence for the rename being named correctly.
- `the_stored_bytes_survive_a_drop_and_a_reopen_under_the_same_id`.

## One fixture-scope weakness, recorded because it was in the brief

The brief asked me to prove the *representative* sample covering create, rename, dirty handle, stable
snapshot id, packed inode, sync, drop, reopen and real bytes, and then the same sample on OLD.

The case that covers that whole shape is
`the_stored_bytes_survive_a_drop_and_a_reopen_under_the_same_id`, and it **passes on OLD**.

The reason is in its own body: `id` and `packed` are captured from the `SnapshotEntry` **returned by the
rename**, never compared against the pre-rename id. So it pins that a reopen preserves whatever the
rename handed back, which is a real and useful property, but it does **not** pin identity **across** the
rename. Its assertions are therefore not sensitive to the change.

This is not a defect in the case: it is named for reopen durability and it does that correctly.
It only means it is not the discriminator, and the discriminating case is
`a_rename_keeps_the_id_the_root_and_the_file_numbers`.
I report it because naming a reopen case as the identity proof would overstate what it shows.

## `critic2b` on the final head

`cargo test --locked -p cowfs-core --test critic2b -- --test-threads=1` on the NEW arm:
**27 passed, 0 failed, 1 ignored**, exit 0.
The single ignored test is reported as ignored, not as passed.
I did not add, remove or re-scope any `critic2b` case, and the three obligations recorded in `fe65` are
carried forward unchanged; this review takes no new review scope over them.

No full workspace run, no performance, timing, soak, crash or new matrix of any kind.

## Integration with `main`, read-only

`git merge-tree --write-tree origin/main 4b3daed9`: merge base `cf67e8a6`, which is current `main`, tree
`bc6e81dd35490afccef3869b355b1b757ef1d633`, **0 conflict lines, exit 0.**
Read-only computation, not a merge.

## CI at the exact head, one snapshot

Run state on `4b3daed9`, not polled, not rerun, not dispatched, no workflow or runner change:

```
linux-fuse             completed/success
check (macos-latest)   in_progress
check (ubuntu-latest)  in_progress
commit_status          pending
```

Two of three are still running, so this is not a green claim and not a "no checks configured" claim.
`linux-fuse` alone is **not** evidence about the Core cases: it is the FUSE job and does not run the
`core_atomic_rename` target.
My local runtime result is a local result on this machine and does not substitute for the pending
platform runs.
All three conclusions must be read again once they land, and the exact head must be green before merge.

## Closing references and issue state

`closingIssuesReferences` is empty for PR 141.
The single commit message `test(core): name the promotion case for the property it actually pins` carries
no closing verb bound to an issue reference.
The PR body, 124 lines, has no closing verb bound to an issue reference in any form.

The already-merged dependency commit `57232166` still contains the negated form "nothing here closes
#42", which is what auto-closed #42 when #136 merged. It is on `main`, already superseded by the reopen,
and history is not rewritten.
`#42` is `open` with `state_reason: reopened`, verified directly, and **whole #42 stays open**: the
`swap.rs` promotion and crash-recovery paths, the hole flag, the inode reservation work and the `Core`
integration are untouched by this branch, and this review asserts nothing about any of them.
PR 141 is `draft=true`, `state=open`.

## Cap and lane discipline

`bench/out` before any archive or build: 4.97 GiB of the 8 GiB cap, 272.9 GiB free against the 20 GiB
floor.
After one shared target for NEW and one isolated target for OLD: 5.44 GiB used, 2.56 GiB headroom.
The cap was never exceeded.
No cleanup, pruning, deletion, moving or offloading was performed, and no cap waiver was taken; that is
READY5's approved scope, not this lane's.
One 600-second foreground `mac-heavy.lock` acquisition per batch, each recorded with its UTC time and
exit, appending to this lane's own log.
No signal, no restart, no install, no `sudo`, no mount walk, no lease action and no shared mount or
device; the fixture is in-process.
My artifacts are confined to `bench/out/pr141-final-runtime-critic/`, gitignored, and nothing else was
written: the PR 140 archive, targets and reports `0f27f8bf`, `4ef55a3c`, the PR 139 reports `75a2310b` and
`72d37cc4`, and the PR 141 receipts `fe6515c3` and `53b67930` are all untouched.
The leased worktree is clean at `e7ee215` on `fix/deferred-operation-time-42` with no commits and nothing
pushed.

## Tools

`no-mistakes` is **not initialized** in this repository, `.no-mistakes` and `.claude` are both absent, so
that pipeline was not run and no claim is made about it.
No browser step was taken; `chromium` is not installed, so any browser surface is **UNVERIFIED**.
`codebase-memory-mcp` graph tools were not used; the binding this review required was the extracted
archive compared per file with `git hash-object`, plus executable content hashes.
MisakaNet was available only as a local stdio server and was not consulted; no failure-recall need arose
and no remote call was made.

## Evidence

Under `bench/out/pr141-final-runtime-critic/` in the assigned worktree, gitignored:

| file | what |
| --- | --- |
| `archives/new.tar.gz` | source archive of `4b3daed9`, sha256 `1ba4b7cb6183a2dc2f331caab4b3af4df0ea01edc155161edbd03b4b1291f750` |
| `archives/old.tar.gz` | source archive of `cf67e8a6`, sha256 `b0136657a8827dd899d1fb7adc1d7a39be155d3ec6be4a3bcf5ffbf51220ae31` |
| `archives/new/`, `archives/old/` | the two arms, with the OLD arm's single added fixture declared |
| `bin/core_atomic_rename-new`, `bin/core_atomic_rename-old` | the per-arm executables, distinct by content |
| `logs/new-sample.log` | NEW representative reopen case, 1 passed |
| `logs/old-all10-isolated.log` | OLD full fixture, isolated target, 4 passed 6 failed, with the six identity assertion messages |
| `logs/old-discriminating.log`, `logs/new-discriminating.log` | the discriminating case pair, OLD exit 101 and NEW exit 0 |
| `logs/new-all10.log` | NEW full fixture, 10 passed |
| `logs/new-critic2b.log` | `critic2b` on the final head, 27 passed 1 ignored |
| `logs/lane.log` | every lock acquisition with its UTC time and every exit |