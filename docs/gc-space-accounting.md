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

## Dry run

A dry run changes nothing, so it has no actual gross, rewrite or net, and it reports all three as zero.
The estimate lives in `candidate_bytes`, which is the dead-record bytes in the candidate packs, and the human output labels it as an estimate rather than a result.

## Cancellation and partial failures

The counts follow what the cycle really finished, never what it planned:

- A cancelled cycle counts only the packs it unlinked before the cancel, and only the packs it committed or partially wrote.
  It never counts a pack it merely listed as a candidate.
- A copy abandoned part way is not committed and its pack is not unlinked, but the bytes it wrote to the abandoned target file are counted in `rewrite_bytes`, so net never overstates savings.
- A pack left in place because a condemned block became live again is counted in neither gross nor rewrite for that attempt.

## Human and machine output

The control protocol carries the three fields as new members of `GcReport`.
They are optional on the wire: a payload from before #81 decodes with `gross_removed_bytes` taken from `freed_bytes` and the other two as zero, and a new payload always serializes all three.
The CLI human line for a real cycle reads `freed N blocks; X removed (gross), Y rewritten, net Z reclaimed`, and the dry-run line labels its number an estimate.

## Acceptance

```text
cargo test -p cowfs-gc --test core_reclaim
cargo test -p cowfs-ctl --test wire
cargo test -p cowfs-cli
```
