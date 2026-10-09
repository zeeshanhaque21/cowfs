# Decision memo: what is left of issue 123 and issue 98 (2026-10-09)

Audited against `origin/main` at 254d0d4 (PR 231 merged).
No code was written, because the remainder needs a contract decision that is Zee's.

## Status of the acceptance rows

Issue 98 (provenance persisted and discoverable):

| Row | Status | Evidence |
|---|---|---|
| Provenance (repo, ref, commit) persisted durably through the Snapshots seam | done | `Snapshots::set_base_meta` and `create_meta` in `crates/cowfs-daemon/src/backend.rs` (trait at line 48 and 51, Core impl at 881 and 888, Path impl at 1176 and 1183), backed by `BaseMetaStore` |
| Report says fresh only if a later reader agrees | done | `import::base_refresh` re-reads with `create_meta` before returning (`import.rs` near line 349) |
| Both backends, fresh daemon reopen | done | daemon test `the_core_publishes_a_warm_base_by_ingesting_the_checkout` reopens the store and reads the commit back (`import.rs:837`) |
| Refused duplicate create does not destroy a record | done | `a_refused_duplicate_create_keeps_a_core_snapshot_and_its_whole_record` (`backend.rs:1750`) and the Path twin (`backend.rs:2109`) |
| Swap keeps or clears provenance honestly | done | `a_refused_swap_keeps_the_full_old_tree_and_record_on_both_backends` (`backend.rs:1419`), `crates/cowfs-daemon/tests/swap_provenance_124.rs` |
| Refuse unrecorded replace, per-name in-flight guard, name limit | done | PR 231 |
| Fresh slots cloning the published base | done | ignored acceptance `warm_base_acceptance_over_a_real_core` (`real_project_acceptance.rs:2968`) |

Issue 98 has no remaining acceptance row that I can find.
Recommendation: close 98 with a pointer to PR 134, PR 163 and PR 231.
I did not close it, because that is a status call for Zee.

Issue 123 (tree-native Core publication for the companion):

| Row | Status | Evidence |
|---|---|---|
| Tree-native publication through existing import, promote and fork seams, no PathVfs adoption | done | `Backend::ingest_replacing` (`backend.rs:119`, Core impl 616), `import::replace_tree` (`import.rs:431`), `Handler::base_refresh` no longer calls `can_ingest`; design in `docs/warm-base-publication-123.md` |
| Caller-reachable repository, ref and commit provenance | done | gate `the_core_daemon_publishes_base_refresh_and_leaves_no_worktree` (`real_project_acceptance.rs:1353`) |
| Correct base tree, two fresh slots, writable isolation, reset to unchanged base, fresh-daemon readback | done for a source-only tree | `warm_base_acceptance_over_a_real_core`, which is `#[ignore]`d for time (about 17 minutes) and passed in issue 123 |
| Slots include build artifacts (a warm `target/`) | NOT done | `BaseRefresh::run` (`crates/cowfs-treehouse/src/mode_b.rs:~517`) runs `--build` in a leased slot and then calls `base_refresh`, which checks out a fresh worktree of the ref, so the built tree is never published |
| Companion invokes `mount_snapshot` | NOT done | `CowfsMaterialiser` always refuses (`mode_b.rs:29`), pinned by `the_companion_never_calls_the_mount_snapshot_the_daemon_provides` (`real_project_acceptance.rs:1606`) |
| #97 repair | done | worktree staging path from #97 is in main and used by `base_refresh` |
| Performance claim | out of scope by the issue | 15 and 16 stay open |

## Why this stops here

Both remaining rows change the companion contract, and one changes the wire.

### Remainder A: a warm `target/` in the Core base

The Core stores trees, so the built slot tree can only reach the base by being ingested.
Three options:

1. Add an optional source to `base_refresh` (for example `from_slot` or `from_path`) so the daemon ingests that directory instead of a fresh checkout, and still records the real commit of `git_ref`.
   Cost: wire change in `cowfs-ctl` (`BaseRefreshParams`, golden `wire.tsv`, `docs/v1-control-api.md`), plus a daemon rule that the path is a worktree of `repo` at that commit, otherwise provenance is a lie.
   Benefit: one atomic call, one provenance record, the existing replace and recovery code is reused as is.
2. No wire change: the companion calls `import <built slot> --name <base>` and then `snapshot_promote`.
   Cost: `import` records no repo, ref or commit (`mode_b.rs:203` says a promoted base carries all-null fields), so `base status` cannot say fresh and `find_base` cannot validate it.
   That reintroduces the exact defect issue 98 closed, so I do not recommend it.
3. Keep the base source-only and warm `target/` lazily in the first slot.
   Cost: the first slot per pool pays the full build, which is the cost the feature exists to remove, and issue 123 explicitly asks for artifacts.

Recommendation: option 1, with the daemon verifying that the source directory is a git worktree whose `HEAD` equals the resolved commit and which has no uncommitted tracked changes.
The build would still be the companion's job, and a failed build must leave the old base untouched, which the existing replace path already guarantees.
Open question for Zee: whether `target/` of a build done at the slot path is relocatable (absolute paths in fingerprints), because the base is later mounted at different slot paths.
`--canonical` exists for exactly this, so the answer may be "only supported with `--canonical`".
This is unverified and needs a real measurement before any claim.

### Remainder B: companion calls `mount_snapshot`

This needs no wire change (`mount_snapshot` and `unmount_snapshot` exist), but it is not a one-line swap.
Facts from the code, read not run:

- `Provision::run` calls `materialiser.materialise` before `daemon.snapshot_create` (`mode_b.rs:~163`), but `mount_snapshot` needs the snapshot to exist, so the order must flip.
- The `Materialiser` trait has no daemon handle, so the trait or the call site changes.
- `mount_snapshot` is gated by the daemon's export roots and holder check, so the slot path must sit under `--export-root`.
- Treehouse creates the slot as a populated git worktree, so mounting over it shadows its files, and the `.git` indirection rewrite (gap 7) then runs against the mounted tree.
  Whether that composes with `treehouse return` and `unmount_snapshot` ordering is the real design question.
- Someone must own unmount at return, otherwise exports leak.

Options: (1) companion mounts after create and unmounts in its own return hook, recommended because it keeps the daemon dumb; (2) daemon auto-mounts on `snapshot_create --at`, which is a wire change; (3) leave mounting to the caller as today and only fix the stale error text.

## Recommendation and costs

1. Close issue 98 now.
2. Split issue 123 into two issues, one per remainder, and keep 123 as the tracker.
3. Take remainder A option 1 and remainder B option 1.
   Cost estimate: A is one wire field, one daemon check, one companion change and an extended ignored acceptance (another 17 minute run).
   B is one trait change, one reorder, one unmount hook and a real-mount test.
4. Do not change the on-disk format; neither remainder needs it.

No branch was pushed and no PR was opened.
