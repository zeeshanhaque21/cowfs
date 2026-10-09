# Build train handoff, 2026-10-09

Written before a session restart that raises the subagent cap from 6 to 12.
Read this first, then progress/plan.json and docs/build-train-ideas-and-concerns-20261009.md.

## Standing rules from Zee for this train

- Keep every agent slot busy. Backfill the moment one frees.
- A PR merges only with green CI on the exact head plus a fresh-context critic with no BLOCK.
  Docs-only and test-only PRs may merge on green CI.
- Do not add scope. Collect ideas and concerns in docs/build-train-ideas-and-concerns-20261009.md.
- A pre-registered merge rule (for example the one on PR 222) is only waived by Zee.
- The destructive-command and first-write hooks demand the facts before git reset, checkout and first writes.
  State the files touched, the rollback and the user's instruction, then retry.

## Open PRs

- PR 222, changed-crate test selection with nextest filtersets.
  Head 4972f42, CI green.
  A round-2 critic was running when the session ended.
  Report goes to docs/reviews/222-critic-round2-20261009.md.
  Zee's rule: it merges only if it beats a same-window full run on wall-clock and macOS job time with nothing dropped.
- PR 220, Core swap recovery (issues 176 and 177).
  Remote head 9f8e27e is the version the round-2 critic BLOCKED.
  The BLOCK is a 200-character staging-name collision: two valid long targets share one staging snapshot.
  A builder was told to hash the full target into staging_name, require a trailing newline on intent records, and fix the PR body.
  If remote still shows 9f8e27e, the fix was not pushed and must be redone.
  Report is docs/reviews/220-critic-round2-20261009.md.

## Builder branches that were still running (check the remote before re-briefing)

- fix/nfs-mount-gate-43 (remote 5bd3cc2): verify and fix three reasoned NFS findings.
  The MOUNT EXPORT procedure leaks the export path, the one-shot mount gate re-admits a later MNT, and the connection cap evicts by request count.
  The review is docs/reviews/nfs-round3-security-43-20261009.md and nothing in it is test-verified.
- fix/fuse-torn-read-45-v2 (remote 778cce5): reproduce issue 45 on the cachyos box, then decide cache coherence, Core atomicity or oracle defect.
- Design-only memo for issue 173 power loss, to docs/crash-injection-173-power-loss-design-20261009.md.

## Decisions waiting for Zee

- Issue 123: approve a wire change so base_refresh can publish a built tree, and approve mount-after-create for the companion.
  Memo: docs/warm-base-123-decision-20261009.md.
- g3: add an accepted-established-regression class with sign-off so open/17.t number 2 (issue 204) can go green, or leave it FAIL.
- Issue 121: stays open, one CI sample returned wait_ms 1687 with no frame and no bytes-received log.
- Treehouse: max_trees is 16 and about 8 leases are stale.
  Audit and return finished ones, or raise max_trees, before running 12 agents.

## State of the gates

- g1 and g2: no gate result exists. Need a quiet-host macOS run and a cachyos run.
  Issue 232 tracks the remaining g2 noise rules.
- g3: Linux FUSE arm unmeasured. One established regression remains (issue 204).
- g4: passed on cachyos with a btrfs native arm (docs/reviews/g4-status-20261009.md).
- g5: blocked on xfstests build (issue 101).
- g6: slices 1 and 2 merged. Power-loss slice is being designed.

## Housekeeping to finish

- Local main equals origin/main except the uncommitted tracker edits (progress/*).
  Old edits to docs/v1-core.md and docs/v1-meta.md are in git stash pre-main-update-2.
- Untracked docs in the primary checkout that are on no branch: this file, the ideas file, docs/gc-upgrade-review-handoff.md, several docs/reviews files from returned worktrees, docs/warm-base-123-decision-20261009.md.
  Commit them with a docs-only PR.
- Branch backup/main-9874afa preserves an old local commit.
- Stale local branch ci/changed-tests-nextest2 is empty and safe to delete.
- Scratch dir /tmp/n218r can be removed.
- The live cowfs daemon pid 15263 belongs to Zee. Never touch it.
- Run detect_changes on codebase-memory after the merges.
