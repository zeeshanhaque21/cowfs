# meta42-snapshot-rename-budget-reproduction: cleanup was approved, the cap is unreachable inside the approved scope, so nothing was deleted and the sample was not run

Lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, READY5, held throughout.
Head under test: `9d1e5ef66d55798da08780296a05d6601468611f` on `fix/meta-snapshot-rename-42`, verified clean and equal to the remote before any work.
`crates/cowfs-meta/src/db.rs` is unchanged on this head and stays unchanged.
Prior receipts preserved byte for byte and not rewritten: `a390aaf4b84bcd6a36117b854faa54710543e4c32c20e868185ff08a390be811`, `93c4b1317b7d9445ced3f7d1316e159b3cca472cbee6172c0a81e2b9a8883c0b`, `471a07310e793016763a1f77e4e195bfeca89dcf1613d84fe50b238a6355b0ec`.

## Answer to the question asked

Approve verified cache cleanup.

That approval was acted on and then stopped at the gate the approval itself defines.

## What was approved and what I did with it

Approved: removal of verified idle compiler cache only, preserving test binaries, source archives, fixtures, logs, reports, stores and leases, with an exact file-level manifest and verification before removal.

Done: enumeration, classification, provenance and identity verification, and the pre-deletion safety checks.
Not done: the deletion, and the regression sample that was to follow it.

## Exact cleanup scope, as enumerated

Thirteen Cargo target directories, all created by this lane, all inside this lease's `bench/out`:

`meta-health40-ci/target`, `meta-health40-ci/target-main`, `meta-health40-fixture-lifetime/target`, `meta-health40-fixture-lifetime/target-fix`, `meta-health40-fixture-lifetime/target-old`, `meta42-snapshot-rename/target`, `meta42-snapshot-rename/target-old`, `swap-provenance124/target`, `swap-provenance124-repair/target`, `swap-provenance124-recovery-repair/target`, `swap-provenance124-recovery-repair/target-probe`, `swap-provenance124-final-regression/target-new`, `swap-provenance124-final-regression/target-old`.

Each was confirmed to be a real Cargo target directory by its own `CACHEDIR.TAG` and `.rustc_info.json`, all reached by paths that stay under the owned canonical root, with no symlink encountered on any path walked.

Eligible by category, being generated compiler output with Cargo target provenance and not any protected category: `.o` 3.418 GiB, `.rlib` 2.654 GiB, `.rlib`-bundled object `.bin` 2.010 GiB, `.rmeta` 0.940 GiB.

Explicitly excluded from eligibility and left alone even though they are inside those target directories, because the approval names them as protected: test executables and all other binaries, the `(no-ext)` class, `.dylib` shared objects, `.a` static archives, `.d` dependency files, `.json`, `.timestamp`, and every `.rs` and `.h` source file.

## Bytes, and why the cap cannot be met

| Measure | Physical |
|---|---|
| `bench/out` total before | 20.862 GiB |
| `ready-40`, not this lane's, protected | 10.437 GiB |
| this lane's 13 target directories | 10.375 GiB |
| of which eligible compiler cache | 9.023 GiB |
| of which protected inside those same directories | 1.352 GiB |
| this lane's non-target archives, logs and receipts | 0.050 GiB |

Floor after removing **every** eligible file while preserving every protected one: **11.839 GiB**.
Cap: 8.000 GiB, and the cap needs roughly 1 GiB of headroom left for one focused meta build, so the effective target is nearer 7 GiB.
**Shortfall: 3.839 GiB against the bare cap, and about 4.8 GiB against the effective target. Unreachable.**

The blocker is a single directory, `bench/out/ready-40`, at 10.437 GiB.
It sits inside this lease's `bench/out` but it is not this lane's work: it holds `db.rs.golden*` mutation-golden source snapshots and 57 golden or log receipt files from mutation testing, dated Oct 4 and Oct 5 before the #42 work began.
Those are source archives and receipts, which the approval protects, and expanding into it is expanding into another lane's artifacts, which the same instruction excludes.
Deleting it would be both a scope expansion and a deletion of protected categories, so it was not considered.

By the instruction's own condition, if the cap cannot be met without touching protected files, stop. That is the situation, so the deletion step was not executed and the mutation command was never formed.

## Deleted count

**Zero files. Zero bytes.**

No `unlink`, no `rm`, no directory removal of any kind was issued in this lane in this turn.
No helper script was written, because a helper with nothing to delete would only be a way to make an unnecessary deletion look routine.

## Preserved-hash proof

Prior receipts, all unchanged:

| Path | sha256 | State |
|---|---|---|
| `docs/verification/evidence/meta42-snapshot-rename-regression-correction.md` | `a390aaf4b84bcd6a36117b854faa54710543e4c32c20e868185ff08a390be811` | unchanged |
| `docs/verification/evidence/meta42-snapshot-rename.md` | `93c4b1317b7d9445ced3f7d1316e159b3cca472cbee6172c0a81e2b9a8883c0b` | unchanged |
| `docs/reviews/pr137-meta42-snapshot-rename-final.md` | `471a07310e793016763a1f77e4e195bfeca89dcf1613d84fe50b238a6355b0ec` | unchanged |

Production and fixture sources, unchanged:

| Path | sha256 |
|---|---|
| `crates/cowfs-meta/src/db.rs` | `9f331f905361d88e619d9155d4565b53b78f090edd9c268ec31dcab36aca1026` |
| `crates/cowfs-meta/tests/snapshot_rename.rs` | `048d8edb2cbaca319688dc1f96eed4d865020061b03bdbad06a0c98e0f631e54` |

Prior artifacts still present and unchanged: `bench/out/meta-health40-ci/raw/job-111880928624.log` at `7873236f66e0bcf57227b84586d5c5fdd3a09bf36262eb3801b4952e41c3bd80`, and the prebuilt `snapshot_rename` executable under `bench/out/meta42-snapshot-rename/target/debug/deps/`.

Protected-by-construction state confirmed after the census: branch `fix/swap-provenance-124` still at `6350468f049ad1e9087a72c62fe4231e2d832fe7`, this lease clean at `9d1e5ef66d55798da08780296a05d6601468611f`, shared daemon 15263 untouched and not signalled, all 32 leases untouched, no store, mount, socket or job touched.

## The regression sample was not run

The sample was specified as one representative public regression run of `a_rename_commits_a_pending_tree_change_and_leaves_the_row_readable` against unchanged production at `9d1e5ef`, using a fresh source binding and an isolated target directory.

It was not run, for two reasons, both of which are gates rather than judgement:

1. The instruction is to reach a compliant budget and stop if it is not met, and it was not met.
2. The sample needs a compile in a fresh target directory, which adds roughly 1 GiB to a tree that is already 3.839 GiB over the cap. Running it would move the tree further out of compliance, not closer.

So the runtime status of Finding 1 is unchanged: **source chain confirmed, still not reproduced.**
There is no old-fail output, no exit code, no executable binding and no pass in this turn.
**No new pass is claimed, and no pass of any kind is claimed from this turn.**
The clippy fix likewise remains verified by source reading only, with no clippy run.

## No cap waiver

Free space is 294.4 GiB against a 20 GiB floor.
The gate is `bench/out`, not the filesystem, so free space does not waive it, and the user approval does not either.
The cap is reported as exceeded and unmet.

## What is owed, and who has to unblock it

1. A budget decision that either authorises removing `bench/out/ready-40`, which is protected by category and belongs to another lane's artifacts, or raises or restates the cap for this lane, or names another compliant location for the build train.
   Without one of those three, no amount of cache cleanup inside the approved scope reaches 8 GiB, so the sample and the root fix are both unreachable.
2. With budget compliant, the runtime old-fail for Finding 1 by running this exact fixture against unmodified production.
3. Only then the root fix in `db.rs`, resolving the row's root through `new_roots` as `Extra::Add` does, with the real store-bytes readback probe alongside it.
4. Fresh independent review and green CI after the fix.

No `db.rs` fix is applied in this turn.
No source or test edit was made in this turn.
No commit was made, because a commit here would carry evidence of a runtime result that does not exist.

PR #137 stays a draft and stays BLOCKED, issue #42 stays open, and no new task was created.