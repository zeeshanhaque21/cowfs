# PR 140 correction receipt: lint gates, doc placement, and the reservation cost contract

Lane: READY1 follow-on correction for PR #140, branch `fix/meta-inode-reservation-42`.
Reviewed head: `f5f7bbc8af72e1ffd257e87c3193a7fe0ebe8b9e`.
This receipt supersedes nothing. The author's earlier receipt `4ef55a3c` and the independent review
`0f27f8bf` are both immutable and are neither rewritten nor reinterpreted here.
Where this receipt disagrees with either, this receipt is the newer statement and says so explicitly.

## What this lane may run, and what it actually ran

This lane had no build budget.
The author lane measured `bench/out` at 20.863 GiB against an 8 GiB cap, and the protected
`bench/out/ready-40` cannot be reclaimed, so local Cargo was unavailable and no cap waiver was taken.
Therefore, on this lane: **no `cargo build`, `cargo test`, `cargo clippy`, no `git archive`, no new
target directory, no probe binary, and no deletion, move or offload of anything.**

The only executable check available is a standalone `rustfmt`, which is already installed.

```
$ rustfmt --edition 2021 --check crates/cowfs-meta/src/db.rs crates/cowfs-meta/tests/inode_reservation.rs
rustfmt exit=0
```

`rustfmt 1.10.0-stable (b940084d7e 2026-09-28)`, workspace edition 2021, no `rustfmt.toml`.
Both owned files are clean.

**That is a formatter result and nothing more.**
It says nothing about compilation, tests or lints, and no such claim is made anywhere below.

## Digest verification, done before anything was trusted

The brief supplied the canonical review digest.
It was computed, not assumed.

```
$ shasum -a 256 docs/reviews/pr140-meta42-inode-reservation-final.md
0f27f8bfee40745a18a2c37e85aa43fead10d1df6b71897b3d57f1862f34b62e  docs/reviews/pr140-meta42-inode-reservation-final.md
```

That matches the supplied digest exactly.
The author's earlier receipt was verified the same way, in both the primary checkout and the branch tree:

```
4ef55a3c64a8c0aa104bee5fae52a49bbe10ab8548f273cfa700c508563f8576  docs/verification/evidence/meta42-inode-reservation.md
```

identical in both locations, so the PR body's claim about that digest is accurate.

## Fix 1 and fix 2: the two lint failures that block CI

The independent review read one CI snapshot, run `37392681285`, and localised the failure without
reading CI's log body:

| job | conclusion |
| --- | --- |
| `linux-fuse` | completed, success |
| `check (macos-latest)` | completed, failure |
| `check (ubuntu-latest)` | completed, failure |

Both platform checks agreed step for step: step 5 `cargo fmt --all --check` passed, step 6
`cargo clippy --workspace --all-targets -- -D warnings` failed, step 7 `cargo test --workspace` was
skipped, step 8 bench harness tests were skipped.

Both errors were in this lane's own file, `crates/cowfs-meta/tests/inode_reservation.rs`, at the
reviewed head.

### Fix 1, `crates/cowfs-meta/tests/inode_reservation.rs:49`

Clippy reported `unnecessary >= y + 1 or x - 1 >=`.
`ROOT_INO` is `Ino(1)` at `crates/cowfs-meta/src/types.rs:12`, so `x >= ROOT_INO.0 + 1` and `x > ROOT_INO.0`
are the same predicate on the same `u64`, with no wrap possible on either side.

```diff
-        r.start().0 >= ROOT_INO.0 + 1,
+        r.start().0 > ROOT_INO.0,
```

Same assertion, same message, same intent. Nothing else in the test changed.

### Fix 2, `crates/cowfs-meta/tests/inode_reservation.rs:133` at the reviewed head

Clippy reported `calls to std::mem::drop with a value that implements Copy does nothing`.
`InoRange` derives `Clone, Copy, PartialEq, Eq, Debug` at `crates/cowfs-meta/src/types.rs:20`, so the
`drop` was a no-op statement of intent that the compiler had already discharged.

```diff
-    drop(reserved);
     let m = Meta::open(&path, opts()).unwrap();
```

The line was removed rather than rewritten.
`reserved` is still read afterwards, at the disjointness assertion and the `after.start() >= reserved.end()`
assertion, so removing the no-op removes nothing but the no-op.
The reopen in that test is what proves the floor, and it is unaffected.

All eleven shipped tests keep their existing assertions.
No test was added, removed, renamed or weakened by either fix.

**Honest status of these two fixes.**
They are the mechanical application of clippy's own two diagnostics, verified against the reviewer's
quoted clippy output.
Clippy itself has **not** been run on this lane, so the claim is "the reported errors are corrected",
not "clippy is green". Only CI, or a lane with a budget, can say the latter.

## Fix 3: doc placement on the public surface

The review found that `reserve_inodes` had been inserted between `Meta::sync`'s doc comment and
`Meta::sync` itself, so one contiguous doc block attached to the wrong item.
At the reviewed head, `Meta::reserve_inodes` at `db.rs:1636` began with `Meta::sync`'s paragraph
("Runs `before_sync`, then makes every applied change durable...") followed immediately by its own
first line, and `Meta::sync` at `db.rs:1640` had no doc comment at all.

Two defects, both on the public surface:

- the inherited paragraph is false for `reserve_inodes`, which runs no `before_sync` hook and makes no
  applied changes durable, and
- `Meta::sync` lost its documentation.

The fix is one line, a doc separator, which ends `sync`'s doc block and starts `reserve_inodes`'s.

```diff
     /// metadata". Returns the hook's or the commit's error.
+    ///
     /// Reserves `n` inode numbers before any inode exists, and hands them back.
```

`crates/cowfs-meta/src/db.rs:1617-1620`.
The public documentation of `Meta::sync` is restored, `Meta::reserve_inodes` keeps only its own accurate
paragraphs, and the nine-line self-contradiction is gone.
No behaviour changed, and no signature changed.

## Corrections to claims that are stale or overstated

These are recorded here because the two immutable documents cannot be rewritten, and because a reader
must be able to tell which statement is current.

### Stale line citations in the immutable review `0f27f8bf`

The review's reasoning is sound and its measurements stand, but two of its line citations point at
code that is not what it names at this head.

| the review cites | what is actually there at `f5f7bbc8` | what it names |
| --- | --- | --- |
| `db.rs:689-691, in record_recovery` | `self.note_flush_ok()`, `self.disarm_timer()`, `self.durable_seq.store(...)` | the `ino_reserved + block` floor is at `db.rs:777-779` |
| `clamped at db.rs:1257` | `open_with_backend`, calling `Self::init` | the clamp is at `db.rs:1362`, `opts.ino_block.clamp(1, INO_LIMIT)` |

The review's `db.rs:1636` for `reserve_inodes`, `db.rs:1640` for `sync` and `tx.rs:66-79` for `alloc`
are all correct at this head, and its quoted test-file lines 49 and 133 are correct.

### Stale line citations in the PR body

The PR body was written against an earlier local revision of `db.rs`.
Each of these names the right construct and the wrong line.

| the PR body cites | actual at the new head |
| --- | --- |
| `record_recovery` (`db.rs:729-764`) | `db.rs:765-800` |
| `Meta::open` initialises `next: reserved` (`db.rs:1405-1407`) | `db.rs:1441-1443` |
| a fresh file stores `ino_reserved` as `2` (`db.rs:1349`) | `db.rs:1385` |
| `Tx::create` (`tx.rs:219`) | `tx.rs:233` |
| `reserve_durable` (`db.rs:704-713`) | correct, `db.rs:704-713` including the doc line |

### Overstated by omission in both documents

Neither the PR body nor the review states the cost of a reservation as a function of the requested
count, and the PR body's "this therefore commits in block-sized steps, the shape `alloc` already uses"
reads as if the cost were amortised.
It is not.
`Tx::alloc` pays one commit per block **spread across many ordinary creates**.
`reserve_inodes` pays the whole series **inside one call, under one lock hold**.
That distinction is the subject of the next section, and it is the one substantive gap this receipt
exists to close.

## The cost and lock finding, measured and estimated, kept apart

The review measured the commit count and per-commit cost on its own machine, using a deliberately
small block so the commit count stayed bounded.

| block | n | durable commits | elapsed |
| ---: | ---: | ---: | ---: |
| 8 | 1 | 1 | 11.3 ms |
| 8 | 9 | 2 | 39.3 ms |
| 8 | 81 | 11 | 108 ms |
| 8 | 1001 | 126 | 2.03 s |
| 8 | 100001 | 12501 | **140.1 s** |

Measured. The per-commit cost is therefore about 11.2 ms on that machine, and elapsed time is linear in
the commit count.
The smallest block used in the review was 8, and the source default is 16384, clamped at `db.rs:1362`.

Extrapolating that per-commit constant to the source default and to `INO_LIMIT = 1 << 40`:

| n | durable commits | implied time in one call | status |
| ---: | ---: | ---: | --- |
| `2^20` | 64 | about 0.7 s | **estimate, not measured** |
| `2^30` | 65,536 | about 734 s, roughly 12 minutes | **estimate, not measured** |
| `INO_LIMIT` | 67,108,864 | about 752,000 s, roughly 8.7 days | **estimate, not measured** |

The commit counts are arithmetic and hardware-independent.
Only the per-commit constant is machine-specific, and only the last two rows depend on the default
block rather than the measured block 8.
These are estimates from a measured constant, not measurements, and are labelled as such everywhere
they appear.

Two consequences follow, both source readings over measured commit counts:

- **The `O(1)` shape of `InoRange` does not make reservation `O(1)`.** The accessors are `O(1)`, but the
  operation that produces the range is `ceil(n / block)` durable commits, and the public signature
  bounds `n` only by `INO_LIMIT`.
- **It is a lock-hold and starvation problem, not only a slow call.** The whole loop runs inside
  `self.wlock()`, the same write lock every `mutate` and every `Snapshot::batch` takes, so one
  `reserve_inodes(1 << 30)` blocks every other writer in the process for the duration of the estimate
  above.

The eleven tests are safe on this axis, and that was checked before they were run: the largest literal
`n` in the file is 20 at `tests/inode_reservation.rs:158`, and `INO_LIMIT` and `u64::MAX` appear only on
refusal paths, which return before the loop and commit nothing.

## Design proposal: the cheapest mechanism that is safe under the existing recovery contract

Nothing in this section is implemented.
The current API and the current loop are preserved unchanged until a design is selected, and the
public surface is untouched by this receipt.

### The constraint that makes this hard, stated from the source

The durability contract is not "commit once".
It is:

> **I1.** Every durable commit advances the persisted `ino_reserved` by at most `block`.

and recovery discharges exactly that:

```rust
// crates/cowfs-meta/src/db.rs:777-779, in record_recovery
let ino_floor = meta_get(&meta, "ino_reserved")?
    .saturating_add(block)
    .min(INO_LIMIT);
```

A lost commit therefore costs at most one block of inode numbers, and recovery moves the floor past
exactly one block.
`reserve_inodes` respects I1 by clamping each step (`db.rs:740-742`), and so does `Tx::alloc`
(`tx.rs:72-75`).

The floor must also be durable **before** any number is handed out, or a crash plus recovery would put
the reopened floor below numbers already given to a caller.
That forces a commit before the handout, and I1 forces that commit to be block-granular.
**Block-granular commits are a lower bound under the current format, not an implementation choice.**

One consequence is worth stating because it is easy to get wrong: `align_up(ino_reserved, block)`
does not help.
The persisted value carries no record of how far the last commit jumped, so recovery cannot derive a
larger safe skip from it, and a larger skip cannot be justified from the stored data.

### Option A, recommended: refuse more than one block per call, and let the caller loop

Add one guard to `Inner::reserve_inodes`, after the two existing refusals, using the block already
clamped and already in the allocator:

```rust
if n > s.ino.block.max(1) {
    return Err(Error::LimitExceeded("a reservation covers at most one inode block per call"));
}
```

and state the restriction in the public doc of `Meta::reserve_inodes`.

**This is an explicit contract restriction and is not a silent cap.** Stated plainly: after this change
the public `reserve_inodes(n: u64)` **no longer admits an arbitrary count in a single call.**
For `n` above one block it returns `Error::LimitExceeded` and writes nothing.
A caller that needs more must call repeatedly and loop.
That is a real narrowing of what request 4 asked for, it is visible to every caller, and it is the
honest way to state it.
The alternative of quietly reserving less and returning a short range is **rejected**, because a caller
that checks `len() == n` would then be wrong, and a short range that looks like a full one is a reuse
hazard rather than a bound.

The second effect is the one that actually matters.
Once `n <= block`, the loop provably runs exactly once:

- `s.ino.reserved >= s.ino.next` holds at all times, so
  `s.ino.next + n <= s.ino.next + block <= s.ino.reserved + s.ino.block.max(1)`;
- the existing guard `n > INO_LIMIT - s.ino.next` already ensures `s.ino.next + n <= INO_LIMIT`;
- so `step = min(next + n, reserved + block, INO_LIMIT) = next + n`, one commit, and the re-test
  `next + n > next + n` is false.

The nine-line loop therefore collapses to one commit and can be deleted outright:

```rust
let new = s.ino.next + n;
self.reserve_durable(new)?;
s.ino.reserved = new;
let start = s.ino.next;
s.ino.next = new;
Ok(InoRange::new(Ino(start), Ino(new)))
```

Net effect: fewer lines than today, one durable commit per call, and **latency independent of `n`**.
The lock hold becomes bounded by a single durable commit, about 11.2 ms measured on the reviewer's
machine, rather than by `ceil(n / block)` of them.
A caller looping gets the lock back between calls, so the starvation property is fixed too, not just
the latency.

Invariants under Option A:

| # | invariant | status |
| --- | --- | --- |
| I1 | each durable commit advances persisted `ino_reserved` by at most `block` | holds, one commit advancing by `n <= block` |
| I2 | recovery's single-block skip covers any lost commit's handout | holds, unchanged, `record_recovery` untouched |
| I3 | persisted floor is at or above every number ever handed out | holds, see the proof below |
| I4 | a rollback wastes at most `block` numbers | holds, in fact at most `n` |
| I5 | lock hold is independent of `n` | new, this is the fix |

Minimal file changes: `crates/cowfs-meta/src/db.rs` for the guard, the doc paragraph and the loop
deletion, plus `crates/cowfs-meta/tests/inode_reservation.rs` for the fixture.
No change to `record_recovery`, `tx.rs`, `Options`, `types.rs`, the on-disk format, `Cargo.toml`, or
anything under `crates/cowfs-core`.
No new dependency, no new public API, no schema, no allocator change, no consumer.

**Test fixture consequence, stated before anyone implements this.**
`opts()` in the test file uses `ino_block: 8` at line 27, and the largest literal `n` is 20 at line 158,
the loop at lines 349-350 at the reviewed head, 348-349 after this receipt's fix 2, peaks at 9.
Under Option A those calls would be refused, so `opts().ino_block` must exceed 20; propose 32.
The comment at line 126, "Cross a block boundary so more than one durable reservation commit happens",
becomes false under a one-commit contract and must be replaced with a note that durability no longer
depends on crossing a boundary.
Every existing assertion stays exactly as it is.
Because this lane cannot compile or run tests, the effect of that fixture change on the other nine
tests is **unverified**, and a lane with a budget must confirm all eleven still pass before this is
believed.

### Why no number is ever handed out twice, under Option A and today

This is a source proof, and it is stated as one because no crash test was executed on this lane.

- `reserve_inodes` commits `reserved >= next + n` before advancing `next`, so the persisted floor is
  already at or above every number the call hands back, at the moment it hands them back.
- `mutate` persists `s.ino.reserved` on every commit (`db.rs:620-625`), and on close persists
  `min(next, reserved)`, which equals `next` because `next <= reserved` always.
- `Meta::open` reads the persisted `ino_reserved` (`db.rs:1426`) and initialises
  `next: reserved, reserved` (`db.rs:1441-1443`), so `next` never begins below the persisted floor.
- `record_recovery` raises both floors with `.max(...)` (`db.rs:795-796`) and never lowers either.

Reuse would require some path to persist a floor below a number already handed out, and no such path
exists.
The reopen half of that is executed: `numbers_reserved_and_never_used_are_not_reissued_after_a_reopen`
covers it, and the review ran it, 11 passed.
The rollback half is executed for ordinary `alloc` by the existing rollback fixture, and **not**
executed for `reserve_inodes` on this lane.

### Behaviour when the durable commit fails

**Under Option A**, the multi-iteration case disappears, so the only case left is a failure of the
single commit.
The `?` returns before `s.ino.next` moves, so no number is exposed; nothing is persisted; `next` and
`reserved` are both unchanged in memory; and the call is fully retryable with zero waste.
`Ok(InoRange)` is unreachable on that path.

**Under today's code**, which this receipt does not change, the review's reading is that a failure on
iteration `k > 0` leaves the persisted floor at `step_{k-1}`, which is above `next`, so at most one
block of numbers is wasted and never reissued, and no number is exposed.
That is the safe direction and it matches the crate's stated rule that a number can be wasted but never
handed out twice.
It is a reading. **It was not executed and is not claimed as proven.**

### Alternatives considered, and why each is not chosen

| option | why not |
| --- | --- |
| silently cap `n` to one block and return the short range | rejected outright: `len()` would not match `n`, and a short range that looks complete is a reuse hazard, not a bound |
| raise the default `ino_block` from 16384 | the only zero-diff throughput lever, but it shifts the constant instead of bounding it, leaves `INO_LIMIT / block` commits as the worst case, and multiplies rollback waste. A coordinator decision, not taken here |
| persist the intended per-commit handout so recovery can skip it | needs a new `META` key, which is an on-disk format change, and is unnecessary once `n <= block` |
| a durable reservation-intent ledger, then the floor | a new key, a new format, new recovery code, and a two-record protocol where one commit already suffices |
| hand out numbers below the durable floor and commit lazily | **unsound**: `next` would pass the persisted floor with no commit, so a crash plus recovery's single-block skip reopens the floor below numbers already given out, which is reuse. Ruled out by I3 |
| keep the loop and let `n` stay arbitrary | that is the finding being corrected, not a fix |

### Proposed test-only fault seam, before any implementation

The review named this gap and did not paper over it: `reserve_durable` is a private method wrapping a
redb write transaction with no injectable seam, so the persist-failure half of the contract is argued
from source and never executed.

The seam, proposed and **not written**, reuses the pattern this repository already uses on `Core`:

- `Core::set_gate_fault` is `#[cfg(test)] #[doc(hidden)] pub fn set_gate_fault(&self, kind: u8)` at
  `cowfs-core/src/lib.rs:541-545`, driving a `#[cfg(test)] fault: AtomicU8` field at
  `cowfs-core/src/gate.rs:43-45`;
- its consumer is `#[cfg(test)] mod gc_barrier_window;` declared at `cowfs-core/src/lib.rs:13-14`.

That second point is the constraint that decides where the test must live.
`#[cfg(test)]` items are **not** visible to `tests/inode_reservation.rs`, which links the crate as an
external dependency, so a `cfg(test)` fault setter could not be reached from the shipped integration
test file.
The seam therefore needs a `#[cfg(test)]` module inside `cowfs-meta/src/`, declared from
`crates/cowfs-meta/src/lib.rs`, exactly as `cowfs-core` does.
`cowfs-meta/src/lib.rs` declares no `cfg(test)` module today, so this adds one line to `lib.rs` and one
test module.

Cases to write once a design is selected:

1. fail before the first commit: assert the call returns an error, no number is exposed, the allocator
   did not move, nothing was persisted, and a retry of the same `n` succeeds and returns the range.
   This is the only case Option A needs.
2. fail at the third commit under today's code, with `block = 8` and `n = 40`: assert no number is
   exposed, the persisted floor sits above `next`, and no number is reissued after a reopen.
   This case becomes unreachable under Option A and should be written only if the loop is kept.

Until that runs, the commit-failure behaviour above stays a source-argued gap.
It is labelled as such and is not upgraded by this receipt.

## State

Draft, held, not merged.
**PR #140 is not ready**, and this receipt does not call it ready.
The lint gates and the public documentation are corrected, but the two substantive items the review
raised, the unbounded lock-hold and the unexecuted commit-failure proof, are open by design and await
a selected design.

Issue #42 stays open and unchanged.
This work is part of the existing request 4 and is not a new feature, a new issue, or a new task.
No `Core` migration and no public consumption API is proposed.
The PR body is corrected by appending a pointer to this receipt, not by rewriting it, and the immutable
review and the author's receipt are both left byte-identical.

Nothing in this lane touched `crates/cowfs-core`, any other lane's files, any leased worktree, any
store, socket, job or mount, and no signal, restart, install or `sudo` was issued.
The independent reviewer working in this same worktree on PR #141 was left alone; the metadata paths
this lane owns are `crates/cowfs-meta/src/db.rs` and `crates/cowfs-meta/tests/inode_reservation.rs`.

Refs #42 request 4.