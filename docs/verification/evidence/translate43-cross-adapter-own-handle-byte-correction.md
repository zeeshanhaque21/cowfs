# #43 cross-adapter byte proof: owned-handle correction

Branch `test/nfs-separate-adapter-namespace-43`, draft PR #145.
Prior head `cdc3a55703d3f045fae3adf14a1b847b42cc0a59`; new head `a8b8941e7a765b6c8a16ef45dda7bd25ecb9d850`.
File: `crates/cowfs-daemon/tests/separate_adapter_namespace.rs` (test only).

## Exact bug
`Observation::main_roundtrip` wrote `doc` through adapter A and read it back through adapter B using **A's** handle.
Each `Adapter::new` mints its own BLAKE3 handle key, so B rejects A's handle with `NFS3ERR_BADHANDLE`.
The field was therefore **always false** in all runs; a `false == false` race-versus-serial equality could pass while the "bytes cross-adapter" oracle dimension was never exercised.

## Fix
- Byte proof resolves `MAIN` through **B's own** root: B holds B's valid handle for the shared inode.
  A writes through A's handle; B reads through B's own handle; the two adapters must agree on the shared `fileid`.
- Sidecar oracle reads back through B's own handle when `._doc` is a live channel; the channel oracle now compares booleans only, so an absent channel reads as `None` rather than a raw RPC failure that falsely diverged.
- Race test asserts the byte/identity oracle fields are **TRUE** in the successful serial history and in the race, so the equality can no longer pass on `false == false`.

## Bounds honored
No global handles; no production `Handle` change; cross-namespace is not cross-capability.
Existing false bug retraction, legal serial histories, and diagnostic observer preserved; no new lock, no global serialization, no new feature.

## Assertions
- `main_cross_adapter_bytes` TRUE in the successful serial product and in the race.
- `main_identity_stable` TRUE in the successful serial product and in the race.
- Sidecar channel round-trip TRUE where the channel exists; junk refused `NOTSUPP` non-destructively.

## Runtime
UNEXECUTED. CI for `a8b8941`: run `37545504784` auto-triggered on push, in progress; no completed macOS/ubuntu log.
Prior-head run `37544295704` in progress, no log available. macOS is the real gate (file is `#![cfg(target_os = "macos")]`).
