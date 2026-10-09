# Clock and Core rename merge receipt

## Delivered

PR #139 merged reviewed head `5e89c3abde3cceebf7f930784fe7af3f45a7c166` as `08557fbed127df29340818069db5020203915265`.
PR #141 merged reviewed head `4b3daed988faf2dcd63d16a8c2b9aa6140b1d5a4` as `89353e17e5085000711dc428e834f9cc41840a1f`.
Both merges used the GitHub merge API with the full reviewed SHA and `merge_method=merge`.
PR #141 left draft only after independent source, regression, and exact-head CI-log review.
Its head remained unchanged after leaving draft, and all three head checks were read back as completed/success before merging.
Remote main was read back as `89353e17e5085000711dc428e834f9cc41840a1f`.
Issue #42 was read back as open/reopened immediately after each merge.

## Evidence and limits

Independent final CI-log audit: `docs/reviews/meta42-final-ci-gates.md`, SHA256 `c33d6ea19675ddeb25c208a52309ac98765c47d3bc0509c2fe0f810fab087ebc`.
Run `37394945153` tested #139 integrated into `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`, including the permanent clock regression on Ubuntu and macOS.
Run `37394623027` tested #141 integrated into that same base, including all ten atomic-rename cases on Ubuntu and macOS.
The audit records ignored tests, skipped tests, and FUSE annotations separately from passes.
The combined tree was checked for conflicts and preservation of both changes, not compiled or executed as a combined tree.
These merges do not establish mounted performance, physical GC reclamation, crash acceptance, or complete POSIX acceptance.
The audit and this receipt exist in the primary checkout but are not yet committed or published.

## Remaining #42 work

PR #140 remains draft and held at `1214142ffc17b1fedc3b31d3a6f2a343aa7e8d36`.
Its reservation contract, executed commit-failure proof, and Core selected-id consumer transition remain incomplete.
No one-block count restriction or recovery-format change is approved by these merges.
The fixed 68-item scope and current tracker counts remain unchanged.
No local builds, cleanup, runner changes, workflow dispatches, reruns, lease changes, or shared-resource changes were performed.
The primary checkout was not reset or advanced over its existing dirty documents.
