# meta42-snapshot-rename-format-correction: the real rustfmt check, one deviation, both files now clean

Lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, READY5, held throughout.
Verified before any edit: branch `fix/meta-snapshot-rename-42`, `HEAD` `c66befa4bcf2c5a1a6f7af3c385d5a1578275679`, working tree clean, local equal to the remote.
Parent of this work is `c66befa`, whose parent is the red head `9d1e5ef66d55798da08780296a05d6601468611f`.
Immutable, preserved unedited: `91aeec0e68d2c2b85daef8a1e61c70afb8de6642a9a12570a65ac6e45848f3da`, `93c4b1317b7d9445ced3f7d1316e159b3cca472cbee6172c0a81e2b9a8883c0b`, `a390aaf4b84bcd6a36117b854faa54710543e4c32c20e868185ff08a390be811`, `471a07310e793016763a1f77e4e195bfeca89dcf1613d84fe50b238a6355b0ec`, `b66ea61b14d2507ea0d9eb1975c0cd40e40c774a132f0271296818970a8d824e`.

## What was run, on what, and what it returned

Standalone `rustfmt` was already installed, so nothing was installed: `/Users/zeeshanhaque/.cargo/bin/rustfmt`, `rustfmt 1.10.0-stable (b940084d7e 2026-09-28)`.
Configuration was read rather than assumed: workspace `Cargo.toml` sets `edition = "2021"`, `crates/cowfs-meta/Cargo.toml` inherits it with `edition.workspace = true`, and there is **no** `rustfmt.toml` and no `.rustfmt.toml` anywhere in the tree, so default config applies.

The check was run per file on the two owned paths only, never workspace-wide:

```
rustfmt --edition 2021 --check crates/cowfs-meta/src/db.rs                  -> exit 0
rustfmt --edition 2021 --check crates/cowfs-meta/tests/snapshot_rename.rs  -> exit 1
```

`--check` writes nothing. No `cargo fmt`, so no other file in the workspace was touched.

**The warning I gave last turn was half wrong, and the check is what showed it.**
I said hand-written code that had never been through rustfmt was "the single most likely reason this run goes red".
For `crates/cowfs-meta/src/db.rs` that was false: exit 0, zero diff, the eleven-line root fix was already conforming.
It was true for the test file, and for exactly one place.
`crates/cowfs-meta/tests/snapshot_rename.rs:583`, one `assert_eq!` whose two arguments rustfmt puts on one line:

```diff
     assert_eq!(
-        read_back,
-        body,
+        read_back, body,
         "every byte must come back identical through the renamed snapshot's root"
     );
```

## What was changed

Two edits, `2 insertions(+), 7 deletions(-)`, formatting and comment only.

**The rustfmt deviation**, applied exactly as rustfmt asked: the `assert_eq!` argument list collapsed to `read_back, body,`.
Nothing else about that assertion changed. The comparison, both operands, and the message are the same.

**The `db.rs` comment, five lines to one**, per the project default of commenting only a non-obvious why in one short line:

```rust
let e = s.snaps.get(id).ok_or(Error::NoSuchSnapshot)?;
// The dirty flush above may have moved the root before session publication.
let root = new_roots
```

**The root-resolving code is byte-identical and semantically unchanged**: the `new_roots` lookup, the `.map(|(_, r)| *r)`, the `.unwrap_or(e.info.root)` fallback, `info.root = root`, and `info.name` are all untouched, and the `Extra::Rename` arm still writes nothing but `SNAPSHOTS` and `SNAP_NAMES`.
The diff for `db.rs` against `c66befa` is five comment lines out and one comment line in, with no code line added or removed.

## The re-check, which is the actual proof

```
rustfmt --edition 2021 --check crates/cowfs-meta/src/db.rs                  -> exit 0
rustfmt --edition 2021 --check crates/cowfs-meta/tests/snapshot_rename.rs  -> exit 0
```

Both re-check diffs are **0 bytes**.
That is a real formatter exit code on the two owned files at the workspace edition, and it is the whole of what it proves.

## Exact source carry

**Line 372 did not move.** The only test-file edit is at line 583, which is 211 lines after the assertion the red was pinned to, so `crates/cowfs-meta/tests/snapshot_rename.rs:372:5` still holds the same `assert_ne!` with the same operands and the same message, `the committed row must carry the flushed root, not the pre-flush one`.
I chose not to distort formatting to hold a line number, and in the event there was no conflict to resolve: the deviation rustfmt reported was not anywhere near line 372.
Git keeps the red pinned to its own parent commit, so the old failure stays addressable regardless, and the assertion identity is what carries forward.

**All test meaning is preserved.** No assertion was removed, weakened, reordered or retargeted. The file still holds ten tests. The existing dirty regression, `a_rename_commits_a_pending_tree_change_and_leaves_the_row_readable`, is untouched by this correction. The real `cowfs-store` byte-readback case and the forked-shared-root case are both intact with every one of their assertions, including the chunk-count check, the length-coverage check, the non-placeholder id check, `store.contains` per block, the same-block check, the full byte-for-byte comparison, and `check()`.

## Still owed to CI, and not claimed here

This change is **formatting and a comment**. rustfmt is not a compiler, a linter or a test runner, so everything that was pending at `c66befa` is still pending, and none of it is claimed:

1. **That the ten tests compile.** The two new cases are still the first in this repo to drive `cowfs_store` from a `cowfs-meta` integration test, and their API use is still verified by reading source, not by a compiler.
2. **That `cargo clippy --workspace --all-targets -- -D warnings` passes.** A formatter exit code says nothing about lints.
3. **That the root fix turns the pinned old failure into a pass.** No run has shown that.
4. **That the byte-readback case passes**, including that concatenating `store.get` over the chunk refs reproduces the body exactly.
5. **The missing-node and `check()` behaviour past line 372**, still unobserved in any run.

**No pass of any kind is claimed from this turn**, and no clippy, test or compile result is claimed.

One caveat on the proof itself: the local formatter is `1.10.0-stable` built `2026-09-28`, while CI installs its own `stable` toolchain.
Formatting is stable across those in practice but is not guaranteed identical, so a fmt difference on CI is possible and would be a toolchain skew, not a claim failure.

## Budget, unchanged and not waived

No cargo invocation of any kind: no build, no test, no clippy, no archive, no target directory, no offload, no cache deletion, no `ready-40` removal, no directory prune, no user-approval extension.
`bench/out` is unchanged at **20.862 GiB** against the 8 GiB cap, with the same 11.839 GiB floor, still unreachable without `bench/out/ready-40`, which holds another lane's mutation-golden source snapshots and receipts and stays protected.
Free space is not the gate. The only bytes added are the tiny check and re-check logs under `bench/out/meta42-snapshot-rename-format-correction/`.
Every binary, source archive, golden file, log, receipt and prebuilt test executable is intact.

## State

PR #137 stays a **draft** and stays **BLOCKED** until there is a real runtime pass on both platforms, a fresh independent review, and green CI.
Issue #42 is **open**, with `state_reason` `reopened`, so the history is not trusted on an empty reference list alone: an earlier commit in this repository's history closed it through a negated phrase and it was reopened, which is why the closing-keyword shape is avoided in this commit's own wording and why the state is asserted from the API rather than inferred from refs.
No new issue was created and nothing here completes request 1 or the issue: the API still has no consumer, `cowfs-core` still stages its own rename, and nothing outside `cowfs-meta` changes behaviour.
No browser verification was done. `no-mistakes` is uninitialized in this lane and was not initialized. Misakanet is local-only here and was not consulted.