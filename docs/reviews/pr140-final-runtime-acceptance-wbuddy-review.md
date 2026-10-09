# PR 140 final runtime acceptance review: repaired head `1c12ef84302d954898ef01d7b01f5e268ee45a2e`

Reviewer: independent read-only audit (wbuddy lane).
Scope: PR 140 request 4 (`Meta::reserve_inodes`), branch `fix/meta-inode-reservation-42`.
Final head: `1c12ef84302d954898ef01d7b01f5e268ee45a2e`.
Repair code commit: `d8080553bf906db7f739c240b7607f55185aa1f5`.
PR base: `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0` (`main`).
Current remote `main`: `89353e17e5085000711dc428e834f9cc41840a1f`.
The prior reviews `pr140-required-proof-wbuddy-review.md` (SHA-256 `d38f6b25...`) and `pr140-repaired-required-proof-wbuddy-review.md` (SHA-256 `f5be4382...`) are immutable and are not rewritten here.

## Verdict

METADATA-ONLY: **MERGE_READY** at the commit/CI level. Whole issue #42: **still open** (Core consumer PR 142 is not merged and is currently failing).

- The repaired head `1c12ef84302d95` is green: run 37518576562 all three jobs COMPLETED SUCCESS, and it is the same code as run 37518490605 (green). T10 now passes on both OS.
- T11 timing is now VISIBLE and MEASURED: real `Instant` samples printed to fd 1 on both ubuntu and macOS. Values quoted below.
- The tests-only production delta was already reviewed at 573/1c12; no new production fact changes that (573 runtime green, 355-1c12 mechanism unchanged).
- Integration against current `main` is PROVEN: CI checked out the GitHub merge ref, not the head. See merge-readiness section. No later integration compile is needed for this PR.

## Pins

| object | SHA |
| --- | --- |
| 1c12 head | `1c12ef84302d954898ef01d7b01f5e268ee45a2e` |
| d808055 code | `d8080553bf906db7f739c240b7607f55185aa1f5` |
| 37c parent | `37c197b57934b42612081fa45232d0372d28b695` |
| PR base / `main`@PR | `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0` |
| current remote `main` | `89353e17e5085000711dc428e834f9cc41840a1f` |
| GitHub merge ref `refs/pull/140/merge` | `bdd4dbabb261ce4ba09bad8787df5cf547641790` |
| remote branch tip | `1c12ef84302d95` (matches local) |
| db.rs @1c12 | `143528696d5e19491e129a79cfb064923516c8ae` |
| PR140 live mergeable | `true`, `mergeable_state = clean`, draft, open, 13 files, +3134 / -20 |

## Actual runtime: exact head run 37518576562 (COMPLETED SUCCESS)

| job | id | conclusion | window |
| --- | --- | --- | --- |
| check (ubuntu-latest) | 112457763474 | success | 19:23:19Z - 19:33:22Z |
| check (macos-latest) | 112457763281 | success | 19:23:26Z - 19:39:50Z |
| linux-fuse | 112457763559 | success | 19:23:20Z - 19:28:13Z |

Code run 37518490605 (`d808055`): all three jobs success. Same `db.rs` blob as 1c12 (`14352869...`); `1c12ef8` is a docs-only mirror commit on top, so the code tested is identical.

### Named test results (both check OSes, all `ok`)

- `db::tests::the_inode_ceiling_admits_the_last_range_and_refuses_the_overflow ... ok` (T9)
- `db::tests::a_range_ending_on_the_limit_is_the_largest_legal_one ... ok` (T9b)
- `db::tests::open_recover_keeps_a_reservation_bound_across_a_real_rollback ... ok` (T10)
- `db::tests::a_large_reservation_is_measured_against_a_single_one ... ok` (T11)

T10 was the only failing test at `37c197b5` (both ubuntu and macos, `1000406` vs `1000402`). It now passes at 1c12 on both OS. The failure-to-green transition is real and observed in logs, not a source-only claim.

### Suite counts

- ubuntu: 156 suites, 1689 passed, 0 failed, 122 ignored.
- macos: 156 suites, 1686 passed, 0 failed, 69 ignored.
- `cowfs-meta` lib `#[cfg(test)]` (private): 12 `db::tests::` names pass on both OS (includes T1-T11 plus `a_bound_left_behind_is_exactly_what_recovery_skips_to`, `a_file_without_a_bound_still_recovers_by_one_block`, etc.).
- Integration `crates/cowfs-meta/tests/inode_reservation.rs`: 11 passed, 0 failed on both OS. Named: `a_reservation_is_contiguous_with_an_exclusive_end`, `a_reservation_past_the_limit_is_refused_and_writes_nothing`, `a_reservation_returns_numbers_without_creating_anything`, `a_reservation_and_ordinary_creation_never_hand_out_the_same_number`, `a_zero_reservation_is_refused_and_writes_nothing`, `numbers_reserved_and_never_used_are_not_reissued_after_a_reopen`, `the_durable_floor_only_moves_up_across_reservations`, `two_reservations_are_disjoint_and_the_second_follows_the_first`, `a_reservation_racing_creation_stays_disjoint`, `concurrent_reservations_are_disjoint`, `a_reservation_leaves_existing_snapshots_alone`.
- linux-fuse: 9 suites, 107 passed, 0 failed, 0 ignored. Contains the pre-existing `cowfs-fuse` xattr `PermissionDenied` fixture failures and injected-panic tests, all matched/tolerated by the suite gate; job success. Not Meta evidence.

## T11 timings: MEASURED (verbatim from the completed logs)

The repair routes output through `/dev/stdout` (fd 1) so it survives libtest capture. Real values appeared for the first time in this run.

ubuntu (job 112457763474):
```
inode reservation timing: env CI=true reps=3 node_size=512 ino_block=8 n=1000000 -> 1.707179ms (floor 1000002)
inode reservation timing: n=1 reps=3 min=1.488185ms median=1.707343ms max=1.911049ms
inode reservation timing: n=1000000 reps=3 min=1.509156ms median=1.51221ms max=1.512211ms
```

macos (job 112457763281):
```
inode reservation timing: env CI=true reps=3 node_size=512 ino_block=8 n=1000000 -> 6.015625ms (floor 1000002)
inode reservation timing: n=1 reps=3 min=5.605792ms median=6.568666ms max=7.049ms
inode reservation timing: n=1000000 reps=3 min=4.093625ms median=5.956875ms max=6.708917ms
```

Interpretation, bounded and honest:

- These are real `Instant` wall-clock samples, units milliseconds, `reps=3` under CI, fresh store per sample, `n=1` vs `n=1_000_000`, identical commit conditions.
- Reservation-only API: the n=1 vs n=1e6 medians are within run-to-run noise on both OS (ubuntu 1.707 ms vs 1.512 ms; macos 6.569 ms vs 5.957 ms). This is consistent with the design intent that the durable cost is a fixed two commits independent of `n`; it is NOT a proof of algorithmic equality, only that the observed medians do not grow with `n` at this scale.
- Understated CI load variance: these ran on shared GitHub runners under `reps=3`, single run. A median is stable but min/max spread (ubuntu n=1 max 1.911 ms vs min 1.488 ms) shows noise. These are samples under CI load, not controlled measurements.
- **NOT the design 1.5x filesystem gate** (`docs/design.md:111`, build overhead within 1.5x native on `cargo build` and `git status`). This test measures `reserve_inodes` alone, no filesystem, no native baseline, no ratio. It cannot accept or reject the 1.5x gate.
- **NOT whole-cowfs acceptance** and not the success criteria 1-3. It is one reservation-only data point.
- No invented 1.5x reservation threshold is claimed. The test asserts no timing.

## T10 semantics: real repair, newest-vs-deeper, no reuse

Verified in source at 1c12 (tests-only delta; production unchanged from 573):

- Drives the real path: `Meta::open_recover` -> damage scan requires `Meta::open` err AND `open_recover.rolled_back`, then `record_recovery` via the on-disk repair (clearing the two-phase flag, setting recovery flag). Not `record_recovery` called directly.
- Newest-vs-deeper discriminator: `assert_eq!(floor, original.end().0, ...)`. `record_recovery` prefers the durable bound (`ino_reserved_intent = end`); a newest-only rollback yields `floor == end`, a deeper rollback falls back to `old_floor + block` and lands strictly below, failing the equality. This pins "newest commit only", not merely "some rollback".
- No reuse after the extra 4: `next = reserve_inodes(4)` then `next.start() >= floor` and `>= original.end()`, and after reopen `reopened.start() >= raised` where `raised = next.end()`. The prior stale-equality bug (`db.rs:2444`, `1000406` vs `1000402`) is gone; T10 now passes.
- Preserved invariants: `rolled_back`, `recoveries == 1`, `durable_bound == None` (bound spent), `floor >= original.end()`, survivors, `m.check()` and `again.check()`.

## Source verdict (unchanged from prior reviews, all clear)

- `37c197b5..1c12` is tests-only: 5 hunks, all after `mod tests` (`db.rs:1972`). `check.rs` / `tx.rs` / `types.rs` unchanged 37c..1c12.
- Production mechanism `355b5fca` source: PASS. `573b02f5` runtime: PASS (run 37511041828). No accidental weakening; `reserve_inodes` guard `n > INO_LIMIT - s.ino.next` intact.
- Old-store / fault / persist-Err / retry / limit-count: no cap added. Fault semantics `1/2/3` unchanged; persist-Err path unchanged; T5/T6/T7 pin them.
- No new actual fact contradicts any prior finding.

## Merge readiness

| fact | value |
| --- | --- |
| PR 140 state | open, draft: yes, merged: no |
| mergeable | `true`, `mergeable_state = clean` |
| base | `main` @ `cf67e8a6` |
| head | `1c12ef84302d95` |
| current remote `main` | `89353e17` |
| files vs base | 13 (5 code + 8 docs), all under `crates/cowfs-meta/**` and `docs/**` |
| unrelated new production | NONE (no core/store/fuse/vfs/ctl/daemon change in this PR) |

Integration is PROVEN by CI's own checkout, not assumed. `ci.yml` uses `actions/checkout@v4` on `pull_request`, which checks out `refs/pull/140/merge`. Both check jobs logged:
```
[command]/usr/bin/git checkout --progress --force refs/remotes/pull/140/merge
HEAD is now at bdd4dba Merge 1c12ef84302d954898ef01d7b01f5e268ee45a2e into 89353e17e5085000711dc428e834f9cc41840a1f
```
So CI tested the merge of head `1c12ef84` **into current `main` `89353e17`** (merge commit `bdd4dba`), on ubuntu and macOS. It is NOT a head-only checkout. `main` gained core changes since the PR base (`cowfs-core`: io.rs, lib.rs, queue.rs, swap.rs, and tests) - the "combined clock/rename changes" - and the merge-ref build compiled and passed with them. **No later integration compile is required for this PR**: the tested ref already includes current `main`.

Scope caveat: `main` moved after this run only if it advanced past `89353e17`; at audit time remote `main` is still `89353e17e5085000711dc428e834f9cc41840a1f`, the exact commit the merge ref merged into. So the tested integration is current.

## Remaining gates (whole issue #42)

- PR 140 itself: metadata-only MERGE_READY. The 1.5x filesystem gate is NOT part of this PR and remains a v1 acceptance item, tracked in design/issue #18.
- The Core consumer is a SEPARATE PR: PR 142 `test(core): reserved-inode consumer regression for #42 request 4`, open, draft, checks `0 passed, 2 failed, 3 total`. It is NOT merged and currently failing. PR 140's metadata API delivery does not substitute for the whole #42 delivery; #42 stays open until the consumer lands.
- The snapshot native warm-base / whole-filesystem acceptance remains out of scope of this PR.

## Evidence trail

- Fresh read of run 37518576562 (all three jobs completed success) and run 37518490605 (success).
- Completed logs fetched once via `gh api .../jobs/<id>/logs` with curl: ubuntu 370507 B, macos 359563 B, fuse 94739 B. Derived counts, named tests, timing lines, and merge-ref checkout lines in code.
- Read `docs/design.md` at 1c12 (1.5x gate text), `ci.yml` at 1c12 (checkout + test invocation), the PR 140 body/state via `gh-axi`/API, PR 142 state, and remote `main`/merge-ref via `git ls-remote`.
- NOT run: cargo build/test/clippy, target dir, `git archive`, probe, cleanup, offload, cap waiver, commit, push, merge, draft-state change, issue close, PR body edit, CI rerun/dispatch, runner or workflow change, lease, signal, daemon, mount. No checkout. 8 GiB cap and free-space constraint treated as binding.
- Not touched: `ses_eed5b7b0fffesJmKd2fYb2N3xB` (READY3 Core consumer, active) and `ses_ef6d6f10cffeA7SelmYbknZnIX` (READY7 NFS43, active).
