# PR 144 review: `Options::ino_block` at its extremes (issue #40 M5)

Reviewer lane: independent read-only audit (wbuddy lane).
Subject: PR 144 `test(meta): cover Options::ino_block at its extremes (#40 M5)`.
Head: `46eb2c78cd0585cb6e2ac3064ac3123e5ece46ae` (tree `7b10144ddbee36357eff6a0df045505c146a4cdc`).
Test commit: `abef93ad051fbf5da16463725b628c431213b42e`.
Base: `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` (current remote `main`).
Receipt: `docs/verification/evidence/meta40-allocation-option-extremes.md` (author SHA-256 prefix `486b16f6`).
Prior audit `docs/reviews/meta40-current-delivery-and-acceptance-audit.md` (SHA-256 `f9517a29...`) and the original receipt are immutable and not rewritten here.

## Verdict

**SOURCE BLOCK.** The tests are real, public-API, and well-shaped, but they do **not** exercise the allocation path that M5 is about. They would pass with the clamp removed, so they do not cover the historical panic/overflow. Metadata-only delivery is NOT MERGE_READY as M5 coverage.

- The 4 tests drive `reserve_inodes(n)` exclusively. `alloc()`, the ordinary `Tx` allocator, is never called.
- The M5 defect lives in `alloc()`: `let new = (a.next + a.block.max(1)).min(INO_LIMIT)` at `crates/cowfs-meta/src/tx.rs:72`, reached by `create`/`mkdir`/`symlink`.
- `reserve_inodes` uses `let target = s.ino.next + n` at `db.rs:825`, and never reads `block`. So the clamped `ino_block` value is not observed by any assertion in the new file.
- The receipt states "The block is consulted by `record_recovery` only." That is factually wrong: the block is also consulted by `alloc()` on every ordinary create when `next >= reserved`. This incorrect source model is why the tests miss the path.

## Pins

| object | SHA |
| --- | --- |
| PR 144 head | `46eb2c78cd0585cb6e2ac3064ac3123e5ece46ae` |
| PR 144 head tree | `7b10144ddbee36357eff6a0df045505c146a4cdc` |
| test commit | `abef93ad051fbf5da16463725b628c431213b42e` |
| base / remote `main` | `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` |
| remote branch tip | `46eb2c78cd0585cb6e2ac3064ac3123e5ece46ae` (matches PR head) |
| db.rs @head | `143528696d5e19491e129a79cfb064923516c8ae` (production unchanged) |
| tx.rs @head | `fd10680a...` (production unchanged) |
| PR 144 files | 2: `crates/cowfs-meta/tests/allocation_option_extremes.rs`, `docs/verification/evidence/meta40-allocation-option-extremes.md` |
| production delta | NONE (tests + docs only) |

## Production delta

`git diff e488a17b 46eb2c78`: 2 files, +315 / -0, both `crates/cowfs-meta/tests/allocation_option_extremes.rs` and one evidence doc. No production file is touched. `Meta`, `Options`, `db.rs`, `tx.rs`, `types.rs` are byte-identical to the merged base. So any coverage claim rests entirely on the new test file.

## The M5 path, in source

The historical defect: `Options::ino_block` is `u64` with no upper validation. `alloc()` computes `a.next + a.block.max(1)`. With `ino_block = u64::MAX` and `next = 2`, `2 + u64::MAX` overflows: debug panics, release wraps.

- `crates/cowfs-meta/src/tx.rs:66-77` `alloc()`:
  ```
  if a.next >= a.reserved {
      let new = (a.next + a.block.max(1)).min(INO_LIMIT);
      (self.reserve)(new)?;
      self.ino.reserved = new;
  }
  ```
  This is the only place `block` multiplies allocation. It is reached by `self.alloc()` at `tx.rs:206`, the ordinary create/mkdir/symlink inode allocation.
- `crates/cowfs-meta/src/db.rs:1455` `init`: `let ino_block = opts.ino_block.clamp(1, INO_LIMIT);` - the clamp, persisted at `db.rs:1477` and used to seed `InoAlloc.block` at `db.rs:1539` (`block: stored_block.unwrap_or(ino_block)`).
- `crates/cowfs-meta/src/db.rs:811-832` `reserve_inodes`: `n > INO_LIMIT - s.ino.next` guard, then `let target = s.ino.next + n`. No `block`. Its own doc says "draws on the same allocator as `mutate`" (the lock and `next`/`reserved`), but it does not use the block step at all.

The clamp protects `alloc()`; `reserve_inodes` does not consult the field the clamp bounds. A test that only calls `reserve_inodes` cannot detect whether the clamp exists.

## Why the 4 tests do not cover M5

Each test calls only `reserve_inodes(n)`:

| test | call | reaches `alloc()`? | reads `block`? |
| --- | --- | --- | --- |
| `a_zero_block_is_clamped_to_one_and_still_reserves` | `reserve_inodes(5)` x2 | no | no |
| `a_u64_max_block_is_clamped_and_never_overflows_or_panics` | `reserve_inodes(3)`, `reserve_inodes(1)` | no | no |
| `a_max_clamped_block_still_refuses_a_request_past_the_limit` | `reserve_inodes(u64::MAX)`, `reserve_inodes(2)` | no | no |
| `the_default_block_is_a_normal_step` | `reserve_inodes(1)` x2 | no | no |

Clamp-removal conceptual mutant, traced in source (NOT executed):

- Remove `clamp` at `db.rs:1455`. With `ino_block = u64::MAX` the stored value is `u64::MAX`, and `InoAlloc.block = u64::MAX`.
- `reserve_inodes(3)` still does `s.ino.next + 3`; it never reads `block`. Result identical. Test 2 passes.
- `reserve_inodes(5)` and `reserve_inodes(1)` also read no `block`. Tests 1 and 4 pass.
- Test 3's `LimitExceeded` comes from the `n > INO_LIMIT - next` guard, independent of the clamp. Passes.
- The `floor <= INO_LIMIT` assertion holds because `reserve_inodes` caps `target` via the guard, not via the clamp.

So all 4 tests survive clamp removal. They do not discriminate the fix. The one path that would fail - `create()` (or `mkdir`/`symlink`) with `ino_block = u64::MAX`, where `alloc()` would compute `2 + u64::MAX` - is never driven. This is the exact gap the audit's M5 item named, not a coverage of it.

## What would actually cover M5 (minimal owned seam)

One test in the same new file that drives the ordinary allocator:

- Open with `ino_block = u64::MAX`, then `new_snapshot` + `Snapshot::create(ROOT_INO, b"f", 0o644)` (or `mkdir`). First allocation runs `alloc()` with `next = 2` and `block = u64::MAX`; with the clamp this is `(2 + INO_LIMIT).min(INO_LIMIT) == INO_LIMIT`, no overflow; without it, `2 + u64::MAX` overflows. Assert the file exists, its `ino.0 <= INO_LIMIT`, `health().ino_floor <= INO_LIMIT`, and `check()` passes.
- Symmetrically, `ino_block = 0` with a `create()`: `alloc()` does `a.next + 0.max(1) == next + 1`, so the file still gets a number and the floor advances (proves the clamp-to-1 is load-bearing on the ordinary path).
- Reference `INO_LIMIT` (public, `1 << 40`) for the bound, consistent with the existing constant use.

This keeps the test small (one file, one object), public-API only, and makes the clamp removal fail the suite.

## Blind spots to record

- `reserve_inodes` vs `alloc`: the two share the lock and `next`/`reserved`, so the tests do exercise real reservation/floor/reopen/isolation semantics - they are not fake - but they are orthogonal to the block-step overflow. The doc comment's claim "`ino_block` is only consulted on create" is half true: it is consulted on create through `alloc()`, which the tests never reach, and on recovery through `record_recovery`.
- No test asserts the persisted-block-governs-reopen property through the allocator either; Test 2's reopen uses `reserve_inodes`, so it re-checks the reservation path, not `block`.
- The `a_max_clamped_block_still_refuses_a_request_past_the_limit` test is good and passes regardless of the clamp; it does not depend on the fix.

Not assessed or claimed: no large allocation, no exhaustion-to-`INO_LIMIT` walk, no million-object run, and none is required by the M5 item. The `INO_LIMIT = 1 << 40` public constant and the granularity policy are consistent with existing use; M5 is not a cap on large user requests, and the tests do not add one.

## Actual CI

PENDING. At audit time both runs for this branch are `in_progress`, no conclusion:

- run `37522974960` (`event=pull_request`, head `46eb2c78`) - `in_progress`.
- run `37522917066` (`event=pull_request`, head `abef93a`) - `in_progress`.

The three check-runs on head `46eb2c78` (`check (ubuntu-latest)`, `check (macos-latest)`, `linux-fuse`) are `in_progress`. The PR runs `pull_request`, so `actions/checkout@v4` checks out `refs/pull/144/merge` (head merged into current `main` `e488a17b`); the linux-fuse job is not Meta evidence and does not stand in for the new tests. No poll, wait, rerun, dispatch or runner change was made. Source-only discovery is reported as such; no runtime or green-CI claim is made.

## Evidence trail

- Fresh reads: `gh api` PR 144 + check-runs + `actions/runs?head_sha=`, `git ls-remote` branch tips, `git fetch --no-tags` of head and test commit (no checkout), `git rev-list --parents`, `git rev-parse ^{tree}`, `git diff --stat`/`--name-only base..head`, `git show` of `db.rs`, `tx.rs`, and the new test file and receipt at exact SHAs.
- NOT run: local cargo/build/test/clippy/fmt, target dir, archive, probe, cleanup, offload, cap waiver, commit, push, merge, PR body edit, issue/comment, new issue, close, CI rerun/dispatch, runner/workflow change, lease, signal, daemon, shared store, mount, history rewrite. No checkout. 8 GiB cap and free-space floor treated as binding.
- Not touched: `ses_eed5b7b0fffesJmKd2fYb2N3xB` (READY3 Core/Meta) and `ses_ef6d6f10cffeA7SelmYbknZnIX` (READY7 NFS).
- The conceptual clamp-removal mutant was traced in source, not executed; no mutant run is claimed.

## Scope statement

This review covers PR 144's M5 test coverage only. It does not claim whole-issue #40 completion; #40 stays open with the residual items the audit lists (real-store crash harness, ported mutation harness, Core `Health` wiring, whole #42 consumer acceptance).
