# PR145 own-handle byte final review (wbuddy)

PR #145 `test(nfs): separate-adapter namespace regression for #43`; branch `test/nfs-separate-adapter-namespace-43`; draft, unchanged.
Exact head `a8b8941e7a765b6c8a16ef45dda7bd25ecb9d850` == `refs/pull/145/head` (HTTPS-verified); tree `e9914f3bc286924e52b7def6490779ada721ed1b`; parent `cdc3a55703d3f045fae3adf14a1b847b42cc0a59`; merge-base `93cfef94`.
`main` = `01fa855fc3521c519e8a93fe0b867dc56ebfdc5d` unchanged; primary HEAD `9874afae` unchanged.
Changed from parent: exactly 1 file, `crates/cowfs-daemon/tests/separate_adapter_namespace.rs`, +108/-21, test-only.
The NFS surrogate `crates/cowfs-nfs/tests/separate_adapter_namespace.rs` is UNCHANGED at this head.

## Byte correction (source)

Real fix. Old `main_roundtrip` wrote `doc` through A's handle then read through B using A's handle; each `Adapter::new` mints its own BLAKE3 key, so B rejected A's handle with `NFS3ERR_BADHANDLE` and the field was always false in all runs.
New `main_cross_adapter_bytes` (daemon:766-780): `ca.lookup("doc")` and `cb.lookup("doc")` each OK with non-empty handle -> A writes via A's handle, B reads via **B's own** handle, and `ca.fileid(&a_fh) == cb.fileid(&b_fh)` forces one shared inode id. Correct; a per-adapter cache cannot pass.

## Serial-oracle false-green analysis

`Observation::diff` (diff-based, not equality of a mixed struct) compares bools including `main_cross_adapter_bytes` and the two `Option<bool>` channel fields; the race must match **at least one** actual serial product on every field.
The old always-false coincidence is closed: the main test asserts the successful serial product (`create_status == OK` branch) has `main_kind_rpc == Some(FTYPE_REG)` AND `main_cross_adapter_bytes` TRUE AND `main_identity_stable` TRUE, and separately asserts `race.main_cross_adapter_bytes` TRUE and `race.main_identity_stable` TRUE. With the byte proof TRUE in the legal product, `false == false` (old oracle) can no longer pass.
MAIN-absent exemption is sound: the exemption only applies to the failed-mkdir serial product, which by construction is not the `create_status == OK` product selected for the TRUE assertions; the race's `main_kind_rpc == Some(FTYPE_REG)` assertion blocks a MAIN-absent race from matching vacuously.
No skipped match arm leaks: `_ => false` only fires when a `lookup("doc")` is non-OK or handle-empty, which is exactly the MAIN-absent case, and that case is excluded from the TRUE assertion set.

## Boolean channel oracle (`None` vs failure)

Inspected against the stated risk (all BADHANDLE/IO converted to `None`).
`channel_roundtrips`/`channel_refuses_junk` are `Option<bool>`; `None` arises **only** from the no-channel branch (`side_kind_rpc != FTYPE_REG`, the `else` at daemon:816-818), matching the serial histories when `._doc` is a real directory.
A live channel whose valid write fails yields `Some(false)` (`side_write_st.map(|st| st == OK && ...)`), NOT `None`. A failed read path yields `None` for `side_got`, which makes the mapped `Some(bool)` **false**, still not hidden.
`channel_refuses_junk` requires `st == NOTSUPP && side_got == blob && side_recovered`; junk is refused non-destructively with the good bytes intact. Residual gap (coverage, not false PASS): if a race lacks a live channel while its matching serial product also lacks one, both sides are `None` and the channel dimension is unexercised that run; the standalone `the_sidecar_channel_round_trips_valid_bytes_and_refuses_junk` covers the live case directly.

## Actual runtime and tree

One bounded read-only CI query of the designated run `37545504784` at this exact head: `status=in_progress`, `conclusion=""`. Jobs `check (macos-latest)`, `linux-fuse`, `check (ubuntu-latest)` all `in_progress`; no log exists. No completed macOS or ubuntu log for this head, so the four daemon tests and the two NFS-surrogate tests have NO CI observation of their named results, no `REAL-CORE`/`SURROGATE` lines, no fmt, no clippy. PER BINDING: no completed run -> STOP, no poll/wait/dispatch/rerun. Runtime PENDING.
The daemon file is `#![cfg(target_os = "macos")]`; ubuntu `cargo test --workspace` never compiles it, so macOS is the real gate and it has not produced a completed result. No Linux daemon-test claim is made or supported.

## Verdict

- Source: byte correction valid; `main_cross_adapter_bytes` now exercises the real cross-adapter byte dimension through owned handles and shared `fileid`; the always-false oracle field and its `false == false` pass are closed; boolean channel oracle does not hide failed writes/shortwrites/errors as `None`; failed-write and junk cases map to `Some(false)`.
- Runtime: PENDING at `a8b8941`; run `37545504784` in_progress, all three jobs in_progress, no completed log.
- Merge-ready: NO. Slice scope only, no completed CI at this head; whole #43 (critic, security, dead-server, Store mode, conformance, warm build) stays open.
- Missing to reach MERGE_READY: a completed macOS `check` run at this exact SHA showing the four daemon tests pass with `REAL-CORE` lines, plus the two SURROGATE tests and fmt/clippy green; all other #43 obligations remain open.
