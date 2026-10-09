# PR #76: persisted mark-cache upgrade blocker

## Status

Independent Sonnet review blocks source `69ae44874487e7e2a9fe2b24781f20706c916ead` on persisted cache compatibility.
The walked-root identity fix itself passed a deterministic real-Core regression with positive reclamation and clean reopened fork readback.
Do not merge or deploy this source, or treat PR #78's successful workload as upgrade-safety acceptance.

## Reproduced upgrade failure

The old collector can write a listed root paired with blocks from a different, newly committed root into `mark.bin`.
The corrected collector still trusts that persisted root key because the cache format is unchanged.
Independent review generated such a cache with the old collector while reclamation was disabled, then opened the same private store with the new collector.
The new cycle unlinked 22 packs, and the surviving fork's file failed after reopen.
Changing the cache magic to `COWMARK2` in the reviewer's private copy caused a full walk instead.
That control unlinked 21 packs and preserved the fork.

## Implementation acceptance

- Invalidate old mark caches through a versioned magic or an equally explicit compatibility discriminator.
- Treat the cache as derived data: recompute rather than trusting potentially mislabeled old associations.
- Reproduce the two-phase old-writer/new-reader upgrade using private stores and isolated source/target directories before patching.
- Add a committed regression that creates the old-format poisoned cache, then runs the corrected collector over the same store.
- Require reopened fork content and clean fsck, plus actual positive dead-pack reclamation.
- Preserve correct new-format cache reuse and fail-closed handling of incomplete or corrupt cache files.
- Correct both documentation statements claiming an already-persisted wrong-key mark cannot be reused.
- Never run collection or upgrade experiments against the shared daemon or store.

## Ownership and sequencing

The GC builder in slot 5 is currently repairing production feature wiring.
Queue this task behind that turn; do not launch an overlapping editor in its branch.
The independent report is in slot 13, `docs/reviews/gc-integration-final.md`, with private probes under its ignored outputs.
Preserve reviewer-owned untracked reports and historical evidence.
After both source repairs, require Sonnet re-review and a new private daemon validation against the exact final production head.

## Other findings to retain accurately

- Hook observations conflict: the daemon verifier observed feature unification enabling `test-hooks`, while the source reviewer saw no hooks in the normal dependency graph.
  Resolve this with the exact production build command, feature resolution and a compile probe, not one string search or a graph that omits relevant dependency kinds.
- The fairness change admits parked readers, but a measured handoff can consume the entire 50 ms per pack.
  This is a liveness improvement, not a proven performance gate pass.
- The relative stall assertion cannot discriminate a collector holding the barrier throughout the sweep.
  Do not claim it proves short writer stalls.
- A fresh collector may conservatively retain a removed base's garbage while another recorded root remains.
  This is safe retention, not the reproduced data-loss blocker, and needs separate follow-up tracking.
- Local Linux and full-stack crash injection remain unrun in this review.
