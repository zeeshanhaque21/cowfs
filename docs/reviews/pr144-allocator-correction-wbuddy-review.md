# PR 144 re-review: `Options::ino_block` at its extremes, allocator correction (issue #40 M5)

Reviewer lane: independent read-only audit (wbuddy lane).
Subject: PR 144 `test(meta): cover Options::ino_block at its extremes (#40 M5)`, corrected head.
Head: `5b6856ea28a87c11f5d3948dc148e835c69693b1` (tree `72e05fe9`).
Test commit: `d627598725b6e8029383949933e45caf947375a5`.
Base: `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` (current remote `main`).
New receipt: `docs/verification/evidence/meta40-allocation-option-allocator-correction.md` (blob SHA-256 `c418d15c49259e87c17a9558da1f6522f4eb3c372ad14f19d7c2696996b0c555` at head).
Immutable prior artifacts: `docs/reviews/pr144-allocation-option-extremes-wbuddy-review.md` (SHA-256 `d7ae5465...`), `docs/verification/evidence/meta40-allocation-option-extremes.md` (`486b16f6...`), `docs/reviews/meta40-current-delivery-and-acceptance-audit.md` (`f9517a29...`).

## Verdict

**METADATA-ONLY, SOURCE-COVERAGE MET, MERGE_READY PENDING CI.** The corrected tests now drive the real M5 path and discriminate the fix in source. They are not yet runtime-proven: CI is PENDING on this head. No MERGE_READY-as-green claim is made. Whole issue #40 stays open.

- The prior block was: tests called `reserve_inodes(n)` only, which never reads `block`, so they passed with the clamp removed.
- The correction replaces them with ordinary `create(ROOT_INO, b"f", 0o644)` calls that reach `Tx::alloc`, the exact M5 site.
- Source trace shows the load-bearing MAX test fails if the `init` clamp is removed, and the author states this explicitly as source-only, not executed.

## Pins

| object | SHA |
| --- | --- |
| PR 144 actual head | `5b6856ea28a87c11f5d3948dc148e835c69693b1` |
| head tree | `72e05fe9` |
| test commit | `d627598725b6e8029383949933e45caf947375a5` |
| receipt commit | `5b6856e` (parent `d627598`) |
| base / remote `main` | `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` |
| remote branch tip | `5b6856ea28a87c11f5d3948dc148e835c69693b1` (matches PR head) |
| `db.rs` base vs head | SAME (production unchanged) |
| `tx.rs` base vs head | SAME (production unchanged) |
| `types.rs` base vs head | SAME (production unchanged) |
| PR files | 3: test file, new receipt, prior evidence doc |
| production delta | NONE (tests + docs only) |

`git diff e488a17b 5b6856e --name-only`: test file plus two evidence docs. `db.rs`, `tx.rs`, `types.rs` rev-parse identical to base. Any coverage claim rests on the test file alone, as it should.

## The M5 path, in source (unchanged from base)

- Clamp at create: `crates/cowfs-meta/src/db.rs` `init`: `let ino_block = opts.ino_block.clamp(1, INO_LIMIT);` persisted, seeds `InoAlloc.block = stored_block.unwrap_or(ino_block)` on open.
- The allocator that reads the block: `crates/cowfs-meta/src/tx.rs` `alloc()`:
  ```
  if a.next >= a.reserved {
      let new = (a.next + a.block.max(1)).min(INO_LIMIT);
      (self.reserve)(new)?;
      self.ino.reserved = new;
  }
  ```
  reached by `self.alloc()` inside `Tx::new_child`, the backing of ordinary `create`/`mkdir`/`symlink`.
- `reserve_inodes` at `db.rs`: `let target = s.ino.next + n;` with a `n > INO_LIMIT - s.ino.next` guard. It reads no `block`.
- Fresh store: `ino_reserved = 2` (`db.rs` insert), so `next = reserved = 2`. First ordinary create enters `next >= reserved`.
- `ROOT_INO = Ino(1)` (`types.rs:12`). `health().ino_floor = s.ino.reserved` (`db.rs:450`).

## Corrected tests: what they actually do

Four tests in `crates/cowfs-meta/tests/allocation_option_extremes.rs`. `reserve_inodes` is absent from the file entirely (grep: only doc mentions). No private field access. Public API: `Meta`, `Options`, `new_snapshot`, `create`, `lookup`, `health`, `check`, `sync`.

| test | call | reaches `alloc()` | reads `block` | discriminates clamp removal |
| --- | --- | --- | --- | --- |
| `a_u64_max_block_is_clamped_in_the_ordinary_allocator` | `create(ROOT_INO,b"f",0o644)` MAX, reopen+create | yes | yes | **yes** (load-bearing) |
| `a_zero_block_still_creates_with_a_valid_file` | `create` + reopen `create` | yes | yes | no (author says so) |
| `the_default_block_creates_valid_files_and_persists` | `create` + reopen `create` | yes | yes | no (control) |
| `ordinary_creation_never_returns_the_root_or_the_limit` | `create` sweep `0,1,4,MAX` | yes | yes | no (boundary) |

### MAX case, discrimination traced in source (NOT executed)

With the clamp present, `ino_block = u64::MAX` -> stored `block = INO_LIMIT = 1 << 40`. First `create` -> `alloc()`: `next=2 >= reserved=2`, so `new = (2 + 2^40).min(2^40) = 2^40 = INO_LIMIT`. No overflow. The test asserts `f.ino.0 == 2`, `floor == INO_LIMIT`, and a clean `check()`.

With the clamp removed, stored `block = u64::MAX`. First `create` -> `alloc()`: `new = (2 + u64::MAX)`, which overflows: debug panics, release wraps. The test's `create` `.expect("... must not overflow")` therefore fails. This is the historical M5 defect, and the test is sensitive to it.

### Zero case honesty

The author correctly does NOT claim zero is clamp-sensitive: `alloc()` already writes `block.max(1)`, so zero is bounded locally without the `Options` clamp. The zero test is presented as bounds coverage for the zero end, not as a load-bearing mutant detector. That is accurate.

### Floor / reopen / exhaustion semantics

The MAX test asserts the durable floor after drop-all-reopen is still `INO_LIMIT`, that the next ordinary create is refused with `Error::LimitExceeded("inode numbers exhausted")` rather than wrapped into, and that the already-created inode is not reissued (`lookup` returns the same ino). This is honest exhaustion, asserted through public observables (`health().ino_floor`, `create` result, `lookup`, `check`), not a private-field oracle.

## Receipt accuracy

The new receipt corrects the two false claims of the prior receipt: it states the block IS read by `Tx::alloc` on every ordinary create (not "by `record_recovery` only"), and that the prior tests did not discriminate the fix. It labels the mutant trace source-only and declares the lane UNEXECUTED. It cites the correct M5 site (`tx.rs`), the clamp site (`db.rs` init), and the persisted-block-governs-open property. It preserves the immutable prior artifacts by reference. This is accurate.

One pin inconsistency, not a defect in the work: the receipt's header says "New head: `d627598`", which is the test commit, while the actual PR head is `5b6856e` (the receipt commit on top of it). The receipt commit is a docs-only child, so `e488a17b..d627598` and `e488a17b..5b6856e` differ only by the receipt file; production and test content are identical. The PR head is correctly `5b6856e` with parent `d627598`.

## Actual CI: PENDING

One bounded read-only inspection, no poll/wait/rerun/dispatch.

- Actual head `5b6856e`: run `37523745105` (`ci`, `pull_request`) - `in_progress`. Three check-runs `in_progress`: `check (ubuntu-latest)`, `check (macos-latest)`, `linux-fuse`.
- Test commit `d627598`: run `37523696990` (`ci`, `pull_request`) - `in_progress`. `check (ubuntu-latest)` and `check (macos-latest)` `in_progress`; `linux-fuse` `completed success`.

The PR is a `pull_request` event, so `actions/checkout` checks out `refs/pull/144/merge` (head merged into current `main` `e488a17b`). The `linux-fuse` job is not Meta fixture evidence: the FUSE suite does not run the Meta test binary and does not stand in for the new tests. No named result exists yet for `allocation_option_extremes`. Reported as PENDING; no green claim, no merge-ready-as-green.

Because the PR head `5b6856e` and the test commit `d627598` differ only by the doc receipt, the merge tree exercised by the pending run is the same code under test either way.

## Blind spots (recorded, not blockers)

- Source-only mutant trace is not executed mutation proof; the author states this and does not claim it as run. Confirmed by reading the receipt, not by any mutation run (none permitted on this lane).
- No test walks to full exhaustion via repeated allocation; the MAX case reaches exhaustion in one step by design (the extreme block reserves the whole space), which is the correct and bounded way to hit the boundary without an oversized allocation. This matches the receipt's stated ranges (requests of 2 to 5 numbers; sweep `0,1,4,MAX`).
- CI pending is the only thing between source coverage and a green merge-ready claim for this item.

## Evidence trail

- Fresh reads: `gh api` PR 144, check-runs, `actions/runs?head_sha=` for both head and test commit, `git ls-remote` branch tips, `git fetch --no-tags` of head (no checkout), `git rev-list --parents`, `git rev-parse ^{tree}`, `git diff --stat`/`--name-only base..head`, per-file rev-parse for `db.rs`/`tx.rs`/`types.rs` base vs head, `git show` of the test file, `db.rs`, `tx.rs`, `types.rs`, and the new receipt at exact SHAs, `git grep` for `ROOT_INO`/`INO_LIMIT`.
- NOT run: local cargo/build/test/clippy, target dir, archive, probe, cleanup, offload, cap waiver, commit, push, merge, PR body edit, issue/comment, new issue, close, CI rerun/dispatch, runner/workflow change, lease, signal, daemon, shared store, mount, history rewrite. No checkout. 8 GiB cap and free-space floor treated as binding.
- Not touched: `ses_eed5b7b0fffesJmKd2fYb2N3xB` (READY3 Core/Meta) and `ses_ef6d6f10cffeA7SelmYbknZnIX` (READY7 NFS).
- The clamp-removal mutant was traced in source, not executed; no mutant run is claimed.

## Scope statement

This review covers PR 144's corrected M5 test coverage only. It does not claim whole-issue #40 completion. #40 stays open with the residual items the prior audit lists (real-`cowfs-store` crash harness, ported mutation harness, Core `Health` wiring, whole #42 consumer acceptance). No new task, acceptance threshold, or cap change is introduced.
