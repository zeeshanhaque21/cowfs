# #40 original-requirement acceptance checklist

This is the previously recorded current-main acceptance audit, now written in the primary checkout.
It maps the original #40 body to delivered evidence and missing acceptance, without inventing new requirements.
It is a coordinator audit, not an independent critic approval, and #40 stays open.

Verified current main is ca164b8d55cdcc518e6275f4e1a30e27f73a57b0.
PR #152's tested merge 68eb5ce6df04f9e7ad0fdc67808f07ce6efd1c8d shares its tree 3127e319804f8ffec0dcd063ce2662efdb06a80c.
Direct run 37557242357 logs from macOS job 112586187039 and Ubuntu job 112586187394 supply the named retained tests identified below.
Source inspection of pending fixes is separate from runtime on this main tree.

| Original #40 requirement | Concrete evidence | Current acceptance |
|---|---|---|
| M1 background hook panic and repeated flush health | Both jobs directly pass `background_flush_survives_a_panicking_sync_hook`; verified #147 receipt covers repeated Meta failures and full Core health propagation | Delivered health slice; not a waiver of M2 |
| M3 recovery cannot reuse handed-out inode/snapshot numbers | Both jobs directly pass `a_smaller_block_at_recovery_does_not_re_issue_inode_numbers`, `a_snapshot_id_lost_to_a_rollback_is_not_handed_out_again`, and `recovery_refuses_a_file_whose_reservation_block_is_unknown`; original #116 receipt records persisted reservation bounds and its independent review | Delivered recovery slice with explicit refusal of unknown legacy bounds |
| M2 corrupt-read poisoning and discarded pending window | Both jobs pass `handle_fails_closed_after_detecting_corruption`; current `Inner::note`, `check_writable`, `commit` and `finish_on_drop` preserve poisoning and refuse a poisoned commit | Pending-window concern still open; passing refusal is not preservation proof |
| M4 empty splice at a non-boundary | #153 original Ubuntu job 112590729866 executes the public regression and returns incorrect `Ok(2)` at its intended assertion | Reproduced; fixed head 1c3e48b runtime pending |
| M5 extreme ino_block | Verified #144 receipt covers zero/MAX/legal extreme cases; both current-main jobs directly retain `a_zero_block_still_creates_with_a_valid_file` | Delivered allocator-extreme slice; this retained zero test alone is not the entire MAX proof |
| M6 zero-length refs collapse | #153 original Ubuntu job 112590729866 executes the public regression and returns incorrect `Ok(2)` for zero-length block before a valid replacement | Reproduced; fixed head 1c3e48b runtime pending, including hole and set_content assertions |
| Follower premature ack mutant gap | Verified merged #146 receipt includes positive follower and exact-panic branch bypass on both platforms | Delivered in-process control, not an external source-mutant rebuild |
| Reap durability mutant gap | #151 old corrected head passes both platforms; head 88e959f passes named reap/format controls on Ubuntu | Current post-#152 head c51eaa1 acceptance pending |
| Metadata magic removed mutant gaps | Verified merged #148 receipt has positive refusal and both exact-panic controls with equal tested/merged tree | Delivered format-control slice |
| Snapshot-limit removed mutant gap | #149 positive and exact-panic control pass Ubuntu before/after format integration | Current post-#152 head 190b5b6 acceptance pending |
| Inode-limit removed mutant gap | #150 old head passes both platforms; head ba2c388 passes inode/format controls on Ubuntu | Current post-#152 head 5fbbd39 acceptance pending |
| Real Store crash harness | Verified merged #152 runs real pack-file crash images, durable-byte positive and specific MissingLiveBlock control on both platforms | Delivered modeled crash images; not physical power loss or mid-GC proof |
| Performance documentation leads with durable/random figures | #152 commits the canonical v1-meta measurement reorder with all seventeen original metric rows preserved and separately attributed critic figures | Delivered historical-documentation correction, not new performance measurements |
| Core requests for rename, timestamps and hole flag | Existing #136/#137/#138 receipts and #42 tracker map their implementation slices | Remain linked to #42's broader recorded acceptance; no umbrella closure inferred |

## M2 source boundary

Current `crates/cowfs-meta/src/db.rs` sets poisoned on Error::Corrupt in Inner::note.
Inner::commit starts with check_writable, which refuses a poisoned handle.
Inner::finish_on_drop ignores its commit result, sets closed and clears the session snapshots.
Thus the source still permits discarding an uncommitted window after detecting corruption.
The current retained corruption fixture syncs before injecting flaky reads and asserts subsequent writes are refused.
It does not create an unsynced window before corruption or verify that window after drop and reopen.
These are inspected source facts and coverage limits, not a newly executed pending-loss reproduction or a claim of durable data loss.
Do not silently unpoison the handle or commit unchecked state merely to make a preservation assertion pass.
Any correction needs a real public pending-window reproduction and must preserve fail-closed corruption handling and previously durable data.

## Bounded next acceptance steps

1. Verify #153's two fixed-head public tests, including unchanged attributes/version/refs, reopen, zero-length hole and set_content coverage, and legal boundary baselines.
2. Verify post-#152 #149/#150/#151 named controls on both platforms and merge only with guarded heads and tested/merged-tree equality.
3. Reproduce the M2 pending-window case before choosing its responsible-layer correction; keep corruption refusal and durability guarantees intact.
4. Keep Core/#42 integration and whole-filesystem physical-crash/performance/reliability gates separate from accepted #40 slices.

The do-nothing baseline is current main with the existing passing tests above.
No local Cargo, runner, shared daemon/store/mount, cleanup or lease operation was performed for this audit.
Fixed tracker scope remains 68 items, 43 accepted.

## Self-check

Accuracy 4/5: runtime rows cite direct named logs or explicit historical receipts; M2 pending loss remains source-only.
Completeness 3/5: every original #40 row is mapped, but M2 and current combined-head gates remain unmet.
Clarity 4/5: acceptance is separated from historical evidence; multiple pending heads still require careful tree binding.
Actionability 4/5: exact test names, head prefixes and next gates are supplied, but no independent critic completion is claimed.
Conciseness 4/5: one original-requirement table replaces inference from stale tracker fields.
Overall 3.8/5 for the acceptance map, not cowfs completion.
The highest-impact improvement is executed M2 pending-window proof and fixed-head #153 results.
The user would reasonably reject whole #40 closure now, so it remains open.
