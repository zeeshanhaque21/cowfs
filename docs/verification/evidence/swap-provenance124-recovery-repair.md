# swap-provenance124-recovery-repair: the block was right, the ordering argument was wrong, and the fix is now the safety floor

Lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, READY5.
Base: merged `main` at `93cfef94457a989d031cb6b0a475ac4edbdb85ef`.
Correcting head `264eec1`, on top of the blocked head `4646f513a02bb9b3256920d138a9f01551e35c34`.
Branch `fix/swap-provenance-124`, pushed over HTTPS.
Host: Apple M3 Max, macOS 26 aarch64, `rustc 1.99.0` / `cargo 1.99.0`.
Blocking review this answers: `docs/reviews/pr134-swap-provenance124-final.md`, sha256 `803ea5c5e049225369ab72b50829a9f6483b2ac96ff1d06a9b16405d58c17e7`, committed on this branch byte for byte.

## Verdict

The critic's block was correct and my previous receipt was wrong.

My claim that "a returned `Err` from `promote_base` means the swap rolled back and the old tree is still there" is false.
I asserted it, and two hundred lines above it in the same document I quoted the file that refutes it.
The repair now treats an error from the staged swap as *uncertain* and never restores the old record from it, and the path backend no longer deletes the old tree when publishing the new target fails.

The previous receipt `swap-provenance124-repair.md`, sha256 `0dcb6a695a8b933528a348209c46c13c216d5a933a396daa7a33ff187e4f80f9`, is preserved unedited and is superseded by this one on the specific point of the rollback argument.
Its reproduction receipt `3fde9b054fea530288e1fe3d4577bdbe97a626fad2f6f26cf92611b8d3a029a1` is untouched and was independently confirmed correct by the critic.

## The error, stated exactly

My Claim A, at `swap-provenance124-repair.md`, said a returned `Err` implies rollback.

My Claim B, in the same document, quoted `crates/cowfs-core/src/swap.rs:194-197`:

> Past this point an error cannot be reported as "nothing happened", so the swap is rolled forward instead and the call succeeds. The only exception is a failure of the roll forward itself (an I/O error), which returns `Err` with the intent file on disk: the next `Core::open` completes it.

Claim B is right and Claim A denies it.
I verified the source myself before changing anything:

- `swap.rs:186-192` unregisters the victim, so the old tree is gone from the live namespace at that point.
- `swap.rs:201` is `let done = self.finish_swap(&staged, new);` and `swap.rs:205` returns `done` verbatim.
- `finish_swap` at `swap.rs:238-257` propagates `?` from `snap_by_name_raw`, `flush_snapshot`, `sc.snap.fork(target)` and `register`. Any of those failing after the victim was unregistered yields `Err` with the old tree gone and the intent file still on disk.
- `swap.rs:198-204` confirms the critic's second point: faults 4 and 5 are written into `last_error` and swallowed, so `set_swap_fault` cannot reach this window at all.

So the helper was not the problem; its precondition was false and `swap` supplied an error that violates it.
`restore_base_record` is deleted and the comment that stated the invariant as an unconditional truth is gone.

## Causal proof with a real fault, not a permission substitute

My earlier record-save test only ever exercised the before-mutation branch, because `invalidate_base_record` ran before any tree operation. That is the critic's point about why the nine tests missed this, and it is correct.

The proof here uses the existing public seam instead.
`cowfs_meta::Options::before_sync` is a documented hook: when it returns an error the durable commit does not happen (`crates/cowfs-meta/src/db.rs:89`).
`Core::open_with_meta` (`crates/cowfs-core/src/lib.rs:198`) is the public entry point that hands the caller the `cowfs_meta::Options` it opens with, so the hook can be replaced there.
No production file is edited and no fault framework is added.

The probe is a private crate outside the repository workspace, at `bench/out/swap-provenance124-recovery-repair/probe-crate`, with its own manifest, its own `CARGO_TARGET_DIR` and path dependencies pointing at this lease, so no workspace manifest and no dependency list of the PR is touched.

Output, exit `0`:

```text
probe: promote_base returned Err(i/o error: probe: injected meta sync failure)
probe: intent file present at Err = true
probe: after reopen tree="BBBB-from-srcB" commit=Some("commit-AAA")
probe: on-disk record = {   "repo": "/repoA",   "git_ref": "refs/heads/main",
                           "commit": "commit-AAA",   "promoted": true }
probe: commit via public API = Some("commit-AAA")
```

Every element of the counterexample is present in the causal chain:

- the swap really returned `Err`,
- the intent file survived the error, so the failure landed past the point of no return,
- the next open installed `BBBB-from-srcB`, the new tree, and removed the intent,
- the base record still says `commit-AAA`, read both as the real `BaseMetaStore` output on disk and through the daemon's public `create_meta`.

So restoring `commit-AAA` on that `Err` produces exactly the state #124 was filed for, and that is now impossible.

## The contract now implemented

Once a swap has been admitted, the swap may have replaced the target, so the provenance stays invalidated on any error.
The old commit is never restored on the strength of an `Err` alone.

Refusals that are decided before anything is touched are checked *before* the record is cleared, so they still preserve the full old tree and the full old record:

- a swap with itself, `InvalidInput`,
- a source that does not exist, `NotFound`,
- a target that does not exist, `NotFound`.

Holding `bases.exclusive` across the check and the swap is what makes that race-safe, and it is an existing seam rather than a new one: `create`, `remove`, `rename` and `promote` all mutate the namespace inside this same section, so nothing can appear or disappear between the check and the swap.
No new status API, transaction framework or core change was needed, so the escalation boundary was not reached.

A record publication that fails is different and is still strict.
`invalidate_base_record` runs before any tree operation, so a store whose record cannot be written fails with the old tree and the old record both intact.
That is the pre-mutation case, and the existing test for it still holds without weakening.

The honest cost, stated rather than hidden: on a swap that really did roll back, provenance that was still true is lost and the base reports itself stale rather than fresh.
That is the correct answer for code that cannot tell rollback from failed roll-forward, and it is strictly better than the alternative, which is a base that claims a commit which did not produce the bytes under its name.

## The path backend's retired tree

The critic's secondary finding is fixed in the same commit.

Previously the third rename failing meant the old tree was only at `retired`, an unconditional `force_remove_dir_all(&retired)` deleted it, and the record was then restored for a name with no tree at all.

Now, when publishing the new target fails:

- if the old tree reached `retired`, it is renamed back to where it was,
- if that rename cannot be done, the retired copy is left in place and the error says where it is, because it is the only remaining copy of the tree the target had,
- the staging directory is cleaned up in either case.

Nothing is deleted on the failure path, so no user or intruder work is destroyed to tidy a directory.
The critic was right that this behaviour is pre-existing rather than a regression from this PR, and the observable state before this commit was the same as on `main`; it is fixed here because it falls inside this PR's rollback claim, and it is not waived as pre-existing.

This part is verified by reading the code and by the existing `snapname_drift` and namespace suites passing, not by an injected `std::fs::rename` failure.
The critic was also right that no seam exists in this repository for that fault, so I report it as a source-level fix and do not claim a runtime counterexample for it.

## Scoped results at the correcting head

One 600 second foreground `mac-heavy.lock` hold, one flock retry budget, isolated `CARGO_TARGET_DIR` and a project-local `TMPDIR`, receipts printed before the batch.
No daemon, socket or mount was needed.

| Check | Result | Exit |
| --- | --- | --- |
| `--test swap_provenance_124`, repeat 1 | 9 passed, 0 failed, 2.18s | 0 |
| `--test swap_provenance_124`, repeat 2 | 9 passed, 0 failed, 2.26s | 0 |
| private causal probe, `--test recovery_probe` | 1 passed, 0.45s | 0 |
| `cargo test -p cowfs-daemon --locked --lib` | 100 passed, 0 failed, 9.60s | 0 |
| `--test namespace_durability` | 3 ignored, 0 failed | 0 |
| `--test snapname_drift` | 4 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --locked --test swap` | 3 passed, 0 failed | 0 |
| `cargo fmt -p cowfs-daemon -- --check` | clean | 0 |
| `cargo clippy -p cowfs-daemon --all-targets --locked -- -D warnings` | zero warning or error lines | 0 |

The 3 ignored `namespace_durability` tests carry the same `#[ignore]` as the base and are reported as ignored, not as passing.
The `cowfs-core` swap suite is included because it is the seam this repair's ordering depends on, and it passes unmodified.

## Scope and receipts

| Path | State | sha256 |
| --- | --- | --- |
| `crates/cowfs-daemon/src/backend.rs` | modified | `bc98830db838a38c1855fb7fac2c2296dc4e1d3513d5fb43cf2644960be25372` |
| `crates/cowfs-daemon/src/base_meta.rs` | unchanged | `4a8eac20294e6d5785a0fb34776731cfab077cdb40685e6bd80255813c1b4b92` |
| `crates/cowfs-daemon/tests/swap_provenance_124.rs` | unchanged this commit | `db58ba7658b2f4759df0805a9ca3b7a27b44b81a697516937bce4762d3e367c2` |
| `crates/cowfs-core/src/swap.rs` | unchanged | blob `4cbc3f289b68e8d626321a481525ced5f5562af0` |
| `docs/reviews/pr134-swap-provenance124-final.md` | added, byte identical | `803ea5c5e049225369ab72b50829a9f6483b2ac96ff1d06a9b16405d58c17e7` |

No new API, no metadata format change, no core change, no `cowfs-vfs` or `cowfs-ctl` change, no dependency change, no workflow change.
`restore_base_record` is gone: `grep -c restore_base_record crates/cowfs-daemon/src/backend.rs` returns `0`, and the false clause "means it rolled back" returns `0`.

Raw artifacts under `bench/out/swap-provenance124-recovery-repair/`: `causal-probe.log`, `gate-run.log`, `probe1.log`, `gate.sh`, and the private `probe-crate/`.
The earlier lanes' artifact directories are untouched.

## Limitations, stated plainly

- The daemon's uncertain-error branch is not directly driven by an end-to-end test, because no public seam reaches it: `Core::open_with_meta` is a `cowfs-core` entry point and the daemon's `CoreBackend` does not expose a meta-options hook, so a fault that makes the daemon's own swap return `Err` cannot be injected without a production change. The causal probe proves the core mechanism with a real fault and the daemon contract is established by the absence of any restore path plus the pre-mutation record-save test. That combination is the honest limit of what is proven here.
- The path-backend retired-tree fix is source-level. No `std::fs::rename` fault seam exists in this repository, so no runtime counterexample is claimed for it.
- The probe replaces `before_sync`, which `Core::from_parts` documents as the store-durability hook. The probe's store is private and it is not measuring durability ordering; it is measuring which commit fails. The swap suite passes unmodified.
- The causal probe proves the ordering defect for the `before_sync` failure class. It does not claim to prove every possible post-victim-removal failure.
- A reset base now reports stale rather than fresh until the next `base_refresh` after an admitted-swap error. Intended and disclosed.
- The 3 ignored `namespace_durability` tests were not run.
- No live socket round trip and no global filesystem claim.

## What has to happen next

The original critic lane resumes on this head.
Issue #124 stays open, PR #134 stays a draft, and nothing is merged or closed here.