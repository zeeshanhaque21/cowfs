# #40 exact real-Store crash ordering control

Refs #40.
Branch `test/core-crash-exact-ordering-control-40` starts from merged main `707ff62ec79ed1c578ddaafcc7712ece77f096e6`.
Commit `440a069` changes only `crates/cowfs-core/tests/crash.rs`.

## Requirement and source mapping

The existing positive crash-image test uses real Store pack files, production Store-before-Meta ordering, modeled crash images, durable-byte checks, invariants, fsck, a post-crash write, and a second reopen.
Its existing negative control removes the actual Meta before_sync hook in World::new, but its broad should_panic could count an unrelated failure.
The shared verifier now diagnoses `cowfs_store::Damage::MissingLiveBlock` explicitly, before retaining the general fsck-clean assertion.
The negative test now requires the exact panic text `crash verifier found a missing live block`.
Reopen failures, corruption checks, metadata invariants, missing snapshots, lost-file checks, or unrelated panics can no longer satisfy that negative assertion.
The same missing-live-block check also protects the positive verifier.
The original durable-file checks and post-crash readback remain intact.
No production code, workload sizes, seed policy, hook implementation, reservation behavior, or acceptance thresholds changed.

## Executed checks and pending acceptance

Standalone rustfmt and whitespace checks pass.
The source diff is nine added lines and two removed lines in one existing test file.
No local Cargo, runtime, or mutation result is claimed; the preserved artifact-cap restriction remains in force.
Require both named tests to execute on Ubuntu and macOS at the integrated head before merge.
The negative result must satisfy the new exact-panic requirement, not merely pass under the former broad annotation.
For an authorized focused sample, run `cargo test -p cowfs-core --test crash` at the existing default sample size before expanding it.
Require tested and merged tree equality before accepting this slice.
This remains modeled crash-image evidence using a real Store, not actual physical power loss, a long soak, or whole #40 completion.
The previous real-Store runtime receipt remains historical positive evidence, not runtime proof of this stricter control.

No runner, workflow, dispatch, daemon, mount, store, cleanup, lease-return, or new subagent operation occurred.

## Self-check

Accuracy 3/5: source and formatting checked, the stricter runtime result remains unknown.
Completeness 3/5: the broad-panic gap is patched, but both-platform acceptance remains missing.
Clarity 4/5: the exact diagnosis is explicit; physical-crash proof is still separate.
Actionability 4/5: the existing named tests and focused command are preserved, but cannot yet justify merge.
Conciseness 4/5: the existing verifier is reused with one added diagnosis; no new harness is introduced.
Overall 3.6/5, not a completion claim.
The next improvement is named runtime proof; the user would reasonably reject acceptance without it.

Draft PR #152 is open for this slice.
The graph change audit identifies crash.rs as the only changed source file.
No-mistakes remains uninitialized and was not reconfigured; browser rendering remains unverified.

## Original measurement-table documentation requirement

Head 3af207d also addresses #40's recorded requirement to lead the measurement table with durable and random-access figures.
The canonical `docs/v1-meta.md` in the primary checkout and its committed branch mirror are byte-identical, SHA256 8cac6df1025cfb37fff0ddd8957bc8dedfd55256e0ef928b3eb306a3a34eed1e.
All 17 original metric rows and all measured numeric cells are preserved; only the ambiguous warm-lookup before cell changes from `same` to an explicit description of the historical mixed lookup.
Durable rows now lead, followed by random warm/cold/cache-off lookups, then hot-cache and applied-only figures.
A separate table attributes the critic's 32.8 ms durable create, 24 us random median with 15 to 56 us range, and 44 ms remove_snapshot at 200k inodes to the live issue #40 report at load 28 to 85.
Its repetition count is labeled unspecified, and its different workload/load is not presented as a matched regression or speedup.
The documentation is explicitly historical, not a new benchmark or current-main acceptance claim.
Whitespace checks pass, and the real documentation change was pushed immediately.
Integrated-head runtime for the stricter crash control remains pending.

## Format integration

Merged #148 at 7f7b505 is integrated in pushed head 5be872d.
The merge changed only db.rs by retaining the accepted format-control slice; crash.rs and the measurement documentation remain intact.
New combined-head runtime remains pending.

Current exact integrated head is 5be872ddda7570896a16a99851e0d72a406cd1fc, run 37557242357.
The latest read-only status is in_progress, not a runtime pass.

## Integrated Ubuntu exact-control execution

Run 37557242357's completed Ubuntu job 112586187394 succeeds at exact head 5be872ddda7570896a16a99851e0d72a406cd1fc.
The direct log confirms crash_images_reopen_consistent_and_keep_fsynced_data and the_crash_test_notices_a_missing_store_sync_before_metadata_commits pass.
The latter now requires the specific missing-live-block panic, rather than any failure.
All three retained format tests, Core health propagation and the fixed-cost large-reservation test also pass on the same head.
macOS remains pending, so merge and whole-issue acceptance remain held.

## Verified delivery

Run 37557242357 completed all three checks successfully at 5be872ddda7570896a16a99851e0d72a406cd1fc.
Direct macOS job 112586187039 confirms both named crash tests, all three format tests, Core health propagation and fixed-cost large reservations pass, matching the Ubuntu evidence above.
PR #152 merged with that exact head guard at ca164b8d55cdcc518e6275f4e1a30e27f73a57b0.
Tested CI merge 68eb5ce6df04f9e7ad0fdc67808f07ce6efd1c8d and actual merge both have tree 3127e319804f8ffec0dcd063ce2662efdb06a80c.
This accepts the specific real-Store crash control and original measurement-documentation delivery only.
Physical power loss and remaining whole #40 requirements remain unclaimed.
