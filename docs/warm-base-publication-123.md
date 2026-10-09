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
3. Ingest that checkout through `Backend::ingest` (free name) or `Backend::ingest_replacing` (taken name), which stage under the Core's reserved hidden name `<name>.cowfs-swap0` and verify byte for byte.
   No snapshot a user can see or create is made or deleted.
4. For a taken name, the old base's record is cleared first and stays cleared.
   Then the swap's intent record is written, the old tree is removed, and the staged tree is forked into place (`replace_with_staged`, rolled forward by `Core::open` after a crash).
5. Promote `<name>` as a base, then persist provenance (`repo`, `git_ref`, real `commit`) with `set_base_meta` (issue 98).
6. Re-read the record with `create_meta` and return it, so the report says fresh only if a later reader of the store agrees.
7. Notify the daemon that the snapshot set changed, as `import` does.
8. Remove the staging worktree on every path.

The published base is a source-only tree of the checkout.
Slots are made with the existing `snapshot create --from <base>` fork and `snapshot reset`, unchanged.
A warm `target/` is not part of this slice (see the remainder).

## Existing seams reused

- `import::base_refresh`: commit resolution, staging path, worktree add and cleanup (issue 97).
- `Backend::ingest` and the Core's hidden staging name plus `finish_swap`, in place of `copy_tree`.
- `Snapshots::{promote, set_base_meta, create_meta}` and the exclusive base-record section behind them (issue 98).
- `snapshot create --from`, `snapshot reset` and `mount_snapshot` for slots.
- The real-project acceptance test as the end-to-end proof.

The change is `cowfs_core::ingest_replacing`, `Backend::ingest_replacing`, the Core branch of `import::replace` (`replace_tree`), and `Handler::base_refresh` no longer calls `can_ingest`.
`can_ingest` stays on the `import` fallback, where it still refuses a backend with no writer.

## Failure modes

- Ref or repo missing: `not_found`, nothing created.
- `git worktree add` fails: a half-made directory is removed if git does not know it, nothing else changes.
- Ingest is cancelled or refused: nothing visible changed.
  The old tree is untouched and its record is restored, so the old base still reports its commit.
- Any other ingest error on a replacement: the old record stays cleared and the base reports stale, because the error cannot say whether the swap happened.
- Crash during staging: only the hidden staging snapshot remains, which `Core::open` does not clear (it only recovers intent files); the next ingest or swap of that name does.
- Crash after the intent record is written: `Core::open` rolls the replacement forward.
- Crash after install and before promote or provenance: the base exists but reports unknown rather than fresh (issue 98), and a retry converges.
- Provenance write fails: the refresh fails instead of reporting a base that cannot be found again.
- A user snapshot named `cowfs-import-<name>` (or a case variant) is never touched, because staging is not in the user namespace.
- A name so long that the swap's `swap-<name>` intent file exceeds the filesystem limit publishes the first time.
  Replacing it fails cleanly with the old tree intact and its provenance cleared, so it reports stale, which is the Core's own limit.
- A failure after the old tree is removed keeps the staged tree and the intent file, so `Core::open` rolls the replacement forward.
  There is no fault-injection test for that step.
  A same-name retry unregisters the kept staged tree first, so a second failure then loses both trees (issue filed, not fixed here).
- A refresh of a name that is a plain user snapshot, not a base, replaces it, as the passthrough backend does.
  Callers pick the name, so the contract is that `base_refresh` owns the name it is given.
  Refusing an unrecorded existing name unless the caller asks to replace it is a product decision (issue filed).
- Two concurrent refreshes of one name are not serialised (issue 167, unverified).
  The replacing ingest also runs outside the bases lock, so a snapshot created under `<name>` during the ingest would be replaced as the victim.
- Replacing a base that has forked slots or live holders is not exercised here and is unverified.

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
- Daemon test `import::tests::a_user_snapshot_named_like_staging_survives_every_core_refresh`: user snapshots named like the old staging name survive publish, replace and a cancelled ingest, which also keeps the old base and commit.
- Daemon test `import::tests::a_base_name_at_the_length_limit_publishes_and_never_leaves_partial_state_on_the_core`.
- Gate `the_core_daemon_publishes_base_refresh_and_leaves_no_worktree`: exit 0, the real commit, `base status` fresh, no worktree left, and a loud failure on a host that cannot mount.
- Ignored acceptance `warm_base_acceptance_over_a_real_core`: two fresh slots cloned from the published base, real `cargo build` and `cargo test` in each over an NFS mount, reset to a byte-identical base, base intact.
