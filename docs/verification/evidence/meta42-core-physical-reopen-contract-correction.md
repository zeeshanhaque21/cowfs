# #42 Core: reopen identity is physical, not a virtual session alias

Old head `b096e9a438e74bb58eb58383632ca03faae05a34`; run `37538981596` completed `failure`, both `check` jobs (ubuntu + macos).
Failure: `crates/cowfs-core/tests/core_atomic_rename.rs:232` `assert_ne!(reopened_file, file)`, `left == right == 1099511627778`.
`1099511627778 = 0x10000000002 = pack(snap=1, m=2)`: a packed physical meta number, virtual bit clear.

ID classification: `ns.rs::make` sets `ino = pack(snap, ticket.ino().0)`, a reservation-backed packed meta number, not a `VIRT`-tagged virtual alias.
The old assertion encoded the pre-reservation contract ("a new session must not reuse an earlier session's virtual inode").
The branch's own `reserved_inode_identity.rs` pins the opposite: a created number is durable and identical across reopen.
A fresh `Core::open` resolves the file via `dent_lookup` -> `canon` -> `pack(snap, m)`, so the same number is the correct new behavior.
So the same ID does not violate the physical contract; the virtual-reuse expectation is the stale artifact.

New head `39a87479593a8fb2e8a37b4cf98eab82b2764a59`, run `37541353589` completed `failure` (target already green).
Replaced the weak `assert_ne!` with stronger checks, existing asserts (`e.id`, `e.ino`, exact bytes, `work` NotFound, `check`) preserved:
`reopened_file == file`; first-session number non-virtual; reopened number non-virtual; `meta_inode(reopened_file) == Some(file & VIRT_COUNTER_MASK)`.
`core_atomic_rename` on the new head: `test result: ok. 10 passed; 0 failed`.
Legacy virtual stale/session tests untouched; one file changed; no old ID discarded; no production, alias, Meta, NFS, CI, conformance or dependency change.

Remaining runtime (independent, not in any owned path; `crates/cowfs-core/tests/critic2b.rs`, not touched):
`a_lost_mark_starts_far_above_the_old_counter_and_logs` (`critic2b.rs:390`, asserts legacy `a & VIRT_COUNTER_MASK > (1<<32)`) and
`a_rolled_back_virtual_mark_never_hands_out_the_same_number` (`critic2b.rs:332`, `unwrap` on OS `NotFound` from the `virt_mark_child` helper).
Both are legacy virtual-mark expectations contradicted by the same reservation design; out of this narrow task's scope and owned by another writer.
Whole-#42 acceptance is not claimed. No PASS is claimed beyond the two named CI results above.
