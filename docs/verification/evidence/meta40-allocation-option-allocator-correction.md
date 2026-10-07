# Issue #40 M5 correction: the `ino_block` extremes are driven through the allocator

Lane: READY5 follow-on coverage for issue #40 item M5, branch `test/meta-allocation-option-extremes-40`.
New head: `d627598725b6e8029383949933e45caf947375a5`.
Previous head: `46eb2c78cd0585cb6e2ac3064ac3123e5ece46ae`, blocked by review `d7ae5465a5dcb2848e2b77e0306a41f403fe514532efaf198ac8691eda528eb7`.
Draft PR: #144.
Base: `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e`, current remote MAIN.

## Immutability

This receipt is a correction, not a rewrite.
It does not touch and does not supersede

- `docs/reviews/meta40-current-delivery-and-acceptance-audit.md` (SHA-256 `f9517a29...`),
- `docs/reviews/pr144-allocation-option-extremes-wbuddy-review.md` (SHA-256 `d7ae5465...`),
- `docs/verification/evidence/meta40-allocation-option-extremes.md` (SHA-256 `486b16f6...`).

The prior receipt at `486b16f6...` and the test commit `abef93a` remain in history.
What follows corrects two false claims in that prior receipt and replaces the tests.

## What the prior receipt and prior test got wrong

1. **False source model.** The prior receipt said the block is "consulted by `record_recovery` only."
   That is wrong.
   The block is also read by the ordinary allocator `Tx::alloc` in `crates/cowfs-meta/src/tx.rs`:

   ```
   if a.next >= a.reserved {
       let new = (a.next + a.block.max(1)).min(INO_LIMIT);
       (self.reserve)(new)?;
       self.ino.reserved = new;
   }
   ```

   reached by `self.alloc()` inside `Tx::new_child`, which backs every ordinary
   `create`/`mkdir`/`symlink`.
   This is the only place the block multiplies allocation, and it is the M5 path.

2. **Tests that did not discriminate the fix.** All four prior tests called `reserve_inodes(n)`
   only.
   `reserve_inodes` computes `let target = s.ino.next + n` (`crates/cowfs-meta/src/db.rs`) and
   never reads the block.
   So every one of those tests passes with the clamp removed.
   They are reservation-path tests, not allocator-path tests, and the review correctly blocked on
   that.

## The real path, at the merged MAIN

- Clamp, at create: `crates/cowfs-meta/src/db.rs`, `init`: `let ino_block = opts.ino_block.clamp(1, INO_LIMIT);`.
- Persisted at create and seeded on open: `block: stored_block.unwrap_or(ino_block)` (`db.rs`, the
  `InoAlloc` construction in `open`).
- The allocator that reads it: `Tx::alloc` (`tx.rs`), `(a.next + a.block.max(1)).min(INO_LIMIT)`.
- Historical defect: `ino_block = u64::MAX` with `next = 2` gave `2 + u64::MAX`, debug panic and
  release wrap.

`reserve_inodes` is a different path and is left as it was.

## The corrected tests

`crates/cowfs-meta/tests/allocation_option_extremes.rs`, public API only, each case on a fresh store
because the block is read only at create.

- `a_u64_max_block_is_clamped_in_the_ordinary_allocator` is the load-bearing case.
  Open with `ino_block = u64::MAX`, `new_snapshot`, then an ordinary `create(ROOT_INO, b"f", 0o644)`.
  The ordinary create succeeds instead of overflowing, returns inode `2`, and the inode reads back
  through `lookup` with the same ino and kind.
  The clamped block reserves up to `INO_LIMIT`, `check()` is clean, and after dropping every handle
  and reopening the floor is still `INO_LIMIT` and the next create is refused with
  `Error::LimitExceeded("inode numbers exhausted")` - honest space exhaustion, not a wrap and not a
  wedge. The already-created inode is not reissued.
- `a_zero_block_still_creates_with_a_valid_file`: `ino_block = 0` still creates a valid file and does
  not collide across a reopen. `alloc()` already writes `block.max(1)`, so zero is bounded locally
  even without the `Options` clamp; this is bounds coverage for the zero end and is not claimed as a
  load-bearing clamp-removal case.
- `the_default_block_creates_valid_files_and_persists`: the control, a valid file, the next id, and
  the floor across a reopen.
- `ordinary_creation_never_returns_the_root_or_the_limit`: a sweep over `0, 1, 4, u64::MAX` asserting
  an ordinary create never returns the root or `INO_LIMIT`.

The existing `reserve_inodes` tests are preserved untouched; this change adds the allocator path on
top.

## Mutant sensitivity, source-only

The conceptual mutant is removal of the clamp at `db.rs` `init`.
With the clamp removed and `ino_block = u64::MAX`, `InoAlloc.block = u64::MAX`, and the first
ordinary `create` computes `2 + u64::MAX` in `Tx::alloc`, which is the historical overflow; the
load-bearing test's `create` therefore fails.
This mutation was **not executed** - no local `cargo` build or test is permitted on this lane (see
Runtime). The sensitivity is a source trace, labelled as such until the named tests run in normal CI.

The prior file did not have this property: it would have passed with the clamp removed.

## Runtime status

**UNEXECUTED on this lane.**
No local `cargo build`, `test`, `clippy`, `target`, archive or probe was run.
This Mac's heavy-command resource is shared, and the READY5 artifacts already exceed the 8 GiB cap,
so no local build was taken and no waiver claimed.
Standalone `rustfmt --edition 2021 --check crates/cowfs-meta/tests/allocation_option_extremes.rs` is
clean.
The tests are ordinary workspace tests and run under the existing `cargo test --workspace` step of
normal CI on the head above.
They are not claimed green, and no number is claimed, until that run says so.

## Runtime facts

```
$ rustfmt --edition 2021 --check crates/cowfs-meta/tests/allocation_option_extremes.rs
exit=0
$ git log --oneline -3
d627598 test(meta): drive the extreme ino_block through ordinary creation (#40 M5)
46eb2c7 docs(meta): publish the issue 40 M5 extremes receipt (#40)
abef93a test(meta): cover Options::ino_block at its extremes (#40 M5)
$ git rev-parse HEAD
d627598725b6e8029383949933e45caf947375a5
$ git rev-parse 46eb2c78cd0585cb6e2ac3064ac3123e5ece46ae^
e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e
```

No CI run, rerun, dispatch, trigger, workflow or runner change was made for this receipt.
The push starts a normal CI run on `d627598`.

## Remaining #40 items, not this lane

- Real-store crash harness pointed at `cowfs-store`: open.
- Ported mutation harness for the five named mutants: partial.
- Core `Health` wiring into `cowfs_core::Health`: Core seam.
- Whole #42 reserved-inode consumer and snapshot acceptance: Core and NFS scope.

Whole issue #40 stays open. This lane addresses only the M5 allocator-path coverage.
