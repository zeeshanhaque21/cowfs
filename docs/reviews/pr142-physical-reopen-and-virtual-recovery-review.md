# PR142 physical-reopen and virtual-recovery review

Scope: independent READONLY audit of PR142 head `39a87479593a8fb2e8a37b4cf98eab82b2764a59`, run `37541353589`.
Fixed request 68 scope. Whole issue 42 stays open. No code changed. Fixed-scope verdict only.

## Verdict

CONDITIONAL PASS for the fixed scope (replacement assertions in `core_atomic_rename.rs`), with a corrected-doc defect.
FAIL for the corrected-doc claim about `reserved_inode_identity.rs` and about `ns.rs::make`; both are unproven at this head.
The two critic2b failures are NOT obsolete failures. They are real and are currently correct tests of live production code.

## Exact CI counts (run 37541353589, both check jobs failure)

- `tests/core_atomic_rename.rs`: ubuntu `test result: ok. 10 passed; 0 failed` (deps `core_atomic_rename-9ba069cacaca2f5a`, 0.28s); macos `test result: ok. 10 passed; 0 failed` (deps `core_atomic_rename-c895e13d0dba8e84`, 0.61s). 10/0 on both platforms. Verified in that binary.
- `tests/critic2b.rs`: ubuntu `FAILED. 25 passed; 2 failed; 1 ignored` (3.27s); macos `FAILED. 25 passed; 2 failed; 1 ignored` (10.46s). Same two tests on both.
- `a_lost_mark_starts_far_above_the_old_counter_and_logs` panics `critic2b.rs:390:5`, message `0x10000010002 starts at zero after a lost mark`.
- `a_rolled_back_virtual_mark_never_hands_out_the_same_number` panics `critic2b.rs:332:74`, `Os { code: 2, kind: NotFound }` reading `virt.ino.b` in the `torn` branch.

## Production truth at this head (source-proven)

- `ns.rs:177` `make` calls `self.alloc_virt(sc.id)`. `inner.rs:257-270` `alloc_virt` -> `ino::virt(snap, n)`, which sets the `VIRT` bit (`ino.rs:64-67`). The create path STILL issues virtual IDs.
- No `reserve_inodes` exists anywhere in the tree (`grep -rn reserve_inodes crates/` = 0). The reservation seam is described in `reserved_inode_identity.rs` but not implemented here.
- The corrected doc's line `ns.rs::make sets ino = pack(snap, ticket.ino().0)` is false at this head. That describes the intended NEW seam, not this code.
- `reserved_inode_identity.rs` asserts `a.ino & VIRT == 0` on a created file, which fails on this tree. It is test-first and deliberately failing.

## CI execution gap (decisive)

- `cargo test --workspace` stops at the first failing binary. On ubuntu the run went `... core.rs -> core_atomic_rename.rs -> crash.rs -> critic.rs -> critic2b.rs` and stopped.
- Every cowfs-core binary sorted after `critic2b.rs` never ran: `durability`, `reserved_inode_identity`, `reserved_inode_metadata`, `names_ino`, `poison`, `swap`, `model`, etc. 0 occurrences in the log.
- Therefore `reserved_inode_identity.rs` was NEVER executed in this run. The doc's claim that it "pins the opposite" is static, not CI-verified, and would fail if run.

## Responsible fixture/production seams

- Production still issuing virtual IDs: `crates/cowfs-core/src/inner.rs::alloc_virt` and `crates/cowfs-core/src/ino.rs::virt`; called from `crates/cowfs-core/src/ns.rs::make`.
- Failure 390: `critic2b.rs:390` asserts `a & VIRT_COUNTER_MASK > (1<<32)`. The mark was lost (`virt.ino.a`+`b` removed) so `Mark::Missing` with committed state returns `SAFETY = 1<<32` (`ino.rs:186-198`, `ino.rs:202`). The file got `0x10000010002`, i.e. counter `0x10002`, which is below `1<<32`. The failure means the counter did NOT restart at SAFETY. Root cause is in the recovery wiring, not the test; a spike is needed to show whether `virt_reserved`/`next_virt` are initialized from `mark` before the first `alloc_virt` on a store whose mark was deleted (`lib.rs:231,243-244`). Not resolved by static reading alone.
- Failure 332: `critic2b.rs:332` reads `virt.ino.b` in the `torn` case and it does not exist. `write_virt_mark` writes only to `other` from `newest_copy`; on a fresh store `a=b=0` picks `(VIRT_A, VIRT_B)` so the first write lands in `virt.ino.b`, not `a`. The child `virt_mark_child` created one file then `abort()`; the parent then expects both copies. The exact on-disk state after the child aborts is not determined from static source and needs a spike.

## Is virtual allocation eliminated, or must legacy virtual sessions stay protected?

Neither eliminated nor obsolete. Virtual allocation is STILL the live create path here. Legacy virtual sessions MUST remain protected: `virt.ino` recovery, `write_virt_mark`, `Aliases`, and the lost/rolled-back mark invariants are all still load-bearing production.
The specific actual API issuing virtual IDs is `Core` create -> `ns::make` -> `Inner::alloc_virt` -> `ino::virt`. The two failing critic2b tests belong on that virtual path and must stay there.

## Recommendation (no code changed)

1. `reserved_inode_identity.rs` is test-first against an unimplemented seam. Either exclude it from `cargo test --workspace` until the reservation seam lands, or land the seam; do not present it as green evidence.
2. Do NOT delete the two critic2b tests, lower the `1<<32` threshold, or skip NotFound. They expose real recovery bugs on the live virtual path.
3. Investigate failure 390 as a recovery-wiring bug (mark-missing -> SAFETY not applied before first allocation). Fix production, keep the assertion.
4. If the virtual path is later replaced by reservation, the virtual-only tests may migrate to a fixture that drives the genuine virtual API, preserving the original safety assertions (no reuse after crash, counter starts at SAFETY). Until then, the physical path does not exist to prove equivalence.
5. Fix the corrected doc: it must not claim `ns.rs::make` uses `pack(snap, ticket.ino().0)` or that `reserved_inode_identity.rs` is passing.

## Preserved recovery invariants

- A rolled-back or deleted virtual mark never hands the same number out again (rolled-back/deleted/zero/zero-byte/torn).
- A lost mark restarts the counter at `SAFETY`, far above any prior session, and logs loudly with "virt".
- Both mark copies are written alternately so at least one survives a crash.
- Physical reopen: same id, same packed root, same file number, same bytes, non-virtual, same durable meta (`core_atomic_rename.rs` 10/0).

## Report hash

SHA256 recorded after write; see session return value.
