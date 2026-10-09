# #40 M4 and M6 public content regressions

This addresses the original empty-splice and zero-length-ChunkRef requirements in #40, not a new feature.
Branch `test/meta-content-edge-regressions-40` starts from verified main 7f7b50595a464fd2de7257d5ad385fc31673556f.
Commit 2b89bed adds only `crates/cowfs-meta/tests/content_edges40.rs`.
No production fix is made before the public-API regressions execute and reproduce their intended failure.

## Real sample and requirement mapping

Both tests use a real redb metadata file, public Meta/Snapshot/Tx APIs and an eight-byte file with one valid block reference.
The fixture does not use a model transaction or mutate internal state.
It records actual content version, attributes and chunk list before the invalid operation.

`an_empty_splice_inside_a_chunk_is_refused_without_changing_the_file` calls splice_content with start=end=3 inside that eight-byte chunk.
It requires Invalid rather than a successful version bump, unchanged attributes/version/chunks, valid invariants and retained version/chunks after graceful close and reopen.
It also retains a positive legal empty-splice check at the covered-end boundary.

`a_zero_length_ref_before_a_real_splice_ref_cannot_silently_collapse` submits a zero-length ref before the valid replacement at the same offset.
That targets offset-map collapse, including the case where the valid replacement matches existing content and skips encoding entirely.
The intended assertions cover zero-length block refs and hole refs, both splice_content and set_content, unchanged version/attributes/chunks, invariants and close/reopen readback.
The same replacement without the invalid ref remains a positive baseline.
The checks after the first expected refusal are not claimed executed until the fixture passes.

## Source observations, not runtime findings

The inspected current Tx::splice_content boundary check treats an empty extent result as on_boundary and compares its zero length with end-start.
The new_chunks offset map can overwrite an earlier ref when it has zero length.
ChunkRef::validate in the Store crate currently checks hole/id/maximum-length combinations, not zero length.
These observations motivate the exact public tests but are hypotheses about runtime behavior until their named assertions execute.
The graph snippet for Tx::splice_content returned mismatched surrounding source, so direct rtk source inspection was used rather than trusting stale graph offsets.

## Validation and limits

Standalone rustfmt and whitespace checks pass after formatting the test array.
Attr derives PartialEq/Eq, and the tests use the existing public content_version/chunks APIs.
No local Cargo, mounted filesystem, runner operation, cleanup, shared store change or lease action occurred.
The authorized focused sample is `cargo test -p cowfs-meta --test content_edges40` before any expanded workload.
Require actual runtime failure at the intended assertions before a production fix, then both-platform green results and tested/merged-tree equality before accepting delivery.
Source-only confidence is not reproduction, and this fixture is not whole #40 acceptance or physical-crash proof.
The fixed tracker remains 68 items with 43 accepted.

## Delivery self-check

Accuracy 3/5: source and formatting checked; runtime and compilation are not yet verified.
Completeness 2/5: the original M4/M6 conditions have runnable regressions, but no reproduced result or production correction yet.
Clarity 4/5: intended assertions and positive baselines are explicit; later assertions remain unreachable until the first check passes.
Actionability 4/5: one focused test target and exact acceptance sequence are supplied; CI still must execute it.
Conciseness 4/5: two public fixtures reuse current APIs with no dependency or production abstraction.
Overall 3.4/5 for test delivery only.
The next improvement is actual public-API reproduction, then the narrow responsible-layer fix with retained reopen and legal-boundary checks.
The user would reject a completion claim without that evidence, so no issue is closed.

## Public reproduction and narrow fix

PR #153's first run 37558676198, Ubuntu job 112590729866, compiles and passes workspace fmt/clippy, then executes both public tests and fails.
At original head 2b89bed46fee591df3a6340f36bdef2ad26cfe10, the empty interior splice returns `Ok(2)` at test line 15 and the zero-length block preceding a real ref returns `Ok(2)` at line 51.
Those are the intended runtime failures, not compilation failures or unrelated setup panics.
Later unchanged-state and reopen assertions did not run in those failing tests.

Merged #152 was integrated cleanly in head 64212d3, changing only the existing crash test and measurement documentation.
Commit 1c3e48b then fixes the reproduced defects in Tx::set_content and Tx::splice_content.
Both reject zero-length refs before any content mutation or offset deduplication, and an empty interior splice requires an actual chunk key at its start.
The covered-end boundary remains legal, and the fixture also retains a positive empty splice at the initial chunk boundary.
The fix adds fourteen production lines and three positive-baseline test lines, with no dependency, new abstraction, reservation policy change or format change.
Standalone rustfmt and whitespace checks pass; the fix was pushed immediately.
Both-platform fixed-head runtime and tested/merged-tree equality remain pending before acceptance.
No-mistakes remains uninitialized, PR bodies have no images, and browser rendering is not verified.
