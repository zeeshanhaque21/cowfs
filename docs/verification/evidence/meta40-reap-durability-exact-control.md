# #40 periodic-reap durability control

Refs #40.
This is a remaining acceptance slice, not a new tracker item or whole-filesystem completion.

## Source and requirement mapping

Branch `test/meta-reap-durable-control-40` starts from merged main `707ff62ec79ed1c578ddaafcc7712ece77f096e6`, including the verified physical-ID consumer and full Core health propagation.
Commit `7a99c39` changes only `crates/cowfs-meta/src/db.rs`.
It reuses `tests/common/backend.rs` under cfg-test, instead of adding another backend or dependency.
Production builds retain the same 16-step durability cadence and two-phase commit.
Reservation authority, counter floor handling, and support for large ranges remain unchanged.

`reap_periodic_commit_syncs_the_prior_nondurable_step` creates a retained snapshot with a real file and removes another snapshot.
The private fixture expands that removed leaf root's queued references to 257, with matching REFS count, next_reap counter, and queue length.
It checks metadata invariants before running the real reaper.
This controlled accounting fixture avoids hundreds of separate snapshot commits and timing-dependent multi-node trees.
The step counter starts two operations before the periodic boundary.
The first actual reaper operation consumes 256 references without a backend sync and leaves exactly one queued reference.
The second removes the final reference and node, preserves the retained file, passes metadata invariants, and must issue a tagged sync_data call through the actual recording backend.
The positive case then reopens a copied backend image taken before graceful close, verifies the queue is empty and the removed snapshot remains absent, and verifies retained file identity and invariants.

`reap_verifier_rejects_the_real_durability_guard_removal_control` runs the same verifier with the real periodic durability promotion disabled for only the second operation.
Both actual reaper operations, queue drain, node removal, retained-file lookup, and invariant check must succeed before the exact assertion `reap verifier observed no periodic durable sync` can satisfy the expected panic.
Unrelated setup or storage errors cannot count as a successful negative control.
The private test switch is thread-local and resets through Drop, including unwinding.
This is compiled in-process mutation sensitivity, not an external source-mutant rebuild.

## Executed checks and acceptance limits

Standalone rustfmt and whitespace checks pass.
No local Cargo execution or runtime sample is claimed; the preserved artifact-cap restriction still holds.
When authorized, the focused sample command is `cargo test -p cowfs-meta --lib reap_` and must execute both named new tests.
Require their named results on Ubuntu and macOS at the integrated head, together with retained health, identity, reservation, and recovery coverage, before merge.
Require CI-tested and merged tree equality before accepting the slice.
The model records actual backend sync calls but is not a physical disk or power-loss test.
The seeded queue proves this controlled cadence boundary, not a long-running workload or all crash histories.
Full Store ordering and real crash acceptance remain separate existing requirements.

No runner, workflow, dispatch, daemon, mount, store, cleanup, lease-return, or new subagent operation occurred.

## Self-check

Accuracy 3/5: source and formatting verified, runtime not executed yet.
Completeness 3/5: the requested cadence control is authored, but both-platform acceptance and full #40 remain missing.
Clarity 4/5: seeded accounting and model limitations are explicit; runtime remains pending.
Actionability 4/5: focused command and named acceptance tests are provided, but cannot yet justify merge.
Conciseness 4/5: one shared verifier and existing backend are reused; the accounting fixture still needs setup assertions.
Overall 3.6/5, not a completion claim.
The next improvement is actual named runtime verification; the user would reasonably reject an accepted-state claim without it.

Draft PR #151 is open for this slice.
The graph change audit identifies db.rs as the only changed source file.
No-mistakes remains uninitialized and was not reconfigured.
PR-body API readback and image-free content are checked separately; browser rendering is not claimed.

## Compiler correction

Run 37555945350's completed Ubuntu job 112582078308 failed before runtime at the four pending_reap comparisons.
The fixture treated pending_reap as u64, but the actual API returns Result<u64>.
Commit 9a43f35 unwraps exactly those four reads, preserving every queue, node-removal, retained-file, invariant, backend-sync, and exact-panic assertion.
Standalone rustfmt and whitespace checks pass for the correction, which was pushed immediately.
The failed compiler run is not evidence that either new test executed.
Corrected-head runtime remains pending; PR #151 stays draft.

## Corrected-head runtime and format integration

Run 37556132850 completed all three checks successfully at 9a43f359c5f97df8e93f2b2c4d37d13af066add0.
macOS job 112582683916 and Ubuntu job 112582683969 directly confirm both reap_periodic_commit_syncs_the_prior_nondurable_step and reap_verifier_rejects_the_real_durability_guard_removal_control pass.
Both jobs also pass both metadata_sync_failures tests and the fixed-cost large-reservation test.
This supersedes the earlier pending statement for the compiler-corrected head only.
Merged #148 at 7f7b505 is integrated in new head 88e959fbf57b3491ac1324a19e0d635d41395da9, run 37557794819.
All three reap fixture functions and all four format fixture functions are byte-identical to their original sources after resolving the shared insertion conflict.
Standalone rustfmt and whitespace checks pass; integrated-head runtime remains pending before merge.
This is real backend-sync and modeled-image evidence, not physical power loss or whole #40 completion.

## Integrated Ubuntu runtime

Run 37557794819's completed Ubuntu job 112587931414 succeeds at 88e959fbf57b3491ac1324a19e0d635d41395da9.
Direct logs confirm both periodic-reap tests and all three merged format tests pass together, including each exact-panic control.
Core health propagation and the fixed-cost large-reservation test also pass on this same head.
macOS remains pending; no merge or whole #40 acceptance is claimed.

Merged #152 at ca164b8 is now integrated in pushed head c51eaa1.
Only crash.rs and the original measurement documentation changed in that clean merge; the reap and format fixtures remain intact.
New combined-head runtime acceptance remains pending.
