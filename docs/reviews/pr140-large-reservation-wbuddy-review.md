# Independent source review: PR140 request 4, large inode reservation

Reviewer: wbuddy (independent, read-only).
Reviewed revision: `355b5fcaee9c87a1da1f527071be816f0b66dd57` (parent `1214142ffc17b1fedc3b31d3a6f2a343aa7e8d36`).
Scope: source review of the metadata-layer large-inode-reservation change only.
Verdict: **SOURCE PASS**, with three disclosed non-blocking gaps (see "Owed items").
Runtime status: **UNEXECUTED**. No build, no test run, no checkout, no archive.

## What was reviewed

The change makes a single large reservation of inode numbers durable and crash-safe.
The mechanism lives entirely in `crates/cowfs-meta/src/db.rs`.
The pinned blob is `355b5fcaee9c87a1da1f527071be816f0b66dd57`; all citations below are from `git show` of that pinned blob, not the mutable working tree.

Changed regions read at the pinned revision:

- `db.rs`: `INO_INTENT` key, `reserve_intent` (lines 784-798), `reserve_durable` (751-775), `reserve_inodes` (811-832), `record_recovery` (852-893), tests T1-T8 (1967-2222).
- `check.rs`: bound validation (61-71).
- `tx.rs`: `Tx::alloc` (66-79).
- `types.rs`: `INO_LIMIT` (line 75).

Evidence artifact: `docs/verification/evidence/meta42-large-inode-reservation.md`.
Its SHA-256 was verified as `f4850a34861a97c77120ede543246c747c84c15b3bf98c799079aa67bb5db2bb`, matching the author-reported value.

## Mechanism as reviewed

Two-phase reservation over redb, held under the write lock for the whole operation.

- `reserve_inodes` takes `self.wlock()` and runs both commits before releasing it.
- Phase one, `reserve_intent` (784-798): writes the `INO_INTENT` key recording the pending intent.
- Phase two, `reserve_durable` (751-775): writes `ino_reserved=new` and `meta.remove(INO_INTENT)` in one transaction (764-765).
- `record_recovery` (852-893): reconciles a store that reopened with a pending intent.
- Overflow is guarded at `db.rs:820`; `INO_LIMIT = 1 << 40` (`types.rs:75`).

`Tx::alloc` (66-79) allocates ordinary inodes through the `reserve` closure under the same `wlock` via `mutate` (`db.rs:902`), so ordinary allocation cannot interleave with a large reservation.

The background `reap_step` (`db.rs:1090`) never writes `ino_reserved` or `INO_INTENT`, so it cannot corrupt the reservation floor.

Plain `Meta::open` (`db.rs:1344-1346`) ignores `INO_INTENT`. This is safe because a pending intent implies the floor move did not land, so no inode number in the reserved range was ever exposed to a caller.

`open_recover` flips `GOD_RECOVERY` (`db.rs:1415-1417`), confirming the loss-of-latest-commit premise that makes two-phase reservation the correct shape.

## Failure cuts traced to source

Every named cut was traced to source and found handled:

- Fail before intent persist: no intent on disk, no floor move, nothing exposed.
- Uncertain intent persist: intent either present or absent; both reopen paths are safe.
- Fail before floor write: intent present, floor unchanged; `record_recovery` reconciles.
- Error after floor persisted: `reserve_durable` is one transaction, so floor write and intent removal commit together or not at all.
- Retry after a failed reservation: a fresh reservation replaces the intent.
- Reservation target decrease: handled; floor never moves backwards below a handed-out number.
- Ordinary `Tx::alloc` while an intent is pending: serialized by the same `wlock`.
- Future one-block alloc that would remove the intent: none found that removes an intent it did not create.
- Reopen with a pending intent and no repair: number not yet handed out, so no reissue.
- Legacy stores with no intent: absent key reads as no pending intent, behavior unchanged.
- `INO_LIMIT` overflow: guarded at `db.rs:820`.

No reachable cut was found where a handed-out number is reissued or the tree is torn.

## Owed items (non-blocking, disclosed)

F1. The eight new tests are uncompiled and unrun. This review did not build or execute the crate. The verdict is a source verdict only.

F2. No test drives the real `open_recover` repair path. T3 exercises `record_recovery` directly rather than through `open_recover`. The author did not rubber-stamp this as a real `open_recover` repair, and the evidence artifact discloses it (owed item 4). This is honest reporting, not a hidden gap.

F3. The O(1) claim is O(1) in commit count, not in latency. The two-phase shape bounds the number of commits, not their wall-clock cost.

## Out of scope

The core-consumer transition is request 4's deliverable and is not delivered. That is outside this metadata review's verdict and does not affect the SOURCE PASS above.

## Revision pinning note

This review covers the pinned revision `355b5fcaee9c87a1da1f527071be816f0b66dd57` only. A new head would require a separate final-delta review.

## Status

- SOURCE: PASS.
- RUNTIME: UNEXECUTED (no build, no test run).
- Blocking findings: none.
- Non-blocking gaps: F1, F2, F3 above.
