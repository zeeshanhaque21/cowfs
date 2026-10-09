# PR #145 LOOKUP decoder correction

Refs #43.

PR #145 adds separate-adapter namespace coverage using real Core snapshot views and a MemVfs surrogate.
Both legal serial RPC histories remain the oracle for the concurrent history.
No production defect or new locking requirement is claimed.
Earlier stale-guard illegal-outcome and cross-adapter foreign-handle claims remain retracted.
Each adapter resolves its own opaque handle, and successful exact document byte readback and stable file identity remain mandatory.

## Reproduced fixture defect

The macOS job 112548418037 in run 37545504784 failed on head a8b8941 because the successful mkdir-first serial history reported a regular-file sidecar channel with failed byte assertions.
LOOKUP returns a bare handle followed by optional attributes, whose first word is the attributes-present flag.
The fixture's skip_and_kind consumed that flag as the file type, reporting 1 for a real directory.
The extracted original parser fails a directory reply with exit 101: actual type 1, expected type 2.

## Correction

Commit ad004ee consumes the flag before decoding the attributes and returns None when attributes are absent.
The permanent regression covers directory, regular-file, absent attributes, and exact cursor alignment.
The extracted exact helper and permanent test pass 1/1 with exit 0.
Restoring the original flag-as-type parser makes that same test fail with exit 101: Some(1) versus Some(2).
Standalone rustfmt --edition 2021 --check passes.
These are parser-level checks, not a full Core/NFS integration run.
The correction changes only crates/cowfs-daemon/tests/separate_adapter_namespace.rs.
No document byte, identity, channel-readback, or junk-refusal assertion was removed or weakened.

## Remaining gates

Normal push-triggered exact-head CI is pending.
The daemon integration fixture is macOS-gated, so Ubuntu success cannot establish its execution.
Real-Core serial/race runtime acceptance, other #43 obligations, and whole-item completion remain open.
No local Cargo build, runner modification, workflow dispatch, rerun, artificial trigger commit, resource cleanup, or lease operation was performed.
Historical receipts remain unchanged.

## Aftercare

The prior PR body was inspected after push and its stale pending-head statement is superseded by this correction.
No evidence images or local-only image links are used.
The no-mistakes status command reports that the primary repository is not initialized; it was not initialized or reconfigured.
Browser rendering was not verified.
