# PR #146 executed follower control and main integration

Refs #40.

Run 37548503540 completed success for 3c29cfb5a391df8ba36c5e9d6d401a917a070b48 on all three required checks.
Completed logs directly confirm these four named cases passed on Ubuntu and macOS:

- db::tests::follower_wait_does_not_ack_before_the_leader_publishes_durable_seq
- db::tests::follower_verifier_rejects_the_real_branch_early_return_control, with the required should-panic expectation
- single_durable_ack_follows_the_hook
- every_durable_ack_is_durable_on_return_including_followers

The executed control is the test-only early return inside the actual if-led follower branch before its wait witness.
The positive case and negative control use the same verifier.
The exact expected branch-order assertion distinguishes the intended negative control from a setup error or unrelated panic.
This establishes compiled in-process early-return mutation sensitivity; it does not claim an external source-mutant rebuild executed.

Head e998a99 integrates current main f31c81d3, including merged allocator-extreme and separate-adapter tests.
The actual source integration requires new joint-tree CI before merge, despite the earlier successful head.
No workflow, runner, public fault seam, store, mount, artifact cleanup, or lease was changed.
The PR remains draft pending that joint-tree audit.
The other recorded mutation controls, real Store crash proof, Core health integration, and whole #40 remain open.

Accuracy 4/5: named tests are verified on both platforms, but the integrated head is not yet verified.
Completeness 2/5: this delivers one recorded control; other #40 gates and the fixed 68-item objective remain incomplete.
Clarity 4/5: in-process mutation sensitivity and external source-mutant rebuilding are explicitly distinguished.
Actionability 4/5: source integration is pushed and normal CI can judge its exact tree.
Conciseness 4/5: no new production abstraction or fault API was introduced.
Overall 3.6/5.
Next is integrated-head named-test and tree verification, not another source-only acceptance cycle.
The user would reasonably still regard the overall objective as unfinished.

## Verified integrated delivery

Run 37550267162 completed successfully on all three required checks for head e998a9912c37162d8603b930301262168ce63e43.
The completed Ubuntu and macOS logs each contain successful execution of all four named cases above, including the exact-panic early-return control.
All three jobs checked out CI merge 7e73272fd8142d8026d485adc49f4236080af887, tree 8cb223fc5393858312e3a8abff474e55bfa4725e.
PR #146 merged at a759e6bc714388045146b4eb8e37fb82e64ab00b with exactly that tree.
The merge used a SHA-guarded API request because gh-axi pr merge does not support --match-head-commit.
This supersedes the pending-integration statements above, without changing the historical receipt.
Only this follower coverage slice is accepted; #40 and its other recorded obligations remain open.
