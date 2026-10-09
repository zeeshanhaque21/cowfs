# #40 five recorded mutation controls: acceptance evidence

Lane: bounded acceptance work on the five named builder-suite mutant gaps.
Subject: issue #40 "Mutant gaps in the builder suite: follower acked before the leader's fsync, reap step not durable, magic check removed, snapshot limit removed, inode limit removed. The critic's harness kills the first; port it."
Accepted reference: `docs/reviews/meta40-current-delivery-and-acceptance-audit.md`, tested tree `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e`.
Current remote main at this read: `1580e69b9d987f63c07b2430f8c0b4547ecd8622`.
Commits `e488a17b..1580e69b` are PR #143 only (NFS namespace race, `7afe722`/`8c3f816`/`11e5c13`); no `crates/cowfs-meta` blob changed.
Blob-identity check, `e488a17b` vs `1580e69b`: `db.rs` `143528696d5e19491e129a79cfb064923516c8ae`, `types.rs` `22fcbabbb6fe9ce74906c387e9988f8d2fa830a4`, `error.rs` `c6cae08f5241ba49f9931f6efaa9cd935f1373d6`, `review.rs` `06d0fe37fa3ad124d453ada1b3b91c296ecdf5c8`, `critic.rs` `819dbdca1f7376913b0661ade180090d6f40bd43`, `inode_reservation.rs` `2f0deab56f87090248e5f585691b51775c21bd12` all SAME.
Method: read-only `git show`/`git grep`/`git ls-tree` at exact SHAs, issue #40 body + comments, critic report `spikes/nfs-loopback/out/critic8b/report.md`.
No local cargo, build, test, or mutation run was performed; resource prohibition observed.
No mutation was fabricated and no pass was claimed that was not recorded.

## Verdict

Original #40 mandates the harness: "The critic's harness kills the first; port it."
No ported harness and no discriminating test for any of the five exists at the accepted tree or at current main.
The critic's own results are lane-local (`crit2.rs`/`crit3.rs`), not in the repo, and are not the accepted suite.
Of the five, four have a documented equivalent control test by source and the audit permits that as an alternative; the follower one the issue explicitly requires as a port and does not permit substituting a green test.
None of the five has a recorded red-control-fails plus fixed-passes mutation result at a named head.
All five rows below are MISSING discriminating executed evidence.

## The five controls

| requirement | test / assertion | production / test blob compatibility | red-control / fixed-green evidence | smallest responsible next implementation if missing |
| --- | --- | --- | --- | --- |
| follower acked before leader fsync (issue says "port the critic's harness") | none; `git grep wait_durable\|leader\|follower` over `crates/cowfs-meta/tests/` at `e488a17b` returns nothing | source `db.rs:994 wait_durable` SAME across both heads; no test blob targets it | MISSING. Critic report item 11 records `follower-acked-early` SURVIVES the builder suite; the kill lived only in the lane-local `crit2.rs` harness (fol.log 407/602 ACKED-DURABLE). Not ported | port the critic `crit2.rs` B/D harness (4-thread `Ack::Durable` group-commit crash) into `crates/cowfs-meta/tests/`, asserting no caller is acked durable before its seq is durable |
| reap step not durable | `critic.rs:357 crash_every_event_reap_steps` exercises `reap_step` under crash but no damage-rewrites-the-reap-step mutant run | `critic.rs` SAME both heads | MISSING. Critic report item 11 records `reap-never-durable-step` survives, "harmless"; the audit calls it equivalent control, but no red-control-fails run is recorded | add a mutant run that rewrites/reverts a reap step and asserts the recovered queue redoes it (backlog bound), or record the executed control result |
| magic check removed | `review.rs:741 foreign_files_are_refused_and_left_alone` asserts `Error::Format` on a foreign redb | `review.rs` and `error.rs` SAME both heads | MISSING as mutation evidence. Critic item 11 records `magic-check-removed` survives the builder suite; the audit's line 108 offers `review.rs:741` as the equivalent control, but no old-fail/new-pass mutation logs exist | run the magic-check-removed mutant against `review.rs:741` and record the fail, or state the equivalent control is accepted |
| snapshot limit removed | `review.rs:375/381` `pack_ino` boundary asserts `None` at `SNAPSHOT_LIMIT`; audit cites `review.rs` packing + `LimitExceeded` | `review.rs`, `types.rs`, `inode_reservation.rs` SAME both heads | MISSING. Critic item 11 records `snapshot-limit-removed` survives ("no builder test at the limits"); crit3 killed it lane-locally only | add a test that drives `new_snapshot` to `SNAPSHOT_LIMIT` and asserts `LimitExceeded`, persisting across restart; then record the mutant run |
| inode limit removed | `inode_reservation.rs:325` `reserve_inodes(INO_LIMIT)` asserts `Error::LimitExceeded`; `:329` `reserve_inodes(u64::MAX)` | `inode_reservation.rs`, `types.rs` SAME both heads | MISSING. Critic item 11 records `ino-limit-removed` survives ("no builder test at the limits"); crit3 killed it lane-locally only. This is a count path, distinct from the M5 block-size clamp | add a test driving `Tx::alloc` to `INO_LIMIT` and asserting `LimitExceeded`; run the ino-limit-removed mutant and record the result |

## Audit of the earlier M1-M4/M6 "still open" reviewer claims

The canonical audit already marks M1, M1b, M3, M4, M6 DELIVERED with mutant-sensitive or equivalent tests; the stale reviewer claims are its own corrections, not a new regression.
No delivered fix is reopened here, and no evidence was found contradicting the audit on those items.
- M1: `health.rs:258 background_flush_survives_a_panicking_sync_hook` (blob `b45de310cbf84dda94b410796f562b4422e20c5a`).
- M1b: `health.rs:382 repeated_flush_failures_are_counted_reported_and_eventually_refused`.
- M3: `recovery40.rs` 4 tests (blob `3256eb193f2723ba8489e95edf81e3b94ee20ddf`) plus `health.rs:450 a_rollback_never_hands_out_a_number_it_already_handed_out`.
- M4: `review.rs` splice range tests; M6: `hole_flag.rs`.
These remain the audit's DELIVERED status; they are not the five controls.

## Explicit non-claims

- No claim whole #40 is complete: Core real-store crash harness (audit item 2) and Core `Health` wiring (item 4) remain separate and open.
- No claim any test passed or any mutant was killed in this lane; none was run.
- No treatment of an equivalent control test as an executed mutant.
- Old reports are immutable and were not edited.
