# meta42-large-inode-reservation: arbitrary counts in a fixed number of durable commits

Lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, READY5, held throughout.
Branch: `fix/meta-inode-reservation-42`, verified clean at `1214142ffc17b1fedc3b31d3a6f2a343aa7e8d36` with the local branch equal to the remote, before any edit.
Base: `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0` at the time this branch was cut.
Parent head: `1214142ffc17b1fedc3b31d3a6f2a343aa7e8d36`.
Older receipts, all untouched: `0f27f8bf…`, `9eb4fad0…`, `4ef55a3c…`, `c3e37608…`, `fc186d4e…`, `fe6515c3…`.

## What this is

Issue #42 request 4 asks for "`Meta::reserve_inodes(n)` **or** `Tx::create_with_ino`", so that a caller can get an inode number before the transaction that creates the inode, which is what currently forces Core to invent virtual numbers and keep an alias table.

The additive API exists at `1214142`. What it did **not** have was a way to take a large `n` without holding the store's writer lock for minutes.
The loop it shipped committed the durable floor one block at a time, because `record_recovery` could only prove a lost commit had moved the floor by one block.
Measured on the reviewer's machine at `ino_block` 8: `n = 100001` needed **12501 durable commits and 140.1 s** inside one `self.wlock()`, the same lock every `Snapshot::batch` and every `mutate` takes.

This change makes the commit count **independent of `n`** while keeping `n` arbitrary.
No count cap was added.
A previous proposal in this lane to bound `n` at one block was **rejected**, correctly, because it would have narrowed the objective rather than met it.

## The blocker, and why a single commit is not enough

`record_recovery` runs only when `Meta::open_recover` has had redb repair a file, and it moves the inode floor to `ino_reserved + block`.
Its justification, in its own doc comment, is that redb's repair "falls back exactly one commit", so the lost commit could have advanced `ino_reserved` by at most one block.

So the naive fix, one durable commit that jumps `ino_reserved` by the whole requested `n`, is unsafe.
If that commit is the one redb discards, the floor reverts, recovery skips only one block, and **every number in the skipped-over range gets handed out a second time**.
That is precisely the corruption `docs/design.md` forbids: "Bounded loss of recent writes on a crash is acceptable. A torn or corrupt tree is not."
An inode number given to a caller and later reissued is exactly that: the tree's identity becomes ambiguous.

The constraint that follows is real and worth stating plainly: **the bound describing a floor move must not live in the commit that performs the move.**

## The mechanism

A second key in the existing `META` table, written by its own commit **ahead of** the floor move.

| step | commit | writes |
|---|---|---|
| 1 | bound | `ino_reserved_intent = target` |
| 2 | floor | `ino_reserved = target`, removes `ino_reserved_intent` |

then, in memory only, `next` moves to `target` and the range is returned.

Two durable commits whatever `n` is, and **zero** when the cached floor already covers the range, since the loop is skipped entirely.

Why that ordering survives a lost commit:

- **redb discards commit 2.** `ino_reserved` reverts to its old value, but `ino_reserved_intent` was committed in step 1 and is still there. `record_recovery` reads it and sets the floor to `max(old + block, bound)`, which is the bound, because the bound is larger. No number in the range can come back.
- **redb discards commit 1.** Nothing was written and nothing was handed out. The one-block rule is more than sufficient, because there is nothing to skip.
- **Nothing is discarded.** The floor is at `target` and the bound is spent.

The recovery change is three lines of arithmetic and one removal, plus a reordering so a bound is enough on its own:

```rust
let bound = meta.get(INO_INTENT)?.map_or(0, |g| g.value());
let reserved = meta_get(&meta, "ino_reserved")?;
let floor = if bound > 0 {
    bound
} else {
    let Some(block) = self.ino_block else {
        return Err(Error::Format(
            "cannot recover: this file predates the persisted inode reservation block, \
             so the size of the lost reservation cannot be proven".into(),
        ));
    };
    reserved.saturating_add(block)
};
let ino_floor = floor.max(reserved).min(INO_LIMIT);
```

**A file written before this change keeps the old behaviour exactly.** It has no bound, so it takes the `else` branch and skips one block, which is the rule it was written under.
A file that does carry a bound no longer needs the persisted block to recover, because the bound names the move exactly rather than bounding it.

## Why two commits and not one

One commit would have to write the bound and move the floor together, and then losing it loses both, leaving only the one-block bound.
That is the unsafe case above.
Two commits is the smallest arrangement that puts the bound in a commit that outlives the move.
The cost is one extra durable commit per reservation that needs the floor, which is a fixed cost and not a function of `n`.

## Files changed

Two, both in `crates/cowfs-meta`, and no production behaviour outside this crate:

| File | Change |
|---|---|
| `crates/cowfs-meta/src/db.rs` | `INO_INTENT` key; `reserve_intent`; `reserve_durable` also spends the bound; `reserve_inodes` rewritten; `record_recovery` reads the bound; private `cfg(test)` fault seam and commit counter; 8 unit tests |
| `crates/cowfs-meta/src/check.rs` | validates the bound when present |

No new dependency, no `Cargo.toml` or `Cargo.lock` change, no schema version bump, no new table, no new on-disk file, no public API change.
`InoRange` and `Meta::reserve_inodes` keep exactly the signatures they had at `1214142`, so **this is backwards compatible and additive**.

`Tx::alloc` is untouched and still advances the floor one block at a time.
That path has no bound written for it, which is correct: it moves the floor by at most one block, so its lost commit is already covered by the one-block rule.
The bound key is simply absent when only `alloc` has run.

## The tests, and their status

**None of these has been run.**
Local cargo is blocked: `bench/out` in this lane measures **20.865 GiB** against an 8 GiB cap, and the protected `bench/out/ready-40` holds another lane's mutation-golden source snapshots and receipts, so the cap cannot be met without deleting files that are not mine.
No cap waiver was taken, no cleanup, no prune, no offload, no new target directory.

**What was executed:** standalone `rustfmt 1.10.0-stable (b940084d7e 2026-09-28)`, `--edition 2021`, `--check` on the two owned files.
Both **exit 0 with 0 bytes of diff**.
That is a formatter result and nothing more: it says the code parses and is formatted, not that it compiles or behaves.

Eight unit tests inside `db.rs`, in a `#[cfg(test)] mod tests`, deliberately **not** an integration test.
That placement is the point: the tests need private access to `Inner::db`, `Inner::reserve_intent` and `Inner::record_recovery`, and the fault seam has to be invisible to production and to `tests/inode_reservation.rs`, which links the crate as a dependency.
Two mistakes are specifically avoided, both recorded in this lane's earlier review: a `pub(crate)` setter is **not** reachable from an integration test, and a `cfg(test)` item is **not** visible to one either, so neither can be the whole answer; a unit test inside the module can reach both.

The fault seam is a `thread_local!` with three settings: fail before the floor commit, fail after it has persisted, fail before the bound commit. Thread-local because the reservation path runs under a process-wide write lock, so a process-wide fault would bleed into whichever other test happened to hold it.
A commit counter on the same thread is what makes the O(1) property assertable rather than assumed.

| test | what it pins |
|---|---|
| `a_large_reservation_costs_two_durable_commits_however_big` | `n = 100001` costs **exactly 2** durable commits, the floor lands on the range end, the bound is spent, and `check()` passes. This is the regression this change exists for |
| `a_range_the_cached_floor_covers_commits_nothing` | a covered range costs **0** commits |
| `a_bound_left_behind_is_exactly_what_recovery_skips_to` | the lost-floor-move case: bound durable, move not, recovery skips **to the bound** and not one block, the bound is then spent, and a reopen cannot resume below it |
| `a_file_without_a_bound_still_recovers_by_one_block` | a legacy file keeps the one-block skip it always used |
| `a_failure_before_persisting_exposes_no_number_and_consumes_nothing` | fail before the commit: `Err`, the durable floor is unmoved, and the same numbers are still available afterwards |
| `a_failure_after_the_floor_persisted_leaves_the_floor_ahead_and_never_reissues` | the uncertain outcome: the floor really did persist and the caller still got `Err`, and after a reopen the covered numbers are **not** reissued |
| `a_failure_before_the_bound_commits_exposes_nothing` | fail before the bound: floor unmoved, no bound left behind |
| `ordinary_creation_still_starts_above_a_large_reserved_range` | ordinary `Tx::create` still cannot land inside a reserved range of 50000 |

`a_failure_after_the_floor_persisted_leaves_the_floor_ahead_and_never_reissues` is the one worth reading closely, because it is the case that a rollback test cannot fully replace.
It pins the specific asymmetry this design depends on: **an `Err` does not mean the reservation rolled back.**
The commit may have landed. The safety argument is not "it rolled back", it is "the floor only ever moves forward, so an unspent floor is wasted work, never a reissued number".
The test asserts the floor is genuinely ahead and that a reopen cannot hand those numbers back.

## What is still owed, and is not claimed

1. **Compilation.** These eight tests have never been compiled. The API surface they use was cross-checked by reading the source, not by a compiler.
2. **Runtime.** No test has passed. There is no green here.
3. **`clippy --workspace --all-targets -- -D warnings`** has not run on this tree.
4. **The rollback test itself is not covered.** `record_recovery` is exercised directly, which proves its arithmetic and the `check()` invariant afterwards, but it does **not** prove that redb actually discards the newest commit on a real damaged file. `open_recover` performs a genuine repair by clearing the two-phase flag and flipping `GOD_RECOVERY`, and no test in this crate drives that path for the reservation. That remains owed and is not claimed.
5. **No latency measurement.** The O(1) commit count is asserted by a counter, not by a wall clock. The 140.1 s figure being replaced was the author's measurement, not mine, and I have not measured its replacement. I claim a commit count, not a latency improvement.
6. **`n` at the limit.** A reservation of exactly the remaining numbers below `INO_LIMIT` is checked by arithmetic and not exercised, because it would need the allocator near its ceiling.

## The Core consumer transition, which is required and is NOT done here

**Request 4 is not delivered by this change, or by `1214142`, or by both together.**
Both are metadata only.
`grep reserve_inodes` over `crates/cowfs-core/src` and `crates/cowfs-ctl/src` returns nothing.
Core still keeps `<root>/virt.ino` at `crates/cowfs-core/src/ino.rs:13-14`, still maps both ways through its alias table, and still hands out virtual inode numbers itself.

What Core needs, separately, once the metadata side has a green runtime:

- call the reservation where it currently allocates a virtual number, and take the number from the returned `InoRange`;
- prove the range is durable before handing any of it to a caller, which the API already guarantees;
- retire the virtual-number path and the alias table, including the `virt.ino` durable high-water mark;
- keep the packed inode stable, because `crate::ino::pack` combines the snapshot id with the low bits and a reused low range would alias.

**No `Meta::create_with_ino` is designed here.**
Whether that is the better answer to request 4 is open, and choosing it needs a spike on who durably owns a consumed number, not a decision made inside a metadata receipt.
I claim neither that it is required nor that it is impossible.

## Honest residual requirements

- Compile and run the eight tests before this is worth anything. Two commits and a counter are a design argument until a run confirms them.
- Drive the real `open_recover` repair path, not just `record_recovery`, for the lost-commit case.
- Measure the lock hold, since the whole point was the 140.1 s.
- Decide the Core transition on its own review.
- `docs/v1-meta.md` does not yet document the bound key or the two-commit arrangement. That is a doc gap this change should close, and it is not closed here because this receipt is scoped to what was executed.

## What I did not do

- No local cargo, build, test, clippy, archive, target directory, probe or fault run. `bench/out` is untouched at 20.865 GiB and `ready-40` is intact.
- No cleanup, prune, move, offload or cap waiver.
- No change to `crates/cowfs-core`, `cowfs-store`, `cowfs-fuse`, `cowfs-ctl`, `cowfs-nfs`, the daemon, GC or any provider.
- No public fault API, no `pub(crate)` test setter exposed to integration tests, no new dependency, no schema version change, no new table.
- No commit, push or merge yet at the time of writing; no new issue and no new task.
- No checkout, reset, stash, rebase or branch change: this lane is `fix/meta-inode-reservation-42` and stays there.
- All 32 leases, the shared daemon, the Linux hosts, every store, mount and job untouched and unsignalled.
- Browser unverified, so nothing is linked or claimed visually. `no-mistakes` is uninitialized in this lane. Misakanet is local-only here and was not consulted.

Issue #42 stays open. This is request 4's metadata half only.