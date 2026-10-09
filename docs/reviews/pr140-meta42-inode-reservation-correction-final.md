# PR #140 final review: SCOPED PASS on the doc and lint corrections, a BLOCK on merging the head as delivery, and a bounded-work proposal that is not approved unilaterally

Reviewer lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, READY5, held throughout.
Reviewer is independent of the metadata author lane. The metadata author's work is finished at `1214142ffc17b1fedc3b31d3a6f2a343aa7e8d36`; this lane did not author, extend or fix any of it.
My earlier Core #141 source review, `docs/reviews/pr141-meta42-core-atomic-rename-final.md` sha256 `fe6515c318a58d6640fcaf2040b94245b8d43507aa6f4528a43018c024ff3f50`, is complete and immutable, and a different lane (READY1) is running the Core #141 runtime review on its own lease.

Head reviewed: `1214142ffc17b1fedc3b31d3a6f2a343aa7e8d36`.
Chain: base `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0` (current `main`), then `f5f7bbc8af72e1ffd257e87c3193a7fe0ebe8b9e`, `2e6a31acd92c2418b90b664a23571a32bd8c648f`, `1214142`.

Canonical receipts, all present in the MAIN primary checkout and all left untouched:

| Receipt | sha256 (first 16) | bytes |
|---|---|---|
| `docs/reviews/pr140-meta42-inode-reservation-final.md` (original BLOCK) | `0f27f8bfee40745a` | 20450 |
| `docs/verification/evidence/meta42-inode-reservation-correction.md` | `9eb4fad0b7cc0e49` | 22204 |
| `docs/verification/evidence/meta42-inode-reservation-rustdoc-correction.md` | `c3e37608fb141926` | 11163 |
| `docs/verification/evidence/meta42-inode-reservation.md` (author) | `4ef55a3c64a8c0aa` | 13341 |

Review date: 2026-10-06.
This document: canonical PRIMARY copy, `docs/reviews/pr140-meta42-inode-reservation-correction-final.md`.

## Verdict

**SCOPED PASS on the documentation and lint corrections.** They are correct, minimal, and the two prior false claims are now properly retracted rather than quietly rewritten.

**BLOCK on treating this head as delivery of request 4.** Not because the code is wrong, but because the additive API alone is not what #42 asked for, and three substantive gates are still open.

**The bounded-work proposal is not approved.** It needs an operator decision, and I give evidence for and against it below rather than a rubber stamp.

## What I verified about the corrections

**The doc-attachment retraction is right, and I confirmed the mechanism independently.** `c3e37608` retracts `9eb4fad0`'s claim that adding a blank `///` line ended one doc block and started another. A blank `///` does not terminate a rustdoc comment: every consecutive `///` line above an item belongs to one attached doc comment. Adding one inserted a blank *paragraph* inside an already-contiguous block and changed no attachment at all. The receipt's own line-level attachment analysis shows the merged run was 20 lines carrying both the `sync` text and the reservation text, which is what the false claim missed.

**`1214142` actually moves the block instead of separating it.** The diff removes the `sync` doc run from above `reserve_inodes` and re-inserts it directly above `Meta::sync`, and **no blank separator line was added anywhere**. At the head the `sync` doc sits immediately above `pub fn sync`, and `reserve_inodes`'s own doc is a separate contiguous run ending above `pub fn reserve_inodes`. That is the fix that works, and it is a real `///` doc comment attached to the right item rather than a blank separator or a `//` note.

**The stripped-document digest matches across all three commits, and I verified how it was computed.** `git show <c>:crates/cowfs-meta/src/db.rs | grep -vE '^\s*///' | shasum -a 256` yields `d2cb7ada41339d1df1b774c6171551d39b39dd073e1fdc62c8a1c1dad0a31009` at `f5f7bbc`, `2e6a31a` and `1214142` alike.

I have to correct the brief on one detail, because the claim as given does not hold literally. The **full-file** `db.rs` digests are **not** equal across the three commits:

| commit | full `db.rs` sha256 |
|---|---|
| `f5f7bbc` | `a869faf1d6550df7d457a55423187cb67fc1173c60a0e131b8acde6270c33617` |
| `2e6a31a` | `4dabd6439879c8a31c574a8ac860bb121091edec2b19af8665e93686d5abeba9` |
| `1214142` | `6ec445d68a638099bbfc6ee6d4ad5d39c88c46a011ad510416d9d7510eb4b70b` |

They differ precisely because `2e6a31a` added one `///` line and `1214142` moved a doc block. What is equal is the **stripped** document digest above, and separately the **reserve loop region** at lines 715-750, which is byte-identical across all three (`852978dd…` for 715-750, `7ab76544…` for 716-749). So the intended claim is true in substance: no executable line changed at any point, and `d2cb7ada…` is the right invariant to quote, provided it is labelled as the stripped-document digest rather than the file digest.

**The two test lint fixes are correct and did not weaken assertions.**

```diff
-        r.start().0 >= ROOT_INO.0 + 1,
+        r.start().0 > ROOT_INO.0,
```

That is the clippy `manual_range_contains` style fix, and it is the same assertion: `ROOT_INO.0 + 1` is exactly `ROOT_INO.0` plus one, so `>= ROOT+1` and `> ROOT` are identical over the integers. No case was admitted or removed.

The `drop(reserved)` removal is also correct rather than a shortcut. `InoRange` is `Copy`, so the binding was already dead by the time the reopen happened; removing the drop removes a clippy `drop_non_drop` finding and changes nothing about the test's meaning. The fixture still reserves eleven numbers, creates nothing, and the reopen check still asserts no reissue.

**Test file is unchanged by `1214142`.** `git diff 2e6a31a 1214142 -- crates/cowfs-meta/tests/inode_reservation.rs` is empty, and the head carries **11** `#[test]` cases.

**What `1214142` actually is: format and docs only.** Two files, `db.rs` 7 lines changed (the doc move) and one new receipt. The reserve loop, the allocator behaviour and every test are as they were at `2e6a31a`. That is exactly the shape claimed, and it means **no clippy-green result carries forward from any earlier head.** `f5f7bbc` and `2e6a31a` had clippy findings; `1214142` is a different tree and its clippy status is only whatever CI reports.

## Actual CI at the time of writing

One run for the head, not finished:

| run | head | status | conclusion | created |
|---|---|---|---|---|
| `37395338690` | `1214142` | `in_progress` | none yet | `2026-10-06T00:41:48Z` |

`check (ubuntu-latest)`, `check (macos-latest)` and `linux-fuse` all `in_progress` at the moment of the snapshot. No result is claimed and no polling was done.

When it lands, what would actually close the gate is specific: all three jobs green **and** the log showing `cowfs-meta --test inode_reservation` executing **11 cases** with `0 failed`, not merely a passing `fmt` or `clippy` step. A green run where the new target is absent from the log proves nothing about the fixture.

**Even a fully green `1214142` does not close three gates**, which stay open by design:

1. **Lock-hold latency is open.** The author's own measurement, which I accept as reported and did not re-run, is `block 8, n 100001 -> 12501 commits, 140.1 s`, with per-commit cost about 11.2 ms and elapsed time linear in `n/block`. That is a single `wlock()` held for over two minutes while every `Snapshot::batch` and every `mutate` waits on the same lock. The 140.1 s figure is measured; anything at large `n` beyond it is extrapolation and I do not present it as measured.
2. **Commit-failure behaviour is open.** There is no deterministic fault seam on the durable reservation path, so the guarantee that a failed `reserve_durable` exposes no number is argued from source, not executed. See the seam design below.
3. **Consumer delivery is open.** Nothing outside `cowfs-meta` calls the API. `grep reserve_inodes` over `crates/cowfs-core/src` and `crates/cowfs-ctl/src` returns nothing, `virt.ino` still exists at `crates/cowfs-core/src/ino.rs:13-14`, and the alias table is still there.

## Is a bounded `n` aligned with the request? The audit, both ways

**What was actually promised.** Issue #42 request 4 says: "`Meta::reserve_inodes(n)` or `Tx::create_with_ino`. Would remove `Core`'s virtual inode numbers and its alias table. Meta now reserves durably inside itself, but a caller still cannot get a number before the transaction that creates the inode." It names no bound on `n`, and I checked the public docs: `docs/v1-meta.md` and `docs/design.md` contain no statement about reservation counts at all, so there is no published contract being broken and none promising unboundedness either.

So: **arbitrary `n` is the current de facto contract, bounded only by `INO_LIMIT`.** The brief asked me to establish this before approving any restriction, and that is what the evidence shows. A cap is therefore a **new restriction**, not a clarification, and the user's standing instruction forbids substituting a narrower objective to get tests passing. I am not approving it, and the proposal below is put forward as a decision for the operator, not as a merge condition I impose.

**Evidence that a bound is defensible as an API invariant.** Core's actual need is small and bounded: it hands out one number per created inode and releases each alias as soon as nothing can hold it, with the receipt citing 130 live aliases after 500,000 creates, and `virt.ino` reserving in blocks of 2^20. A consumer that needs a handful of numbers before a transaction would never approach a block. Under a bound of one block, the call takes **at most one** durable commit, and possibly **zero** if the cached `reserved` already covers `next + n`; I checked the loop and the `while` condition is `s.ino.next + n > s.ino.reserved`, so a covered range commits nothing. The author's own rejection table is right that silently truncating `n` and returning a short range must not happen, because `len()` would not match `n` and a short range that looks complete is a reuse hazard.

**Evidence against, which is why this needs a decision.** The bound is on **count per call**, not on lock-hold, unless it is tied to the block. `n <= block` gives one commit, but the default `block` is 16384, so one call would still be one durable commit, and the useful property is bounded lock-hold, not a small `n`. More importantly, the recovery argument does **not** support raising the floor arbitrarily, and I verified that in source rather than trusting the comment.

`record_recovery` at `db.rs:765-780` computes:

```rust
let ino_floor = meta_get(&meta, "ino_reserved")?
    .saturating_add(block)
    .min(INO_LIMIT);
```

Its documented premise, at `db.rs:751-759`, is that inode numbers "are reserved one block at a time, each reservation its own durable commit, and redb's repair falls back exactly one commit, so the lost commit advanced `ino_reserved` by at most one block."

**This is a real constraint, not a stylistic one.** If a single durable commit advanced `ino_reserved` by much more than one block and that commit were the one a repair discarded, the recovery skip of exactly one block would be **smaller than what was handed out**, and a number could be reissued. That breaks the crate's never-reused rule, which `docs/design.md` treats as non-negotiable.

So the constraint is: **any single durable commit may advance `ino_reserved` by at most one block.** That is a genuine existing invariant, and it is the correct thing for the proposal to be built on. It also means the naive fix, one commit that jumps the floor by the whole requested `n`, is unsafe whenever `n` exceeds one block, and I would not approve it.

**Can `n` stay arbitrary within that constraint?** Only by committing per block, which is what the code does today, and that is exactly what makes the lock-hold linear in `n`. Every route I can see that keeps `n` arbitrary and the lock bounded requires one of: releasing the lock between commits, which breaks contiguity of the returned range; a lazy or raised floor, which needs a persisted amount so recovery can skip correctly, which is a schema change; or a provenance ledger, which is a new on-disk structure. All three are excluded by the brief's constraints and by the design principle of not adding new on-disk state without need.

**So the honest conclusion: under the current on-disk format, contiguity, a single `n`, bounded lock-hold and the one-block recovery bound cannot all hold at once.** Something gives. The choices are which, and that is an operator decision, not mine.

**My recommendation, with the reasoning attached so it can be overridden.** Prefer the bounded contract, because the recovery invariant is a correctness property and lock-hold latency is a performance property, and correctness outranks performance here. But bound it to **one block per call, tied to `Options::ino_block`, returning `Error::LimitExceeded` above it and writing nothing**, and document it as an explicit restriction rather than a silent cap. Then a caller needing more calls repeatedly, which is a natural fit for the way Core allocates.

Two things that must be true before it ships: the test fixture's `ino_block` must exceed its largest literal `n` (the file uses 8 with a largest `n` of 20, so 8 must rise, and the receipt proposes 32), and the comment at line 126 about crossing a block boundary has to keep being true, since a bound of one block makes "more than one durable reservation commit" no longer reachable from a single call.

I am explicitly **not** approving this, because it is a restriction on a public API that the issue did not ask for, and the user forbade narrowing the objective by decree. What I am approving is that it is **candidly proposed and correctly reasoned**, which it is.

## The missing failure proof, and the exact seam it would need

**What is missing.** No deterministic fault injection on the durable reservation path, so these are unproven:

- that a failed `reserve_durable` returns `Err` **and** leaves the in-memory floor where it was, so no number is exposed;
- what the durable `ino_reserved` actually holds after a failed commit, which is a redb question and must not be assumed either way. I make **no** claim that nothing persisted, and **no** claim that a retry re-runs everything: two-phase commit means the outcome of a failed `wtx.commit()` is genuinely uncertain without measurement.
- that a redb **rollback** losing the reservation commit leaves the persisted floor and the in-memory floor consistent, and that no number in the lost range is reissued.

The existing test proves only the **no-write-on-refusal** half, by showing the allocator did not move after `Error::Invalid` and `Error::LimitExceeded`.

**The seam, following a pattern already in this repository.** `Core` has exactly this shape: `crates/cowfs-core/src/gate.rs:207` is a `pub(crate) fn set_fault(&self, kind: u8)` reached through a public test seam `Core::set_gate_fault`, and it is compiled in unconditionally with no production overhead. `cowfs-meta` already has a `thread_local!` at `db.rs:143`.

Minimum design, **not written here**:

- `crates/cowfs-meta/src/db.rs`: one `pub(crate)` fault setter plus a `thread_local!` guard, both behind the existing unconditional-compile pattern rather than `#[cfg(test)]`. The reason is already recorded in the author's receipt and I confirm it: `#[cfg(test)]` items are not visible to `tests/inode_reservation.rs`, which links the crate as a dependency, so a `cfg(test)` seam cannot be driven from the integration test at all.
- `crates/cowfs-meta/tests/inode_reservation.rs`: one public test seam, in the same shape as `set_gate_fault`.

**Cases needed, four, and no expensive matrix:** fault before the durable commit, asserting `Err` and an unchanged in-memory floor; fault after a real successful persist, asserting the durable floor actually moved so the two cases are distinguishable and neither is assumed; a rollback fixture losing the reservation commit, asserting the persisted floor, the in-memory floor and no reissue; and one concurrency case proving a reservation and an ordinary create stay disjoint across the fault path.

**Two correctness notes on the seam.** It must be **per-thread** or guarded, since the reservation runs under a global write lock and a process-wide fault would make every concurrent test flaky; and it must not require a Core import, a new public fault API on the shared `Error` surface, or a new dependency. All three are avoidable, since `gate.rs` shows the pattern needs only a `pub(crate)` setter and a public test seam in the same crate.

**None of this is implemented and none of it is run here.** Local cargo is blocked.

## What I did not do

- No local cargo, build, test, clippy, archive, target directory, fault test, repetition, probe, cleanup, prune, move, offload or cap waiver. `bench/out` stands at 20.863 GiB against the 8 GiB cap, untouched, with `ready-40` intact.
- No source, test or receipt edit. No commit, no push, no merge, no new issue, no new task.
- No checkout or branch change: this lane is still `fix/meta-inode-reservation-42` at `1214142`, clean, matching its remote, with no build process running.
- Older receipts `0f27f8bf…`, `9eb4fad0…`, `4ef55a3c…`, `c3e37608…` and my `fe6515c3…` are all unmodified.
- No `Meta::create_with_ino` designed. It may or may not be the better answer to request 4, and choosing between it and a reservation needs a spike on durable ownership of consumed numbers, not a decision made in a receipt. I am not claiming it is required, and I am not claiming it is impossible.
- No workflow, runner, dispatch, rerun or trigger change. One CI snapshot, no polling.
- Browser unverified, so nothing is linked or claimed visually. `no-mistakes` is uninitialized in this lane. Misakanet is local-only here and was not consulted.

## Conditions, in order

1. CI on `1214142` green on all three jobs **with** the log showing `inode_reservation` running 11 cases and 0 failed.
2. An operator decision on the `n` bound: accept one block per call as an explicit restriction, or keep `n` arbitrary and accept linear lock-hold, or fund a spike on a persisted reservation amount so recovery can skip correctly. I recommend the first, with the reasoning above, and I do not impose it.
3. The commit-failure and rollback seam written and run, closing the second open gate.
4. Consumer work in `cowfs-core` to actually use the reservation and retire `virt.ino` and the alias table, which is where request 4 is genuinely delivered. Until then this is an additive API, not the request.

None of 2 to 4 is a reason to reject the current head's doc and lint work, which is sound and should stand.