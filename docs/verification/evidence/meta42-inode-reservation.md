# meta42-inode-reservation: `Meta::reserve_inodes(n)` hands out inode numbers before any inode exists, written and unexecuted

Lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, READY5, held throughout.
Branch: `fix/meta-inode-reservation-42`, created fresh from `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`, which is the merge commit of PR #137 and the current tip of `main`.
Verified before any edit: the previous lease was clean on `fix/meta-snapshot-rename-42` at `b5e6f785eeee62f4a60401cf3ebbd113693d3872`, local equal to the remote, and `fix/swap-provenance-124` still at `6350468f049ad1e9087a72c62fe4231e2d832fe7`.
No reset, stash, rebase, force push or new lease.

## Scope: metadata only

This is issue #42 **request 4**, and only the metadata half of it.
Issue #42's own text asks for "`Meta::reserve_inodes(n)` **or** `Tx::create_with_ino`", and names the motivation precisely: Core's virtual inode numbers and alias table exist because meta "reserve[s] durably inside itself, but a caller still cannot get a number before the transaction that creates the inode".
**This adds the missing metadata capability. It removes nothing.** Core's `virt.ino` counter, its alias table, its virtual-inode translation and every existing on-disk format are untouched, and no file under `crates/cowfs-core` was read for editing or edited.
So this does not complete request 4, does not complete #42, and no Core behaviour changes.

One correction to a premise I was working from. The earlier note called the virtual-inode policy a settled decision. It is not: `docs/v1-core.md:570` heads that list **"Decisions for the lead to review"**, and item 1 is literally "Virtual inode numbers with an alias table, **in place of** asking meta for a reservation."
That is a proposal awaiting review, not an approval.
So the request was not blocked on it, and the API is introduced additively alongside the policy rather than as a replacement for it. When the policy is settled, retiring the alias table becomes a separate step with its own migration argument, not a consequence of this commit.

## What was read before writing anything

The design came from the actual allocator rather than from the summary, and one detail changed the implementation because of it.

`crates/cowfs-meta/src/tx.rs:66-79` is the allocator, and it is the whole contract:

```rust
fn alloc(&mut self) -> Result<Ino> {
    let a = &mut *self.ino;
    if a.next >= INO_LIMIT {
        return Err(Error::LimitExceeded("inode numbers exhausted"));
    }
    if a.next >= a.reserved {
        let new = (a.next + a.block.max(1)).min(INO_LIMIT);
        (self.reserve)(new)?;
        self.ino.reserved = new;
    }
    let ino = Ino(self.ino.next);
    self.ino.next += 1;
    Ok(ino)
}
```

`InoAlloc { next, reserved, block }` at `tx.rs:12-16` is `pub(crate)`, shared through `Session.ino` at `db.rs:200`, so the allocator is per-store rather than per-snapshot, and every snapshot draws on the same one.
`Tx` holds `ino: &'a mut InoAlloc` plus `reserve: &'a dyn Fn(u64) -> Result<()>` at `tx.rs:26-27`, wired to `Inner::reserve_durable` at `db.rs:785`.

The detail that mattered: `reserve_durable` at `db.rs:704-713` advances `ino_reserved` by whatever the caller passes, and `record_recovery` at `db.rs:729-764` bounds a lost commit by **one block**, moving the floor to `ino_reserved + block` after a rollback.
A reservation that committed its whole range in one step would move the floor further than that bound, so a rollback could reissue a number.
That is why this implementation commits **in block-sized steps**, exactly the shape `alloc` already uses, rather than one commit for the whole range.
It also means `record_recovery`'s existing invariant still holds unchanged, which is why no recovery code, no schema and no on-disk format needed touching.

The other load-bearing fact: `Meta::open` initialises the allocator as `next: reserved, reserved` at `db.rs:1405-1407`.
`next` therefore starts **at the durable floor**, so numbers below a committed floor are never reissued after a reopen, which is precisely the property this API promises.
And a fresh file stores `ino_reserved` as `2` at `db.rs:1349`, while the root is `Ino(1)`, so the allocator never hands out the root or zero.

`Options.ino_block` (`db.rs:100-106`) is fixed at creation and read back on reopen, so the tests use `ino_block: 8` to cross block boundaries cheaply. `wlock()` at `db.rs:358` returns `Error::Reentrant` when called from inside the `before_sync` hook, and `check_writable` at `db.rs:411` is the existing closed-or-poisoned gate.

## The API

```rust
pub fn reserve_inodes(&self, n: u64) -> Result<InoRange>
```

`InoRange` is a new public type in `types.rs`, next to `Ino`:

| Item | Meaning |
|---|---|
| `start()` | lowest number, inclusive |
| `end()` | one past the highest, **exclusive** |
| `len()` | `end - start`, the count |
| `is_empty()` | always false for a range this crate hands out; a zero request is refused |
| `contains(ino)` | membership, end exclusive |
| `iter()` | every number, lowest first |

O(1) to construct and hold whatever `n` is: it is two `u64`s, not an `n`-element `Vec`.
No new dependency, no new crate, no new trait or framework, and it reuses the existing `Ino` newtype.

Semantics, all of which are asserted in the tests rather than described here for the first time:

- The numbers are **contiguous** and come from the same `InoAlloc` that `Tx::create` draws on, so a reservation and an ordinary create can never hand out the same number.
- The **durable floor is committed before the call returns**, so a number that is reserved and never used is still not reissued after a reopen.
- `n == 0` is `Error::Invalid`. `n` beyond the numbers left below `INO_LIMIT`, including a count that would wrap `u64`, is `Error::LimitExceeded`. **Neither writes anything.**
- `next` advances only after the floor is durable, so a failure exposes no number.
- Because it commits, this is a **durable** operation, not an applied one, and it runs **no `before_sync` hook**, since it carries no chunk references. That is the existing behaviour of `reserve_durable` and is unchanged by this.
- It runs under the same `wlock()` as a batch, so it is serialised against creation and cannot deadlock: `reserve_durable` takes no session lock, which is the arrangement `mutate` already relies on at `db.rs:773-791`.

**What it does not do.** It hands out numbers. It does not create inodes, and there is no `create_with_ino` here.
Creating an inode at a reserved number needs a durable record of which reserved numbers have been consumed, or it risks reusing a number after a reopen or an aborted batch.
That is a real design question with ABA and reuse failure modes, so it is **not** guessed here.
Per instruction, if a small existing seam could consume explicit reserved numbers safely it is proposed in this receipt rather than implemented unreviewed; the seam I found, `Tx::create` at `tx.rs:219`, calls `alloc()` and has no path for a caller-chosen number, so there is nothing safe to reuse and **no extra API is proposed**.
Consuming these numbers needs either a durable consumption record or an in-memory ledger that survives abort correctly, and either way it needs a fixture proving old stores with an existing global floor plus Core's virtual range stay compatible.
That falsification is owed **before** any consumer, and is not claimed here.

## Files changed

Three, all metadata:

| File | Change |
|---|---|
| `crates/cowfs-meta/src/types.rs` | new public `InoRange` |
| `crates/cowfs-meta/src/db.rs` | `Inner::reserve_inodes` after `reserve_durable`; public `Meta::reserve_inodes` |
| `crates/cowfs-meta/src/lib.rs` | export `InoRange` |
| `crates/cowfs-meta/tests/inode_reservation.rs` | **new** integration test, 11 tests |

No `Cargo.toml`, no `Cargo.lock`, no schema, no new dependency, no provider, no daemon, no GC, no crash matrix.
No file outside `crates/cowfs-meta` was touched.

## The tests, and their status

Eleven tests in one new file, each aimed at a specific failure mode:

1. `a_reservation_returns_numbers_without_creating_anything` - numbers come back, length and exclusive end are right, start is above the root and below the limit, and `getattr` fails for every reserved number, so reserving does not create.
2. `a_reservation_is_contiguous_with_an_exclusive_end` - contiguity step by step, `contains` at the start, at the last, **not** at the exclusive end, and not one below.
3. `two_reservations_are_disjoint_and_the_second_follows_the_first` - the second starts exactly at the first's end and they do not overlap.
4. `numbers_reserved_and_never_used_are_not_reissued_after_a_reopen` - reserves 11 across a block boundary, creates **nothing**, drops, reopens, and requires that no number from the first reservation comes back and the allocator resumes at or above the old end.
5. `a_reservation_and_ordinary_creation_never_hand_out_the_same_number` - twenty reserved numbers against twenty creates, zero overlap, no repeats among created, and a later reservation reissuing neither.
6. `concurrent_reservations_are_disjoint` - four threads, five rounds of three, twenty numbers all distinct, none the root.
7. `a_reservation_racing_creation_stays_disjoint` - three threads alternating reservations and real `tx.create` calls, thirty-six numbers, all distinct. This is the case that would fail if the lock were not held across the commit.
8. `a_zero_reservation_is_refused_and_writes_nothing` - `Error::Invalid`, then the next reservation starts where a **freshly created** file's would, so the refused call provably moved nothing.
9. `a_reservation_past_the_limit_is_refused_and_writes_nothing` - `INO_LIMIT` and `u64::MAX` both `Error::LimitExceeded` rather than wrapping, and the allocator is still usable afterwards.
10. `the_durable_floor_only_moves_up_across_reservations` - mixed sizes `1, 2, 5, 3, 9`, never backwards, and after a reopen the allocator resumes at or above the highest handed out.
11. `a_reservation_leaves_existing_snapshots_alone` - an existing snapshot keeps its id and its exact root, and `check()` passes.

**None of these has been run.** See below.

## What was actually executed, and what was not

**Executed, and it is a real result:** standalone `rustfmt --edition 2021 --check` on the four owned files.
`rustfmt 1.10.0-stable (b940084d7e 2026-09-28)` was already installed, so nothing was installed; the workspace edition is 2021 and there is no `rustfmt.toml`.
The first pass found four deviations, in `db.rs`, `lib.rs` and the test file, and all four were corrected exactly as rustfmt asked.
Re-check: `types.rs` exit 0, `db.rs` exit 0, `lib.rs` exit 0, the test file exit 0, all four with **0 bytes** of diff.
That is a formatter result and nothing more.

**Not executed, and not claimed:**

- **The tests have never been compiled.** They are the first in this repo to be written against this API, so every signature they use was cross-checked by reading `db.rs`, `tx.rs`, `types.rs` and `error.rs`: `Meta::open`, `reserve_inodes`, `new_snapshot`, `snapshot_by_id`, `sync`, `check`, `Snapshot::getattr`, `Snapshot::batch`, `Tx::create`, `Error::Invalid`, `Error::LimitExceeded`, `Attr::ino`, and `Display for Ino` at `types.rs:228`.
- **`cargo clippy` has not run.**
- **No test has passed.** No green, no red, no local runtime of anything.
- There is no "old fail" to report, and none is claimed: before this commit the method did not exist, so the absence is a **compile error**, which is not a runtime failure and is not presented as one.
- **A genuine persist-failure injection is not available**, and this is a real gap rather than an oversight. `Options::before_sync` is the only failure hook, and `reserve_durable` deliberately runs no hook because it carries no chunk references, so there is no seam through which to make its commit fail. Test 8 and test 9 therefore prove the **no-write-on-refusal** half of the requirement, by showing the allocator did not move, and the **persist-failure** half is argued from source and from the existing rollback fixture rather than executed.
  Closing that gap needs a fault-injection seam on the durable commit path, which is a separate change and not smuggled in here.

## Budget

`bench/out` in this lane is **20.864 GiB** against an 8 GiB cap, and after removing every eligible compiler cache file the floor is 11.839 GiB, so the cap is unreachable without `bench/out/ready-40`, which holds another lane's mutation-golden source snapshots and receipts and is protected.
Nothing was deleted, moved, pruned or offloaded, and there is no cap waiver.
No cargo build, test, clippy, archive, new target directory or probe was run.
The only bytes written are this document and the tiny `rustfmt` logs under `bench/out/meta42-inode-reservation/`.
Free space is not the gate.

## State

Draft, held, not merged, and **not** a completion of request 4 or of #42.
It needs CI to actually compile and run these eleven tests, then a fresh independent review.
Nothing outside `cowfs-meta` changes behaviour, the Core consumer is untouched and still stages its own inode numbering, and no new issue was created.
Issue #42 stays open.
No browser verification. `no-mistakes` is uninitialized in this lane. Misakanet is local-only here and was not consulted.