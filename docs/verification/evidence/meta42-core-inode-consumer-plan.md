# Core inode consumer plan: PR140 request 4

Status: read-only planning artifact.
Author: wbuddy.
Pinned Core main: `89353e17e5085000711dc428e834f9cc41840a1f` (verified against `refs/heads/main` on the real remote).
Pinned metadata head under review: `355b5fcaee9c87a1da1f527071be816f0b66dd57`.
No build, no test, no checkout, no write to any source tree.
All source citations are `git show` of these two immutable commits.

## Why this document exists

PR140 request 4 adds `Meta::reserve_inodes(n) -> InoRange` to `cowfs-meta`.
The tracker requirement (issue #42, request 4, required, not a new lane) is that the Core consumer creates files at numbers drawn from that reservation, durably, never reused, surviving reopen.
This plan maps the exact seam, states whether new `Tx` surface is required, and gives the minimal handoff. It does not implement anything.

## The one decisive fact

`cowfs-meta` at PR140 is explicit in its own doc comment (`db.rs:1726` region):

> This hands out numbers; it does not create inodes. Creating an inode at a reserved number is a separate concern and is not provided here.

So `reserve_inodes` reserves a contiguous `InoRange` and nothing more.
The commit that would turn a reserved number into an inode does not exist in either crate today.
That is the whole of the missing work, and it is why "just call reserve_inodes from Core create" is not a solution.

## Current producer API (pinned `355`)

- `Meta::reserve_inodes(&self, n: u64) -> Result<InoRange>` (`db.rs:1726`), delegating to `Inner::reserve_inodes` (`db.rs:811`, `pub(crate)`).
- Two-phase durable commit inside `Inner::reserve_inodes`: `reserve_intent(target)` first (`db.rs:784`), then `reserve_durable(target)` (`db.rs:751`). Both write `ino_reserved`; only `reserve_durable` clears `INO_INTENT`.
- `Inner::reserve_inodes` takes `self.wlock()` and advances `s.ino.next` to `target` before returning, so the range is consumed from the same allocator `mutate` uses.
- `record_recovery` (`db.rs:852`) reconciles a store reopened with `INO_INTENT` still set.
- `InoRange { start, end }`, `end` exclusive, exported from `lib.rs:22-23`. `INO_LIMIT = 1 << 40` (`types.rs:75`).

Consequence: numbers below the durable floor after `reserve_inodes` returns are genuinely owned by the caller and are never handed to anyone else, because `s.ino.next` has already moved past them. A number below `s.ino.reserved` but above the pre-call `next` is NOT provably owned; only the exact returned `InoRange` is.

## Current consumer path (pinned Core `89353e17`)

- `Inner::alloc_virt(snap) -> Ino` (`inner.rs:257`) is the only number source on the create path. It takes `next_virt.fetch_add(1)`, extends a durable mark via `reserve_virt` (`inner.rs:275`) writing `virt.ino.a` / `virt.ino.b` through `write_virt_mark`, and returns `ino::virt(snap, n)` (top bit `VIRT = 1 << 63`, `ino.rs:12`).
- `VIRT_BLOCK = 1 << 20` (`ino.rs:15`). The virtual reservation is Core's own, written to its own mark files, entirely separate from meta's `ino_reserved`.
- Create path: `ns.rs::make` (line 168) calls `self.alloc_virt(sc.id)` at line 177, builds a `NodeState` with that virtual `ino`, and queues an `Op::Create`.
- Batch path: `commit_batch` (`inner.rs` ~960-1009) opens `sc.snap.batch(|tx| ...)`, resolves the virtual child through the alias map, and calls `tx.create(p, name, mode)` / `tx.mkdir` / `tx.symlink`. The meta-assigned number is read back as `a.ino.0` and inserted into `newly`:
  ```
  let a = match what { Create::File => tx.create(p, name, *mode)?, ... };
  newly.insert(*child, a.ino.0);
  ```
- `Tx::create/mkdir/symlink` (`tx.rs:233/238/243`) each delegate to `new_child`, which calls `self.alloc()` (`tx.rs:66`). `alloc` returns `Ino(a.next)` and increments; it takes the number it decides, not one the caller passes. There is no `create_with_ino` and no reserved-number parameter anywhere in `Tx`.
- The alias map (`ino.rs Aliases`, `inner.rs:178`) bridges virtual number to meta number for the session; it is a session cache, not durable identity.
- Packed snapshot numbers use `pack(snap, m)` (`ino.rs:74`) and `MAX_SNAP`/`MAX_VIRT_SNAP` bounds. Virtual numbers carry snapshot id in bits 40..62 with the top bit set; meta numbers use `snap << 40 | m`.

## Does Core need selected-ID support in `Tx`?

Yes, if and only if Core must create at a reserved number.
There is no way today for a caller to make `Tx` create an inode with a chosen number. `alloc()` is the only number source on the create path, and it is private.

But the requirement does not say the number must come from `Meta::reserve_inodes`.
It says request 4 must make a large reservation durable and consumed by Core creation.
Two shapes satisfy that, and they differ in blast radius.

### Option A: Core keeps its own virtual allocator, meta reservation stays unused by Core

Not acceptable as the request-4 deliverable.
It leaves `Meta::reserve_inodes` with no consumer, which is the current state.
The tracker requires the consumer, so A is a non-starter for the required work.

### Option B: Core consumes `Meta::reserve_inodes` and `Tx` gains selected-ID creation

This is the only shape that closes request 4 as written.
It requires exactly one new seam in meta, plus the Core wiring to use it.

Minimal meta API (the smallest robust form):

```
// tx.rs, alongside create/mkdir/symlink
pub fn create_at(&mut self, dir: Ino, name: &[u8], mode: u32, ino: Ino) -> Result<Attr>
pub fn mkdir_at(&mut self, dir: Ino, name: &[u8], mode: u32, ino: Ino) -> Result<Attr>
pub fn symlink_at(&mut self, dir: Ino, name: &[u8], target: &[u8], ino: Ino) -> Result<Attr>
```

Each takes the refused-if-taken check and writes the inode record at `ino` instead of calling `alloc()`.
`new_child` already does everything else (directory entry, cookie, nlink, times); only the number source changes.

Safety and lifetime, in the same order the existing code enforces it:

1. The number must be one this `Meta` durably reserved and not yet created. The `Tx` cannot verify ownership from the number alone. `alloc()` maintains `a.next`; a selected-ID create must be admitted only from a range the allocator knows about, and the clean way is for `Inner::reserve_inodes` to hand out an owned token (see Option C) rather than a raw `u64`.
2. Refusal on reuse: if the inode record or a name already exists at `ino`, the create fails. `new_child` already checks `read::entry(dir, name)`; add the `getattr(ino)` check. This is the refusal/error path.
3. Replay: because the number came from a reservation whose floor committed before return, a replay after reopen can reuse the same number only if the original create never committed; the reservation floor already prevents any other caller from taking it, so a retry is safe and idempotent against the same range.
4. Lifetime: the `InoRange` is valid until consumed. The floor is durable, so the range survives reopen. A number in the range that is never created is wasted, never reissued. That is the same never-reuse rule `record_recovery` enforces.

No count cap is introduced: `reserve_inodes(n)` already refuses zero and refuses `n > INO_LIMIT - next`.

### Option C: owned token instead of raw number (recommended minimal form)

Rather than expose `create_at(..., ino: Ino)` where any `u64` is accepted, have `reserve_inodes` return numbers the create path can only obtain from a reservation.
The smallest form that is still one API: keep `InoRange` as the return, and add a type-level split so a selected create takes a value only a reservation produces.

```
// types.rs (already owns InoRange)
pub struct ReservedIno(Ino);   // constructible only inside cowfs-meta
impl InoRange { pub fn next(&mut self) -> Option<ReservedIno> }
```

`Tx::create_at` then takes `ReservedIno`, not `Ino`, so `create_with_ino(anything)` is not expressible.
This is one new type and one new method, no second allocator, no registry, no table, no format change.

Both B and C are proposals. C is the recommended minimal one because it makes "selected ID must come from a valid reservation" a type invariant rather than a comment.
Neither is settled; `docs/design.md` is not re-litigated here.

## Is a new meta API actually required, or can Core coordinate?

Core cannot coordinate its way to selected-ID creation, because:
- `alloc()` is private and is the only number source in `Tx`.
- `Inner` has no way to inject a number into a batch.
- The alias map maps virtual to meta number after the fact; it cannot force meta to pick a number.

So yes: request 4 as written needs the selected-ID seam in `Tx`. There is no Core-only path.

## What must not break

- Virtual numbers (`VIRT` bit, `VIRT_BLOCK`, `virt.ino.a/b`) are Core's own and are untouched by this seam.
- Packed snapshot numbers (`pack`, `MAX_SNAP`, `MAX_VIRT_SNAP`) are unchanged; a reserved meta number is an ordinary meta number below the floor and packs exactly like any other.
- The alias table (`Aliases.fwd/rev`) and its release-on-clean-directory rule (`ns.rs:398`, `inner.rs:625`) are unchanged.
- Snapshot-specific packed IDs are unchanged; the floor is a single store-wide counter (`ino_reserved`), not per snapshot.
- Legacy stores: a store written before the reservation intent has no `INO_INTENT`; `record_recovery` falls back to the block bound and refuses if even that is absent. Existing Core stores keep working because Core never called this API.

## Old-store / compatibility note

The reservation API is additive and backwards compatible: no schema version change, no new table, no new on-disk file beyond the already-authorized `INO_INTENT` key and `inode_reserved` marker in the `META` table.
Previous Core stores have Core's virtual mark files (`virt.ino.a/b`); those are untouched.
A number below the meta floor does not by itself prove ownership or that it is unused. Only a number taken from a live `InoRange` does. Any implementation that accepts an arbitrary number below `ino_reserved` is wrong and must be rejected in review.

## Ownership and sequencing

This plan cannot be implemented while the metadata worker owns `crates/cowfs-meta/src/{db,tx,types}.rs` and the reservation tests.
The seam lands in those exact files, which is the worker's current scope.
Blocked until the worker releases those files or explicitly hands them over.

My owned path is only this document.
I do not touch `db.rs`, `tx.rs`, `types.rs`, the reservation tests, the READY5 metadata/CI repair files, the ci-repair receipt, `main` progress, or my immutable `020` review.

## Acceptance tests to reuse (existing, no new validation matrix)

These already exist and are the differentiators. Run them against the new seam rather than inventing a matrix.

1. `crates/cowfs-core/tests/durability.rs::a_new_virtual_reservation_is_durable_before_any_of_its_numbers_is_handed_out` - prove a reserved number is durable before it is created.
2. `crates/cowfs-core/tests/critic2b.rs::a_rolled_back_virtual_mark_never_hands_out_the_same_number` - prove no reissue after a rollback.
3. `crates/cowfs-core/tests/names_ino.rs::virtual_inode_numbers_are_never_reused_across_a_restart` - reopen identity.
4. `crates/cowfs-core/tests/names_ino.rs::the_virtual_number_reservation_survives_a_crash` - crash survival.
5. `crates/cowfs-core/tests/alias.rs::a_create_past_the_alias_ceiling_is_refused` - refusal path.
6. Existing meta T1-T8 reservation tests at `db.rs:1967-2222` in PR140 - producer side.

OLD-FAIL / NEW-PASS differentiator, one end-to-end case:
- OLD: create a file, crash before the meta commit, reopen. Core's virtual mark may reissue or the file has no durable name. The old behavior fails identity-or-durability on reopen.
- NEW: reserve a large range, create a file inside it, crash, reopen. The number is the reserved one, the name is durable, and the number is never handed to another create. This is the public behavior a user sees: a file keeps its inode number across a crash and reopen.

## Handoff

- Seam file 1: `crates/cowfs-meta/src/tx.rs` - add `create_at`/`mkdir_at`/`symlink_at` beside `create`/`mkdir`/`symlink` (~line 233), reusing `new_child`; change only the number source.
- Seam file 2: `crates/cowfs-meta/src/types.rs` - add the owned-token type if Option C is chosen, next to `InoRange` (line 21).
- Seam file 3 (producer, already shipped in PR140): `Meta::reserve_inodes` (`db.rs:1726`); no change needed if Option C is adopted, since it already returns `InoRange`.
- Consumer file: `crates/cowfs-core/src/ns.rs::make` (line 177) and `crates/cowfs-core/src/inner.rs` `commit_batch` (~960) - draw the child number from a reservation instead of `alloc_virt`, and call the selected-ID create.
- Dependency: `crates/cowfs-meta/src/{db,tx,types}.rs` ownership transfers from the metadata worker.

## Status

- Plan: complete, read-only, source-cited.
- Implementation: BLOCKED, requires the metadata worker to release `db.rs`/`tx.rs`/`types.rs` and the reservation tests, and requires approval of the selected-ID seam (Option B or C) which is not in `docs/design.md` as a settled decision.
- Runtime: everything asserted here is a source read of pinned blobs. No build, test, or run was performed.
