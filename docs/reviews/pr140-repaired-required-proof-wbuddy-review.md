# PR 140 repaired required-proof review: T10 floor-ordering repair and T11 output visibility

Reviewer: independent read-only audit (wbuddy lane).
Scope: PR 140 request 4, branch `fix/meta-inode-reservation-42`, ONLY the T10/T11 repair delta at the new head.
New head under review: `1c12ef84302d954898ef01d7b01f5e268ee45a2e`.
Code (test) commit under review: `d8080553bf906db7f739c240b7607f55185aa1f5`.
Failed parent (prior review head): `37c197b57934b42612081fa45232d0372d28b695`.
PR base: `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`.
Author receipt: `docs/verification/evidence/meta42-reservation-proof-failure-repair.md` (blob sha256 `311aa1eb6417db7aae953edec8965f7e3a849b4459901c3fc97e4c344f3a1e61`).
The prior review `docs/reviews/pr140-required-proof-wbuddy-review.md` (SHA-256 `d38f6b2548d7751ee87a0b3fb75993e44034d669c53b4ef1d9d598cfdf656e80`) is immutable and is not rewritten here.

## Verdict

The repair is a correct tests-only fix of a test-ordering bug, and the T11 output path is the right idea. But the repaired head is NOT green yet and NO timing is visible yet.

- SOURCE: repair delta is tests-only, inside `#[cfg(test)]`, no production line touched. CONFIRMED.
- The author's root cause is CORRECT and I independently reproduce it in source and in the completed logs: T10 reserved 4 numbers at `db.rs:2439` (raising the durable floor from 1000402 to 1000406), then the reopen compared `health().ino_floor` (1000406) against the stale pre-reservation recovery floor (1000402). The left value is exactly right+4, the reservation the test itself made. Test bug, not a production bug.
- RUNTIME: the repaired head is UNPROVEN. Its own run (37518490605 for `d808055`, 37518576562 for `1c12ef8`) is still `in_progress` at audit time. No completed capture exists.
- The completed run at `37c197b5` (37517082073) FAILED on BOTH `check (ubuntu-latest)` AND `check (macos-latest)`, not just ubuntu. Both panic at `db.rs:2444` with the same `1000406` vs `1000402`. The prior review recorded 37c as pending; the completed capture now shows a two-OS failure.
- T11 timing values: MISSING. Zero `inode reservation timing` lines exist in any completed log. The 37c run predates the `/dev/stdout` repair, so its T11 used the old captured `println!`. No numeric latency value has ever appeared.

## Pins (immutable git)

| object | SHA |
| --- | --- |
| 1c12 head commit | `1c12ef84302d954898ef01d7b01f5e268ee45a2e` |
| 1c12 tree | `6542d0b8f722c39af9912cbcd3b73177156956e5` |
| d808055 code commit | `d8080553bf906db7f739c240b7607f55185aa1f5` |
| d808055 tree | `a0e4dc6eb507c4c4dfc344f7799f962053eab539` |
| 37c parent commit | `37c197b57934b42612081fa45232d0372d28b695` |
| 37c tree | `ca86b375d218209965f2202e0d4925b35fb229ac` |
| db.rs @1c12 | `143528696d5e19491e129a79cfb064923516c8ae` (== @d808055) |
| db.rs @37c | `7345991c1c2dd1d0e31ec27c9506a3c1ef4b0c2a` |
| failure-repair receipt @1c12 | `96ee7fba16c4f9965d5c5a9c1cf86d7026e98a45` |
| required-proof receipt @1c12 / @37c | `cca7c4720ac68ff7722230ebd3a71c8383d0fbf6` (unchanged) |
| check.rs / tx.rs / types.rs @1c12 | `5c654c83...` / `fd10680a...` / `22fcbabb...` (unchanged 37c..1c12) |
| design.md @1c12 | `32dd58a8a7cb02aa54c59b860ffb53c0e78e67cb` |
| remote branch tip | `1c12ef84302d954898ef01d7b01f5e268ee45a2e`, matches local |

The worktree copy of the failure-repair receipt is byte-identical to the committed blob (sha256 `311aa1eb...`); it shows as untracked in this primary checkout only because this checkout's HEAD (`9874afae`) predates it.

## Delta shape

`37c197b5..1c12ef843`: two commits, two paths.

- `d808055` `test(meta): repair T10 floor ordering and expose T11 timing to CI`: `crates/cowfs-meta/src/db.rs` +50 -10.
- `1c12ef8` `docs(meta): mirror the PR 140 proof-failure repair receipt onto the branch`: adds the receipt (+209).

Five hunks in `db.rs`, oldStart 2416, 2437, 2468, 2501, 2520. The `#[cfg(test)] mod tests` opens at line 1972. Every hunk is after 1972, so every changed line is inside the test module. No production line moved. `check.rs`, `tx.rs`, `types.rs` are unchanged across the delta. Tests-only production delta: CONFIRMED.

## Author root cause: CORRECT, independently reproduced

At 37c the test body was:

```
db.rs:2439  let next = m.reserve_inodes(4).unwrap();   // durable floor 1000402 -> 1000406
...
db.rs:2442  let again = Meta::open(&path, opts()).unwrap();
db.rs:2443  assert_eq!(again.health().recoveries, 1, ...);          // passed
db.rs:2444  assert_eq!(again.health().ino_floor, floor, ...);       // FAILED 1000406 vs 1000402
```

`floor` was captured at recovery time from `rec.ino_floor` = 1000402. The immediate post-recovery assert `m.health().ino_floor == floor` (37c line 2419) had already PASSED, so the store agreed with the recovery object before the extra reservation. Then `reserve_inodes(4)` legitimately raised the durable floor to 1000406. The reopened `health().ino_floor` (which reads `s.ino.reserved`, db.rs:450) was therefore 1000406. The test advanced the floor and demanded it had not moved. Author's account matches the logs exactly.

## The repair, inspected

### T10 (db.rs:2419-2467)

- NEW discriminating assert (db.rs:2424-2428): `assert_eq!(floor, original.end().0, ...)`. This is genuinely new; at 37c only `floor >= original.end().0` existed (37c line 2414). Correct discriminator: `record_recovery` prefers the bound (`ino_reserved_intent = end`), so a rollback of only the floor move yields `floor == end`, while a deeper rollback that also lost the bound falls back to `old_floor + block` and lands strictly below the end. Equality fires if the damage selects a deeper rollback. This answers the prior review's "which commit was lost" gap with an observable, not a guess.
- The reopen check is now monotonic (db.rs:2452 `let raised = next.end().0;`, then `again.health().ino_floor >= raised`, and `reopened.start().0 >= raised`). Preserved invariants: `floor >= original.end().0` (2402), no reuse on handle (2445) and after reopen, `rolled_back`, `recoveries == 1`, `durable_bound(&m) == None`, survivors, `m.check()`, `again.check()`.
- No assertion was weakened under the excuse of the fix. The slackened compare (exact equality -> `>= raised`) is compensated by the stronger equality-to-end and by keeping the `>= original.end().0` bound. The bound is still enforced, just against a live-raised value instead of a stale one.

### T11 (db.rs:2484-2558)

- NEW `emit` helper (db.rs:2493-2501): opens `/dev/stdout` write-only, writes the line plus `\n`, falls back to `println!` on any error. Standard library only, no new dependency, no `libc`, no unsafe.
- Portability: `/dev/stdout` is a symlink to `/proc/self/fd/1` on Linux and exists as fd 1 on macOS. Both targets of this project (Ubuntu, macOS CI) resolve it. VERIFIED portable for the two CI OSes.
- Why it is needed: ci.yml:23 runs `cargo test --workspace` with NO `--nocapture`, so libtest captures `std::io::stdout` and hides it for passing tests. Only the `cargo-fuse` step (ci.yml:61) uses `--nocapture`. So under the standard workspace step, a passing T11 prints nothing unless it writes fd 1 directly. The repair is the only path by which a passing T11 can surface, and it is the correct one.
- Strict units/reps/env/fresh-baseline: `Instant` wall-clock samples, `reps = 3` under CI else 5, `CI` env printed, fresh `tempfile::tempdir` per sample, representative full case (fresh store, `reserve_inodes(1_000_000)`, durable-floor assert on the range end, close/reopen/no-reuse) runs BEFORE the bounded n=1 vs n=1_000_000 comparison. No timing assert, so no flaky gate. Unchanged from 37c except the emit path.
- No `--nocapture` workflow edit, no threshold, no 1.5x filesystem-gate claim is made by the test.

## Runtime facts (one fresh read, no poll/dispatch/rerun)

| head | run | created | status | ubuntu | macos | linux-fuse |
| --- | --- | --- | --- | --- | --- | --- |
| 573 (prior green) | 37511041828 | 18:24Z | completed | success | success | success |
| 72f19f9 (test commit) | 37516750404 | 19:08Z | completed | FAILURE | FAILURE | success |
| 37c (parent) | 37517082073 | 19:11Z | completed | FAILURE | FAILURE | success |
| d808055 (repair code) | 37518490605 | 19:22Z | in_progress | pending | pending | pending |
| 1c12 (new head) | 37518576562 | 19:23Z | in_progress | pending | pending | pending |

### Completed 37c run 37517082073 (the head the prior review called pending)

- `check (ubuntu-latest)` job 112452678806: completed FAILURE. Aggregate over test-result lines: 778 passed, 1 failed, 70 ignored. T9 `the_inode_ceiling_admits_the_last_range_and_refuses_the_overflow ... ok`, T9b `a_range_ending_on_the_limit_is_the_largest_legal_one ... ok`, T11 `a_large_reservation_is_measured_against_a_single_one ... ok`, T10 `open_recover_keeps_a_reservation_bound_across_a_real_rollback ... FAILED`, panic at `db.rs:2444`, `1000406` vs `1000402`.
- `check (macos-latest)` job 112452679154: completed FAILURE. Aggregate 778 passed, 1 failed, 18 ignored. Same T10 failure, same `db.rs:2444`, same `1000406` vs `1000402`. T9/T9b/T11 ok.
- `linux-fuse` job: success (gated `cowfs-fuse` xattr failures only; not Meta).

This corrects the prior review's recording of 37c as ubuntu-only failure with macos pending: both check OSes failed on 37c.

### T11 output visibility: MISSING

Count of `inode reservation timing` lines across completed logs: 37c ubuntu 0, 37c macos 0, 72f19f9 ubuntu 0. The 37c run started 19:11Z, before the repair commit (d808055 at 19:22:29 local / 1c12 at 19:23Z), so its T11 used the old captured `println!` and printed nothing. The repaired `/dev/stdout` emit has never been observed in any log. No timing value is available. No SPEED PASS.

## Timings

MISSING. No numeric latency value was found in any completed log. No baseline, per-OS, n, or repetition figure can be quoted because none was emitted. The only measurement artifact is the committed harness; its output has not appeared. Any claim of a measured latency for this PR is unsupported by any run I can read.

## Current gate coverage

| gate | state at 1c12 |
| --- | --- |
| tests-only production delta | CONFIRMED (all hunks inside `#[cfg(test)]`, check.rs/tx.rs/types.rs unchanged) |
| T9 / T9b `INO_LIMIT` ceiling | PASSED at 72f19f9 and 37c on ubuntu and macos |
| T10 real `open_recover` rollback | FAILED at 72f19f9 and 37c; repaired source NOT yet run |
| T11 harness | PASSED at 72f19f9 and 37c; repaired emit NOT yet run; no output ever observed |
| repaired head green | UNPROVEN (runs 37518490605, 37518576562 in_progress) |
| measured latency figure | MISSING |
| 1.5x filesystem acceptance gate | NOT MEASURED by any of these tests (design.md:111) |

## Owed items after this review

| owed item | prior status | now |
| --- | --- | --- |
| Real redb `open_recover` repair for the reservation | FAILED (ubuntu) | still FAILED at 37c (ubuntu AND macos); repair source awaiting its run |
| `INO_LIMIT` boundary | PASSED | PASSED, unchanged |
| Measured latency | no harness output | harness repaired for fd 1; still no value; no run observed |
| Which commit was lost (newest-only) | unproven | NEW equality-to-end assert added; not yet executed |

## Acceptance gaps (do not accept `proofs-closed`)

1. Repaired T10 has not run. Its own CI run must confirm the `floor == original.end().0` equality and the `>= raised` monotonic reopen on an ordinary workspace build.
2. T11 timing has never been observed. The next run must show the `inode reservation timing:` lines in the log before any number is claimed. The `/dev/stdout` trick is correct in principle but unproven until its output appears.
3. A published timing figure would still be one data point at unknown CI load, not the 1.5x filesystem acceptance result and not a global speed pass.
4. `linux-fuse` green is not Meta/Core execution proof; the Meta evidence is the workspace `cargo test` job only.

## What was run and what was not

- Read-only git: full SHAs, blob pins, hunk line ranges, ancestry, remote tip. Done.
- Read the author receipt, `docs/design.md` at 1c12, and the ci.yml test invocation. Done.
- Read the bounded completed logs for runs 37516750404 and 37517082073 (all three jobs), via `gh api .../jobs/<id>/logs` with curl. Done. One fresh read of the new runs only to confirm they are still `in_progress`.
- NOT run: any `cargo build`/`test`/`clippy`, target dir, `git archive`, probe, cleanup, offload, cap waiver, commit, push, merge, issue close, new issue, lease, signal, daemon, or shared resource. No dispatch, rerun, runner, or workflow change. The 8 GiB cap is treated as binding; no local execution was attempted.
- Not touched: `ses_eed5b7b0fffesJmKd2fYb2N3xB` (READY3 Core consumer, fixing 01c commit-retry + deadcode, active), `ses_ef6d6f10cffeA7SelmYbknZnIX` (READY7 NFS43, active).
