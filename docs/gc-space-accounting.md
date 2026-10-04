# GC space accounting: gross removal against net reclaim

Issue: #81.

`freed_bytes` was the only space figure a `gc` cycle reported, and it is the file length of every pack the cycle unlinked.
That number is gross: compaction rewrites the surviving live records of a pack into a new pack, so the space actually returned to the host is smaller than the bytes removed.
A report that says "freed 16 MB" when the cycle wrote 1.6 MB back is true about removal and false about savings.

This note defines three figures, keeps the legacy one honest, and states how each is measured so a concurrent writer cannot corrupt them.

## The three figures

A `gc` report now carries, in both the core library (`cowfs_gc::GcReport`) and the control protocol (`cowfs_ctl::GcReport`):

- `gross_removed_bytes`: the file length of every pack this cycle actually unlinked.
  It is the same number as `freed_bytes`, under an explicit gross name.
- `rewrite_bytes`: the bytes this cycle wrote into the new packs it created, file headers included.
  It counts a committed rewrite, and it also counts the bytes of an abandoned partial copy, because those bytes really did land on disk.
- `net_reclaimed_bytes`: `gross_removed_bytes - rewrite_bytes`, signed.
  It is computed in a wider integer and checked against the signed range, so a store large enough to overflow a subtraction cannot produce a wrapped, bogus positive net.

The legacy field `freed_bytes` keeps its original meaning and its original name.
It stays the gross figure so that an existing reader sees exactly what it always saw, and the explicit gross name is added beside it rather than replacing it.
`GcReport::reclaimed()` still returns the gross number for the same reason, and `GcReport::net()` returns the signed net.

Net is signed on purpose.
A cycle whose rewrite cost exceeds the bytes it removed reports a negative net.
That is the truthful no-savings outcome, and saturating it to zero would disguise an unprofitable cycle as a neutral one.

## Cycle-owned, not process-wide

Every figure is measured from what the cycle itself did:

- Gross accumulates one unlink at a time, from the length the store returns when it discards a specific pack, and only for packs the cycle actually unlinked.
- Rewrite accumulates the real file length of each new pack the cycle's compaction committed, plus, for a copy that was abandoned before commit, the bytes the target file had reached when it stopped.

The figures are deliberately not a before/after of the whole store directory.
A concurrent writer appends to the active pack and commits new snapshots while a cycle runs, so a process-wide delta would fold the writer's bytes into the cycle's accounting.
Dividing that delta into "removed" and "rewritten" is not possible from two directory sizes, and reporting it as the cycle's net would be a fabricated number.

Because net is cycle-owned, it equals the physical drop in store bytes only when the store is quiescent.
The regression test `a_mixed_pack_reports_gross_removed_rewrite_and_signed_net` asserts that equality under a quiescent store, then the identity `net == gross - rewrite` stands on its own under any concurrency.
`collections_run_beside_writers_and_forks_without_deadlock_or_loss` asserts that same identity on every cycle while three writers and a fork loop commit concurrently, which is what a concurrent writer's appends must not be able to distort.

## Dry run

A dry run changes nothing, so it has no actual gross, rewrite or net, and it reports all three as zero.
The estimate lives in `candidate_bytes`, which is the dead-record bytes in the candidate packs, and the human output labels it as an estimate rather than a result.

## Cancellation and partial failures

The counts follow what the cycle really finished, never what it planned:

- A cancelled cycle counts only the packs it unlinked before the cancel, and only the packs it committed or partially wrote.
  It never counts a pack it merely listed as a candidate.
- A copy abandoned part way is not committed and its pack is not unlinked, but the bytes it wrote to the abandoned target file are counted in `rewrite_bytes`, so net never overstates savings.
- A pack left in place because a condemned block became live again is counted in neither gross nor rewrite for that attempt.

## A failure after the unlink still counts as a removal

Removing a pack is several steps, and only the first one decides whether the bytes left.
The unlink is the point of no return: after it the file is gone, and a later failure cannot undo that.

So the store reports the effect and the failure separately.
`Store::discard` returns `Err` when the unlink itself did not happen, meaning nothing changed on disk and nothing is claimed.
It returns `Ok` when the unlink did happen, carrying the pack's file length in `removed_bytes` and, if a later step failed, that failure in `durability_error`.

The collector credits `removed_bytes` and reports `durability_error`. Both, not either:
counting alone would report a removal the store cannot vouch for, and reporting alone would claim no removal for packs that are really gone.

Before this, `discard_pack` returned only `Err` once the pack was gone, so the cycle credited nothing for packs it had unlinked.
Gross came out zero and net came out `0 - rewrite`, flipping the sign of a reclaim that had happened.
The pack also stayed named in the writer's in-memory map, so `fsck` on the same open core failed on a file the cycle had itself removed.

Two limits, stated rather than papered over:

- These figures describe the current physical store, not what survives a crash.
  A pack counted here whose unlink could not be made durable is a real removal right now that is unconfirmed for durability, and the daemon answers `io_error` rather than a quiet success.
  `GcReport::unlink_durability_errors` counts them.
- The error is not fixed by the counting.
  The acceptance record that says the pack was whole was not written, so the removal needs re-checking on the next open; `fsck` is what checks it.

Regression coverage is `a_failure_after_the_unlink_still_reports_the_removal_it_performed`, with `a_pack_the_store_still_holds_is_never_claimed_as_removed` as its control on the same fixture.
The fault is a private store-state mutation that makes the acceptance record impossible to rewrite, which fails a step strictly after the unlink.
It stands in for a post-unlink I/O error and is labelled as such: it is not a real `EIO` or `ENOSPC`, and it exercises the same branch one would take.

The abandoned figure is read from the target file's real on-disk length at the moment the copy stopped, and that happens on **every** exit from a copy, not only on a cancel or an exhausted budget.
A copy that fails part way through a batch (a corrupt source record, a short write, a failed sync, a failed commit) has still put bytes on disk, so those bytes belong in `rewrite_bytes` whatever ended the copy.
The on-disk length is the exact figure rather than the compaction's in-memory write cursor: a write that fails part way leaves the file longer than the cursor ever advanced, so the cursor would undercount.

Counting the committed rewrite and the abandoned figure together is not a double count, because a pack is only ever measured once.
A copy either commits, in which case its committed `file_bytes` is added, or it is abandoned, in which case the target file's length is added.
A target the copy never created contributes zero.

Regression coverage for this edge is `a_copy_that_fails_on_a_corrupt_live_record_still_accounts_the_new_pack_bytes`: it corrupts a live record after the new pack has been created and asserts the reported net equals the physical pack drop.
On the pre-fix code that test fails, reporting `rewrite_bytes: 0` for a cycle that left a new pack on disk.

## Human and machine output

The control protocol carries the three fields as new members of `GcReport`, optional on the wire.
A new payload always serializes all three.

A payload from before #81 carries none of the three.
Its gross is real and is taken from `freed_bytes`, but its **net is unknown**, and it is represented as unknown rather than as a number.
A legacy cycle may well have rewritten a pack; deriving `net = gross` or `net = gross - 0` would report a saving that was never measured.
So `rewrite_bytes` and `net_reclaimed_bytes` are `Option` on the control side, `None` meaning unknown, and the CLI human line for such a report says `rewrite and net unknown (legacy report)` instead of printing a false zero.

On the wire, `rewrite_bytes` and `net_reclaimed_bytes` are one unit: both present means a new report with measured figures, both absent means the net is unknown, and exactly one of them is a half-written report and is rejected.

`gross_removed_bytes` is separate, because it only restates `freed_bytes`: an absent one falls back to it, and a present one that disagrees is rejected.
A payload may carry the gross alone, which is what serializing a legacy report produces, since a legacy report knows its gross even though it does not know its net.
A `net_reclaimed_bytes` that is not exactly `gross - rewrite`, or whose difference does not fit an `i64`, is rejected.
Every one of those checks exists so a malformed report surfaces as an error instead of decoding into a wrong number.

### An unknown figure is omitted, not null

A report serializes an unknown `rewrite_bytes` or `net_reclaimed_bytes` by leaving the field out, rather than writing `null`.
So a legacy report serializes to the shape a pre-#81 server would have sent, and decodes back to the same value.

This matters because the CLI prints a decoded report straight back out: `cowfs --json gc` writes `response.data_json()`. When the two unknown figures were written as `null` while the gross stayed present, that JSON was the shape the decoder called a half-written report, so the CLI emitted output its own protocol layer rejected, and any consumer that piped `cowfs --json gc` into a cowfs-protocol parser or stored the JSON to replay it got a hard error on output the tool had just produced.
The regression test `a_legacy_gc_report_round_trips_through_the_json_the_client_prints` decodes, prints through `data_json()`, and decodes again, asserting equality, and it fails against the `null` form.

The CLI human line for a real cycle reads `freed N blocks; X removed (gross), Y rewritten, net Z reclaimed`, and the dry-run line labels its number an estimate.

## A failed cycle still reports its cost

A cycle that hits an error after writing a new pack but before unlinking anything has spent real bytes and reclaimed none.
The daemon returns that as an error, but the message carries the actual gross, rewrite and signed net, so a caller sees the cost the cycle incurred rather than a bare "freed nothing" that hides a rewrite behind a wall.

## Acceptance

```text
cargo test -p cowfs-gc --test core_reclaim
cargo test -p cowfs-ctl --test wire
cargo test -p cowfs-cli
python3 scripts/verify-gc-daemon.py --work <private work dir>
```

The end-to-end verifier records the real numbers of one real cycle on a private store and asserts `gross_removed_bytes`, `rewrite_bytes` and `net_reclaimed_bytes` agree with the physical pack sizes before and after, under the record `reclaim.reported_gross_rewrite_net_agree_with_physical`.
