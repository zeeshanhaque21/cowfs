# Independent integration review: PR #78 after PR #76 landed

Reviewer: fresh native Sonnet 5.5, read-only on tracked files.
Verification head: `e3df413cbb4b0345f585af6090cbd21d44e99610` (branch `verify/gc-daemon-e2e`, local equals `origin/verify/gc-daemon-e2e`).
Main: `9906819f4e7806ff9e359ea315ec717b87b2724e` (PR #76 merge). `crates/` tree of both head and main: `f57af086b063c9563392fcd9bc0da441a61d2b53`.

## Verdicts

- **Artifact (verification-only diff, docs honesty): PASS, with one wording caveat (C1) and historical scope stated.**
- **Current-main control transport: sampled by me, PASS** (14 records, fresh archive of the exact head; see below).
- **CI at exact head, one snapshot:** `check (ubuntu-latest)`, `check (macos-latest)`, `linux-fuse` all `in_progress`. Pending, not polled, not overridden. Not green yet.
- **Merge:** do not merge until CI is green and the coordinator gate passes. PR is still draft against `main`.

## Verification-only identity

- `git diff origin/main HEAD --name-status`: exactly 7 paths: `M docs/v1-daemon.md`, `A docs/verification/gc-daemon-e2e.md`, `A scripts/verify-gc-daemon.py`, `A scripts/gc-fixture-seed/{.gitignore,Cargo.lock,Cargo.toml,src/main.rs}`. Empty diff for `crates`, `Cargo.toml`, `Cargo.lock`, `.github`.
- `diff -r` of `git archive e3df413` against `git archive 9906819 crates Cargo.toml Cargo.lock .github`: identical.
- Parents: `e3df413` has one parent `bf036ab`; `bf036ab` is a merge of `70d602b` (previously reviewed head) and `9906819` (main). The diff of `bf036ab` against its main parent is the same 7 paths.
- Script blob `e5e275a550a4`, seed tree `fb66253ec3d9` and `docs/v1-daemon.md` blob `0b56dd27f6` are identical at `70d602b` and `HEAD`: no harness or seed byte changed since my earlier reviews.
- No tracked review artifact lost: none of my four reports is tracked at HEAD (`docs/reviews/` tracks only main's `live-trial-metrics.md`). The four are untracked, present and unmodified in the worktree (sha256 prefixes `6d0b6fbf`, `c798b0e8`, `3b92ecf4`, `716cf2a5`). I cannot prove the builder's earlier hashes; sizes and mtimes show no change since creation.
- `e3df413` changes only `docs/verification/gc-daemon-e2e.md` (+22/-3): adds a "Dependency landed and source identity carry" section.

## Doc claims checked

- Stated main `crates/` tree `f57af086`: true. Tested tree `fe899f9f` (`f501157`): true.
- "gc, core, store, daemon src and Cargo.toml byte-identical f501157 to main": true (empty diffs, also meta and cli).
- "Only non-test `src` file that differs is `cowfs-ctl/src/server.rs`": true. Non-test `crates` diff `f501157..HEAD` is that file; other changes are tests (`cowfs-gc`, `cowfs-nfs`, `cowfs-ctl/tests`).
- No new-binary claim; the 14 and 72 proofs are stated as measured on `f501157` with `harness_head 7c08311`. Provenance preserved.
- **C1 (wording, non-blocking):** the doc says `server.rs` is "outside the GC path this harness drives" and that the proofs "carry to landed main". That is too strong. `server.rs` is the control-plane transport every harness `cowfs` CLI call uses, including the gc terminal frame and progress frames. PR #73 changes how the terminal frame write and teardown (`finishing` counter, `inflight_empty`) interact. The GC algorithm is unchanged, but the transport path is exercised by the harness, so the claim should say "GC core unchanged; transport changed; sampled on main by reviewer". I did not edit the doc.

## Current-main sample (mine, required because ctl transport changed)

Fresh `git archive e3df413` in `bench/out/gc-landed-critic/archive/`; runtime byte-identical to main; own target dir; `cargo build --locked --offline` 3m09s and seed 41s, both exit 0. Binaries `cowfs` `36808952f675`, `cowfs-daemon` `59f887e3d62a`, seed `95e983132d15`. Private store, unmodified daemon defaults, 16 MiB-pack preseed, short socket under `/private/tmp`, NFS loopback.

14 records, 0 failed:

| Item | Value |
|---|---|
| Dry run | `candidate_bytes` 15,136,450, freed 0, 0 store files changed, sensor control detected |
| Live gc (through the new ctl terminal-frame path) | 202 blocks, gross 16,759,743, `pack-00000000.cpk` unlinked, new pack 1,623,293 |
| Net | 28,964,749 to 13,828,299 = 15,136,450; gross - net = 1,623,293 |
| fsck | ok, 0 problems, 193 blocks; 193 again on the fresh daemon |
| Fresh daemon, new NFS mount, four files vs independently regenerated BLAKE3 | `keep000` `a7daf1dd6d`, `keep001` `cc88a4b51c`, `live000` `2fd43c019a`, `live001` `3677684f96`: all match |
| Teardown | both daemons exit 0, mounts and sockets gone |

All numbers equal the f501157 numbers, so the transport change did not alter observed behavior in this path. I did not run the full 72-record harness: the harness script and fixture are unchanged and no concrete changed behavior in the harness path was identified. My earlier 14 and 72 on `f501157` therefore remain correctly scoped historical proof of the pre-PR #73 transport; the 14 above is the current-transport proof. The builder's 72 on `f501157` stays builder-only for this head.

Limit: my harness's "shared daemon untouched" check watches the old pid 11068 (now gone), so it is vacuous; I separately confirmed restored pid 15263 is alive and addressed by nothing of mine. After the run: 0 mounts, 0 `/private/tmp/cowfs-gc*` entries.

## Gaps retained

- CI on `e3df413` pending.
- Real-core `gc` cancel is still unproven through the CLI (stub only); PR #73's half-close and cancel interplay is covered by its own ctl tests, not by me.
- No full-stack crash or SIGKILL injection; no Linux private-daemon harness run (CI `linux-fuse` is a separate job).
- Organic 256 MiB pack growth not exercised; fixture is a 16 MiB preseed, quiescent, so race fixes belong to the source reviewers' PASS (not rejected here).
- Doc wording C1 above.

## Artifacts

`bench/out/gc-landed-critic/run1/records.jsonl` (14/0), `critic_run.py`, `build.log`, `critic.out`, `archive/`, `ref/`. All gitignored.
