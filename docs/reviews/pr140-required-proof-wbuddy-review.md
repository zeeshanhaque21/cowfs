# PR 140 required-proof review: real `open_recover` rollback, `INO_LIMIT` ceiling, measured latency

Reviewer: independent read-only audit (wbuddy lane).
Scope: PR 140 request 4, branch `fix/meta-inode-reservation-42`, ONLY the new test-proof delta and its receipt.
New head under review: `37c197b57934b42612081fa45232d0372d28b695`.
Prior audited head (baseline): `573b02f5e069f1e52bc32a11f2da4ce4ec8083c4`.
PR base: `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`.
This review does not touch or re-audit `573b02f5`, and it does not touch the older 195-line review `pr140-repaired-reservation-final-wbuddy-review.md`.

## Verdict

`proofs-closed` is REJECTED.

- SOURCE: the test-proof delta at `37c197b5` is real, inside `#[cfg(test)]`, and does not affect production or the existing proofs. The three claimed proofs are present as tests.
- RUNTIME: the delta is NOT green. The bounded completed run for the test commit `72f19f9` (which is the parent of `37c197b5` and carries the identical test code) FAILED on `check (ubuntu-latest)`: `open_recover_keeps_a_reservation_bound_across_a_real_rollback` panicked at `crates/cowfs-meta/src/db.rs:2444`.
- The run for the current head `37c197b5` is still `in_progress`; there is no completed 37c CI capture to read, so 37c runtime is PENDING, not proven.
- The three previously owed items are now covered by committed tests, but those tests have NOT passed. Coverage is not acceptance.
- The receipt's own claim that the "8 GiB cap" is not binding because it is unconfigured is WRONG: the explicit user cap remains binding regardless of free space (224 GiB reported) or the absence of an enforcer.

## Pins (immutable git, at 37c)

| object | SHA |
| --- | --- |
| 37c commit | `37c197b57934b42612081fa45232d0372d28b695` |
| 37c tree | `ca86b375d218209965f2202e0d4925b35fb229ac` |
| 37c parent | `72f19f961fcb0fc8c4b780a1dc43e6799639846f` |
| db.rs @37c | `7345991c1c2dd1d0e31ec27c9506a3c1ef4b0c2a` |
| db.rs @573 | `ba1bdecb30a0cd091894ee59f715859a383b9416` |
| receipt @37c | `cca7c4720ac68ff7722230ebd3a71c8383d0fbf6` |
| receipt @main 9874 | `cca7c4720ac68ff7722230ebd3a71c8383d0fbf6` |
| check.rs @37c | `5c654c83d6eaf7814675b585caf6fbc4b065d475` (unchanged 573..37c) |
| tx.rs @37c | `fd10680a4c2c573f9acab8f2cd6d74add3a30acd` (unchanged 573..37c) |
| types.rs @37c | `22fcbabbb6fe9ce74906c387e9988f8d2fa830a4` (unchanged 573..37c) |
| design.md @37c | `32dd58a8a7cb02aa54c59b860ffb53c0e78e67cb` |
| cf67e8a (base) | `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0` |
| branch tip (remote) | `37c197b5` (HTTPS `fix/meta-inode-reservation-42`), matches local |

## Delta shape

`573b02f5..37c197b5`: two commits, only two paths.

- `72f19f9` `test(meta): prove the reservation bound across a real open_recover rollback`: `crates/cowfs-meta/src/db.rs` +302, tests only.
- `37c197b5` `docs(meta): mirror the PR 140 required-proof receipt onto the branch (#42)`: adds `docs/verification/evidence/meta42-reservation-required-proof.md` (+179).

Two diff hunks in `db.rs`, both at oldStart 2002 and 2224. The `#[cfg(test)] mod tests` opens at line 1972 (new file). Both hunks are inside it. There is no hunk before line 1972, so no production line moved. Added items: `seed_floor` helper (test-only) and tests T9, T9b, T10, T11. `check.rs`, `tx.rs`, `types.rs` are unchanged across the delta.

Receipt blob on the branch and the receipt blob on local `main` at `9874afae` are byte-identical (`cca7c472...`). Local `main` is one commit ahead of `origin/main` and unmerged; `origin/main` is `89353e17`. The author commit `9874afae` added ONLY the new receipt on `main`; no other main content moved.

## Runtime evidence (bounded, no dispatch, no rerun)

Runs read from the actions API:

| head | run | created | status | ubuntu | macos | linux-fuse |
| --- | --- | --- | --- | --- | --- | --- |
| `573b02f5` (prior, green) | 37511041828 | 18:24Z | completed | success | success | success |
| `72f19f9` (test commit) | 37516750404 | 19:08Z | in_progress | FAILURE | in_progress | success |
| `37c197b5` (new head) | 37517082073 | 19:11Z | in_progress | in_progress | in_progress | success |

### Completed failed run `72f19f9` (run 37516750404, ubuntu job 112451518504)

Aggregate over 67 `test result:` lines: 778 passed, 1 failed, 70 ignored.

```
test db::tests::the_inode_ceiling_admits_the_last_range_and_refuses_the_overflow ... ok
test db::tests::a_range_ending_on_the_limit_is_the_largest_legal_one ... ok
test db::tests::a_large_reservation_is_measured_against_a_single_one ... ok
test db::tests::open_recover_keeps_a_reservation_bound_across_a_real_rollback ... FAILED

---- db::tests::open_recover_keeps_a_reservation_bound_across_a_real_rollback stdout ----
thread 'db::tests::open_recover_keeps_a_reservation_bound_across_a_real_rollback' (14974) panicked at crates/cowfs-meta/src/db.rs:2444:9:
assertion `left == right` failed: and so is the recovered floor
  left: 1000406
 right: 1000402
test result: FAILED. 27 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.18s
error: test failed, to rerun pass `-p cowfs-meta --lib`
##[error]Process completed with exit code 101.
```

T9, T9b, T11 passed on this run. T10 failed.

### `72f19f9` linux-fuse (job 112451517743, success, completed)

Contains the pre-existing gated failures, job still success:

```
FAIL cowfs xattrs xattr_on_directory_and_symlink ... unexpected error: permission denied (PermissionDenied)
thread 'cowfs-fuse-w2' panicked at crates/cowfs-fuse/tests/common/mod.rs:185:13:
known = r"^FAIL\s+cowfs\s+xattrs\s+xattr_on_directory_and_symlink\s+.*unexpected error: permission denied \(PermissionDenied\)$"
```

The `known` regex is the gate's exact control evidence: the failure is matched and tolerated by the fixture. This is a `cowfs-fuse` suite, it does not exercise `cowfs-meta`. FUSE green is NOT Meta/Core test-execution proof for this delta.

### `37c197b5` (run 37517082073)

ubuntu, macos still `in_progress`; linux-fuse `success`. No completed 37c capture exists, so no 37c test counts or failure names can be derived. 37c runtime: PENDING.

## The observed failure, root-caused in source

The failing assert is the reopen after recovery:

```
db.rs:2442  let again = Meta::open(&path, opts()).unwrap();
db.rs:2443  assert_eq!(again.health().recoveries, 1, "the count is durable");   // passed
db.rs:2444  assert_eq!(again.health().ino_floor, floor, "and so is the recovered floor");
```

`health().ino_floor` returns `s.ino.reserved` (db.rs:450). The value read back at 37c is `1000402`; the value captured in the `rec` struct during recovery was `1000406`. There is a 4 offset between the recovery object and the persisted/reloaded floor.

`durable_reserved`/`durable_bound` read the raw `ino_reserved`/`INO_INTENT` keys. `health().ino_floor` reads the cached `Session.ino.reserved`. The authoritative durable floor and the cached floor disagree across the reopen after a real rollback.

The receipt's mechanism narrative is also not what the code does. The receipt says "a one-commit rollback of the floor move lands on the bound and `record_recovery` skips to it". But in `open_recover` the rollback is driven by clearing the two-phase flag and setting `GOD_RECOVERY` (db.rs:1417) and letting redb repair; the `health().ino_floor` value is a post-reopen cached read, not a direct read of the key that the assert compares against the recovery object. The observed 4 difference means the "lands exactly on the bound" claim is empirically false on ubuntu.

## Claim-by-claim findings

### F1 (T10, real `open_recover` repair) - BLOCKED by runtime failure

- Fixture: builds a store, does `reserve_inodes(1_000_000)`, copies bytes BEFORE drop, then scans 1- and 2-page `0xA5` damage on scratch copies, requiring `Meta::open` err AND `open_recover` `rolled_back`. It then applies the chosen damage to the real file. The damage selection is real and fails closed before recovery (assert at db.rs:2403). Good.
- Drives the real path: `Meta::open_recover` at db.rs:1408, `record_recovery` at db.rs:1424. It does not call `record_recovery` directly. Good.
- But the "newest commit not both / unknown" concern is not fully pinned. The fixture scans consecutive pages and accepts the FIRST `(page, pages)` that satisfies the predicate. It never asserts that the newest commit is the one whose loss is observed, nor that only one commit was lost. `redb.rolled_back` plus a later `ino_floor` mismatch is consistent with the repair having discarded more than the single floor-move commit. The 4-offset is exactly the signature of "the rollback landed somewhere other than the receipt says".
- The assertion that the recovered floor covers the previously returned range IS present (db.rs:2414-2418 `floor >= original.end().0`). That part is the right target. It is not merely a premature-failed-reservation check.
- Result: the test does target the right property, but it does not pass, and the fixture does not prove which commit was lost. UNEXECUTED-GREEN / FAILED.

### F2 (T9 / T9b, `INO_LIMIT` ceiling) - PASSED on the completed run, coverage acceptable

- Uses the real allocator: `seed_floor` writes the same `ino_reserved` key the production path writes, then a real `Meta::open` loads it. Not an arithmetic-only claim, so this is stronger than the prior review's owed item.
- Last legal range: `reserve_inodes(1)` from `INO_LIMIT-1` gives end exactly `INO_LIMIT` (asserts `<= INO_LIMIT`). Refused beyond: `[1,2,1<<20]` all `LimitExceeded`; floor unmoved; persists and stays refused after reopen.
- T9b pins the guard `n > INO_LIMIT - s.ino.next` from both sides: 11 past ten remaining refused and consumes nothing, exactly 10 accepted ending on the limit.
- No weakened limit request cap: `reserve_inodes` guard is unchanged (db.rs:820), `check.rs` unchanged.
- Result: PASSED.

### F3 (T11, measured latency) - PASSED as a harness, but proves NO speed

- Real `Instant` timing around a real `reserve_inodes`, fresh store per sample, close/reopen/non-reuse cycle FIRST (representative case before comparison), then n=1 vs n=1_000_000 under identical conditions. Prints min/median/max, reps, and `CI` env. No hidden static fold; no file creation in the million loop.
- No timing assert, so no false PASS. The receipt explicitly labels this unexecuted and claims no number. Correct framing.
- But this yields NO actual SPEED PASS. Even when it runs, the numbers are the deliverable. The design gate is build overhead within 1.5x native (`docs/design.md:111`), which this test does NOT measure; it measures two reservation calls, not build/git-status overhead. And the 1.5x filesystem gate stays untouched by this test.
- Print visibility: `cargo test --workspace` default hides per-test stdout when the test passes. The receipt says the lines surface only under `--nocapture`. So a passing T11 in normal CI produces NO timing output in the job log, meaning CI will never show a latency number. There is no way to accept a SPEED result until someone runs the test with `--nocapture` and shows the printed numbers.
- Result: PASSED (harness green), but NO SPEED PASS is available from any run seen.

### F4 (scope / no production change) - CONFIRMED

- Only `db.rs` tests + the receipt changed. All added lines are inside `#[cfg(test)] mod tests`. No production seam added by the test; `seed_floor` uses `m.h.inner.db` and `META`, both already reachable from the test module. Existing proofs (`health.rs`, `inode_reservation.rs`, T1-T8) are untouched. No manifest, CI, Core, ctl, daemon, store, NFS change.
- Result: CONFIRMED.

### F5 (cap claim) - WRONG

- Receipt: "That is not a disk cap and it is not enforced in this repository ... free space is 224 GiB ... The real constraint is a shared heavy-command lane."
- The explicit user cap on `bench/out` (8 GiB) remains binding regardless of free space and regardless of whether an enforcer exists in the checkout. Free space does not waive a user constraint. This reviewer observed the cap as binding for local execution and did not run cargo/build/test/clippy, matching the assignment. The receipt's reframing of the cap as non-binding is rejected.
- Result: WRONG.

## Owed items, status after this review

| owed item | prior status | now |
| --- | --- | --- |
| Real redb `open_recover` damaged-file repair for the reservation | UNEXECUTED | committed test T10 present; FAILED on ubuntu run 37516750404 |
| `INO_LIMIT` boundary | arithmetic-only | committed T9/T9b; PASSED on 37516750404 |
| Measured latency | absent | committed T11 harness; PASSED; no SPEED number produced or shown |

Proof of source is available for all three. Proof of execution is: T9/T9b/T11 pass, T10 fails; the current head 37c run is pending; no SPEED number is produced by CI.

## Acceptance gaps (do not close `proofs-closed`)

1. T10 must pass on ubuntu at the head, or the fixture's rollback assumption must be corrected. The 4-offset (`1000406` vs `1000402`) between `rec.ino_floor` and the reopened `health().ino_floor` is an unresolved correctness signal.
2. The T10 fixture must prove WHICH commit was lost (newest only, not more), not merely "some damage that rolls back". The receipt asserts "a one-commit rollback lands exactly on the bound", which the run contradicts.
3. T11 produces no observable output under default CI (stdout hidden on pass). A SPEED PASS requires a run with `--nocapture` and the printed numbers, or a different capture. The 1.5x design gate is not measured by T11 at all.
4. The 37c head run (37517082073) has no completed capture; 37c runtime remains PENDING.
5. The cap-waiver reasoning in the receipt is rejected; the explicit cap remains binding.

## What was run and what was not

- Read-only git inspection: full SHAs, blob pins, `--name-status`, diff hunks, ancestry, remote branch tip. Done.
- Read main `docs/design.md` at 37c and the new receipt. Done.
- Read bounded completed CI logs for run 37516750404 (ubuntu job 112451518504, linux-fuse job 112451517743) via `gh api .../jobs/<id>/logs` with curl because gh-axi rejects escape sequences and `run view` lacks `--json`. Done.
- Deferred two hunks: the 573 production region and the older 195-line review are untouched by this review.
- NOT run: any `cargo build`/`test`/`clippy`, any target dir, `git archive`, probe binary, cleanup, offload, cap waiver, any commit/push/merge, issue close, new issue, lease, signal, daemon, or shared resource. No dispatch, rerun, runner, or workflow action. The cap is treated as binding.
- Not touched: active `ses_eed75c2a3ffe8MjtPcXS1Zgdvl` READY3 Core+Meta ownership, `ses_ef6d6f10cffeA7SelmYbknZnIX` READY7.
