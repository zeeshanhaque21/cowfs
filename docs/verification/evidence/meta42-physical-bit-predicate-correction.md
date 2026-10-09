# #42 critic2b B5: correct an inverted physical-bit predicate that false-failed CI

PR #142 head `010f00a` -> `17893f6`; one test-only file `crates/cowfs-core/tests/critic2b.rs`; no production/alias/Meta/NFS/CI/dep change.
Pre-fix CI run `37546185283` (`010f00a`) = `failure`; job `112550645880` (`check (ubuntu-latest)`), `linux-fuse` green.
`legacy_mark_corruption_never_reissues_a_live_physical_number ... FAILED` at `critic2b.rs:370:9`: `\`left != right\` failed: zeros: the child handed out a virtual alias, not a physical number: 0x10000000002` (left 0, right 0).
`0x10000000002` = `pack(snap=1, m=2)`, a packed physical meta number with bit 63 clear, so `first & (1 << 63)` is 0.
The wrong assert `assert_ne!(first & (1 << 63), 0)` demanded the VIRT bit **set** and rejected that physical number; its message read backwards. `17893f6` corrects it to `assert_eq!(first & (1 << 63), 0)`, the same predicate the sibling reopen fixture (`:505`) already uses.
No other bit predicate in the file is inverted; the retained `assert_ne!(b.ino, first)` no-reuse floor/check/readback asserts are unchanged.
Prior review `docs/reviews/pr142-physical-recovery-fixtures-final-wbuddy-review.md` (head `84e5193`) called it "VIRT bit is clear, correct", a false static approval; `84e5193` also failed to compile (E0308), masking the inversion until `010f00a` fixed the type mismatch.
Executed result on the fixed head `17893f6`, run `37546880854` (`check (ubuntu-latest)` job `112552900478`): `critic2b.rs` = `27 passed; 0 failed; 1 ignored`; the corrected B5 test is `ok`.
The same run then fails `durability.rs` (`4 passed; 2 failed`): `a_new_virtual_reservation_is_durable_before_any_of_its_numbers_is_handed_out` at `:145` (`the reservation did not rewrite the mark: []`) and `a_reservation_refuses_when_its_directory_cannot_be_made_durable` at `:183` (`a number was handed out from an unreserved counter`).
That is the same retired virtual-mark class (they assert a `virt_mark_renamed` trace and `sync_file:virt.ino` the reservation no longer produces), it was masked on `010f00a` because `critic2b` failed first and aborted the run, and it is in a file this task does not own: NOT fixed here, reported as out of scope.
`17893f6` is not acceptance-ready: the branch still fails `check` on `durability.rs`, and independent review is required.
Local `cargo build`/`test`: UNEXECUTED (resource binding). `rustfmt --check --edition 2021` on the changed file: PASS.
