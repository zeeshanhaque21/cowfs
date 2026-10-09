# #40 inode-limit guard control

Refs #40.
This is one remaining acceptance slice, not whole-issue completion or a new tracker item.

## Source and acceptance mapping

Branch `test/meta-inode-limit-control-40` starts from merged main `23cae2e86b7d5d03b477e5d1f9472fa0614d51d6`.
Commit `4862247` adds two unit tests and a private cfg-test thread-local switch at the actual `Tx::alloc` exhaustion guard.
Production exhaustion behavior, reservation authority, and support for large contiguous requests are unchanged.

`inode_limit_refuses_unrepresentable_ids_across_reopen` creates an actual file at `INO_LIMIT - 1`, closes and reopens the metadata store, and verifies its identity and durable allocation floor.
It requires ordinary create, mkdir, and symlink to refuse exhaustion without installing their names or advancing the floor.
It checks metadata invariants, closes and reopens again, verifies all refused names remain absent, and requires another ordinary create to remain refused.
Only the fixture's in-memory allocator is positioned near the boundary, before the last-valid create.
The create's normal production reservation path establishes the durable floor.

`inode_limit_verifier_rejects_the_real_guard_removal_control` uses the same verifier but disables the actual allocator guard for one create.
It must successfully create and look up an inode at `INO_LIMIT` before panicking with exactly `inode-limit verifier accepted an unrepresentable inode id`.
An unrelated setup, persistence, or lookup failure cannot satisfy the expected-panic assertion.
The switch resets through Drop, including unwinding, and is thread-local so other test threads retain the real guard.
This is compiled in-process mutation sensitivity, not an external source-mutant rebuild.

## Executed checks and limits

Standalone rustfmt checks on both edited source files pass.
The whitespace check passes.
No local Cargo test, runtime sample, mutation run, or whole-filesystem proof is claimed.
Local execution remains held by the preserved artifact-cap restriction.
Both named tests must execute on Ubuntu and macOS at the exact integrated head before merge.
The tested CI merge tree must equal the actual merged tree before accepting this slice.
Reap durability, full crash evidence, and all other existing #40 obligations remain separate.

No runner, workflow, dispatch, daemon, mount, store, cleanup, lease-return, or new subagent operation occurred.

Draft PR #150 is open for this slice.
The codebase graph change audit identifies only db.rs and tx.rs as changed from merged main.
PR-body readback matches this canonical receipt and contains no images.
No-mistakes remains uninitialized and was not reconfigured; browser rendering remains unverified.

## Focused verification command

When runtime execution is authorized, run `cargo test -p cowfs-meta --lib inode_limit_` first.
Require exactly two executed tests, including the expected-panic control, rather than a zero-test success.

## Self-check of this delivery slice

Accuracy 3/5: source and formatting checked, but the authored tests have not executed yet.
Completeness 3/5: the inode control is authored; both-platform runtime and tested-tree acceptance remain missing.
Clarity 4/5: the receipt maps named tests to assertions, but acceptance is still pending.
Actionability 4/5: draft #150 and the focused command exist, but runtime is held for CI.
Conciseness 4/5: one shared verifier handles both cases, though the two reopen phases add fixture length.
Overall 3.6/5, not a completion claim.
Highest-impact improvement is to verify both named runtime results before merging this slice.
The user would reasonably reject a filesystem-complete claim while 25 existing items remain unaccepted.

## Merged-health integration

Head c18f0ae integrates #147's verified merge 707ff62ec79ed1c578ddaafcc7712ece77f096e6, including Meta sync accounting, full Core health propagation, and its two tests.
The source merge was conflict-free and pushed immediately.
This joint tree must execute the inode positive and exact-panic control, together with retained health and reservation coverage, before merge.
No authored inode test has yet been claimed as executed or accepted.

## Integrated Ubuntu execution

Run 37555592401's completed Ubuntu job 112580958523 passes on head c18f0ae84e67126aee6e7187156a2a8980c369ba.
The direct log confirms inode_limit_refuses_unrepresentable_ids_across_reopen and inode_limit_verifier_rejects_the_real_guard_removal_control both pass.
The real last-ID create, refusal across reopen, retained absence/floor/invariant assertions, and exact guard-removal verifier therefore executed successfully on Ubuntu.
Both named metadata_sync_failures tests and the fixed-cost large-reservation test also pass on this same integrated head.
This supersedes the earlier unexecuted-source statement for Ubuntu only.
Ubuntu and FUSE checks pass; macOS remains pending, so merge and whole #40 acceptance remain held.

## Both-platform execution and format integration

Run 37555592401 completed all three checks successfully at c18f0ae84e67126aee6e7187156a2a8980c369ba.
macOS job 112580958481 directly confirms both named inode tests, both metadata_sync_failures tests, and the fixed-cost large-reservation test pass.
Merged #148 at 7f7b505 is integrated in new head ba2c388.
Its db.rs test insertion conflict was resolved by retaining both fixture families; all three inode functions and all four format functions are byte-identical to their original accepted sources.
Standalone rustfmt and whitespace checks pass, and the merged head was pushed immediately.
The earlier both-platform run is historical evidence; this combined head still requires runtime acceptance before merge.

Current exact integrated head is ba2c38831084efd6f15390fd8a385df2d01a603e, run 37557525376.
The latest read-only status is in_progress, not a runtime pass.

## Integrated Ubuntu runtime

Run 37557525376's completed Ubuntu job 112587088078 succeeds at ba2c38831084efd6f15390fd8a385df2d01a603e.
Direct logs confirm both inode-limit tests and all three merged format tests pass together, including each exact-panic control.
Core health propagation and the fixed-cost large-reservation test also pass on this same head.
macOS remains pending; no merge or whole #40 acceptance is claimed.

Merged #152 at ca164b8 is now integrated in pushed head 5fbbd39.
Only crash.rs and the original measurement documentation changed in that clean merge; the inode and format fixtures remain intact.
New combined-head runtime acceptance remains pending.
