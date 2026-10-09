# PR #142 physical reservation durability fixture correction

Refs #42, request 4.

PR #142 implements the Core consumer of store/session-bound one-use inode reservations, preserving physical identity through commit, retry, and reopen.
Large contiguous reservations remain supported without a small-request cap.
The branch remains draft; this document does not claim whole-issue acceptance.

## Executed baseline

Head 17893f6 run 37546880854 completed failure.
The corrected physical-bit fixture passed, with critic2b reporting 27 passed and 1 ignored.
Execution then reached durability.rs and failed two legacy virtual-mark tests, with 4 passed and 2 failed.
Their assertions require virt_mark_renamed and refusal of a virtual-mark directory sync, but Core create now obtains physical metadata reservations and never writes that mark.
The original tests were not valid probes of the current create path.

## Direct correction at 71cd5e6

The public integration test a_physical_reservation_is_durable_before_its_number_is_handed_out checks successful store sync before create returns, the physical bit, and the metadata reservation floor covering the returned number before flush.
It then verifies exact identity, bytes, nondecreasing floor, and a distinct next inode after a fresh reopen.
The public integration test a_physical_reservation_refuses_when_metadata_cannot_be_made_durable injects failure through the existing before_sync hook.
It requires refusal and no visible entry, then successful retry with physical identity, covering floor, exact bytes, and fresh-reopen identity.
These tests do not assume an error proves no metadata state was persisted.

Legacy write-order and failed-directory-sync checks are retained as private unit tests against the actual cfg-test write_virt_mark helper.
They preserve bytes-before-rename-before-directory ordering and failed-sync refusal without claiming a production virtual allocator still exists.
The existing torn-mark and alias unit coverage remains intact.
Legacy mark tests share a unit-test mutex because fsops tracing is process-wide.
No production implementation, public seam, dependency, runner, workflow, store, mount, or lease was changed.

## Verification and remaining gates

Standalone rustfmt --edition 2021 --check passes for both modified files.
No local Cargo execution was performed: the artifact cap and projected-headroom restrictions remain binding.
Normal push-triggered exact-head CI is pending, including the representative physical reservation cases before acceptance of broader results.
Both new physical cases and both relocated legacy cases require actual named runtime results.
Conformance, full identity/retry/crash proof, session limits, and filesystem performance acceptance remain open.
Prior reports remain unchanged.

## Aftercare

The PR body is replaced with this current scope and gate statement, superseding the prior head's runtime status without changing historical receipts.
No images or local-only evidence links are present.
No-mistakes is not initialized in the primary checkout, and no configuration changes were made.
Browser rendering was not verified.
