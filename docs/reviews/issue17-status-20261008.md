# Issue 17 status: Linux mount namespaces for canonical clone paths

Date: 2026-10-08.
Read-only audit, no code changed.
The tracker line "issue115 deterministic interleaving / ETXTBSY blocks merge" is stale: PR 92 merged as 252b93e on 2026-10-05.

## The issue has one acceptance sentence

The body of issue 17 is: "Optional per-agent namespaces so artifacts embedding absolute paths stay byte-identical. See docs/design.md."
There is no bullet list.
The one comment is the spike 6 evidence: namespaces matter mainly for mode (a), where unmodified treehouse builds each slot independently.
The table below splits that sentence into the testable claims it contains, plus the design.md line (line 61).

## Status table

| # | Claim | Status | Evidence |
|---|---|---|---|
| 1 | Optional, Linux only | MET | `scripts/cowfs-ns-run.sh` refuses off Linux with exit 77 and the UNMEASURABLE text. `run_build` takes `Option<&Canonical>`, so absent flags change nothing. Tests: `bench/test_namespaces.py` (17 tests), `crates/cowfs-treehouse/tests/canonical.rs` (default compatibility, flag pairing). |
| 2 | Per-agent namespace places a clone at a canonical path | MET | Helper uses `unshare --user --map-root-user --mount --propagation private`, rbind, `pwd -P` check. Isolation tests in `bench/test_namespaces.py` (mounts unchanged after clean exit, failing exit and signal; canonical dir empty outside; writes owned by real user). |
| 3 | Artifacts from two snapshots are byte-identical | MET on one fixture | `docs/linux-namespaces.md` "Measured on a real cowfs FUSE mount": A1 = A2 = B1 (sha256 `d10a6ec0...`), native control N1 differs. Host moonscape, kernel 6.12, ext4 under `fuse.cowfs`. `rustc` only. |
| 4 | Fails closed, never runs unwrapped | MET | `test_no_namespace_never_falls_back_to_the_raw_path`, `test_both_routes_refused_names_both_in_the_message`, treehouse probe plus `a_passing_payload_seventy_seven_stays_a_failure`. |
| 5 | Wired into the treehouse build path | MET (stub-level in CI, real run on moonscape) | `base refresh --build CMD --canonical DIR --ns-helper H`. `canonical.rs` runs in CI on ubuntu and macos, green on every recent main run. Real wiring run: `scripts/namespaces17-treehouse-linux.sh`, reviewed PASS in `docs/reviews/linux-namespaces17-*.md`. |
| 6 | Wiring publishes a warm base that the control API reports | PARTIAL | Source fix b54ed1b (#98, durable provenance) and 8602040 (#115, create vs publish race) are in main. Issue 98 is still OPEN and issue 124 (backend swap keeps provenance of the replaced tree) is OPEN. The tracker defers both. See gaps. |
| 7 | Isolation is tested in CI | NOT MET | GitHub `ubuntu-latest` denies `CLONE_NEWUSER`, so CI reports `OK (skipped=10)` and runs only the 8 refusal tests. Isolation evidence is a moonscape run, not a CI gate. Documented in `docs/linux-namespaces.md` "Running it". |
| 8 | Works with `cargo`, not just `rustc` | NOT MET | The doc states cargo same-path debug rebuilds are only 92% identical (spike 6), so the canonical path alone does not give byte identity for cargo. No cargo-level canonical build was run. |
| 9 | Works on the Core backend | NOT MET | `base_refresh` refuses on Core by design. The integration run uses the Path backend. |
| 10 | Works with a leased treehouse slot | NOT MET | moonscape has no `treehouse` binary. The run uses `--slot`, the same `run_build` call site with a different slot provider. |
| 11 | Other kernels and filesystems | NOT MET | One kernel (6.12), one filesystem (ext4 under the mount). No btrfs, XFS or older kernel. |
| 12 | ETXTBSY flake in the wiring tests is resolved | MET, with a stated limit | See below. |

## ETXTBSY: not an open defect

Origin: CI run 37244286403 at 2c559ebe, `canonical.rs` `write_stub` then `run_build` probe spawn, errno 26 from `execve`.
It was a test-fixture defect, not production code and not the namespace helper.
`docs/verification/evidence/etxtbsy17-spike.md` reproduced it at the seam on moonscape: in-place writer 97 of 6800 execs, waited child writer 0 of 3200.
Fix 4c2d7ec (test only, `canonical.rs`): `write_stub` writes from a short-lived `/bin/sh -c cat` child and waits for it.
Evidence: `docs/verification/evidence/etxtbsy17-repair.md`, old 47 of 3200 against new 0 of 3200.
4c2d7ec is an ancestor of origin/main.

Fresh check run for this audit, using the GitHub API only:

- 200 CI runs created after 4c2d7ec, 134 success, 64 failure, 1 cancelled, 1 unfinished.
- Failed-job logs of all 64 failures were searched for "text file busy" and "os error 26". Zero hits. A control pull of one old failed run returned 2454 log lines, so the search had content to match.
- The last 60 runs on `main`: 58 success, 2 failure, no ETXTBSY.

Limits: the original test-binary failure was never reproduced directly, the mechanism (deferred fput on a write-opened inode) is inferred, and zero hits in CI is a sample, not proof of zero.
The ubuntu leg was inferred to exist on every run from the workflow matrix, not verified per run.
I ran nothing on Linux, because no claim needed it.
Decision: stop at the status table. No repro, no worktree, no PR.

## What needs a product decision or heavy infra (listed, not built)

1. Close issue 17 or keep it open. PR 92 deliberately did not use a closing keyword. Closing needs a decision on whether rows 6, 7, 8, 9 and 10 are in scope for "done" or become follow-up issues.
2. Isolation in CI (row 7). Needs a runner that permits user namespaces, for example the self-hosted cachyos runner or a privileged container. That is infra and a CI policy call.
3. Cargo-level determinism (row 8). A canonical path is necessary but not sufficient. Needs a decision on whether cowfs also sets `CARGO_INCREMENTAL=0` or a `--remap-path-prefix` companion, which changes what "byte-identical" promises.
4. Real treehouse lease run (row 10) and Core backend (row 9). Needs a Linux host with the `treehouse` binary, and a decision on how Core publishes a base from a directory.
5. Issues 98 and 124 stay open. Both are owned by other lanes and are outside this audit.
6. btrfs, XFS and older-kernel matrix (row 11). Heavy infra, low priority.

## Stale-issue check

Issue 17 is not already fixed in full: rows 7 to 11 are open by the doc's own admission.
The ETXTBSY merge block in the tracker is already fixed and merged, and the tracker note should be refreshed.
Issues 115 and 97 are CLOSED. Issues 98 and 124 are OPEN.
