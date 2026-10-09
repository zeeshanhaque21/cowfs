# Snapshot-limit guard control for #40

Commit c8be5a9 on test/meta-snapshot-limit-control-40 adds the recorded snapshot-limit mutation control.
Base is verified main a759e6bc714388045146b4eb8e37fb82e64ab00b.
READY5's existing lease, ignored artifacts, #146 branch, and #148 branch are preserved.
No other checkout was edited.

## Prompt-to-artifact checks

- snapshot_limit_refuses_unrepresentable_ids_across_reopen drives the actual new_snapshot path.
- The private fixture moves only its fresh session's next ID near the boundary, then creates the last representable snapshot through the real commit path.
- After close and reopen, the fixture verifies the original snapshot ID and durable floor at SNAPSHOT_LIMIT.
- A further create must return exactly LimitExceeded with snapshot ids exhausted, leave no name, and leave the floor unchanged.
- Metadata check, another close/reopen, and another actual create refusal prove the boundary survives restart.
- snapshot_limit_verifier_rejects_the_real_guard_removal_control uses the same fixture and verifier with only the actual limit guard disabled by a private cfg-test thread-local switch.
- An accepted create must return the exact unrepresentable ID and close successfully before the required verifier panic.
- Any unrelated error or panic has a different message and cannot satisfy the negative control.
- A Drop guard restores the switch immediately after the create attempt, including unwind paths.

Non-test builds retain the original snapshot limit unchanged.
No public fault API, inode reservation limit, small-request cap, or production allocator semantics changed.
Standalone rustfmt passes; no local Cargo build or test ran under the artifact/headroom restrictions.
Normal CI must execute both named tests on Ubuntu and macOS before acceptance.
This is in-process guard-removal coverage, not an external source-mutant rebuild.
The existing inode-limit and reap-durability controls remain separate obligations.
Whole #40 remains open, and the tracker still has 68 items with 43 accepted completions.

## Delivery self-check

Accuracy 3/5: the fixture reaches the real boundary and exact verifier, but runtime is pending.
Completeness 2/5: this implements one remaining recorded control, not the full issue or filesystem.
Clarity 4/5: restart refusal and in-process control are explicit, with no external mutation claim.
Actionability 4/5: draft PR #149 has both tests in the standard suite; exact-head logs are the next proof.
Conciseness 4/5: one file reuses existing allocator and redb infrastructure without a public seam.
Overall 3.4/5.
The user would still regard the full objective as unfinished.
PR-body API verification has no images to check; browser rendering is not claimed.

## Executed Ubuntu control and physical-Core integration

Run 37553509729's completed Ubuntu job 112574281289 directly confirms both named tests pass at c8be5a99757e9ade9af3a9c050aaa25d9a385809.
The last-valid-ID, restart refusal, unchanged-floor, and exact guard-removal verifier assertions therefore executed successfully.
Head f3dfab7 genuinely integrates now-merged #142 at 23cae2e86b7d5d03b477e5d1f9472fa0614d51d6.
The merge completed without conflicts and preserves the test switch, real boundary guard, and both assertions.
Joint-head runtime remains pending; prior execution does not verify the new combined tree.
No runner operation, workflow change, dispatch, local Cargo run, cleanup, or lease return occurred.

## Merged-health integration

Head 1c51a66 integrates #147's verified merge 707ff62ec79ed1c578ddaafcc7712ece77f096e6, including Meta sync accounting, full Core health propagation, and its two tests.
The source merge was conflict-free and pushed immediately.
New combined-tree runtime acceptance remains pending.

## Integrated Ubuntu execution

Run 37555637284's completed Ubuntu job 112581105853 passes on head 1c51a66891a8c6fa055f317c359fbccbc4d79d06.
The direct log confirms the positive snapshot-limit/reopen test and its exact-panic guard-removal control pass.
Both named metadata_sync_failures tests and the fixed-cost large-reservation test also pass on this same integrated head.
Ubuntu and FUSE checks pass; macOS remains pending, so merge and whole #40 acceptance remain held.

## Format integration

Merged #148 at 7f7b505 is integrated in new head f47a118.
The shared test insertion and thread-local declaration conflicts were resolved by retaining both fixture families and both test-only guards.
All three snapshot fixture functions and all four format fixture functions are byte-identical to their original sources.
Standalone rustfmt and whitespace checks pass, and the merged head was pushed immediately.
Combined-head runtime acceptance remains pending; historical Ubuntu results do not prove the new tree.

Current exact integrated head is f47a118f8f475e5755d863fea3150c608d1f43fb, run 37557707909.
The latest read-only status is in_progress, not a runtime pass.

## Integrated Ubuntu runtime

Run 37557707909's completed Ubuntu job 112587656609 succeeds at f47a118f8f475e5755d863fea3150c608d1f43fb.
Direct logs confirm both snapshot-limit tests and all three merged format tests pass together, including each exact-panic control.
Core health propagation and the fixed-cost large-reservation test also pass on this same head.
macOS remains pending; no merge or whole #40 acceptance is claimed.

Merged #152 at ca164b8 is now integrated in pushed head 190b5b6.
Only crash.rs and the original measurement documentation changed in that clean merge; the snapshot and format fixtures remain intact.
New combined-head runtime acceptance remains pending.
