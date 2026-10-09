# PR145 serial-oracle revision: independent review (final)

PR #145 `test(nfs): separate-adapter namespace regression for #43`; branch `test/nfs-separate-adapter-namespace-43`; draft.
Exact head audited: `cdc3a55703d3f045fae3adf14a1b847b42cc0a59` (== `refs/pull/145/head`), tree `88255a397f31591645df1f0f6de79a2879a62890`, parent `fc98b42`, merge-base/base with `main` `1580e69b`.
Source read from the actual git blobs at HEAD (not a stale index): daemon sha256 `bc25de5c4c6bb7`, nfs sha256 `0d12334485943b`.
Changed: exactly 2 files, 985 + 401 insertions, test-only, no production source, no manifest. `main` = `01fa855fc3521c519e8a93fe0b867dc56ebfdc5d` unchanged; primary HEAD `9874afae` unchanged.

Verdict: SOURCE STRUCTURE VALID, one weak oracle field; RUNTIME PENDING (no completed CI at this head).

Runtime (one bounded query of run `37544295704`, no poll/retry/dispatch).
`status=in_progress`, `conclusion=null`, `head_sha=cdc3a55`. Jobs: `linux-fuse` completed success; `check (macos-latest)` and `check (ubuntu-latest)` both in_progress.
No completed macOS or ubuntu log exists for this head, so the four daemon tests and the two NFS-surrogate tests have NO CI observation of their named results, `REAL-CORE`/`SURROGATE` lines, fmt, or clippy.
The daemon file is `#![cfg(target_os = "macos")]`, so ubuntu `cargo test --workspace` never compiles it; macOS is the real gate and it is unrun. No claim that a daemon test ran on Linux is made or supported. PENDING.

Serial-oracle structure (source).
Both files run three scenarios through one topology: `mkdir(SIDE)` then `create(MAIN)`, `create(MAIN)` then `mkdir(SIDE)`, and the controlled race (daemon holds adapter A at its `SIDE` mutation after its `MAIN` guard read answered, via `GuardedView`).
The race is accepted iff it equals at least one actual serial `Observation` on every significant field: exact RPC statuses, returned-handle presence, final `ftype` of `doc`/`._doc`, within-run `fileid3` identity stability, and the sidecar channel bytes.
`depth`/`peak`/`overlap`, `guard_saw_main`, `main_present_at_mutation` are diagnostic-only (`eprintln` at daemon:808-814); no assertion reads them. The previously retracted illegal-outcome assertion on `main_at_mutation` is gone - not resurrected. Comparison is `diff()`-based on a fixed field list, no fallthrough, no empty-observation always-match.
Failed paths are not silently equal: `read`/`lookup` return empty on non-OK but the fields test `st == OK`, so a read error yields a real `false`, not an empty-pass. No numeric `fileid` compared across independent store fixtures.

Correctness of the two source fixes.
`fileid` decodes `fattr3.fileid3` at word 14: `skip_fattr_words(&mut r, 13)` then `ru64` reads words 14-15 = `fileid`; skipping 11 would read words 12-13 = constant `fsid`. Verified against the vendored `crates/nfsserve/src/nfs.rs` `fattr3` order and the 21-word total. Big-endian throughout (`to_be_bytes`/`from_be_bytes`).
`Args::opaque` uses `is_multiple_of(4)` (daemon:87). Workspace MSRV `1.89` (`crates/cowfs-store/Cargo.toml`); `is_multiple_of` for ints stabilized `1.87`, so it satisfies MSRV.

Weak oracle field (non-blocking, source-evidenced).
`Observation::main_roundtrip` (daemon:767-773) writes `doc` through adapter A then reads it back through adapter B using A's handle. Each `Adapter::new` mints a fresh random BLAKE3 handle key (`HandleCodec::new` -> `random_key`, `crates/cowfs-nfs/src/handle.rs`), and `Server::start` builds its own adapter, so `decode` on B rejects A's handle with `NFS3ERR_BADHANDLE`. The field is therefore always `false` in all three runs - it can never distinguish and cannot cause a false PASS, but the "`doc` bytes cross-adapter" oracle dimension the PR body and evidence doc claim is not actually exercised. The shared-namespace premise is instead proven correctly by `the_two_adapters_share_one_snapshot_namespace`, comparing `fileid` across adapters with each server's own handle. Recommend either dropping `main_roundtrip` from the claimed oracle list or reading back with a handle from B's own `lookup`.
Static claim only: resolving it needs the macOS run.

Root-facade-vs-view coverage.
`the_core_root_facade_is_read_only_for_make` pins `Core::mkdir(ROOT_INO)` refused while `snapshot_view().mkdir(ROOT_INO)` succeeds, so the fixture cannot pass on a name landing where the product never writes. This asserts the real facade/view public API, not a `ReadOnly` restatement. It covers the #43 first bullet (`._name` as a real file when no main file; sidecar whole-file refusal) at the topology level; whole-#43 (critic, security, dead-server, Store mode, conformance, warm build) remains open and is not claimed.
Scheduling bounds exit on failure, not just timeout (`wait_reached`/`wait_peak`/`hold_if_armed` all `Instant` deadlines), no lock is required; unrelated directories are not globally locked. `is_multiple_of` is the only build-affecting change from the prior RED clippy head.

Scope and compliance.
Fixed issue-68 scope; no new lock, no production change, no global serialization, no scope growth. Prior RED head `37541923711` failed clippy `manual_is_multiple_of` before `cargo test`; that fix is present. 8 GiB cap / 20 GiB floor: projected compile-peak preflight UNVERIFIED; the old preflight is not retroactively compliant and free-space observations are not a projected peak. The dirty primary (`docs/v1-core.md`, `progress/*`) is preserved, not touched.

Verdict.
- Source: valid serial-oracle structure; `fileid3` word-13 decode and `is_multiple_of` MSRV fix correct; retraction honored.
- Source weakness: `main_roundtrip` is always-false (per-adapter handle key); non-blocking, no false PASS, misdescribed as an exercised oracle field.
- Runtime: PENDING at `cdc3a55` - run `37544295704` in_progress, no completed macOS/ubuntu log; daemon tests unexecuted in CI.
- Merge-ready: NO - this is a coverage slice only; whole #43 stays open, and the macOS gate has not produced a completed result.
