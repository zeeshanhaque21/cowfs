# PR144 clean-close runtime acceptance: final independent review

Independent, read-only review of PR144 head `fbdd104f01aa51ad7a19b5ce0aa67b67a04e5ba8` on `test/meta-allocation-option-extremes-40`, testfix `ac2d88056c3b8f939a955507e3a8b341265f04c0`.
Subject: issue #40 item M5 test coverage for `Options::ino_block` at its extremes.
Read with `docs/design.md`, `docs/verification/evidence/meta40-allocator-extremes-ci-failure-correction.md`, and prior source-acceptance `docs/reviews/pr144-allocator-correction-wbuddy-review.md`.

No source, checkout, local cargo/build/test, probe, cleanup, offload, cap waiver, lease, signal, SSH, shared resource, GitHub mutation, or MAIN commit/push was performed.
No re-run, dispatch, wait, or poll.
Old reports and receipts are immutable and were not touched.
8 GiB cap and 20 GiB free-space floor treated as binding; no local execution.
Primary dirty `docs/v1-core.md`, `progress/index.html`, `progress/plan.json` preserved.
READY3 Core author, READY5 M5 author, and the active Core reviewer are not overlapped.

## Pins

| object | value |
| --- | --- |
| reviewed head | `fbdd104f01aa51ad7a19b5ce0aa67b67a04e5ba8` |
| head tree | `ab660d7d81547f6ceb77fe9bcc9f6504120dcd66` |
| head parent | `ac2d88056c3b8f939a955507e3a8b341265f04c0` |
| testfix commit | `ac2d88056c3b8f939a955507e3a8b341265f04c0` (parent `5b6856ea...`) |
| testfix parent (prior head, CI-failed) | `5b6856ea28a87c11f5d3948dc148e835c69693b1` |
| remote branch tip | `refs/heads/test/meta-allocation-option-extremes-40` = `fbdd104f...` |
| remote PR head | `refs/pull/144/head` = `fbdd104f...` |
| base (review base) | `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` |
| current remote `main` | `1580e69b9d987f63c07b2430f8c0b4547ecd8622` |
| test blob (`ac2d8805` and `fbdd104f`) | `bd58d08f0ac34fe0cfc0d73faa8f68a24410779e` |
| test blob at prior head `5b6856ea` | `4689ca74353d44de67f6aca43aa6961de8244ab8` |
| new receipt blob at head | `0085f02d622b4db4f0bfe6ca518bb08438b69a82` |
| `db.rs` base = head | `143528696d5e19491e129a79cfb064923516c8ae` |
| `tx.rs` base = head | `fd10680a4c2c573f9acab8f2cd6d74add3a30acd` |
| `types.rs` base = head | `22fcbabbb6fe9ce74906c387e9988f8d2fa830a4` |
| `check.rs` base = head | `5c654c83d6eaf7814675b585caf6fbc4b065d475` |

Commit chain: `fbdd104` (doc receipt) -> `ac2d880` (test fix) -> `5b6856e` (doc receipt) -> `d627598` (test).
`git diff --name-only 5b6856e fbdd104f` = exactly two paths: the test file and the new receipt.
`git diff --name-only e488a17 fbdd104f -- crates/cowfs-meta/src/` = empty (no production delta).
Scope: test-only plus one receipt doc. No production, Core, store, vfs, NFS, ctl, daemon, CI, or dependency change.

## Verdict

**Source verdict: the corrected test logic is sound, honest, and does not over-claim. Production is unchanged and correct.**
**Runtime verdict: FAILED CI, no named test result.**
The reviewed head failed `ci` on both `check` jobs at `cargo clippy --workspace --all-targets -- -D warnings`, a lint error in the new test file, before `cargo test` ran.
There is no passing named result for `allocation_option_extremes` at this head.
The prior receipt and PR body say "pending" and "no green claim until it says so"; the run has since completed as failure, so that status is now stale.
This is not merge-ready on runtime.

## The actual head CI result (derived from job logs)

Run `37531555084` (`ci`, `pull_request`) = **completed / failure**.
Checked out `b9eb6be = Merge fbdd104f01aa51ad7a19b5ce0aa67b67a04e5ba8 into 1580e69b9d987f63c07b2430f8c0b4547ecd8622`, so the exact reviewed head was genuinely tested against current `main`.

- `112501915412` `check (ubuntu-latest)`: failure
- `112501915551` `check (macos-latest)`: failure
- `112501915284` `linux-fuse`: success (runs no Meta tests; not evidence for this item)

Workflow order (`.github/workflows/ci.yml:21-23`) is `cargo fmt --all --check`, then `cargo clippy --workspace --all-targets -- -D warnings`, then `cargo test --workspace`.
`fmt` passed.
`clippy` failed.

Ubuntu, exact diagnostic:
```
error: unnecessary `>= y + 1` or `x - 1 >=`
  --> crates/cowfs-meta/tests/allocation_option_extremes.rs:85:9
   |
85 |         floor >= created.0 + 1,
   |         ^^^^^^^^^^^^^^^^^^^^^^^ help: change it to: `floor > created.0`
note: `-D clippy::int-plus-one` implied by `-D warnings`
error: could not compile `cowfs-meta` (test "allocation_option_extremes") due to 1 previous error
```
macOS shows the identical error at the same line.
That is the only compile error; the second `error:` line is its cascade.

Because clippy runs before tests, **the four corrected tests never executed at this head**, and no `running 4 tests` / `test result:` line exists for `allocation_option_extremes`.
The blocker is a one-character lint fix (`>= y + 1` -> `> y`), not the test logic and not production.

The same lint failure is present on the testfix commit: run `37531480788` tested `1b53de1 = Merge ac2d8805 into 1580e69b` and failed identically at line 85.
The doc-only commit `fbdd104f` did not change it.
So neither `ac2d8805` nor `fbdd104f` has a green named result.

## The original failure the receipt describes is accurate

Run `37523745105` tested `5b6856e` merged into `e488a17` and is completed / failure.
The `check (ubuntu-latest)` job there reached `cargo test` (clippy passed on the old file) and the Meta binary ran four tests:

```
running 4 tests
test a_u64_max_block_is_clamped_in_the_ordinary_allocator ... FAILED
...
thread 'a_u64_max_block_is_clamped_in_the_ordinary_allocator' panicked at
crates/cowfs-meta/tests/allocation_option_extremes.rs:77:5:
assertion `left == right` failed: the reserved floor survives a reopen
  left: 3
 right: 1099511627776
test result: FAILED. 3 passed; 1 failed
```
`1099511627776 = 1 << 40 = INO_LIMIT` (`crates/cowfs-meta/src/types.rs:75`).
The receipt's root cause is correct and verified in source.

## Why production is right and the old test was wrong

On a clean close, the close commit writes the collapsed floor:

`crates/cowfs-meta/src/db.rs:664-669`
```
let reserved = if closing { s.ino.next.min(s.ino.reserved) } else { s.ino.reserved };
meta.insert("ino_reserved", reserved)?;
```
`Meta::close` and the drop path both pass `closing = true` (`db.rs:499`, `:511`, `:725-726`).
After the single ordinary create, `next = 3`, so the durable floor is `3`.
`Meta::health().ino_floor` is `s.ino.reserved` in memory (`db.rs:450`), which is `INO_LIMIT` inside the live session, but the reopened store reads `ino_reserved = 3`.
The old test compared the live in-memory floor (`INO_LIMIT`) against the correctly collapsed post-close floor (`3`).
Production is right; the test asserted a crash-path invariant on a clean-close path.
This matches the receipt exactly.

## What the corrected tests do (verified line by line)

Fresh store seeds `ino_reserved = 2`, so `next = reserved = 2` (`db.rs:1478`, `:1534-1540`).
`Tx::alloc` (`tx.rs:66-79`):
```
if a.next >= INO_LIMIT { return Err(Error::LimitExceeded("inode numbers exhausted")); }
if a.next >= a.reserved {
    let new = (a.next + a.block.max(1)).min(INO_LIMIT);
    (self.reserve)(new)?;
    self.ino.reserved = new;
}
let ino = Ino(self.ino.next);
self.ino.next += 1;
```

MAX case `a_u64_max_block_is_clamped_in_the_ordinary_allocator` (lines 37-112):
- First `create(ROOT_INO, b"f", 0o644)` under `ino_block = u64::MAX`; `.expect("... must not overflow")` at line 49 is the load-bearing assertion.
- Asserts `f.ino.0 == 2`, `kind == File`, `f.ino > ROOT_INO`, `lookup` returns the same ino and mode, live `health().ino_floor == INO_LIMIT`, and `check()` clean.
- After drop-all and reopen with `opts_with_block(4)` (the stored block governs on open, `db.rs:1509-1517`, `:1539`), asserts the floor is a legal inode `> ROOT_INO` and `<= INO_LIMIT`, and `>= created + 1`; the created inode is not reissued (`lookup` returns the same ino); a fresh create yields `g.ino > created` and `g.ino < INO_LIMIT`; `check()` clean.

Zero case (lines 117-150): valid file, no collision across reopen. The doc comment correctly says zero is bounded locally by `block.max(1)` and is **not** claimed as a clamp-removal detector. Honest.

Default control (lines 155-192): valid file, floor covers the created inode, no collision across reopen, `check()` clean.

Extreme sweep (lines 197-217): `block in [0, 1, 4, u64::MAX]`, an ordinary create never returns the root or `INO_LIMIT`.

`reserve_inodes` appears only in doc comments, never called (verified). Public API only: `Meta`, `Options`, `new_snapshot`, `create`, `lookup`, `health`, `check`, `sync`, `tempfile`. No private-field oracle.

## Clamp-removal mutant, source trace (not executed)

`init` stores the clamped value: `db.rs:1455` `let ino_block = opts.ino_block.clamp(1, INO_LIMIT);`, inserted at `:1477`.
Remove the `.clamp(1, INO_LIMIT)`: with `ino_block = u64::MAX` the stored block is `u64::MAX`, so on open `block = u64::MAX` (`:1539`).
The first ordinary `create` reaches `Tx::alloc` with `next = 2 >= reserved = 2`, computing `2 + u64::MAX` **before** the `.min(INO_LIMIT)`, which overflows: debug panic, release wrap.
The test's line-49 `.expect("... must not overflow")` fails.
The mutant is caught by the first create; the load-bearing property is real.
This was not executed locally (correctly stated) and is now also discriminating in real CI in the sense that the three reservation-free controls and the load-bearing create are the same split the mutant targets, though at this head the run never reached the tests.

## The precise-invariant question

The prior review's bar was: do not accept merely an inequality that lets a duplicate or wrong floor slip.
Assessment of the corrected reopen block:

- Duplicate reissue IS caught: `lookup` returns the created ino after reopen (`assert_eq!(back.ino, created)`), and a fresh create must exceed it. A floor wrongly below the used inode would fail these.
- A wrap IS caught: `g.ino < INO_LIMIT` plus the first-create `.expect`.
- A silently-too-high but legal floor is NOT pinned: `floor > ROOT_INO && floor <= INO_LIMIT` and `floor >= created + 1` all pass for, say, `floor = 5`, which is not the true clean-close value `3`.

So the reopen floor assertions are honest bounds, not an exact-value identity.
This is consistent with the file's own doc comment ("proves the clamp held and the created inode survived a reopen, not that the id space stayed reserved"), so there is no over-claim.
If an exact clean-close floor is wanted, the assertion would need `floor == created + 1` (i.e. `== 3` here); that is a strengthening suggestion, not a defect in the current honest claim.

## Remaining #40 gates

Issue #40 stays open.
This PR addresses only M5's test coverage, and even that waits on a green run.
Other open #40 items, untouched here:
M1 (background-timer panic health signal), M3 (`open_recover` counter rollback / never-reused `Ino`, opt-in), M2 (transient corrupt read poisons the handle), M4 (empty splice at a non-boundary bumps version), M6 (zero-length `ChunkRef` collapse), the ported mutant harness for the five named mutants, and the real-`cowfs-store` crash harness.
Also open outside this lane: Core `Health` wiring, and whole #42 reserved-inode consumer acceptance.

The crash-path floor proof is already runtime-accepted on PR140 (issue #42); this review does not demand a new exhaustion mechanism and does not fabricate one.
A clean close collapsing the floor is correct, not a gap.

## Constraints

- No local `cargo` was run on this lane (READY5 artifacts exceed the 8 GiB cap; no waiver). Confirmed: the receipt and PR body state this, and no local test artifacts appear in the evidence.
- No tiny-request cap: the failure is a lint error in `cargo clippy`, with no request-size or resource gate involved.
- All work is in the leased READY5 worktree; this report is the only new file, written to the MAIN primary checkout.
- No poll/wait/re-run/dispatch/runner/workflow change: the run status was read once from the completed job logs.

## Immutability

Unchanged (verified):
- `docs/verification/evidence/meta40-allocation-option-extremes.md` = `486b16f6c675db038eaaa5e150e7778ea9c80dc976e9115af8c7a8c700d5e2de`
- `docs/verification/evidence/meta40-allocation-option-allocator-correction.md` = `c418d15c49259e87c17a9558da1f6522f4eb3c372ad14f19d7c2696996b0c555`
- `docs/reviews/pr144-allocator-correction-wbuddy-review.md` = `f89aaae47e6cbb5ee852ab48e22be570bdb39c987a688465d2e5c30f5507eb76` (untracked in MAIN; not part of the head tree, correctly)
- `docs/reviews/pr144-allocation-option-extremes-wbuddy-review.md` = `d7ae5465a5dcb2848e2b77e0306a41f403fe514532efaf198ac8691eda528eb7`

## Findings

| id | severity | finding |
| --- | --- | --- |
| R1 | blocking | Head `fbdd104f` failed CI on both `check` jobs at `clippy -D warnings`, `int_plus_one` on test line 85, before `cargo test`. No named test result exists. Not merge-ready on runtime. |
| R2 | blocking | Same lint fails on `ac2d8805` (run `37531480788`). Neither corrected commit has a green named result. |
| R3 | medium | Receipt and PR body say "pending / no green claim until it says so"; the run has completed as failure. The runtime status statement is stale and must be corrected to "failed". |
| R4 | low | Reopen floor assertions are inequalities; a too-high-but-legal floor is not pinned. Duplicate reissue and wrap ARE caught. Honest per the doc comment; a `floor == created + 1` strengthening is optional. |
| R5 | info | Original `5b6856e` failure (`allocation_option_extremes.rs:77`, `left 3 / right 1099511627776`) is verified exactly as the receipt states; root cause is correct. |
| R6 | info | Production `db.rs`/`tx.rs`/`types.rs`/`check.rs` byte-identical base-to-head; scope is test-only plus one receipt. |
| R7 | info | `linux-fuse` is green and is not evidence for this item; it runs no Meta tests. |

## Bottom line

The corrected test content is the right content: it drives the real M5 path through ordinary `create`, removes the false clean-close exhaustion claim, pins identity readback and a fresh create above the used inode, and honestly labels the zero case and the source-only mutant.
Production is unchanged and correct.
But the reviewed head has not passed CI: both `check` jobs failed at `clippy::int_plus_one` on test line 85, so the four corrected tests never ran and there is no named green result.
The fix is a one-character edit (`>= y + 1` -> `> y`); after that, the tests must actually run green before this item can be called runtime-accepted.

Whole issue #40 stays open.
This review is not a new test policy, not a new exhaustion requirement, and not a whole-#40 closure.


