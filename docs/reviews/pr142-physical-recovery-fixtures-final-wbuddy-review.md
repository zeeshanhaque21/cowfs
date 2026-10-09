# PR142 physical-recovery fixtures: final independent review

READONLY. Exact head reviewed: `84e5193548fbfddb92284c1bc17cedb1a6a6f762`.
Parent `39a87479593a8fb2e8a37b4cf98eab82b2764a59`; blob `0b5513a3e17b50244a78f32b29721f58998292ab` (critic2b.rs); +154/-39, one file, test-only.
Merge base with primary main `9874afae288b51159738f4b5f4a243bd3c822856` is `93cfef94`, so the reviewed tree is the real head, not main ancestry.

## Verdict

DO NOT ACCEPT as-is. The exact-head tree does not compile.

## Finding 1 (blocking): CI at this head failed on a compile error
Run `37545084282` (`head_sha` = this head, `event` = pull_request) concluded `failure`.
`check (ubuntu-latest)` failed and `check (macos-latest)` failed; `linux-fuse` passed.
Both failing jobs die at `cargo clippy --workspace --all-targets -- -D warnings`:

```
error[E0308]: mismatched types
   --> crates/cowfs-core/tests/critic2b.rs:396:67
396 |  let mut b = std::fs::read(&tb).unwrap_or_else(|_| encode_mark(2000));
    |  expected `Vec<u8>`, found `[u8; 16]`
error[E0308]: mismatched types
   --> crates/cowfs-core/tests/critic2b.rs:398:25
398 |      b = encode_mark(2000);
```

`ensure_mark`/`encode_mark` returns `[u8; 16]`; both new call sites in the `torn` branch need `.to_vec()`.
The new `reservation_child_allocates_physical_numbers`, `legacy_mark_corruption_never_reissues_a_live_physical_number`, and `a_physical_reservation_identity_persists_across_a_reopen_under_the_floor` tests never ran: the binary does not build, so the whole `critic2b` target is excluded.
The prior receipts' claim that a fresh bounded run was still owed is now resolved: the fresh run happened and it failed.

## Finding 2 (blocking): "runtime passes" cannot be claimed

No green run exists at this head for any gate that matters.
Run `37541353589` is a prior head, only `10/0` critic2b, not this tree.
`reserved_inode_identity.rs` has 4 tests (`a_created_file_keeps_one_durable_identity_across_a_flush_and_reopen`, `a_created_number_is_never_the_virtual_alias_shape_or_the_root`, `a_virtual_alias_number_is_stale_after_a_reopen`, `a_create_that_failed_its_first_flush_keeps_its_number_and_bytes`) plus the named MetaT12/T14 and reservation T11, and alias/reserved inode coverage.
None of their exact result lines were produced here: the workspace first-failing binary stops later test targets, so "preserved coverage intact" is unverified, not proven.

## What the source does correctly (once it compiles)

Retired-API diagnosis is accurate at this head: `ns.rs::make` packs via `pack(snap, ticket.ino().0)`; `ino.rs::virt`/`write_virt_mark` are `#[cfg(test)]`; no live `alloc_virt`.
`legacy_mark_corruption_never_reissues_a_live_physical_number` drives the four damages (`zeros`, `delete`, `zero-byte`, `torn`), asserts each landed (`:360` zeroed, `:369` deleted, `:378` emptied, `:399` copies disagree), then asserts on the physical authority: `b.ino != first`, `ino_floor > first & VIRT_COUNTER_MASK`, and `first` does not resolve to `SECOND`.
Child really allocates (`fs.create` pops a durable ticket), `sync`s and `abort`s; the parent asserts the VIRT bit is clear, correct for a physical number.
`a_physical_reservation_identity_persists_across_a_reopen_under_the_floor` asserts no legacy marks, identity/bytes across a real close/reopen, and a fresh number past the earlier one (no ABA).
`child()` parses stdout only; abort yields status 0, so no false-pass here, but a non-abort child failure parsing `C first` would pass - latent, not a bug for these cases.

## Coverage gaps before re-claiming

- The two replacement tests never execute at head; the compile fix is required proof.
- `torn` invents `virt.ino.a`/`.b` via `encode_mark`; confirm the assertion still holds when both are genuinely absent (reservation never writes them), or label it a read-side robustness probe.
- Confirm `VIRT_COUNTER_MASK` equals the reservation floor width so `floor > first_meta` is not apples-to-oranges.

## Report integrity and runtime pins

`head_sha` `84e5193...`; run `37545084282` `completed/failure` (`check` ubuntu+macos failed, `linux-fuse` success); parent run `37541353589` prior head, not this tree.
Exact-head log is the authority; it shows only the two E0308s above and no test result lines.
Report SHA256 over this file at write time; READONLY, no local cargo/build/test/checkout/GitHub mutation, no production patch, scope fixed to the one test file plus this document.
