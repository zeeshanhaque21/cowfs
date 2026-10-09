# Core inode consumer safety correction: PR140 request 4

Status: read-only safety correction. Supersedes the token design in `meta42-core-inode-consumer-plan.md`.
Author: wbuddy.
Pinned Core main: `89353e17e5085000711dc428e834f9cc41840a1f`.
Pinned metadata head: `355b5fcaee9c87a1da1f527071be816f0b66dd57`.
Runtime: UNEXECUTED. Every claim is a source read of the pinned blobs via `git show`. No build, test, or run.

## What this corrects

The prior plan (SHA-256 `6973b58b…`, left immutable) proposed:

```
pub struct ReservedIno(Ino);   // constructible only inside cowfs-meta
impl InoRange { pub fn next(&mut self) -> Option<ReservedIno> }
```

as the "owned token" that makes selected-ID creation safe.
That claim is wrong, for three independent reasons found in the pinned source.
The prior plan is superseded on this point. Its producer/consumer mapping, the missing-commit finding, and the Core coordination argument all still hold.

## Why the prior token claim is unsound

### 1. `InoRange` is `Copy`, so the range itself is a capability anyone can duplicate

`types.rs:21`:

```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InoRange { start: Ino, end: Ino }
```

`Copy` means `let a = range; let b = range;` both work.
A `next(&mut self)` cursor on a `Copy` range does not prevent duplication: copy the range before iterating, and both copies mint the same numbers.
`pub const fn new(start, end)` (`types.rs:26`) is also public, so any caller can fabricate an `InoRange` for any numbers, including ones another caller holds.
`pub fn iter(&self)` (`types.rs:57`) yields every `Ino` in the range, repeatedly, with no consumption.
So a `ReservedIno` minted from `InoRange` proves only "this number was in some range someone had", not "this store reserved it and has not handed it out".

### 2. Private constructor does not prove consumption or store ownership

`ReservedIno(Ino)` with a private field stops a stranger writing `ReservedIno(7)`.
It does not stop:
- copying a live `ReservedIno` (if it is `Copy`), or cloning it (if `Clone`), to mint the same number twice;
- holding a `ReservedIno` from store A and passing it to a create on store B, because the type carries no store identity;
- replaying a `ReservedIno` after a reopen, because it carries no session generation;
- using a `ReservedIno` in the wrong snapshot, because it carries no snapshot id.

A private constructor bounds who can *make* a token, not what store or session the token *belongs to*, and not whether it was *already spent*.

### 3. The store/session identity exists but the prior token does not use it

The pinned source does have exactly the identity a safe token needs:

- `Meta { h: Arc<Handle> }` (`db.rs:1328-1329`).
- `Snapshot { h: Arc<Handle>, id }` (`db.rs:1783`), sharing the same `Arc<Handle>`.
- `Handle { inner: Arc<Inner> }` (`db.rs:1276-1277`).
- `InoAlloc` lives inside `Session` (`db.rs:244`, `Session` at `db.rs:240`), reached through `Inner.session`.
- `Snapshot::batch` calls `self.h.inner.mutate(self.id, f)` (`db.rs:1817-1818`), so a batch is bound to one `Arc<Inner>` and one `SnapshotId`.

The `Arc<Handle>` allocation identity plus the `SnapshotId` is the natural owner key.
The prior `ReservedIno(Ino)` discarded both.

## The copy / replay / wrong-store challenge, per boundary

One focused probe per load-bearing boundary. All read from pinned source.

### Boundary A: queue clone / replay (does Core duplicate the token?)

`Op` is `#[derive(Debug)]` only (`queue.rs:22`), **not `Clone`**.
`Batch` is `#[derive(Debug, Default)]` (`queue.rs:69`), **not `Clone`**.
The only `Clone` near the queue is `Create` (`queue.rs:14`), which holds `File | Dir | Symlink(Arc<[u8]>)` and carries **no inode number**.

- `Op::Create { child: Ino, ... }` (`queue.rs:24-30`) stores a Copy `Ino`, not a token.
- `Queue::drain` (`queue.rs:232`) moves ops out with `std::mem::take(&mut self.ops)`. No copy.
- `Queue::restore` (`queue.rs:247`) moves a failed batch back with `ops.append(&mut self.ops)`. No copy.
- `commit_batch` on failure calls `sc.q.lk().restore(batch)` (`inner.rs:846`). Move.

Result: the queue does **not** clone or duplicate whatever sits in `Op::Create.child`.
The token would ride through drain/restore as a move.

But this is exactly why the token cannot be the queue's Copy `Ino`: `commit_batch` reads `*child` many times (resolve closure at `inner.rs:942-960`, `b.elided.contains(child)` at `inner.rs:951`, `subject()` at `queue.rs:49`), and `ns::make` stores the number in `Node.ino` and the dentry table. Every one of those is a `Copy` `Ino`. If the token is `Copy`, those copies re-mint it.

So the type that travels in `Op::Create` must stay a plain `Ino` (Copy is fine there, because one queue entry is one logical create), and the *capability* to choose that number must live and be consumed at the point of `ns::make`, guarded by the store/session, not carried as a Copy token.

### Boundary B: wrong store (`ReservedIno` from store A used on store B)

A `ReservedIno(Ino)` is a `u64` with a private constructor. Store B's `Tx::create_at` cannot tell it apart from one of its own.
The pinned source has no check that would reject it: `Inner::reserve_inodes` (`db.rs:811`) advances `s.ino.next`, but nothing on the create path compares a passed number against `s.ino`.

Fix required: the token must be bound to the `Arc<Inner>` (or `Arc<Handle>`) that minted it, and `create_at` must reject a token whose owner is not `self`. `Arc::ptr_eq` on the handle is the identity check; it needs no table and no field.

### Boundary C: reopen / replay

After a reopen, `Inner` is a fresh allocation. A token holding a raw `u64` from the previous session cannot be distinguished from the new session's.
Binding to the `Arc<Inner>` pointer makes a stale token detectably foreign, so the create refuses rather than reusing a number under a new session.
Durability of the number itself is already handled by `record_recovery` (`db.rs:852`) and the floor; the token only needs to be refused if it outlived its session.

### Boundary D: cross-snapshot use and hardlinks

`InoAlloc` is per-`Session`, not per-`Snapshot` (`db.rs:244`), so the floor is store-wide and a number reserved on one snapshot is physically free on another. A create on snapshot 2 at a number minted while snapshot 1 was current would be a legal store-wide number.
Whether that is *desired* is a product question, but the safety rule is: the token must name the `SnapshotId` it was minted for, and `create_at` must be called within a `batch` on that same snapshot (or the token must be explicitly snapshot-agnostic by design).
Hardlinks (`Op::Link`, `tx.link` at `inner.rs` ~989) reuse an existing inode number and never allocate, so they are unaffected; the mechanism must not touch the link path.

## Smallest safe mechanism

Move-only, backend-owned, no new table, no registry, no format change, additive.

The two guarantees needed are (1) reserve and create are the same store+session, and (2) a reserved number is spent at most once.

Minimal design that gets both without a Copy capability:

```
// tx.rs - selected create takes an owned, non-Copy ticket
pub fn create_at(&mut self, dir: Ino, name: &[u8], mode: u32, ticket: InoTicket) -> Result<Attr>
pub fn mkdir_at(&mut self, dir: Ino, name: &[u8], mode: u32, ticket: InoTicket) -> Result<Attr>
pub fn symlink_at(&mut self, dir: Ino, name: &[u8], target: &[u8], ticket: InoTicket) -> Result<Attr>
```

```
// types.rs - non-Copy, non-Clone ticket; Ino is private; only cowfs-meta mints it
#[derive(Debug)]                       // no Clone, no Copy
pub struct InoTicket { ino: Ino }

impl InoTicket {
    pub const fn ino(&self) -> Ino { self.ino }   // read-only, value is Copy but the ticket is not
}
```

Ownership rules, each tied to a source fact:

1. Minted only inside `Inner::reserve_inodes` / the batch path, and stored in `Session` next to `ino: InoAlloc` (`db.rs:244`) so it is destroyed with its `Inner`. Spent once: `create_at` consumes the ticket by value, and `Inner` removes it from the session's outstanding set inside the same `wlock` that `mutate` takes (`db.rs:895-901`). No second mint for the same number.
2. Store binding: the ticket is minted from, and only usable with, the `Arc<Handle>` that owns the `Session`. `create_at` runs inside `mutate`, which already holds that handle's `wlock`; a ticket from another handle is rejected (the session's outstanding set is per-`Inner`).
3. Session/reopen binding: because the outstanding set lives in `Session` (in-memory, recreated on open), a stale ticket from a previous session is simply not in the set and is refused. No pointer comparison is strictly needed if the set is the authority; `Arc::ptr_eq` is a cheaper early rejection if wanted.
4. Snapshot: `mutate` is already snapshot-scoped (`self.id`), so a ticket spent on the wrong snapshot is refused by the same set check if the set records the `SnapshotId`, or allowed if the design decides the floor is store-wide. This is the one open product question (see below).
5. Refusal on reuse: `new_child` already checks `read::entry(dir, name)` (`tx.rs` create path). `create_at` adds the inode-exists check, so a number already named is refused rather than overwritten.

Why this duplicates no ownership: the ticket is `!Copy + !Clone`, so it cannot be duplicated to mint twice; it is `!Copy`, so it cannot be quietly copied into `Op::Create` and back out; and it is validated against a session-owned outstanding set that `Inner` alone controls. `Op::Create.child` stays a plain `Ino` (one per queue entry, Copy is correct there).

Why no new table/registry: the outstanding set is transient session state next to `InoAlloc` in `Session`, not a redb table. Durability of the number is already the reservation floor. Nothing new is persisted.

## Additive compatibility

`Meta::reserve_inodes(n) -> InoRange` (`db.rs:1726`) is unchanged and stays. `InoRange` stays `Copy` with its public `iter`/`new` for existing callers.
The ticket is a separate, additional type. The prior plan's mistake was trying to mint the capability *from* the Copy range; the correction is to mint it from the `Inner` session and never expose a Copy capability. No existing API changes.

## Direct seam vs allocator framework

Direct seam wins. A ticket plus three `*_at` methods reusing `new_child` is the whole mechanism.
An allocator framework (new trait, registry, per-store allocator object) is not needed for the existing request, adds surface, and is not justified by anything in the pinned source. Only add it if a second consumer with different ownership semantics appears; none exists.

## Unresolved real block (only one, and it is not approval)

The single genuine open question: **is the inode floor per-store or per-snapshot for the sanctioned use?**
`InoAlloc` is per-`Session` (`db.rs:244`), and `record_recovery` moves a single store-wide `ino_reserved` floor (`db.rs:852-890`). So the store's numbers are globally unique, but Core mounts snapshots and may create in one snapshot while a reservation was taken for another.
If the intended use is "reserve once, create across the mounted snapshot set", the ticket is snapshot-agnostic and the set need not record `SnapshotId`.
If the intended use is "reserve for one snapshot", the set records `SnapshotId`.
This is a one-line decision, and it is an implementation detail, not a new format, table, or policy expansion. It does **not** require separate user approval.

Corrected status: this is an **in-scope implementation handoff**, not "blocked on approval".
The earlier "blocked on approval of Option B/C" was overstated; the seam is within the already-authorized #42 large-reservation work. The only thing that gates *starting* is that the active builder owns the metadata files right now.

## Handoff

- Producer: no change. `Meta::reserve_inodes` / `Inner::reserve_inodes` already exist in PR140.
- Seam file 1: `crates/cowfs-meta/src/tx.rs` - add `create_at`/`mkdir_at`/`symlink_at` taking `InoTicket`, beside `create`/`mkdir`/`symlink` (~line 233), reusing `new_child`; add the inode-exists refusal.
- Seam file 2: `crates/cowfs-meta/src/types.rs` - add `InoTicket` (`!Copy`, `!Clone`, private `ino`) next to `InoRange` (line 21).
- Seam file 3: `crates/cowfs-meta/src/db.rs` - store the outstanding ticket set in `Session` (`db.rs:240-245`) beside `ino: InoAlloc`; mint in `reserve_inodes`; spend under `wlock` in `mutate` (`db.rs:895`).
- Consumer: `crates/cowfs-core/src/ns.rs::make` (line 177) mints/uses a ticket instead of `alloc_virt`; `commit_batch` (`inner.rs` ~940-1000) calls `*_at`. `Op::Create.child` stays `Ino`.
- Dependency: metadata file ownership transfers from the active builder.

## Acceptance tests (reused, one probe per boundary, no new matrix)

- Queue replay: existing `restore` path test plus a create-and-fail-flush test; assert the ticket is spent once and the retry uses the same number.
- Wrong store: a test that mints a ticket on one `Meta` and calls `create_at` on another; assert refusal.
- Reopen: existing `names_ino.rs::virtual_inode_numbers_are_never_reused_across_a_restart`; assert a stale ticket is refused.
- Cross-snapshot: one test using a ticket on a second snapshot; assert the chosen policy.
- Reuse: `create_at` at an inode that already exists; assert refusal.

## Status

- Corrects the prior token claim; ahead of it on every point above.
- Runtime: UNEXECUTED. Source reads only.
- Real remaining question: per-store vs per-snapshot floor semantics for the sanctioned use. Not an approval gate.
- Prior plan `meta42-core-inode-consumer-plan.md` left immutable; its `ReservedIno` section is superseded by this document.
