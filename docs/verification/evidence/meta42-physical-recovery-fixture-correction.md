# #42 critic2b: replace two wrong-API virtual-mark fixtures with physical-reservation recovery

PR #142 branch `fix/core-reserved-inode-consumer-42`, head `39a87479593a8fb2e8a37b4cf98eab82b2764a59`.
One file changed: `crates/cowfs-core/tests/critic2b.rs`. No production source touched.

## Why the two fixtures were wrong

`ns.rs::make` sets `ino = pack(snap, ticket.ino().0)` (reservation-backed packed meta number).
`ino.rs::virt` and `write_virt_mark` are `#[cfg(test)]`. So `critic2b.rs:390`
(`a & VIRT_COUNTER_MASK > 1<<32`) asserted a safety floor on a packed physical number, and `:332`
(`unwrap` on `virt.ino.b`) read a file production never writes. Obsolete fixture, not a live
recovery-wiring bug (CI 37541353589: `critic2b` 25 passed / 2 failed / 1 ignored).

## Exact replacement (source-pinned to head)

Removed: `virt_mark_child`, `a_rolled_back_virtual_mark_never_hands_out_the_same_number`,
`a_lost_mark_starts_far_above_the_old_counter_and_logs`. Added, preserving the no-reuse/safety intent:

1. `reservation_child_allocates_physical_numbers` - crash child. A real `create` pops a ticket from
   the durable reservation pool, returns a physical packed number, then `sync` + `abort` without
   close. Topology is physical identity (VIRT bit clear), not a virtual alias.
2. `legacy_mark_corruption_never_reissues_a_live_physical_number` - matrix `["zeros","delete",
   "zero-byte","torn"]`. Each case: child allocates/returns a live physical number then aborts;
   parent lays damage on the retired `virt.ino.{a,b}` files and **asserts the corruption landed**
   (not a no-op); reopens the real store; asserts `b.ino != first` (no reuse),
   `health().ino_floor > first & VIRT_COUNTER_MASK` (no ABA), old number returns no other bytes.
3. `a_physical_reservation_identity_persists_across_a_reopen_under_the_floor` - identity, floor,
   non-virtual-ness and bytes persist across a real close/reopen with legacy marks absent.

## Legacy coverage, unchanged (genuine unit API)

`crates/cowfs-core/src/ino.rs::tests::the_virtual_mark_round_trips_and_a_torn_one_falls_back_to_the_other`
(head `:292`) drives `read_virt_mark`/`write_virt_mark`/`Mark::counter` directly and asserts
`Mark::Value(0)`+state and `Mark::Missing`+state both yield `SAFETY` (`:308`). No test removed, no
`NotFound` ignored, no threshold lowered (the `1<<32` probe is gone because it read the wrong object,
not because the safety margin changed), no case skipped.

## Runtime

UNEXECUTED locally (resource cap binding). CI `37541353589` (head `39a8747`) predates this change;
the new head needs a fresh bounded run. Independent review required. #42 not claimed.