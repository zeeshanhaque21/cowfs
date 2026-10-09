# Independent targeted review: PR #78 against production head f501157

Reviewer: fresh native Sonnet 5.5, read-only on tracked source, harness and docs.
Verification head: `70d602b7cb98e31f01ca5520a380512b48977451` (branch `verify/gc-daemon-e2e`, slot 14).
Production source head: `f5011574383dbe4d65bd2a5995cfff9b2d168b3d` (PR #76 head, `crates/` tree `fe899f9f425d40d5db36d13c8400ef3438676879`).
Earlier reports (`gc-daemon-e2e.md`, `gc-daemon-e2e-repairs.md`, `gc-daemon-final-source.md`) are preserved untouched.

## Verdicts (kept separate)

- **Artifact and end-to-end behavior at `70d602b`, scope = quiescent 16 MiB-pack actual reclaim through the unmodified daemon and CLI: PASS.** All numbers were re-measured on freshly built `f501157` binaries, not carried from the earlier PASS.
- **Production source safety: NOT CERTIFIED here.** The `NoSuchSnapshot` skip, the concurrent-vanish race and the old-cache upgrade are Sonnet 13's scope. My fixture is quiescent and cannot exercise them.
- **CI at exact head `70d602b`, one check:** `check (ubuntu-latest)` success, `check (macos-latest)` success, `linux-fuse` success.
- **Merge gate: not met.** PR #76 is open at `f501157`; PR #78 still contains its changes against `main`. Retarget or rebase to verification-only after PR #76 merges. No action taken.

## Source and ownership identity

- `git diff f501157 70d602b -- crates Cargo.toml Cargo.lock .github` is empty. `HEAD:crates` and `f501157:crates` are both `fe899f9f425d40d5db36d13c8400ef3438676879`.
- I exported `git archive 70d602b` and `git archive f501157 crates Cargo.toml Cargo.lock .github` and ran `diff -r`: `crates/`, root manifests and `.github` byte-identical.
- The branch adds exactly 7 files over `f501157`: `docs/v1-daemon.md`, `docs/verification/gc-daemon-e2e.md`, `scripts/verify-gc-daemon.py`, and four under `scripts/gc-fixture-seed/`. No `crates/` file is touched by any owned commit.
- **Commit count correction:** there are **9** owned commits above `f501157`, not 8 commits against 7 patch-ids. None is empty (each has a patch-id). The four harness/seed commits keep the patch-ids I verified before: `75c0c50` `5948b57c`, `99913e3` `dadd6a64`, `556fef8` `2163d2d1`, `b21f5aa` `2f26493b`. Five commits are docs-only: `3d0709b`, `b3ec93f`, `435b82d`, `7c08311`, `70d602b`.
- Harness script blob `e5e275a550a4f0cfa6de7951dee673d27cb0647c` and seed tree `fb66253ec3d9` are identical at `095c1a6` and `70d602b`; `git diff 095c1a6 HEAD -- scripts` is empty. The script sha256 `a5125907...` matches the doc.
- `7c08311..70d602b` changes only `docs/verification/gc-daemon-e2e.md`. The builder's run used harness `7c08311`; `70d602b` is a docs-only commit and its own run is not claimed.

## What `f501157` changes in production (static)

`89b202c..f501157` touches 7 files, all under `crates/cowfs-gc` and `docs/`: `src/lib.rs` (+43 lines non-test), `tests/common/mod.rs`, `tests/core_reclaim.rs`, `tests/race.rs`, `tests/regressions.rs`, `docs/gc-core-integration.md`, `docs/gc-race-bounds.md`. No `Cargo.toml`, `cowfs-core`, `cowfs-store`, `cowfs-meta`, `cowfs-daemon`, `state.rs`, `gate.rs` or `fsops.rs` file changed, so the earlier feature-graph and `cfg(test)` gate proofs carry by byte identity. Semantics: `live_blocks_with_root` returning `NoSuchSnapshot` is now skipped (one error only; all others still stop the cycle). I did not verify that logic's safety.

## Build and binary identity

- Fresh archive built with `cargo build --locked --offline -p cowfs-cli -p cowfs-daemon`: 3m26s, exit 0. Seed built `--release --locked --offline`: 45s, exit 0. Own target dirs under `bench/out/gc-snapshot-fix-critic/archive/`; no builder target reused.
- My binaries: `cowfs` `cb1b6413...`, `cowfs-daemon` `39e91ed4...`, seed `3ee983ac...` (path-dependent hashes).
- Builder slot binaries `cowfs` `fcc58209...` and `cowfs-daemon` `bbf27848...` match the doc and the run record. The daemon mtime (09:55:24) is newer than every `crates/**/*.rs` (0 newer files). The build-head label stays a proxy.
- Daemon string profile identical in both builds: `COWMARK2` 1, `COWMARK1` 0, `C7D_EXIT` 0, `set_gate_fault` 0, `remove_barrier` 0, `test-hooks` 0, `injected sync fault on` 1.

## Independent runs (mine)

**1. Small private deliverable first** (`run1`, 14 records, 0 failed): unmodified daemon defaults (`--store --mount --socket --backend core` only), fresh socket under `/private/tmp`, NFS loopback, 16 MiB-pack preseed.

| Item | Value |
|---|---|
| Seeded packs | `pack-00000000.cpk` 16,759,743; `pack-00000001.cpk` 12,205,006; total 28,964,749 |
| Expected digests | regenerated in Python from the fixture LCG; equal to the seeder's declared digests for all 4 files |
| Dry run | `candidate_blocks` 368, `candidate_bytes` 15,136,450, `freed` 0; sha256 of all 7 store files before vs after: 0 changed; one-byte sensor control detected |
| Live gc | `freed_blocks` 202, `freed_bytes` 16,759,743 (gross); `pack-00000000.cpk` unlinked; new `pack-00000002.cpk` 1,623,293 |
| Net | 28,964,749 to 13,828,299 = 15,136,450 = `candidate_bytes`; gross - net = 1,623,293 |
| fsck | ok, 0 problems, 193 blocks, again 193 on the fresh daemon |
| Fresh daemon, new NFS mount, all four files | SHA-256 equals pre-gc and BLAKE3 equals the independent digest: `keep000` `a7daf1dd6d`, `keep001` `cc88a4b51c`, `live000` `2fd43c019a`, `live001` `3677684f96` |
| Teardown | both daemons exit 0, mount and socket gone; shared pid 11068 command unchanged |

**2. Full harness, my own run** (`archive/bench/out/run-final`): committed script blob `e5e275a5...`, `env.harness_head` `70d602b`, `production_crates_tree` `fe899f9f`, **72 records, 0 failed**, same numbers as run 1; seeder binary `3ee983ac` was freshly built by me. `production_build_head` reads `''` in the archive (nested-path proxy weakness, as before).

**3. Builder's full run, audited in code, not re-run by it** (`bench/out/gc-daemon-e2e/run-f501157/evidence/records.jsonl`): 72 records, 0 failed, `harness_head` `7c083110`, tree `fe899f9f`, `production_build_head` `f5011574`, script `a5125907`, binaries `fcc58209`/`bbf27848`, eight selftests green, candidate 15,136,450, dry-run state hash `0e436f514fed` equal before and after, mutation control ok, gross 16,759,743, net 15,136,450, rewrite 1,623,293, one pack unlinked, fsck 193 blocks, four reopen BLAKE3 matches, four of four mounts NFS. Matches my measurements exactly. My run 2 is a separate full 72, so the two full runs agree.

BLAKE3-required, no-self-comparison and negative controls were not redone: the script blob is unchanged since the previous review, where they were exercised.

After all runs: 0 matching mounts, 0 `/private/tmp/cowfs-gc*` entries, no stray processes, `git status` in slot 14 shows only untracked `docs/reviews/`. Builder `run*` dirs and earlier critic dirs untouched.

## Hook and fault-API surface (new head)

- External compile probe (own crate, `--offline`): `Core::set_gate_fault` E0599, `cowfs_core::gate::Gate` E0603, `cowfs_core::test_hooks` E0433, as before. `cowfs_core::fsops::{arm,disarm}` still compiles externally (residual public `doc(hidden)` seam; already text-corrected in the doc).
- **New, not mentioned in the doc or PR body:** `f501157` adds an always-compiled hook in `cowfs-gc`. `Gc::set_between_lookup_and_walk` (`#[doc(hidden)]`, `lib.rs:291`) and a `pub` field `between_lookup_and_walk: Mutex<Option<Box<dyn FnMut(SnapshotId) + Send>>>` (`lib.rs:165`) run an arbitrary closure inside the mark walk, once per listed snapshot (`lib.rs:632`). My probe shows both compile from an external crate (method and field), so the hook is callable from in-process library code. It is reachable through the public `Core::collector(..).gc()`. The daemon, ctl and cli have 0 references; the control protocol has no verb for it; the only users are `cowfs-gc/tests/core_reclaim.rs` and `regressions.rs`. It is `None` in production, so it is inert unless an embedder sets it. No new risk was reproduced and I did not assess whether a hook-placed removal can defeat the skip logic; that is Sonnet 13's scope. Classification: embedder-callable, not daemon-remote-invokable, not a barrier fail-open, static caveat only.
- The doc bullet "hook source unchanged across heads" lists `gate.rs`, `gc_barrier_window.rs`, `fsops.rs`, both `Cargo.toml`s and `fsio.rs` as byte-identical, which is true, but its heading overstates because this new `cowfs-gc` hook is not in the list. Non-blocking doc nit.
- The workspace-unified `cowfs-store` `fault-injection` caveat from the previous review is unchanged (store manifest and `fsio.rs` byte-identical); I did not re-run the all-targets check.

## Other findings

- **Seeder binary reuse (non-blocking, undisclosed):** the builder's runs `run6`, `run-89b202c` and `run-f501157` all record `reclaim.seed_helper_present` with the same `binary_sha256` `6c7bf880...`, a binary built at 00:37 in the `ea95894` era. The harness only checks that the file exists, so the seeder was not rebuilt against the current `cowfs-core` and `cowfs-store`. The doc's "built from this exact committed crate tree" applies to the daemon and CLI, not the seeder. My freshly built seeder (`3ee983ac`) produces byte-identical pack sizes and digests, so behavior is unaffected here. Fix: rebuild always (cargo is incremental) or record the seeder's source tree.
- Harness hygiene items from earlier reviews are unchanged because the script blob is unchanged and remain disclosed: mislabeled `gc.dry_run.no_pack_change_negative_control`, and the early-failure `/private/tmp` directory leak.
- PR body and doc use consistent labels: `harness_head` `7c08311`, tree `fe899f9f`, build head `f501157` (proxy), script `a5125907`. #81 is cited; the CLI still prints gross bytes unlabeled; no capacity-saving claim.

## Gaps retained

- Fixture is quiescent: it does not exercise the lookup-to-walk vanish race, the `NoSuchSnapshot` skip, the root-walk race or the old `COWMARK1` upgrade.
- Real-core `gc` cancellation is unproven; the SIGINT, exit 130 and progress test uses the stub backend only.
- No SIGKILL or power-loss injection through the full stack; no Linux FUSE run from this harness (CI `linux-fuse` is a separate job).
- Sealed packs come from a 16 MiB fixture, not organic growth of the 256 MiB default.
- One cycle reclaims only the eligible sealed pack (202 of 368 dead blocks).
- Not run, per instruction: `workspace/race.rs`, hosted workflow reruns, CPU stress, 256 MiB batches. No timing claim.

## Artifacts

- `bench/out/gc-snapshot-fix-critic/run1/records.jsonl` (14 records, 0 failed).
- `bench/out/gc-snapshot-fix-critic/archive/bench/out/run-final/evidence/records.jsonl` (72 records, 0 failed).
- `bench/out/gc-snapshot-fix-critic/probe/` (compile probes), `critic_run.py`, `build.log`, `critic.out`, `archive-run.out`. All gitignored.
