# PR 140 final independent review: `Meta::reserve_inodes`

Reviewed head: `f5f7bbc8af72e1ffd257e87c3193a7fe0ebe8b9e`, branch `fix/meta-inode-reservation-42`.
Base: `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`, which is current `main`.
Author: READY5. Reviewer: READY1, metadata read-only, disjoint from the author's files.

**Verdict: BLOCK.** The change is correct in substance and its durability invariant is real, but CI is
red on a clippy gate that fails on the author's own test file, and there are two further defects in the
shipped API. Not merged by this review, not fixed by it.

This review edited no source, no test and no manifest, made no commit, pushed nothing, merged nothing,
and filed no issue.

## Digest, verified before anything was trusted

| document | sha256 | verified where |
| --- | --- | --- |
| `docs/verification/evidence/meta42-inode-reservation.md` | `4ef55a3c64a8c0aa104bee5fae52a49bbe10ab8548f273cfa700c508563f8576` | canonical PRIMARY copy, and the blob committed at `f5f7bbc8` |

The brief flagged that a digest in this lane had been wrong before, so it was recomputed rather than
trusted. It matches in both places, so the committed document is the canonical document.

## What the author could not run, and what I actually ran

His record discloses that he had **no test, clippy, or local budget** and that all eleven tests were
**unexecuted**.
Nothing is carried from his side.

What I ran, in a fresh `git archive` of the reviewed head, **665 tracked files, 0 mismatched, 0 extra**,
one isolated `CARGO_TARGET_DIR` and one private `TMPDIR`, one 600-second foreground `mac-heavy.lock`
batch per step:

| check | result | exit |
| --- | --- | ---: |
| one representative public-API sample (my probe) | 1 passed | 0 |
| `cargo test --locked -p cowfs-meta --test inode_reservation`, the 11 shipped tests | **11 passed, 0 failed**, 0 ignored | 0 |
| `cargo test --locked -p cowfs-meta --lib` | 16 passed, 0 failed | 0 |
| the rest of the `cowfs-meta` suite, 15 targets | all `ok`, 0 failed | 0 |
| `cargo fmt --all -- --check`, pristine head | clean | 0 |
| `cargo clippy --locked -p cowfs-meta --lib -- -D warnings` | clean, 0 diagnostics | 0 |
| `cargo clippy --locked -p cowfs-meta --all-targets -- -D warnings` | **2 errors** | **101** |

The eleven tests pass. The production library is clean under clippy and fmt.
The failure is in the shipped test file, and it is a gate failure, which is finding 1.

## Finding 1: CI is red on clippy, in the author's own test file, blocking

One snapshot of run `37392681285` on the exact head, not polled, not rerun, not dispatched:

```
linux-fuse             completed/success
check (macos-latest)   completed/failure
check (ubuntu-latest)  completed/failure
commit_status          pending
```

The two platform checks fail at the same step on both runners:

| step | name | result |
| --- | --- | --- |
| 5 | `cargo fmt --all --check` | completed/success |
| 6 | `cargo clippy --workspace --all-targets -- -D warnings` | **completed/failure** |
| 7 | `cargo test --workspace` | **skipped** |
| 8 | Bench harness unit tests | skipped |

I could not retrieve the CI log body: the jobs-logs endpoint returned **0 bytes** for both failing jobs,
so I am not claiming to have read CI's own output.
The step-level results above need no log body and are sufficient to localise the failure, and they match
my local reproduction exactly: fmt passes, clippy `--all-targets` fails, tests never run.

The two errors, from my local run on the pristine head, both in
`crates/cowfs-meta/tests/inode_reservation.rs`:

```
error: unnecessary `>= y + 1` or `x - 1 >=`
  --> crates/cowfs-meta/tests/inode_reservation.rs:49:9
49 |         r.start().0 >= ROOT_INO.0 + 1,

error: calls to `std::mem::drop` with a value that implements `Copy` does nothing
  --> crates/cowfs-meta/tests/inode_reservation.rs:133:5
133 |     drop(reserved);
```

Both are default-on clippy lints denied by the repository's own `-D warnings`.
The CI workflow runs `cargo clippy --workspace --all-targets -- -D warnings`, so this is the gate the
branch has to pass.

A consequence worth stating plainly: because step 7 was skipped, **the eleven tests are unexecuted in CI
as well**. My local run is the only execution that exists, on this machine only.

The fix is two lines in the author's own test file and is not mine to make.

## Finding 2: a public API carries a doc comment that is false and self-contradictory

`reserve_inodes` was inserted between `Meta::sync`'s doc comment and `Meta::sync` itself, so the
contiguous doc block now attaches to the wrong item.

At the reviewed head, `Meta::reserve_inodes` at `db.rs:1636` begins with:

```
/// Runs `before_sync`, then makes every applied change durable. The hook runs on every call,
/// also when nothing is pending, so a caller can use this as "sync the store, then the
/// metadata". Returns the hook's or the commit's error.
/// Reserves `n` inode numbers before any inode exists, and hands them back.
```

and `Meta::sync` at `db.rs:1640` has **no doc comment at all**.

Two problems, both in the public API surface:

- The inherited first paragraph is false for `reserve_inodes`. It runs **no** `before_sync` hook and
  makes **no applied changes** durable; it commits a bare counter. The author's own next paragraph says
  exactly that, so the doc contradicts itself nine lines apart.
- `Meta::sync` lost its documentation entirely.

This is documentation-only and no behaviour changes, but it is the public surface a lead reviews, and it
currently misdescribes the new call.

## Finding 3: a public API with unbounded work, measured

`reserve_inodes` advances the durable floor in a loop, **one durable commit per iteration**, with each
iteration bounded to at most one `ino_block` of progress.

Constants read from the source: `INO_LIMIT = 1 << 40`, and the default `ino_block = 16384`, clamped at
`db.rs:1257`.

So the number of durable commits in one call is `ceil(n / block)`, and `n` is any `u64` the public
signature accepts up to `INO_LIMIT - next`.

I measured the per-commit cost with a small block so the commit count stays bounded, on this machine
with `background: false`:

| block | n | commits | elapsed |
| ---: | ---: | ---: | ---: |
| 8 | 1 | 1 | 11.3 ms |
| 8 | 9 | 2 | 39.3 ms |
| 8 | 81 | 11 | 108 ms |
| 8 | 1001 | 126 | 2.03 s |
| 8 | 100001 | 12501 | **140.1 s** |

That is ~11.2 ms per durable commit, and the time is linear in the commit count.
Extrapolating the measured per-commit constant to the crate's **default** `ino_block = 16384`:

| n | commits | implied time in one call |
| ---: | ---: | ---: |
| 1 MiB `2^20` | 64 | ~0.7 s |
| `2^30` | 65,536 | **~734 s, about 12 minutes** |
| `INO_LIMIT = 2^40` | 67,108,864 | **~752,000 s, about 8.7 days** |

The commit count is arithmetic and hardware-independent; only the per-commit cost is machine-specific.

Two things follow.

**The `O(1)` range shape does not make reservation `O(1)`.** `InoRange`'s accessors are `O(1)` and that
is fine, but the operation that produces the range is `O(n / block)` durable commits, and nothing in the
public signature bounds `n`.

**It is a lock-hold and starvation problem, not only a slow call.** The whole loop runs inside
`self.wlock()`, the same write lock every `mutate` and every `Snapshot::batch` takes, so a single
`reserve_inodes(1 << 30)` blocks every other writer in the process for the duration.

The asymmetry with `Tx::alloc` is the point. `alloc` has the same loop shape and the same one-block
bound, but it advances one block only when the allocator has actually consumed a block's worth of
ordinary creates, so the cost amortises to roughly one commit per 16384 inodes spread across many calls.
`reserve_inodes` concentrates the entire series into one call. That is why the existing invariant is
sound and the new API is still a hazard.

The eleven tests are safe on this axis and I checked that before running them: the largest numeric `n`
any test uses is **20**, and `INO_LIMIT` and `u64::MAX` appear only on the refusal paths, which return
before the loop and commit nothing. No test commits billions.

A bound is the obvious remedy and it is cheap: reject `n` above some ceiling, or reserve
`min(n, one block)` per call and let the caller loop. Both are the author's and the coordinator's call;
I did not implement either, and no new framework is needed for either.

## The durability invariant, verified rather than trusted

The design rests on one claim: `record_recovery` bounds a lost commit by one block, so committing the
floor at most one block per commit keeps the floor recoverable.

That is true at the source, not only in the comment:

```rust
// db.rs:689-691, in record_recovery
let ino_floor = meta_get(&meta, "ino_reserved")?
    .saturating_add(block)
    .min(INO_LIMIT);
```

Recovery reads the persisted `ino_reserved` and adds **exactly one `block`**, clamped, then writes it
back and raises both in-memory floors to at least that value.

And the loop respects the bound, because each iteration clamps the step:

```rust
let step = (s.ino.next + n)
    .min(s.ino.reserved + s.ino.block.max(1))
    .min(INO_LIMIT);
self.reserve_durable(step)?;
s.ino.reserved = step;
```

`step` can never exceed `reserved + block`, so no single commit can move the floor further than the one
block recovery skips. `Tx::alloc` at `tx.rs:66-79` computes its step the same way.
This is a source reading and it holds; it is not a measurement, and nothing here is a crash or
power-loss test.

Overflow is also consistent with the source. Every term is bounded by `INO_LIMIT = 2^40`, so the sums stay
near `2^41`, far below `u64::MAX`, and the comment saying so is accurate.

## The commit-failure gap: named, not papered over

The author has **no test for a failing reservation commit**, and neither do I, because
`reserve_durable` is a plain private method that wraps a redb write transaction with **no injectable
seam**. Making it fail would require adding a fault point to production code, which this review does not
authorise and which the brief forbids.

So the following is a **source reading, explicitly unmeasured**:

If `reserve_durable(step)` fails on iteration *k* greater than zero, the `?` returns before
`s.ino.reserved = step` runs for that iteration.
By then `s.ino.reserved` already holds a value that was made durable on iteration *k-1*, while
`s.ino.next` is untouched.
`Ok(InoRange)` is only reachable after the loop finishes, so a failed call exposes **no** numbers, and the
already-durable floor sits above `next`, so those numbers are wasted rather than reissued.
That is the safe direction, and it is consistent with the crate's stated "a number can be wasted, never
handed out twice".
I did not execute it and I do not claim it as proven at runtime.

If a runtime probe is wanted later, the minimal honest instrument is a `cfg(test)` fault point inside
`reserve_durable`, following the `set_gate_fault` pattern this repository already uses on `Core`, plus a
case that fails before the first commit and one that fails after exactly one. That is a small
test-only addition to a file the author owns; I did not write it.

## What the API delivers, and the consumer it does not

`reserve_inodes` hands out numbers and creates nothing. Its own doc says so: "This hands out numbers; it
does not create inodes. Creating an inode at a reserved number is a separate concern and is not provided
here."

That is the right scope and I have no objection to it, but the consequence has to be on the record:
**there is no consumer.** `Tx::create` allocates its own inode number through `alloc` and cannot be given
a preselected one, and `Core` does not call `reserve_inodes`.
So the `Core`-level transition that #42 request 4 ultimately needs is **not delivered**, and #42 must stay
open. That is correct scoping, not a defect, and it is not a reason to invent a durable consumption
ledger, a registry, a new API or a `Core` policy change; I am not proposing any of those, and the
virtual policy the evidence document sketches remains a proposal for the lead and is **not** settled
approval of anything.

## Correctness evidence I produced

**My representative public-API sample**, one test, public surface only, no internal seam:

```
PROBE reserved=2..13 len=11 created=[13, 14, 15, 16, 17, 18, 19]
PROBE VERDICT disjoint_before_reopen=true floor_respected_after_reopen=true created_after_reopen=[31, 32, 33]
```

Read in order: the reservation covered `2..13` with an exclusive end, eleven numbers, none of which named
an inode; seven ordinary creates through `Snapshot::batch` drew `13..19`, strictly above the reserved
range and disjoint from it, the first landing exactly on `reserved.end()`; every reserved number failed
to resolve; after `sync`, drop and reopen a second reservation started at or above the old end and
returned none of the never-used numbers; and creates after the reopen drew `31..33`, clear of both ranges.

That is the disjointness and unused-floor property demonstrated end to end on real public calls, with a
real flush, drop and reopen, not asserted from the source.

The eleven shipped tests pass, and they cover the refusal paths, contiguity, disjointness, the reopen
floor, concurrent reservations, a reservation racing creation, the monotonic floor and snapshot
independence.

## Other checks, and what I looked for and did not find

`cargo fmt --all -- --check` on the pristine head: **clean, exit 0**, which agrees with CI step 5.
`cargo clippy -p cowfs-meta --lib -D warnings`: **clean, 0 diagnostics**, so the new `InoRange` and the
new `db.rs` code are lint-clean.

- **Floor regression**: `reserved` only ever moves up, and `next` only advances after the loop, so a
  reservation cannot lower the floor. `record_recovery` raises both with `.max(...)`.
- **Cached-block overlap**: `wlock()` is the same lock `mutate` takes, and the allocator state is
  in-memory under it, so a reservation and an ordinary create cannot interleave. The sample and the
  author's race tests both exercise that.
- **Range `end` semantics**: `end` is exclusive, `len` is `end - start`, `contains` is
  `start <= ino < end`, and `iter` yields `start..end` ascending. All match `std::ops::Range` semantics,
  so no caller is surprised.
- **Already-rounded private allocator**: `alloc` reuses `reserved` when `next < reserved`, so a
  reservation only pays for the numbers past the existing floor. Correct, and it is why the cost is
  `ceil((n - already_reserved) / block)` rather than always `ceil(n / block)`.
- **Dead `is_empty`**: `n == 0` is refused, so a handed-out range is never empty, and the method's own doc
  says so. Not a defect, and I am not proposing its removal.
- **`InoRange` complexity**: 49 lines of accessors over two `Ino` fields. `Clone, Copy, PartialEq, Eq,
  Debug` and seven accessors mirroring `std::ops::Range` is proportionate for a public type that must be
  usable as a value; I found no unjustified machinery and I propose no refactor.
- **Lock ordering**: the new code takes `wlock()` and then calls `reserve_durable`, which takes a redb
  write transaction. That is the same order `Tx::alloc`'s `reserve` closure already uses from inside
  `mutate`, so no new ordering is introduced. `Inflight::enter` is also first, matching `mutate`.
- **Root and tree**: `ROOT_INO` is never handed out; the tests assert reservations start above it, and the
  reservation moves no tree and creates no chunk reference, so `record_recovery`'s `ino_reserved` floor is
  the only state it touches. Snapshots are untouched, which one of the eleven tests asserts.
- No deadlock, no new shared resource, no mount and no daemon.

## Integration with `main`, read-only

`git merge-tree --write-tree origin/main f5f7bbc8`: merge base `cf67e8a6`, which is current `main`, so the
branch is exactly one commit on top of it.
Tree `c0d6bc511bdeda1441c2fe19cc65ead2aa60eeef`, **0 conflict lines, exit 0.**
`main` has not moved since the base, so the new hole-flag and clock work cannot interact with this delta.
Read-only computation, not a merge.

## Closing-reference audit and issue state

`closingIssuesReferences` is empty for PR 140.
The branch's single commit message `feat(meta): add Meta::reserve_inodes for issue 42 request 4` carries no
closing verb bound to an issue reference: "for issue 42" is not a closing form.
The PR body, 89 lines, contains **no** closing verb at all in any form, negated or otherwise.

The already-merged dependency commit `57232166` still contains the negated form "nothing here closes
#42", which is what auto-closed #42 when #136 merged. It is on `main`, already superseded by the reopen,
and history is not rewritten.
`#42` is `open` with `state_reason: reopened`, verified directly, and whole #42 stays open: request 4's
`Core` transition, the rename, the hole flag, the reservation-at-Core policy and the `Core` integration
are all untouched here.

PR 140 is `draft=true`, `state=open`, 1 commit, 5 changed files.

## Cap and lane discipline

`bench/out` measured before any archive or build: 4.26 GiB of the 8 GiB cap, 274.0 GiB free against the
20 GiB floor.
Peak after this lane: 4.97 GiB, leaving 3.03 GiB.
The cap was never exceeded, and no cleanup, deletion, moving or offloading was performed, and no cap waiver
was taken; that is READY5's approved scope, not this lane's.
One 600-second foreground `mac-heavy.lock` acquisition per step, each recorded with its UTC time and exit.
No signal, no restart, no install, no `sudo`, no mount walk and no lease action.
My artifacts are confined to `bench/out/meta42-inode-reservation-final-critic/`, which is gitignored.
The leased worktree is clean at `e7ee215` on `fix/deferred-operation-time-42` with no commits and nothing
pushed, and every prior #118, #120, #135, #136 and #139 report, log and archive is preserved, including
my own `75a2310b`, `72d37cc4` and the immutable `4ef55a3c`, `a8bc5337` and `b5e1f19c` receipts.
I read no other lane's files and modified none.

## Two failures of my own, recorded rather than dropped

**My probe file broke `cargo fmt`.** My first `fmt` run reported a diff and I initially had to decide
whether it was the author's. Re-running `fmt` on the pristine head with my probe removed gave exit 0, and
CI step 5 independently passed, so the fault was mine: a missing trailing newline in
`zz_indep_reserve_sample.rs`. Reporting that first failure as a branch defect would have been wrong.

**A `cd` invalidated my log paths.** One split run printed `FMT_EXIT=1` and clippy exits of 1 that were
redirect failures against a non-existent relative directory, not check results. Re-run with absolute
paths, the real numbers are 0, 0 and 101. Both are recorded here because the first set of numbers would
have misreported the branch.

## Tools

`no-mistakes` is **not initialized** in this repository, `.no-mistakes` and `.claude` are both absent, so
that pipeline was not run and no claim is made about it.
No browser step was taken; `chromium` is not installed, so any browser surface is **UNVERIFIED**.
`codebase-memory-mcp` graph tools were not used; the binding this review required was the extracted
archive compared per file with `git hash-object`.
MisakaNet was available only as a local stdio server and was not consulted; no failure-recall need arose
and no remote call was made.
The CI jobs-logs endpoint returned 0 bytes for both failing jobs, so the failure was localised from
step-level results and my own reproduction rather than from CI's log text.

## Evidence

Under `bench/out/meta42-inode-reservation-final-critic/` in the assigned worktree, gitignored:

| file | what |
| --- | --- |
| `head.tar.gz` | the source archive of `f5f7bbc8`, sha256 `6acadba9aac80c4e74e122b18d7eb89935f023804164c173370cc1b0be4d6d71` |
| `probe/sample.rs` | this review's representative public-API sample, added to the private archive only |
| `probe/scale.rs` | this review's bounded commit-count and cost probe, private archive only |
| `logs/sample.log` | the representative sample, reserved `2..13`, creates `13..19` and `31..33` |
| `logs/inode_reservation.log` | the eleven shipped tests, 11 passed |
| `logs/scale.log` | the measured commit counts and per-commit cost |
| `logs/fmt-pristine.log`, `logs/clippy-lib.log`, `logs/clippy-all.log` | fmt and the two clippy splits |
| `logs/lane.log` | every lock acquisition with its UTC time and every exit |