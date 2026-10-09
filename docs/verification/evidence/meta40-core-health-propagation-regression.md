# Core metadata-health propagation regression

Refs #40's recorded Core Health integration requirement.

Head 09e2c99 adds one public-API integration regression on main f31c81d3.
No production code is changed yet.
The real Core opens its real Store and Meta, preserving the existing Store-before-Meta sync hook.
Both background threads are disabled so the comparison has no timer or scheduling assumptions.
Two failed Meta sync calls populate the real metadata failure counters and sticky error.
The fixture requires the same error through Core::health and Core::last_flush_error, then requires it to remain visible after a successful retry resets the consecutive-failure counter.
These existing Core polling APIs are used without a hypothetical new field or a model implementation.

## Expected failing baseline

Current Core polling reads only its own last_error and does not read Meta::health.
This is source evidence for the missing propagation, not a runtime reproduction claim.
The test should fail at Core's absent error versus the metadata sentinel after establishing both actual sync failures and exact metadata counters.
The production repair is deliberately withheld until the regression executes and its failure is inspected.
Standalone rustfmt passes; no local Cargo test has run and normal CI is pending.
The fixture does not claim to inject a background panic, perform a power-loss test, or satisfy the remaining Store crash and mutation gates.

## Isolation

The ready pool is full and its fresh-checkout hook seeds Cargo artifacts, so no new slot was created and no cache seed ran.
The clean, completed #145 checkout in READY7 was reused under its existing cowfs-ready43 lease, preserving test/nfs-separate-adapter-namespace-43 at ad004ee and its ignored bench artifacts.
The new branch is test/core-meta-health-40 at main f31c81d3 plus the regression.
No lease return, reset, cleanup, daemon, store, mount, runner operation, workflow modification, or dispatch occurred.
READY3 #142 and READY5 #146 heads remain untouched by this fixture.
The fixed tracker retains 68 items and 43 accepted completions; #40 remains open.

## Self-check

Accuracy 3/5: the fixture uses existing public APIs and the real Store, but run 37551173864 has not yet verified its expected failure.
Completeness 2/5: this is a regression-first step, not the Core health repair or completion of #40.
Clarity 4/5: deterministic Meta-sync failure is explicitly distinguished from background-panic and power-loss evidence.
Actionability 3/5: the test is pushed as draft PR #147, but the production change waits for the actual baseline failure.
Conciseness 4/5: one 54-line test uses existing hooks and adds no framework or public seam.
Overall 3.2/5.
Next is inspection of the exact failing assertion, then wiring the existing Meta health signal into Core without dropping its own error reporting.
The user would reasonably still regard the overall objective as unfinished.

## Executed baseline and first repair

Run 37551173864 completed with failed platform checks at regression head 09e2c993585608d9ef15abc06c84f5fd79488bfa.
The inspected Ubuntu log fails at meta_health.rs line 42: metadata.flush_failures is 0 rather than 2 after two actual explicit sync errors.
It did not reach the Core polling assertion, so this baseline proves missing explicit Meta sync accounting, not yet missing Core propagation.
Commit dd4d63a2e7ea1e1ba1c7a44bee7615337453daf0 records explicit sync success/failure through the existing health helpers.
It adds a separate exact-counter and sticky-error retry test sharing the real-Core fixture, while retaining the Core propagation regression unchanged in substance.
Core production remains untouched until the propagation assertion executes.
Standalone rustfmt passes for both changed files; normal exact-head run 37551773292 remains pending.
No local Cargo build or runner operation occurred.

## Core repair and integrated head

Run 37551773292's completed Ubuntu job passes metadata_sync_failures_count_once_and_an_idle_retry_resets_the_consecutive_count.
The retained propagation regression then fails at line 65: Core reports None versus Meta's before_sync sentinel error.
Commit d271e94 adds the complete cowfs_meta::Health snapshot as Core Health.meta and falls back to Meta's sticky error in both existing polling APIs.
Core's own error remains preferred in last_error; the separate meta field preserves metadata counters and error even when both layers report failures.
The regression compares the entire Meta health snapshot through Core before and after retry, not only an error string.
Local guards are cloned and released before acquiring Meta health locks; the existing lock-audit rows were updated in branch and primary documents.
Standalone rustfmt passes; no local Cargo test was run.
Integrated head a47aaee includes now-merged #146 at a759e6bc without altering its follower assertions or controls.
Latest-head runtime remains pending; the earlier positive accounting result is not acceptance of this combined head.

## Current evaluation

Accuracy 4/5: both failure boundaries were executed, but the complete integrated repair has not passed runtime yet.
Completeness 2/5: #146 is merged and this health repair is pushed, while other recorded #40 controls and whole-item gates remain open.
Clarity 4/5: accounting, propagation, modeled crash coverage, and power-loss proof are distinguished; earlier pending statements are historical.
Actionability 4/5: the concrete repair and exact assertions are pushed, but merge waits for run 37552496145.
Conciseness 3/5: receipts retain prior evidence and corrections, which is longer than a single current-state report.
Overall 3.4/5.
Next improvements are exact-head runtime verification, combined #142 integration, and the remaining existing controls, without starting another review loop.
The user would still regard the complete 68-item objective as unfinished.

## Integrated Ubuntu runtime

Run 37552496145's completed Ubuntu job 112571016303 directly confirms both metadata_sync_failures tests pass on integrated head a47aaee.
The propagation fixture's full Meta Health comparisons before and after retry therefore executed successfully.
The same completed log confirms the existing follower early-return negative control still passes.
Ubuntu and FUSE checks pass; macOS remains pending, so no merge or whole-issue completion is claimed.

## Both-platform runtime and reserved-Core integration

Run 37552496145 completed successfully on all three checks for a47aaeed796c3d6510846679e80799b03548bc3b.
The completed logs directly confirm both named health regressions pass on Ubuntu and macOS.
That runtime tested the earlier main a759e6bc, before the now-merged physical-reservation Core consumer.
Head 8622047 genuinely integrates #142's merge 23cae2e86b7d5d03b477e5d1f9472fa0614d51d6, including its Core and Meta source changes and retained identity, authority, retry, and conformance tests.
The source merge completed without conflicts and was pushed immediately.
The combined head requires new runtime before merge; two separately green source trees are not evidence of their composition.
No workflow, runner, daemon, store, mount, artifact, or lease operation was performed.

## Combined-head Ubuntu runtime

Run 37554090544's completed Ubuntu job 112576156103 passes on integrated head 8622047449c78dbd3f6597421271baa40fd4444f.
The direct log confirms both named metadata_sync_failures tests, foreign-ticket refusal, pre-persist retry, post-persist failure, fixed-cost large reservation, and the follower exact-panic control pass.
The retained Core suites pass seven durability, eight elision, four names/identity, four reserved-identity, and 132 conformance cases.
Ubuntu and FUSE checks pass; macOS remains pending.
This is executed combined-tree evidence for Ubuntu, not both-platform acceptance or whole #40 completion.

## Verified combined-tree merge

Run 37554090544 completed successfully on all three required checks for 8622047449c78dbd3f6597421271baa40fd4444f.
Completed macOS job 112576156385 directly confirms both named health tests, foreign-ticket refusal, pre-persist retry, post-persist failure, fixed-cost large reservations, and the follower exact-panic control pass.
The live/deleted identity and reserved-identity suites each pass four cases, and conformance passes 132 cases.
The CI checkout is merge b6bad1c5900f163d4d208455b00bbe3aae746448, tree 0078c6363dcce9aa1ba50a8f0d51455a2cb98165.
PR #147 merged using the exact-head SHA guard at 707ff62ec79ed1c578ddaafcc7712ece77f096e6, with that identical tree.
This supersedes pending-runtime statements for the delivered health slice.
Whole #40 remains open for its separate remaining crash and mutation requirements.
