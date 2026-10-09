# Issue #43: real-Core serial-oracle correction (PR #145)

## Prior claim retracted
The earlier PR #145 receipt claimed an "illegal outcome" RED "deterministically across five runs" from an observer line `guard_saw_main=false main_at_mutation=true real_dir_took_the_name=true doc_regular=true`.
That claim is **wrong and retracted**.
`main_at_mutation` is a third-party probe injected into the `Vfs` wrapper at one instant, not the result of any operation in the history.
A `mkdir(._doc)` request legitimately spans `[guard read, mutation]`, so a concurrent `create(doc)` may land inside that span while A is still ordered before it at A's guard read.
The observed final state `{._doc = real dir, doc = regular file}` equals the **mkdir-first serial product**, so it is legal.

## Actual oracle
Both files now run the two actual serial histories (`mkdir(SIDE)` then `create(MAIN)`, and `create(MAIN)` then `mkdir(SIDE)`) and the controlled race through the same topology.
The race must equal **at least one actual serial product** on all user-observable fields: exact RPC statuses, returned-handle presence, final `ftype` of `doc` and `._doc`, within-run `fileid3` identity and stability, `doc` bytes cross-adapter, and valid AppleDouble channel bytes with junk refused `NOTSUPP` non-destructively.
`depth`/`peak`/`overlap` and `guard_saw_main`/`main_at_mutation` are diagnostics only, never acceptance constraints.
No numeric `fileid` is compared across independent store fixtures.

## Actual status
At full head `cdc3a55703d3f045fae3adf14a1b847b42cc0a59`, runtime is **UNEXECUTED**: CI run `37544295704` was `in_progress` at write time, and no completed log for this head exists.
No counterexample was shown and no production bug is proven; the source hypothesis (guard read outside `SnapCtx.ns`) is not reproduced at runtime here.

## Fixes in this commit
- `crates/cowfs-daemon/tests/separate_adapter_namespace.rs:81`: `manual_is_multiple_of` clippy error (failed macOS CI `37541923711` before `cargo test`) fixed with `is_multiple_of`, MSRV-consistent.
- Daemon `fileid` helper corrected to read `fattr3.fileid3` (word 13) not `fsid` (word 11, a constant `FSID`), so the shared-namespace premise compares real inode identity.

## Remaining #43
Test-only, no production change, no new lock or global serialization.
Relative to the #43 requirement (root/view and separate-adapter topology safety only): the topology is asserted; the race is shown legal; no defect is established, so no production fix is warranted from this evidence.
