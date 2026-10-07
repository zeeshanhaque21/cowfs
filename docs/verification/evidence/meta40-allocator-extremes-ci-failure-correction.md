# Issue #40 M5 correction: the MAX case asserted the crash-path floor on a clean close

Lane: READY5 follow-on coverage for issue #40 item M5, branch `test/meta-allocation-option-extremes-40`.
New head: `ac2d88056c3b8f939a955507e3a8b341265f04c0`.
Prior head: `5b6856ea28a87c11f5d3948dc148e835c69693b1`, which failed CI.
Draft PR: #144.
Base: `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` (the review's base; remote MAIN has since moved to `1580e69b` after PR #143).

## Immutability

This receipt is a correction of a runtime failure.
It touches and supersedes nothing:

- `docs/verification/evidence/meta40-allocation-option-extremes.md` (SHA-256 `486b16f6...`) unchanged,
- `docs/verification/evidence/meta40-allocation-option-allocator-correction.md` (SHA-256 `c418d15c...`) unchanged,
- `docs/reviews/pr144-allocator-correction-wbuddy-review.md` (SHA-256 `f89aaae4...`) unchanged.

## The actual CI failure

Both `check` jobs failed on Ubuntu and macOS; `linux-fuse` passed and is not Meta evidence.
From the completed Ubuntu run `37523745105`, the Meta test binary ran four tests, three passed and one
failed:

```
running 4 tests
test a_u64_max_block_is_clamped_in_the_ordinary_allocator ... FAILED
test a_zero_block_still_creates_with_a_valid_file ... ok
test the_default_block_creates_valid_files_and_persists ... ok
test ordinary_creation_never_returns_the_root_or_the_limit ... ok

---- a_u64_max_block_is_clamped_in_the_ordinary_allocator stdout ----
thread 'a_u64_max_block_is_clamped_in_the_ordinary_allocator' panicked at
crates/cowfs-meta/tests/allocation_option_extremes.rs:77:5:
assertion `left == right` failed: the reserved floor survives a reopen
  left: 3
 right: 1099511627776
```

It was a test assertion, not a compile error and not a production failure.
It is a test bug.

## Root cause

The MAX case asserted the reopened durable floor equalled `INO_LIMIT`.
That is the crash-path invariant, and it is wrong on the clean-close path.

The close commit in `crates/cowfs-meta/src/db.rs` writes

```
let reserved = if closing {
    s.ino.next.min(s.ino.reserved)
} else {
    s.ino.reserved
};
meta.insert("ino_reserved", reserved)?;
```

`Meta::close` and the drop path both pass `closing = true`.
On a clean close the floor is deliberately collapsed to `next`, because numbers that were reserved
but never handed out no longer need protecting.
After the single ordinary create, `next` is 3, so the durable floor is 3.
`Meta::health().ino_floor` is `s.ino.reserved` in memory, which is `INO_LIMIT` inside the live
session, but the reopened store reads the collapsed `ino_reserved = 3`.

So the failed assertion compared an in-memory, still-live floor (`INO_LIMIT`) against the correctly
collapsed post-close floor (`3`).
Production is right; the test was wrong.

## The fix (test-only, one file)

`crates/cowfs-meta/tests/allocation_option_extremes.rs`, the MAX case only:

- Reopen floor assertion now checks honest clean-close semantics: the floor is a legal inode above the
  one used inode and at most `INO_LIMIT`, not equal to `INO_LIMIT`.
- The created inode is still not reissued after a reopen (`lookup` returns the same ino).
- A fresh ordinary create after the reopen resumes above the created inode and below `INO_LIMIT`.
- The unverified "id space stays exhausted" claim is removed; genuine exhaustion needs a durable
  floor ahead of `next` at open, which only a crash produces, and the crash harness is a separate #40
  item, not this lane.
- The now-unused `Error` import is dropped and the file doc comment corrected, so `-D warnings`
  stays clean.

The load-bearing property is unchanged and still asserted: an ordinary `create(ROOT_INO, ..)` under
`ino_block = u64::MAX` reaches `Tx::alloc` and does not overflow, returns inode `2`, reads back
through `lookup`, and `check()` is clean.

## Mutant sensitivity, source-only

Conceptual mutant: remove the clamp at `db.rs` `init`.
With `ino_block = u64::MAX`, the first ordinary `create` computes `2 + u64::MAX` in `Tx::alloc`,
which overflows (debug panic, release wrap), so the load-bearing create fails.
This was **not executed** locally. It is a source trace, and is now also visible in real CI: the
three reservation-free tests that pass and the load-bearing test that reaches `Tx::alloc` are the
same discrimination the mutant targets.

## Runtime status

- Prior head `5b6856e`: **FAILED in CI** on Ubuntu and macOS (`check`), the failure quoted above.
- New head `ac2d880`: source fix pushed; normal CI starts on this head.
  No green claim until that run says so. No local `cargo` was run on this lane (READY5 artifacts
  exceed the 8 GiB cap; no waiver). Standalone `rustfmt --edition 2021 --check` is clean.

## Runtime facts

```
$ git log --oneline -3
ac2d880 test(meta): assert clean-close floor collapse in the MAX case (#40 M5)
5b6856e docs(meta): publish the M5 allocator-correction receipt (#40)
d627598 test(meta): drive the extreme ino_block through ordinary creation (#40 M5)
$ git rev-parse HEAD
ac2d88056c3b8f939a955507e3a8b341265f04c0
$ rustfmt --edition 2021 --check crates/cowfs-meta/tests/allocation_option_extremes.rs
exit=0
```

## Remaining #40 items, not this lane

- Real-store crash harness pointed at `cowfs-store`: open.
- Ported mutation harness for the five named mutants: partial.
- Core `Health` wiring into `cowfs_core::Health`: Core seam.
- Whole #42 reserved-inode consumer and snapshot acceptance: Core and NFS scope.

Whole issue #40 stays open.
