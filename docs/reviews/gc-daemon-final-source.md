# Independent final review: PR #78 against production head 89b202c

Reviewer: fresh native Sonnet 5.5, read-only on tracked source, harness and docs.
Verification head: `095c1a6a1e35a74ec2f30418d9d7342212823dba` (branch `verify/gc-daemon-e2e`, slot 14).
Production source head: `89b202c893f95a5d287523692a731e51f88fb438` (PR #76 head, `crates/` tree `73a22952f2f4b09d4411dfa2bfd0e53b6a4bfaa2`).
Earlier reports `gc-daemon-e2e.md` and `gc-daemon-e2e-repairs.md` are preserved untouched.

## Verdicts (kept separate)

- **Artifact and end-to-end behavior (PR #78 harness, seed fixture, docs): PASS.** The two earlier artifact blockers (A1, A2) are fixed in the text, and the behavior reproduces on the new source.
- **Production source safety: NOT CERTIFIED by this review.** Barrier and root-walk safety belong to the source critic (Sonnet 13). This review only measured end-to-end behavior and the feature/API surface.
- **CI at exact head `095c1a6`, one check:** `check (macos-latest)` success, `linux-fuse` success, `check (ubuntu-latest)` **in_progress** (pending, not failed, not overridden). I did not poll.
- **Merge gate: not satisfied.** PR #76 (`89b202c`) is still open and unmerged, PR #78 carries its changes, the ubuntu check is pending, and the source critic has not passed. Do not merge.

## Source and provenance

- `git diff 89b202c 095c1a6 -- crates Cargo.toml Cargo.lock .github` is empty. `HEAD:crates` is `73a22952f2f4b09d4411dfa2bfd0e53b6a4bfaa2`, equal to `89b202c:crates`.
- Reviewer exported `git archive 095c1a6` and `git archive 89b202c crates Cargo.toml Cargo.lock .github` and ran `diff -r`: `crates/`, root manifests and `.github` byte-identical.
- Versus `89b202c` the branch adds only 7 files: `docs/v1-daemon.md`, `docs/verification/gc-daemon-e2e.md`, `scripts/verify-gc-daemon.py`, and four under `scripts/gc-fixture-seed/`.
- Rebase patch-ids match for all four rebased commits: `99c7c23`=`100b35c` (`5948b57c`), `a5ba8d0`=`b9aca65` (`dadd6a64`), `f170dc1`=`94e6398` (`2163d2d1`), `8cab23a`=`032dd1c` (`2f26493b`).
- Harness script blob is `e5e275a550a4f0cfa6de7951dee673d27cb0647c` at `8cab23a`, `a607ce2`, `42e9182` and `095c1a6`; the seed tree is `fb66253ec3d9` throughout. No harness or seed byte changed since I reviewed `8cab23a`.
- `42e9182..095c1a6` changes only `docs/verification/gc-daemon-e2e.md` (1 file). The harness was run at `42e9182`; `095c1a6` is a docs-only commit and its run is not claimed.
- The harness `production_build_head` is `git log -1 -- crates` from the containing repo. It is a proxy, not an attestation, and the doc and PR body say so. My isolated build is the identity proof.

## Isolated build from exact source

- Fresh `git archive 095c1a6` in `bench/out/gc-final-source-critic/archive/`, no builder target or fixture reused.
- `cargo build --locked --offline -p cowfs-cli -p cowfs-daemon`: 2m39s, exit 0. Seed `cargo build --release --locked --offline`: 34s, exit 0. (My first launch used a wrong relative directory and built nothing; the log shows `BUILD_DONE=1` with `cd: archive: No such file`. I relaunched correctly.)
- My binaries: `cowfs` `6c065bb2f387...`, `cowfs-daemon` `5631429c9359...`, seed `b180fc11e613...`. They differ from the builder's by embedded target path.
- Builder's slot binaries `cowfs` `f9508d803f5b...` and `cowfs-daemon` `4a9d420bd69e...` match its doc. The daemon was built at 07:06:51, after the newest changed source (`state.rs`, 07:05:30), and no `crates/**/*.rs` is newer than my build.
- Seed source hashes in the doc match my archive: `Cargo.toml` `629b6746`, `Cargo.lock` `275e7f2c`, `src/main.rs` `157430268d`.
- Binary strings, both daemons: `COWMARK2` 1, `COWMARK1` 0, `C7D_EXIT` 0, `set_gate_fault` 0, `remove_barrier` 0, `test-hooks` 0, `injected sync fault on` 1.

## Independent runs

All private, unmodified daemon/CLI defaults, supported 16 MiB-pack preseed, fresh socket under `/private/tmp`, NFS loopback mount, no privileges. The seeder closes `Core` before the daemon opens the store.

**1. Small actual-reclaim deliverable first** (`run1`, 14 records, 0 failed):

| Item | Value |
|---|---|
| Packs seeded | `pack-00000000.cpk` 16,759,743 and `pack-00000001.cpk` 12,205,006, total 28,964,749 |
| Expected digests | regenerated independently in Python from the fixture LCG and equal to the seeder's declared digests for all 4 files |
| Dry run | `candidate_bytes` 15,136,450, `freed` 0; sha256 of all 7 store files before vs after: 0 changed; sensor control (1-byte probe file) detected |
| Live gc | `freed_blocks` 202, `freed_bytes` 16,759,743 (gross); `pack-00000000.cpk` unlinked; new `pack-00000002.cpk` 1,623,293 |
| Net | 28,964,749 to 13,828,299 = 15,136,450 = `candidate_bytes`; gross - net = 1,623,293 = new pack |
| fsck | ok, 0 problems, 193 blocks; again on the fresh daemon |
| Fresh private daemon, new mount path, all four files | SHA-256 equals the pre-gc read and BLAKE3 equals the independently regenerated digest: `keep000` `a7daf1dd6d`, `keep001` `cc88a4b51c`, `live000` `2fd43c019a`, `live001` `3677684f96` |
| Mount | `nfsstat -m` shows `localhost:/cowfs-...` NFS, no native fallback |
| Shutdown | both daemons exit 0, mount and socket gone; shared pid 11068 command unchanged |

**2. Full harness from the clean archive** (`archive/bench/out/run-final`): committed script blob `e5e275a5...`, `env.harness_head` `095c1a6`, `production_crates_tree` `73a22952`, **72 records, 0 failed**. Same numbers as run 1 and as the builder's `run-89b202c` (72/0, builder `harness_head` `42e9182`, script `a5125907...`): candidate 15,136,450, gross 16,759,743, net 15,136,450, new pack 1,623,293, fsck 193 blocks, four reopened BLAKE3 matches, 4 of 4 mounts NFS. `production_build_head` is `''` in the archive because git log runs relative to the nested path; that is the known proxy weakness.

**3. Controls on the unchanged script blob** (re-run, not assumed): dir-state sensor detects a same-size 1-byte change and a new empty file and restores to equal; digest corruption moves SHA-256 and BLAKE3; under a Python without `blake3` the selftest raises `Blake3Unavailable` (FAIL record, no skipped pass). The tampered-digest control (3 expected failures) from the previous review applies by blob identity and was not re-run.

After all runs: 0 matching mounts, 0 `/private/tmp/cowfs-gc*` entries, no stray processes, `git status` in slot 14 shows only untracked `docs/reviews/`. Builder `run*`, `clean-checkout` and prior critic dirs untouched.

## Hook and fault-API proof (default and combined graphs)

- `cowfs-core` has no `[features]` table; `cowfs-gc` has none; `cowfs-core` depends on `cowfs-gc`, and `cowfs-gc` depends on `cowfs-core` only under `[dev-dependencies]`. `test-hooks` appears in no Cargo.toml, no `crates/` file; it appears in tracked docs text only.
- `cargo check -p cowfs-core --features test-hooks`: "package does not contain this feature". Same via `cowfs-gc --features cowfs-core/test-hooks`: rejected.
- External compile probe (own crate depending on `cowfs-core`, checked with `--offline`): `Core::set_gate_fault` gives **E0599**; `cowfs_core::gate::Gate` gives **E0603** (private module); `cowfs_core::test_hooks` gives **E0433** (does not exist). Source: `set_gate_fault` and the gate `fault` field are `#[cfg(test)]` (`gate.rs:44,147,201,206`; `lib.rs:491`; `gc_barrier_window` is `#[cfg(test)] mod`). This is compile-level proof, not strings.
- Default daemon graph (`cargo tree -p cowfs-daemon -e features`): no `fault-injection` or `test-hooks`. `cargo check -p cowfs-daemon --all-targets` JSON artifacts: `cowfs_store`, `cowfs_gc`, `cowfs_core` lib features empty.
- **Combined workspace graph (new caveat):** `cargo check --workspace --all-targets` gives the non-test `cowfs_store` lib features `['fault-injection']`, shared by the daemon bin in that invocation. Cause: `crates/cowfs-store/Cargo.toml` has a self dev-dependency enabling `fault-injection`. That seam (`fsio.rs:149-220,255`) reads `C7D_EXIT_*` environment variables and calls `std::process::exit(77)`. Verified from cargo artifact feature lists only, not from a binary built that way. Default and `-p` builds exclude it (`C7D_EXIT` count 0 in both daemon binaries). This is pre-existing (store `Cargo.toml` is not in the `89b202c` change), local-environment-triggered, a crash-injection seam and not the reference barrier. It is not mentioned in the doc.
- **Residual `injected sync fault on` seam, classified:** `cowfs_core::fsops` is `pub`, `#[doc(hidden)]` (`lib.rs:51`), always compiled, with global process state. `set_fault`, `arm`, `disarm` and `trace_take` compile from an external crate (probe passes), so it is **callable from in-process library code**. It is **not daemon-remote-invokable**: `fsops` has no reference in `cowfs-daemon`, `cowfs-ctl` or `cowfs-cli`, and the control `Request` kinds (Ping through Shutdown) have no fault or arm verb. It is **not unit-only**: the public API is callable. It is **not a barrier fail-open**: an armed fault makes `sync_file` and `sync_dir` return errors (`ino.rs:229,233`, `swap.rs:72,76`). One path, `swap.rs:57`, discards the error (`let _ = ...sync_dir`), so an armed directory-sync fault there is swallowed rather than surfaced. No new risk was reproduced. The doc's "unreachable in production" is accurate for the daemon and its production paths, but overstates for an embedder of the library; I label it a static caveat, not complete hardening. Barrier safety detail is the source critic's.
- The builder's claim that `69ae448` unified `test-hooks` into the daemon is historical; I did not rebuild that head.

## COWMARK scope

`89b202c` bumps `MAGIC_MARKS` `COWMARK1` to `COWMARK2` (`state.rs`); the binary contains `COWMARK2` and no `COWMARK1`. The harness always builds a fresh store and never reads a marks cache, so it does not cover legacy-cache upgrade. Upgrade correctness (`an_old_format_marks_file_is_not_reused`, `core_reclaim.rs`) is the source critic's scope and was not exercised here.

## Prior blockers

- **A1** (clean-checkout 72/72 claim): fixed. The doc now states builder 71 records at `f170dc1` (script `05ca05d5...`) and the independent 72-record run at `8cab23a`. I verified the builder's clean-archive dir contains no harness record, matching the doc's "built, seed hashes match" claim.
- **A2** (stale head label): fixed. The doc and PR body give `harness_head` `42e9182`, script sha `a5125907...`, tree `73a22952`, and separate the historical `ea958947` run6 label.
- **#81** is cited in the doc and PR body as a follow-up. The CLI is unchanged (still prints gross bytes unlabeled); no user-capacity saving is claimed.
- The two non-blocking harness defects I reported (mislabeled `gc.dry_run.no_pack_change_negative_control`, early-failure `/private/tmp` directory leak) are still present because the script blob is unchanged. The doc discloses both. The leak was reproduced last review.

## Non-blocking findings

- The doc does not mention the workspace-unified `fault-injection` caveat above.
- The doc's fsops wording should say "not invoked by the daemon or any production path; public library API".
- The seed `--locked` lock will need regeneration if a future PR #76 head changes dependencies; it fails loudly.
- CI `check (ubuntu-latest)` was pending at review time.

## Gaps retained

- No Linux FUSE run from this harness (CI `linux-fuse` is a separate job, not reviewed).
- Real-core `gc` cancellation is unproven; the SIGINT, exit 130 and progress test uses the stub backend only.
- No SIGKILL or power-loss injection through the full stack; graceful shutdown only.
- Sealed packs come from a 16 MiB fixture, not organic growth of the 256 MiB default.
- The fixture is a quiescent store; it does not exercise the concurrent root-walk race or barrier races.
- One cycle reclaims only the eligible sealed pack (202 of 368 dead blocks); the remainder sat in the then-active pack.
- `workspace/race.rs` batch not run, per instruction (issue 83).
- No timing, throughput or CPU claims.

## Artifacts

- `bench/out/gc-final-source-critic/run1/records.jsonl` (14 records, 0 failed).
- `bench/out/gc-final-source-critic/archive/bench/out/run-final/evidence/records.jsonl` (72 records, 0 failed).
- `bench/out/gc-final-source-critic/check-alltargets.jsonl`, `check-ws.jsonl` (cargo artifact feature lists), `probe/` (compile probe), `critic_run.py`, `build.log`, `critic.out`, `archive-run.out`. All gitignored.
