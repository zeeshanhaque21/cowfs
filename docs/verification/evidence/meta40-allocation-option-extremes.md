# Issue #40 M5 receipt: `Options::ino_block` at its extremes

Lane: READY5 follow-on coverage for issue #40 item M5, branch `test/meta-allocation-option-extremes-40`.
Branch head: `abef93ad051fbf5da16463725b628c431213b42e`.
Parent: `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e`, the current remote MAIN at the time of writing.
Draft PR: #144.
Owed item: M5 in `docs/reviews/meta40-current-delivery-and-acceptance-audit.md`, SHA-256
`f9517a293351603a17183136764bffd6fb6c6f49dfb15fed81e6aaac2614720d`.

## Immutability

This receipt supersedes nothing and rewrites no earlier receipt.
The older receipts and reviews are untouched, including
`meta40-current-delivery-and-acceptance-audit.md`,
`meta42-reservation-required-proof.md`,
`meta42-reservation-proof-failure-repair.md` and the reviews.

## What the audit said M5 was

The audit records M5 as **SOURCE-ONLY**: `ino_block = u64::MAX` panicked in debug and wrapped in
release, and `init` now clamps it with `opts.ino_block.clamp(1, INO_LIMIT)` at create
(`crates/cowfs-meta/src/db.rs`).
But no test drives that value.
The only `u64::MAX` test covers `reserve_inodes(u64::MAX)`, a request count, which is a different
path from the block size.
Quoting the audit:

```
Status: SOURCE-ONLY. The panic is fixed in source by `opts.ino_block.clamp(1, INO_LIMIT)` at
open, but there is no test that passes `ino_block = u64::MAX`.
```

## Source, at the merged MAIN

The clamp is at `init`, in `crates/cowfs-meta/src/db.rs`:

```
let ino_block = opts.ino_block.clamp(1, INO_LIMIT);
```

and the clamped value is what is persisted at create:

```
m.insert("ino_block", ino_block)?;
```

On reopen the **stored** block governs, not the caller's:

```
let stored_block = match meta.get("ino_block")? {
    Some(g) => {
        let b = g.value();
        if b == 0 {
            return Err(corrupt("ino_block of zero"));
        }
        Some(b)
    }
    None => None,
};
```

The block is consulted by `record_recovery` only, where it drives
`reserved.saturating_add(block)`.
`Options::ino_block` is `u64`, default `16384`, and the doc comment states it is used only when the
file is created.

## The test

One new file, `crates/cowfs-meta/tests/allocation_option_extremes.rs`, public API only
(`Meta`, `Options`, `new_snapshot`, `reserve_inodes`, `health`). Four tests, each on a fresh store
because `ino_block` is read only at create:

- `a_zero_block_is_clamped_to_one_and_still_reserves`: `ino_block = 0` clamps to 1, a reservation
  still moves the floor and hands out numbers, the floor is durable on return, and nothing is
  reissued after a reopen.
- `a_u64_max_block_is_clamped_and_never_overflows_or_panics`: `ino_block = u64::MAX` clamps to
  `INO_LIMIT`; create and reserve never panic or wrap, the floor never exceeds `INO_LIMIT`, and a
  reopen with a different caller block still lets the stored block govern with no reissue.
- `a_max_clamped_block_still_refuses_a_request_past_the_limit`: a request past `INO_LIMIT` is
  refused with `Error::LimitExceeded`, writes nothing, and does not wedge the allocator.
- `the_default_block_is_a_normal_step`: the control, so the clamp is not collapsing every value to
  one end.

Each case uses requests of 2 to 5 numbers.
The coverage target is the clamp, not volume, so there is no large file, no range iteration, no
million objects and no wall-clock sensitivity.

The observables are the durable floor (`Meta::health().ino_floor`), the ranges handed back, and
`check()`, plus the ordinary reopen/no-reissue property.
No internal field is read and no private helper is used.

## What is proved at what layer

- SOURCE: the four tests are committed at `abef93a` in one file, inside `tests/`, and use only the
  public API. They do not touch production, Core, store, vfs, NFS, ctl, daemon, CI, manifests or
  dependencies, and they do not change the reservation count, cap a reservation, or alter
  floor/recovery semantics.
- FORMAT: `rustfmt --edition 2021 --check crates/cowfs-meta/tests/allocation_option_extremes.rs`
  is clean on the authoring lane.
- RUNTIME: **unexecuted on this lane.** No local `cargo build`, `test`, `clippy`, `target`,
  archive or probe was run. The lane shares this Mac's heavy-command resource with other workers,
  and the READY5 artifacts already exceed the 8 GiB cap, so no local build was taken and no waiver
  was claimed. The tests are ordinary workspace tests and execute under the existing
  `cargo test --workspace` step of normal CI on the head above. They are not claimed green here.

Source proof and execution proof are distinct, and this receipt claims only the first.

## Runtime facts

```
$ rustfmt --edition 2021 --check crates/cowfs-meta/tests/allocation_option_extremes.rs
exit=0
$ git log --oneline -2
abef93a test(meta): cover Options::ino_block at its extremes (#40 M5)
e488a17 Merge pull request #140 from zeeshanhaque21/fix/meta-inode-reservation-42
$ git ls-remote https://github.com/zeeshanhaque21/cowfs.git refs/heads/test/meta-allocation-option-extremes-40
abef93ad051fbf5da16463725b628c431213b42e  refs/heads/test/meta-allocation-option-extremes-40
$ git ls-remote https://github.com/zeeshanhaque21/cowfs.git refs/heads/main
e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e  refs/heads/main
```

No CI run, rerun, dispatch, trigger, workflow or runner change was made for this receipt.
The push starts a normal CI run on `abef93a`.
No timing number, no green-CI claim and no execution claim is made until that run says so.

## Remaining #40 items, not this lane

- Real-store crash harness pointed at `cowfs-store`, replacing the redb `StorageBackend`: open.
- Ported mutation harness for the five named mutants: partial, no single harness.
- Core `Health` wiring into `cowfs_core::Health`: Core seam, not this lane.
- Whole #42 reserved-inode consumer and snapshot acceptance: Core and NFS scope, not this lane.

Whole issue #40 stays open. This lane closes only the M5 coverage gap.
