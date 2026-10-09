# PR142 virtual-recovery review: exact-head correction

Corrects `pr142-physical-reopen-and-virtual-recovery-review.md` SHA256 `b5ab08e2a4cc88a1feb6204aa8f3aaac4c99787505324f46a0ad60ab394a5676`.
Those claims were derived from a stale primary checkout, not PR142 head.
No source changed. This append corrects the record; the prior receipt is not rewritten.

## Exact head, immutable blobs

PR142 head `39a87479593a8fb2e8a37b4cf98eab82b2764a59` (`test(core): assert physical identity...`).
Blobs at that commit: `ns.rs` `a0c931675596d97708677d2661ecb4886d09e4df`, `inner.rs` `7358303affa390b7e7ea04c2a8325000687c6978`,
`ino.rs` `fa3291b49d57956fb44c5034df8ebf464df17cba`, `lib.rs` `a8e95a87274802ae9b7f39a0c8c8dd33afcd404d`,
`critic2b.rs` `3518e478662c7e3c13828764ebd6e82bdfa4741d`, `db.rs` `809b1d25a2db6f341739d44be0c45d19d99d19ba`,
`reserved_inode_identity.rs` `e71bafbb607b8f6e746de18f062904283563ccc4`.

## False prior claims, corrected

1. `ns.rs::make` calls `self.alloc_virt(sc.id)`. FALSE.
   At head, `ns.rs:182` is `let ticket = self.take_reserved()?;` and `ns.rs:184` is `let ino = pack(snap, ticket.ino().0)?;`.
2. `inner.rs::alloc_virt` issues IDs and `reserve_inodes` does not exist. FALSE.
   No `fn alloc_virt` in `inner.rs`; `inner.rs:259` is `fn take_reserved`. `reserve_inodes` exists at `db.rs:858` and `db.rs:1820`.
3. `ino.rs::virt` is the live create path. FALSE.
   `ino.rs:67` `fn virt` is `#[cfg(test)]`; `ino.rs:4` states the production path only reads a mark.
4. `reserved_inode_identity.rs` is test-first against an unimplemented seam. FALSE.
   The reservation seam is implemented (`db.rs:1820`, `take_reserved`); the test drives the landed API and never ran in CI.
5. The two critic2b failures expose live recovery-wiring bugs. FALSE.
   They are obsolete fixtures over retired virtual machinery; see below.

## Snapshot-mismatch root cause

The prior review's `git show`/`grep` resolved the working tree, not `39a8747`.
Primary HEAD `9874afae288b51159738f4b5f4a243bd3c822856` has `ns.rs:177` = `let ino = self.alloc_virt(sc.id)?;` and zero `take_reserved`.
That is pre-PR source. The review read stale primary main and reported it as PR head.

## CI 37541353589, exact

headSha `39a8747...`, `cargo test --workspace` FAILED.
`core_atomic_rename.rs` 10/0 (deps `-9ba069cacaca2f5a`, 0.28s); `critic2b.rs` `FAILED. 25 passed; 2 failed; 1 ignored` (3.27s).
Fails: `critic2b.rs:390:5` `0x10000010002 starts at zero after a lost mark`; `critic2b.rs:332:74` `Os { code: 2, kind: NotFound }` reading `virt.ino.b`.
The run stopped at `critic2b.rs`; `reserved_inode_identity`, `durability`, `poison`, `swap`, `model` never ran (0 log lines). Unexecuted is not "would fail".

## critic2b: obsolete fixture, not live wiring

Child `virt_mark_child` reports `C first 1099511627778` = `0x10000000002`, VIRT bit CLEAR: a packed meta number, not a virtual one.
Failure 390 asserts `a & VIRT_COUNTER_MASK > (1<<32)` on that packed number. The virtual counter is retired, so the assertion is meaningless here.
Failure 332 reads `virt.ino.b`, which production never writes: `write_virt_mark` is `#[cfg(test)]`, called only from `ino.rs` unit tests.
`lib.rs:232-233` discards the recovered counter (`let (_, mark_warning) = ...`), keeping only a warning string; legacy `virt.ino` is read-only for pre-meta stores.
So both failures are a wrong-API fixture, not a mark-missing->SAFETY wiring bug.

## Preserved invariants (unchanged)

No reuse after rollback or lost mark; lost mark restarts at `SAFETY=1<<32` and logs `virt`; both copies alternate so one survives a crash; physical reopen keeps id, packed root, file number, bytes, durable meta non-virtual. No delete, skip, threshold-lower, or NotFound-ignore.

## Next implementation, if needed

Replace the retired-mark fixture with one that drives the live reservation API: crash-floor and no-ABA on `take_reserved`/`reserve_tickets`, keeping the original assertions (no reuse after crash, counter floor, identity stable across reopen). Source and runtime are separate: the head source is correct; the CI failure is the obsolete fixture, unmasked because the run stops before the reservation tests.
