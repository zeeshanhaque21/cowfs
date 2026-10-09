# Issue #40 current delivery and acceptance audit

Reviewer lane: independent read-only audit (wbuddy lane).
Subject: issue #40 "v1: cowfs-meta follow-ups from round-2 review (health signal, open_recover counters)".
Prompt-to-artifact check: every checklist item from the issue body mapped against the delivered tree.
Tested tree: remote `main` = `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` (PR 140 merge commit).
Source-pin graph location then immutable pins: `git show` at exact SHAs; no local build, no checkout.
Prior reports `docs/reviews/metadata-health40-final.md` (review of PR 116 head `a1f30235`) and all `pr1xx-*` reviews are immutable and are not rewritten here. This audit corrects their provenance where the trackers are stale, per source.

## Verdict

Whole issue #40: **STILL OPEN**. Metadata half: substantially DELIVERED and MERGED, verifiable in source at `e488a17b`. Several items remain genuinely missing or source-only, listed below with the minimal owned seam each needs.

- The metadata requirements M1, M1b, M3 are DELIVERED, MERGED, and now backed by mutant-sensitive tests, not the non-discriminating PR 116 fixture. This closes both "must-fix" findings of the PR 116 review.
- M2 is a DELIBERATE fail-closed policy, not a bug, and is enforced by `check_writable` plus `live_handle_fails_closed_under_read_corruption`. The pending-window-on-drop half is UNCHANGED from the original finding and is a documented design choice, not a repair.
- M4 is DELIVERED in code and covered by `review.rs` range tests including the empty-at-boundary case.
- M5 is SOURCE-ONLY: the `u64::MAX` panic is fixed by a `clamp(1, INO_LIMIT)` at open, but no test drives `ino_block = u64::MAX` (the one `u64::MAX` test is `reserve_inodes(u64::MAX)`, a different path).
- M6 is DELIVERED via the hole-flag change and `hole_flag.rs`; kept in sync by `ChunkRef::validate` on both encode and decode.
- Crash harness still uses redb's `StorageBackend`, NOT the real `cowfs-store`. UNEXECUTED as asked.
- Mutant gaps: partially closed. The M3 mutants are now killed (recovery40.rs). The other named mutants (follower ack before fsync, reap durability, magic removed, snapshot limit removed, inode limit removed) are not one harness; some are covered by existing tests, none by a ported mutation harness.
- Core requests (`rename_snapshot`, batch timestamps, hole flag) are Core side, delivered by PR 141 (`rename_snapshot` consumer) and the hole-flag PR; NOT this metadata lane, and #42 stays open for the full consumer.

## Corrections to the stale tracker

The issue #40 comments and the PR 116 review are stale. Source and API facts at audit time:

| item | stale claim | actual |
| --- | --- | --- |
| PR 116 | "remains unmerged" / under review at `a1f3023` | MERGED, merge commit `efe9a93b1c664197a72cdeea6dc1e8376292405e`, head `fab4c6490c4274262976431384d7468b2e6a2de5` |
| PR 133 | delivered as `2c219a1c` | MERGED, `2c219a1cb6284d2bb1381145b118bfcac39800b2` |
| PR 131 | "will integrate this" | MERGED, `93cfef94457a989d031cb6b0a475ac4edbdb85ef` |
| PR 141 | Core `rename_snapshot` consumer | MERGED, `89353e17e5085000711dc428e834f9cc41840a1f` |
| PR 140 | request 4 reservation | MERGED at `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` (2026-10-06T19:46:24Z) |
| remote main | `89353e17` | NOW `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` |

`e488a17b` parents `89353e17` + `1c12ef84`; tree `f83e7625c11e9e942616d4040a3ce0ec60af9c95` matches the tested CI merge tree exactly.

## Pins

| object | SHA |
| --- | --- |
| remote `main` / tested tree | `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` |
| tested tree root | `f83e7625c11e9e942616d4040a3ce0ec60af9c95` |
| PR 140 head | `1c12ef84302d954898ef01d7b01f5e268ee45a2e` |
| PR 140 base | `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0` |
| PR 116 merge | `efe9a93b1c664197a72cdeea6dc1e8376292405e` |
| PR 116 reviewed head | `a1f302353bc829e30d4d09875df9536be5c1ffcd` |
| PR 133 merge | `2c219a1cb6284d2bb1381145b118bfcac39800b2` |
| PR 141 merge | `89353e17e5085000711dc428e834f9cc41840a1f` |
| db.rs @e488 | `143528696d5e19491e129a79cfb064923516c8ae` |
| recovery40.rs (new M3 tests) | blob in tree at `e488`, 4 tests |
| ready-40.md | `6c90f8617ee5f9a654b5727c57abb58f26be171c` |
| v1-meta.md | `07caf991a0c1720c7bf82ed296cc6bc65175ef93` |

## Checklist item-by-item: delivered vs source-only vs unexecuted vs missing

### M1 - panic in `before_sync` kills the timer thread silently

Status: **DELIVERED, MERGED, RUNTIME-backed.**
- `db.rs`: `Job::Flush` arm now wraps `catch_unwind(AssertUnwindSafe(|| inner.timer_flush()))` at the bg loop; `note_bg_panic` records `background_panics` and calls `note_flush_failure`, and `rearm_flush` restarts the timer. `Job::Reap` arm records `Ok(Err(e))` and re-arms.
- Health signal: `Health.last_flush_error`, `flush_failures`, `consecutive_flush_failures`, `background_panics` are public and reachable; `last_flush_error` is deliberately sticky.
- Test: `health.rs::background_flush_survives_a_panicking_sync_hook` uses a real background thread, real `before_sync`, real durability signal. PR 116 review reproduced the old-arm mutant failing (exit 101) and the fix passing.

### M1b - inline `Ack::Applied` hook panic invisible to Health

Status: **DELIVERED (the minor finding is closed).**
- `db.rs` inline commit path now `catch_unwind`s the commit, and on `Err(p)` sets `s.flush_err`, calls `note_flush_failure("the before_sync hook panicked on the inline commit")`, then `resume_unwind(p)`. So the caller still sees its panic, AND `Health` records why durability was not reached. This closes the PR 116 review's minor finding.

### M2 - one transient corrupt read poisons the whole handle; pending window discarded on drop

Status: **DELIBERATE POLICY (kept), coverage present; pending-window half UNCHANGED.**
- `Inner::note` sets `poisoned` on any `Error::Corrupt`; `check_writable` then refuses all writes with `Corrupt("handle refuses writes after detecting corruption; reopen and run check()")`. This is fail-closed by design, documented in `v1-meta.md`: "A detected corrupt read makes the handle refuse writes with `Corrupt` until it is reopened (F8)."
- Offline/healthy-read separation is respected: only `Error::Corrupt` from a read poisons; a healthy store is untouched. A different store or a healthy read cannot fake a corrupt read.
- Test: `critic.rs::live_handle_fails_closed_under_read_corruption` flips bytes at three rates, drives 80 real ops, and asserts `ok_wrong == 0` ("wrong data returned as success"), i.e. no corrupt read is ever reported as success.
- NOT changed: the pending window is still discarded on drop. This is a bounded-loss choice under the single-user model, not a zero-corrupt-tree guarantee. No tolerance was added, and no failure policy was changed. Flagged as intentional, per instruction not to alter policy without approved design change.

### M3 - `open_recover` reuses inode/snapshot numbers after rollback

Status: **DELIVERED, MERGED, and now MUTANT-SENSITIVE. Both PR 116 must-fixes closed.**
- Code: `ino_block` is persisted in the `meta` table at create (`db.rs` `m.insert("ino_block", ino_block)`), read back and validated on open (`ino_block of zero` refused), and the STORED block governs both `record_recovery` and the allocator. `record_recovery` advances `ino_reserved` to `floor.max(reserved).min(INO_LIMIT)` and `s.reserved`/`s.next` take the max, so the floor only moves up.
- BLOCKER fix proven by source: the smaller-block-at-recovery re-hand-out is now refused or floored from the stored bound, and an unknown/legacy bound (key missing) is REFUSED with a marked `RECOVERY_FAILED` error rather than guessed.
- Coverage fix proven by source: `recovery40.rs` (4 tests) replaces the non-discriminating PR 116 fixture:
  - `a_smaller_block_at_recovery_does_not_re_issue_inode_numbers` - builds with a large block, recovers with `ino_block=4`, asserts zero reuse against the actual `handed` set, then re-reads through a fresh handle.
  - `a_snapshot_id_lost_to_a_rollback_is_not_handed_out_again` - makes the snapshot the newest durable commit, so the `+1` bump is load-bearing, and asserts the lost id is not re-issued.
  - `recovery_refuses_a_file_whose_reservation_block_is_unknown` - strips `ino_block`, asserts the refusal message.
  - `the_stored_block_governs_a_plain_reopen_with_a_different_ino_block` - reopens with `ino_block=4096` and asserts no pre-crash number is re-issued.
- Mutant sensitivity: `build_reservation_latest` arms a failing hook so the newest durable commit is the reservation; `damage_newest` scans pages for a page that produces a real one-commit rollback. `ready-40.md` records the recorded mutants: PR head 3-of-4 covered, no-inode-bump 2-of-4. The PR 116 review's two surviving mutants no longer survive.

### M4 - empty splice at a non-boundary offset returns Ok and bumps the version

Status: **DELIVERED and covered.**
- `tx.rs::splice_content` rejects `start > end || end > covered`, requires the range to sit on chunk boundaries (`on_boundary` and `old_len == end - start`), and requires `new_size >= covered`.
- Coverage: `review.rs::splice_content_is_a_compare_and_swap_on_ranges` exercises `splice(100,100,&[chunk,gchunk],130)` (allowed append) and `splice(100,100,&[],90)` (rejected as shrinking below the chunk list). The empty-at-non-boundary case now errors.

### M5 - `ino_block = u64::MAX` panics in debug / wraps in release

Status: **SOURCE-ONLY.** The panic is fixed in source by `opts.ino_block.clamp(1, INO_LIMIT)` at open, but there is no test that passes `ino_block = u64::MAX`. The only `u64::MAX` reservation test covers `reserve_inodes(u64::MAX)` (a count), not the block size. This is the minimal missing test.

### M6 - zero-length `ChunkRef`s accepted and silently collapse

Status: **DELIVERED and covered.**
- `ChunkRef::validate` rejects a hole/non-hole/len mismatch; `types.rs::encode_chunks` and `decode_chunks` both call it, so nothing reaches or returns from the medium unvalidated.
- Coverage: `hole_flag.rs` (10 tests) includes `a_zero_id_ref_longer_than_a_hole_may_claim_is_corrupt_on_read`, `the_walk_yields_the_real_blocks_and_not_a_hole`, `the_encoding_of_a_hole_is_unchanged_by_the_flag`, and `ChunkRef::hole(0).validate()` boundary cases.

### Mutant gaps - follower acked before leader fsync, reap not durable, magic removed, snapshot limit removed, inode limit removed

Status: **PARTIALLY CLOSED, no single harness.**
- `crash.rs` and `critic.rs` crash-inject at every event and assert recovered state matches a committed boundary; `critic.rs::crash_every_event_reap_steps` covers reap durability; `store_before_meta_ordering_*` cover the ordering; `review.rs:741` covers `Error::Format` (magic); `review.rs` covers `SNAPSHOT_LIMIT`/`INO_LIMIT` packing and `LimitExceeded` via `inode_reservation.rs`.
- No ported mutation harness exists for these five named mutants as a suite. Source-only: the requirement "port the critic's harness" is not literally met as a harness, though equivalent control tests exist for most. Report honestly as partial.

### Crash harness against the real `cowfs-store`

Status: **UNEXECUTED.** `crash.rs` and `critic.rs` inject through a redb `StorageBackend` recording backend, not `cowfs-store`. No `cowfs_store::` use in either test. This remains as the issue states: re-run with the real store once core integrates both.

### Docs - lead the performance table with durable and random-access figures

Status: **DELIVERED.** `docs/v1-meta.md` `## Measurements` leads with durable create (19.3 ms single-thread; 18.4/5.5/1.2 ms at 2/8/32 threads group commit) and the random-access lookups (hot 1.9 us, random warm 18.6 us, cold 28.8 us, cache-off 56.6 us), with before/after columns. The corrections the review asked for are present.

### Core requests - atomic `rename_snapshot`, batch timestamps, hole flag on `ChunkRef`

Status: **CORE SIDE, MITIGATED.** `rename_snapshot` consumer landed via PR 141 (`cowfs-core/src/lib.rs:352`). Hole flag landed in `cowfs-store`/`cowfs-meta`. Batch timestamps are Core-side (`inner.rs` batch time) and not a metadata API gap. These are #42-scope; the metadata lane correctly did not touch them.

### Core Health wiring

Status: **OUT OF THIS LANE.** Wiring `cowfs_meta::Health` into `cowfs_core::Health` is Core side. `Health` is public and reachable from meta; the consumer wiring is a Core seam, currently not delivered here.

## Runtime facts

- PR 140 head `1c12ef84` had all three CI jobs green: run 37518576562 COMPLETED SUCCESS (ubuntu `112457763474`, macos `112457763281`, linux-fuse `112457763559`), 156 suites / 1689 P / 0 F on ubuntu, 156 / 1686 / 0 on macos, FUSE 9 / 107 / 0.
- The tested merge tree is `f83e7625` (head merged into current `main` `89353e17`), identical to `e488a17b^{tree}`. No later integration compile is needed for PR 140.
- `e488a17b`'s own post-merge check-runs were still `in_progress` at audit time; the merge is the exact tested tree, so the head-run evidence carries.
- No local cargo/build/test was run for this audit. No source-only discovery is presented as runtime proof.

## Finite list of actual missing pieces (minimal owned seams)

1. M5 test: add a case passing `ino_block = u64::MAX` (and 0) at `Meta::open` in `crates/cowfs-meta/tests/` asserting clamp to `INO_LIMIT`/refusal, not a panic. Smallest seam: extend `recovery40.rs` or a new `ino_block.rs`.
2. Real-store crash harness: point `crates/cowfs-meta/tests/crash.rs` (and/or `critic.rs`) at `cowfs-store` once Core integrates both, replacing the redb `StorageBackend`.
3. Mutation harness for the five named mutants: port the critic's harness as a suite, or document the equivalent control test for each.
4. Core side: wire `cowfs_meta::Health` into `cowfs_core::Health` (Core seam), and land the batch-timestamp API if #42 requires it.
5. Whole #42: the reserved-inode consumer and snapshot acceptance remain Core/integration scope, owned by the active Core worker (`ses_eed5b7b0fffesJmKd2fYb2N3xB`, READY3) and NFS worker (`ses_ef6d6f10cffeA7SelmYbknZnIX`, READY7). Not this lane.

Do not close #40 until items 1-4 are resolved to the issue's satisfaction; item 5 is tracked under #42.

## Evidence trail

- Fresh reads: `git ls-remote` main (`e488a17b`), `git fetch` of `e488a17b` (no checkout), `git cat-file`/`rev-list`/`rev-parse` for parents + tree, `git show` of `db.rs`, `tx.rs`, `types.rs`, `health.rs`, `recovery40.rs`, `critic.rs`, `crash.rs`, `hole_flag.rs`, `inode_reservation.rs`, `review.rs`, `model.rs`, `ready-40.md`, `v1-meta.md`, `design.md` at exact SHAs.
- API reads: issue #40 body + 3 comments, PR 116/131/133/140/141 states, `e488a17b` check-runs.
- NOT run: local cargo/build/test/clippy/fmt, target dir, archive, probe, cleanup, offload, cap waiver, commit, push, merge, draft/issue/comment edit, CI rerun/dispatch, runner/workflow change, lease, signal, daemon, shared store, mount. No checkout. 8 GiB cap and free-space floor treated as binding.
- Not touched: `ses_eed5b7b0fffesJmKd2fYb2N3xB` (READY3 Core) and `ses_ef6d6f10cffeA7SelmYbknZnIX` (READY7 NFS).
- Corrections presented as source-pinned facts against the stale tracker; no historical report was edited.
