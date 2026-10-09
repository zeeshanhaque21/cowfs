# Metadata format-guard controls for #40

## Scope and pin

Commit fc2ae75 on test/meta-format-control-40 adds the recorded missing-magic-check control.
The clean, completed #146 READY5 checkout was reused under its unchanged cowfs-ready40 lease, preserving its old branch and ignored artifacts.
Base is verified main a759e6bc714388045146b4eb8e37fb82e64ab00b.
READY3 #142 and READY7 #147 were not edited.

## Runnable acceptance

- absent_and_wrong_metadata_magic_are_refused tests both a missing key and a wrong value in the real metadata table.
- header_verifier_rejects_the_real_missing_magic_check_control uses the same fixture and verifier with only the actual open-time magic guard disabled.
- header_verifier_rejects_the_real_wrong_magic_check_control repeats that control with a present but wrong key.
- Both negative tests require the exact panic header verifier accepted an invalid cowfs-meta magic, not any panic.

Each fixture first creates, closes, reopens, checks, and closes a healthy real redb file with a named snapshot.
Before changing the fixture header, a transaction verifies its original MAGIC value.
Only that header key is altered, preserving version, allocator floors, snapshot tables, and tree content.
An unrelated open error is rejected with a different panic and cannot satisfy the negative tests.
An improperly accepted open must still resolve the original named snapshot and close successfully before the exact verifier panic.
The private test-only thread-local switch is restored by a Drop guard immediately after open, including unwind paths.
Non-test builds keep the original magic comparison and refusal unchanged; no public fault seam is added.

## Evidence limits

Standalone rustfmt passes.
No local Cargo build or test executed because the artifact and headroom restrictions remain in force.
Normal CI must execute all three named tests on both platforms before acceptance.
This is compiled in-process guard-removal coverage, not an external source-mutant rebuild.
The remaining reap-durability and snapshot/inode-limit controls are separate existing requirements, not satisfied by these tests.
The fixed tracker remains 68 items with 43 accepted completions; whole #40 stays open.

## Delivery self-check

Accuracy 3/5: the actual guard is controlled and the exact panic excludes unrelated errors, but the new runtime has not executed to completion.
Completeness 2/5: one recorded control is implemented; other controls and the full goal remain unfinished.
Clarity 4/5: the receipt distinguishes compiled in-process control from an external mutant rebuild.
Actionability 4/5: draft PR #148 contains all three automatic tests; exact-head CI is the next gate.
Conciseness 4/5: one file uses existing redb and test infrastructure without a public API.
Overall 3.4/5.
The user would still regard the complete objective as unfinished.
The PR body is verified through the API; it has no images, and browser rendering is not claimed.
No-mistakes remains uninitialized and was not reconfigured.

## Executed Ubuntu control and physical-Core integration

Run 37553155693's completed Ubuntu job 112573153213 directly confirms all three named tests pass at fc2ae750d92b7da5d2eae9ced1c20f8080eb4c48.
Both negative controls execute with their exact expected verifier panic, while the baseline refuses both altered headers.
Head 491cd2b genuinely integrates now-merged #142 at 23cae2e86b7d5d03b477e5d1f9472fa0614d51d6.
The merge completed without conflicts and preserves the control switch, tests, and production header refusal.
Joint-head runtime is pending; earlier control execution is not evidence for this new combined tree.
No runner operation, workflow change, dispatch, local Cargo run, cleanup, or lease return occurred.

## Merged-health integration

Head 9b0f885 integrates #147's verified merge 707ff62ec79ed1c578ddaafcc7712ece77f096e6, including Meta sync accounting, full Core health propagation, and its two tests.
The source merge was conflict-free and pushed immediately.
The new combined tree requires its own runtime acceptance; previous control execution remains historical evidence only.

## Integrated Ubuntu execution

Run 37555619010's completed Ubuntu job 112581048044 passes on head 9b0f885c9e7365fb7dcfedb2109de7f07c8023c0.
The direct log confirms the positive format-refusal test and both exact-panic guard-removal controls pass.
Both named metadata_sync_failures tests and the fixed-cost large-reservation test also pass on this same integrated head.
Ubuntu and FUSE checks pass; macOS remains pending, so merge and whole #40 acceptance remain held.

## Verified delivery

Run 37555619010 completed all three checks successfully.
macOS job 112581048020 directly confirms all three named format tests, both metadata_sync_failures tests, and the fixed-cost large-reservation test pass.
The equivalent Ubuntu named results are recorded above.
PR #148 merged with exact head guard 9b0f885c9e7365fb7dcfedb2109de7f07c8023c0 at 7f7b50595a464fd2de7257d5ad385fc31673556f.
CI merge 75b32464335e00d13b3ed192ef94264aae7fd5b5 and the actual merge both have tree 3719beb178dffdea190835adf0a5c91eb568699b.
This accepts the format-control delivery only; remaining whole #40 obligations stay open.
