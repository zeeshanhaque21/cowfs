# Tree-native warm-base publication on the Core backend (issue 123)

## Problem

`base_refresh` on the Core backend was refused by `can_ingest` with `unsupported: this backend stores snapshots as trees`.
The refusal is intentional for Path-style directory adoption.
The passthrough backend copies a directory into its store, and the Core does not store directories.
It is not a reason the Core cannot publish a warm base.
`import` already reaches the Core through `Backend::ingest`, which writes a source tree through the Core writer, verifies it, and only then makes the name visible.
The refusal blocked `warm_base_acceptance_over_a_real_core` before any publication.
The docs folder has no `specs/` directory, so this follows the existing flat `docs/<topic>.md` convention.

## Contract

`base_refresh {repo, git_ref, name?}` on the Core publishes a warm base by tree-native import.
The Core never adopts a PathVfs directory.
The request and report shapes do not change.

1. Resolve the commit for `git_ref` in `repo`.
   An unreadable ref is `not_found`.
2. Check the ref out into a daemon-owned staging worktree beside the repository (`staging_path` and `git worktree add --detach`, repaired by issue 97).
3. Ingest that checkout through `Backend::ingest` into the staging snapshot `cowfs-import-<name>`.
   The Core verifies the tree byte for byte before the name is visible.
   The name has no leading dot because the Core refuses one.
4. Install it under `<name>`.
   A free name is a `rename`.
   A taken name goes through the Core's crash-safe `swap` (intent record), and the staging snapshot is removed afterwards.
5. Promote `<name>` as a base, then persist provenance (`repo`, `git_ref`, real `commit`) with `set_base_meta` (issue 98).
6. Re-read the record with `create_meta` and return it, so the report says fresh only if a later reader of the store agrees.
7. Notify the daemon that the snapshot set changed, as `import` does.
8. Remove the staging worktree on every path.

The published base is a source-only tree of the checkout.
Slots are made with the existing `snapshot create --from <base>` fork and `snapshot reset`, unchanged.
A warm `target/` is not part of this slice (see the remainder).

## Existing seams reused

- `import::base_refresh`: commit resolution, staging path, worktree add and cleanup (issue 97).
- `Backend::ingest`, the Core import path, in place of `copy_tree`.
- `Snapshots::{rename, swap, remove, promote, set_base_meta, create_meta}` and the exclusive base-record section behind them (issue 98).
- `snapshot create --from`, `snapshot reset` and `mount_snapshot` for slots.
- The real-project acceptance test as the end-to-end proof.

The change is the Core branch of `import::replace` (`replace_tree`), and `Handler::base_refresh` no longer calls `can_ingest`.
`can_ingest` stays on the `import` fallback, where it still refuses a backend with no writer.

## Failure modes

- Ref or repo missing: `not_found`, nothing created.
- `git worktree add` fails: a half-made directory is removed if git does not know it, nothing else changes.
- Ingest fails or is cancelled: the Core never made the staging name visible, and the old base is untouched.
- Swap or rename fails: the staging snapshot is removed and the error is returned.
- Crash between ingest and install: a stale `cowfs-import-<name>` snapshot remains.
  The next refresh of the same name clears it first.
- Crash after install and before promote or provenance: the base exists but reports unknown rather than fresh (issue 98), and a retry converges.
- Provenance write fails: the refresh fails instead of reporting a base that cannot be found again.
- A user snapshot named `cowfs-import-<name>` is removed by a refresh of `<name>`, because the Core staging name lives in the user namespace (the passthrough dot-prefix name could not collide).
- Replacing a base that has forked slots or live holders is not exercised here and is unverified.
  The swap is the Core's own seam, and this slice adds no claim about it.

## Out of scope and remainder

- Performance gates and any measured-speedup claim.
  Issues 15 and 16 stay open until measured acceptance.
- A warm `target/` in the Core base (the companion `--build` path publishing a built slot tree).
  The issue's "including build artifacts" is not met by this slice.
- Companion-side automatic `mount_snapshot` after `get`.
  The acceptance test calls it directly.
- Per-slot `.git` indirection (gap 7 in `docs/v1-treehouse.md`).
- Shared daemon deployment, held-pool mutation, and any wire or protocol change.
- Directory adoption through the control plane on the Core.

## Proof

- Daemon test `import::tests::the_core_publishes_a_warm_base_by_ingesting_the_checkout`: publish, replace, no staging left, reopen the store and read the commit back.
- Ignored acceptance `warm_base_acceptance_over_a_real_core`: two fresh slots cloned from the published base, real `cargo build` and `cargo test` in each over an NFS mount, reset to a byte-identical base, base intact.
