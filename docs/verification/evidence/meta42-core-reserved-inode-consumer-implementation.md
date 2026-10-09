# #42 request 4: Core consumer of reserved inode IDs (implementation status)

Status: WIP saved and pushed, not mergeable.
Compile and test proof: UNEXECUTED.
Reason: a full-worktree artifact capacity block forbids local cargo, build, and test.
This report states only what was actually run and observed.

## What was recovered

A prior worker on slot READY3 (branch `fix/core-reserved-inode-consumer-42`) stopped with repeated provider errors.
Its in-flight work was eight modified source files plus one documentation row, all uncommitted against HEAD `8a6892a`.
The eight files were the Core and Meta partial implementation of the reserved-ID create consumer.
The documentation row added `inner::take_reserved` to the lock-audit table in `docs/v1-core.md`, which is this task's own row for the new function, so it was included with the source rather than discarded.

### Saved local commit

- Commit: `01c8a2c45269ba68e21c5c09dd1c27717c42507a`
- Subject: `wip(core): reserved-ID create consumer with store and session bound tickets (#42)`
- Contents: 9 files, 352 insertions, 9 deletions.
- Source: `crates/cowfs-core/src/inner.rs`, `lib.rs`, `ns.rs`, `queue.rs`; `crates/cowfs-meta/src/db.rs`, `lib.rs`, `tx.rs`, `types.rs`.
- Documentation: `docs/v1-core.md` (one lock-audit row).

### Push

The branch was local-only, ahead of its remote by 8 commits before this save.
The remote branch tip was `10c4a0f9e5f49189db9d19027723f1e65595fdbd`, not the local `8a6892a`.
So the earlier claim that the WIP was already pushed was false; the work existed only on this host.

Push result, HTTPS remote `https://github.com/zeeshanhaque21/cowfs.git`:

```text
+ 7580028...01c8a2c HEAD -> fix/core-reserved-inode-consumer-42 (forced update)
01c8a2c45269ba68e21c5c09dd1c27717c42507a  refs/heads/fix/core-reserved-inode-consumer-42
```

The force used `--force-with-lease` pinned to the exact prior remote oid `7580028ed47e0152a4825b1799df9f563c5ae89c`, so no one else's push could be overwritten.
No reset, stash, or rebase was used.

## Identity and lease verification

Read-only checks, run before any change.

- Assigned path `/Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/3/cowfs` resolves to itself: `os.path.exists=True`, `os.path.islink=False`.
- `treehouse --root /Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave status` shows slot 3 leased as `[fix/core-reserved-inode-consumer-42]`, held by `cowfs-ready21`.
- The default pool's slot 3 (`verify/reuse-68`) is a different registered worktree and was never touched.
- `git worktree list --porcelain` lists the READY3 path at HEAD `8a6892a`, branch `fix/core-reserved-inode-consumer-42`.
- Dependency `573b02f5e069f1e52bc32a11f2da4ce4ec8083c4` is an ancestor of the branch head. The dependency pin is unchanged.
- No lease was acquired or returned, and no other slot, store, mount, daemon, or job was touched.

A first probe misread the path as a broken symlink by resolving through the wrong relative root.
That probe was wrong and was corrected against the coordinator recipe and the raw tool output above.

## What the implementation does

This section describes the code as written, not as verified by a run.

### Capability

`ReservedIno` in `crates/cowfs-meta/src/types.rs` is a move-only ticket: not `Copy`, not `Clone`, with private fields, carrying the store id and the inode number.
Only `Meta::reserve_tickets` mints one.
A create refuses a ticket whose store id differs, whose number is not in the open session's outstanding set, that was already spent in this transaction, or that names an existing inode.
Authorization is therefore by an opaque minted ticket bound to both store and session, never by a raw or copied numeric range, never by numeric absence, and never by a below-floor check.

### One-time consumption and retry

A ticket's number is removed from the session's outstanding set only when the create's batch closure returns `Ok`.
A closure that returns `Err` leaves the ticket spendable, which the existing meta test `a_closure_error_leaves_the_ticket_usable_for_retry` pins.

### Create, mkdir, symlink, replay

Core's `Inner::make` reserves one ticket before taking any Core lock, packs the reserved physical number, and queues `Op::Create` with `reserved: Some(ticket)`.
`create`, `mkdir`, and `symlink` all route through `make`.
Replay dispatches `Op::Create.reserved`: `Some` calls `tx.create_at` / `mkdir_at` / `symlink_at`; `None` calls the old `tx.create` / `mkdir` / `symlink`, preserving the old-store format.
The block size is `RESERVED_BLOCK = 1 << 16`, so there is no small request cap.

## Finding: retry and commit-after-persist safety has a gap (not fixed here)

The task requires preserving failed-create and flush retry, and commit-after-persist error safety.
The current code does not fully preserve it.

In `crates/cowfs-meta/src/db.rs`, the batch path removes each spent reserved number from the session as soon as the closure returns `Ok`, at the loop reading:

```text
for ino in &spent {
    s.reserved.remove(ino);
}
```

That loop runs before the inline commit at `self.commit(s, Extra::None, false, true)`.
On a commit error the code records `s.flush_err`, calls `note_flush_failure`, and returns the error, but it does not restore `s.reserved` and does not roll `e.tree` back to the saved tree (the only `e.tree = saved` sites are the closure `Err` and panic arms).
Core opens Meta with the default `Ack::Applied`, so this inline commit is the normal path.

Consequence: after a commit error, the number is gone from the session's outstanding set while its create did not become durable.
A retry of the same batch reaches `Tx::new_child_at`, where `self.reserved.contains(&ino)` is now false, and is refused with `Invalid("reserved number was not issued by this store's open session")`.
The create cannot be retried in that session, which is the opposite of the documented contract.
The in-code comment states that removal is deferred so a failed closure leaves the ticket usable for a retry; that holds for a closure error, which is what the test covers, but not for a commit error after a successful closure.

The same window exists on the durable path: `wait_durable(seq)?` can return `Err` after the removal has already happened.

No fix is included in this commit.
The repair touches the durability path and cannot be compile-checked or tested under the current capacity block, and shipping an unverified durability change is worse than reporting it.
The minimal correct shape is to keep a spent number in `s.reserved` until its create is durable, and on any commit or `wait_durable` failure to restore the spent numbers and roll `e.tree` back to `saved`.

## Dead code risk (not fixed here)

`Inner::alloc_virt`, `Inner::reserve_virt`, `ino::virt`, and `ino::VIRT_BLOCK` no longer have a live caller now that Core creates through reservation tickets.
`alloc_virt` has zero callers.
`ino::virt` is still exercised by unit tests inside `ino.rs`.
If the crate denies warnings, clippy can fail on the now-unused private items.
This was not changed, because removing them is outside the recovered WIP and cannot be checked under the capacity block.

## Verification that did and did not run

Ran:

- `git status`, `git rev-parse`, `git log`, `git diff`, `git worktree list`, `git ls-remote`.
- `treehouse --root ... status`.
- `gh-axi pr view`, `gh-axi pr checks`, `gh-axi pr list`.
- `rustfmt --edition 2021` over the eight owned source files, exit 0.

Did not run, because the capacity block forbids it:

- `cargo build`, `cargo test`, `cargo clippy`, or any local build, target, or probe.
- The new end-to-end create, write, flush, drop, reopen identity result.
  There is no actual runtime result for the NEW path in this report, and the earlier `1 passed; 2 failed` figure in the PR body is from the OLD fixture, not this head.

The existing Core test `crates/cowfs-core/tests/reserved_inode_identity.rs` at `10c4a0f9e5f49189db9d19027723f1e65595fdbd` remains the acceptance test.
Its two positive assertions and the virtual-tag control are unchanged by this save.

## PR state

- PR 142, title `test(core): reserved-inode consumer regression for #42 request 4`, draft, open, Refs #42, no closing form, no co-author, not merged.
- Head after push: `01c8a2c45269ba68e21c5c09dd1c27717c42507a`.
- Checks at report time: 3 pending (`check (ubuntu-latest)`, `check (macos-latest)`, `linux-fuse`).
- Not mergeable without a real NEW identity test result, which this environment cannot produce.

## Remaining work

1. Restore commit-failure retry safety, and add a meta test that forces a commit error after a successful reserved create and then retries, asserting the ticket is still accepted and no inode was created.
2. Decide the dead virtual-number path: keep it for a documented reason or remove it, then clear any dead-code lint.
3. Run the Core identity test and the meta reservation suite on a machine with build capacity and record the real NEW-path output.
4. Only after 1 through 3 pass, re-check the PR body runtime section, which currently reads UNEXECUTED.
