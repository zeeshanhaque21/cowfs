# PR 142 pending-physical-inode final review (head `6370060`)

Reviewer: wbuddy (fresh recovery, superseding resumed session HTTP422). READONLY. No build/test/dispatch/rerun/lease/GitHub-mutation performed.

## 0. Verdict

**FAIL / DO-NOT-MERGE.** The head does not build its own new test. Runtime gate blocks.
Source seams are present and source-correct, but **no named runtime result exists** because the test binary never compiled on either platform.

## 1. Exact pins

- Head: `6370060209779d25c04c89eda0036f42a4e272d0` (PR #142 API, base `main`; branch `fix/core-reserved-inode-consumer-42`).
- Merge (tested-merge) ref SHA: `3e95877982262324bbcb133805fa17f45ad33e6e`.
- Required tested-merge base `1580e69b`: not independently re-derived here; PR base is `main`, and the branch already merged `origin/main e488a17` at `1a49279`. Compatibility with `1580e69b` is **not proven** by the failing run.
- Prior head `2a8b83d` run `37532795484`: completed `failure` (conformance 197 pass / 26 fail, all `Stale`; alias unit suite green) - matches prior receipts.

## 2. Runtime evidence (completed CI, the actual result) - the load-bearing finding

Run **`37537568861`**, workflow `ci`, event `pull_request`, `head_sha = 6370060209779d25c04c89eda0036f42a4e272d0` (exact match), status `completed`, conclusion **`failure`**.

Jobs:
- `112522358329` linux-fuse - **success**.
- `112522358543` check (ubuntu-latest) - **failure**.
- `112522358457` check (macos-latest) - **failure**.

Both `check` jobs fail at step `Run cargo clippy --workspace --all-targets -- -D warnings`, identically on both platforms:

```
error: unused import: `ROOT_INO`
  --> crates/cowfs-core/tests/reserved_inode_metadata.rs:25:41
25 | use cowfs_vfs::{Error, Vfs, XattrFlags, ROOT_INO};
   |                                         ^^^^^^^^^
error: could not compile `cowfs-core` (test "reserved_inode_metadata") due to 1 previous error
Process completed with exit code 101.
```

Confirmed against head source: `ROOT_INO` occurs **only** on line 25 of the new test file; it is genuinely unused. This is a real, reproducible gate failure, not flakiness.

## 3. Required-gate status on this head

Because `cowfs-core` (test `reserved_inode_metadata`) failed to compile under `-D warnings`, the run stopped before the test stage. Therefore:

- Core conformance (the 26 `Stale` families this head targets): **UNEXECUTED** - the fix is source-traced only, never runtime-verified.
- alias suite, Core `reserved_inode_identity` (4), new `reserved_inode_metadata` (5): **UNEXECUTED**.
- Meta T12/T13/T14 + `inode_reservation` (11): **UNEXECUTED**.
- Ubuntu/macOS fmt+clippy: **FAILED** (clippy).
- FUSE (linux-fuse): green.

The PR body states "New CI on `6370060`: PENDING (no workflow run exists for this head yet)". That is now **false**: run `37537568861` exists for this exact head and failed.

## 4. Source verdict on the three named seams (source proof, not runtime)

All present at head, all consistent with the described intent:

1. `crates/cowfs-core/src/ns.rs` `make`: stores the create sequence on the child (`node.ns_seq.store(seq)` at the create seam, line 250), and the existing read gates test `ns_seq > sc.flushed()` (lines 105/474/486) so a pending child create commits before meta is read. **Source-correct.**
2. `crates/cowfs-core/src/io.rs` `committed_meta`: predicate is now the node's own pending signal (`node.seq <= sc.flushed()`, line 330) then `barrier` - **not** the old `meta_of`-is-`None` check. Covers get/list/remove/setxattr; dead `xattr_exists` helper is gone (grep: absent). **Source-correct.**
3. `crates/cowfs-core/src/inner.rs` `preserve_orphan`: for an uncommitted create (`node.seq > sc.flushed()`) it materializes an empty map instead of reading meta (lines 546-554), lock-order-safe (callers hold the namespace lock; a barrier cannot run). Self-safe for all four call sites. **Source-correct.**

Preserved authority intact: `ino.rs` `Aliases::insert` guard `if pm != virt` (lines 100-112) still avoids the reverse self-map; both unit tests remain (`a_physical_self_alias_does_not_create_a_reverse_self_map` line 332, `..._does_not_disturb_a_virtual_bridge` line 358). Physical IDs, one-time reservation authority, alias self-map guard, session limits, and the no-small-reservation-cap design are unchanged in source. Old alias/conformance expected results untouched (no assertion edits seen).

## 5. Remaining / missing (explicit)

1. Remove the unused `ROOT_INO` import (line 25) and re-run - a one-line fix, but until CI is green nothing downstream is verified.
2. Re-run the whole required set on a fresh head: Core conformance (all), alias, `reserved_inode_identity` (4), new `reserved_inode_metadata` (5), Meta T12-T14 + reservation (11), Ubuntu + macOS, fmt/clippy. FUSE is green and is not a substitute.
3. Prove tested-merge against the required base `1580e69b` exactly (currently only `e488a17` merge is evidenced).
4. Whole-#42 acceptance (identity across crash, session-limit under load, `git status` cost): out of scope here, not claimed.

## 6. Scope honesty

No local cargo/build/test/probe/archive/cleanup/offload/waiver/lease/signals/SSH/install/checkout/GitHub-mutation was performed. This review is source inspection plus reading the completed run `37537568861` logs; no poll/wait/dispatch/rerun. Complete is **not** claimed pending a green runtime. Fixed count 68 target, no scope growth.
