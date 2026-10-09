# PR #145 verified coverage merge

Refs #43.

PR #145 merged at f31c81d3e2ade77e99904eee61c947f9baf12006.
Its head is ad004ee21cb202ec518c30e93ba7308c8a92d174.
Run 37547263126 completed success on Ubuntu, macOS, and linux-fuse.
The actual merged tree is 37615b967f4b2a9e0b61531cc980c6f4cf889ebc, exactly the tree tested by CI merge commit c0c51239d38a6e6b8da7c727243e74a31237e08a.
Both commits have parents 01fa855fc3521c519e8a93fe0b867dc56ebfdc5d and ad004ee21cb202ec518c30e93ba7308c8a92d174.

## Named runtime proof

The completed macOS log reports all five daemon integration tests passed:

- lookup_post_op_attr_decodes_kind_after_the_present_flag
- the_core_root_facade_is_read_only_for_make
- the_two_adapters_share_one_snapshot_namespace
- the_sidecar_channel_round_trips_valid_bytes_and_refuses_junk
- two_adapters_over_one_snapshot_namespace_match_a_serial_product_under_the_race

The daemon fixture is macOS-gated and ran zero tests on Ubuntu; Ubuntu success is not presented as real-Core daemon proof.
Both platforms report two NFS surrogate tests passed: the_two_adapters_share_one_namespace and two_adapters_over_one_namespace_match_a_serial_product_under_the_race.
Formatting and all-target clippy succeeded in both check jobs.

## Accepted slice and remaining scope

The slice adds real-Core separate-adapter serial-oracle, own-handle byte/identity, valid channel, and LOOKUP decoder coverage.
Earlier illegal-outcome and foreign-handle proof claims remain retracted.
No new production locking policy or demonstrated production race defect is claimed.
The final decoder correction was checked directly, including an extracted exact regression that passes corrected and fails with the old decoder.
Earlier independent wbuddy source reviews remain evidence for the unchanged oracle and owned-handle checks, not proof that their then-pending runtime had passed.
Other #43 critic, security, dead-server, Store-mode, mounted conformance, and warm-build obligations remain open.
The authoritative tracker remains 43 completed items out of the fixed 68; this slice does not close #43.

## Resource and aftercare boundary

No local Cargo build, runner modification, workflow dispatch, rerun, artificial trigger commit, mount change, store cleanup, or lease release was performed.
The small extracted parser artifacts were placed under READY7 bench/out/lookup-post-op-attr-ad004ee, with moved binary checksums verified.
The primary checkout's existing dirty files were not reset or overwritten.
The codebase change detector was run after merge; it reflects the local primary checkout, not a claim that its older HEAD has been fast-forwarded.
The current PR body contains this receipt, with no images or local-only image links.
Browser rendering was not verified.
