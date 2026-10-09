# Build train: ideas and concerns, 2026-10-09

Collected while driving the build train.
Nothing here is scheduled work.
Each item is a note for Zee to accept, file as an issue, or drop.
No scope was added to any running builder because of this list.

## Concerns: correctness and gates

- g3 is not closed.
  The Linux FUSE arm is still unmeasured, so the macOS result alone cannot close the gate.
- g1 and g2 have produced no gate result at all.
  Every number so far is labelled NOT A GATE RESULT.
  A quiet-host run on macOS and on cachyos is still required.
- The Linux g2 bar is not settled.
  Re-pinning moves it, but the added-seconds question returns if the new edit is still fast natively.
- g4 passed on a btrfs native arm.
  collapse-range and insert-range are disabled on both arms there, so an ext4 arm needs root and is untested.
- The fsx matrix does not compare st_blocks.
  A zero-range drops cowfs st_blocks from 128 to 64 while btrfs stays at 128, and nothing in the gate can see it.
- PathVfs mknod still has a residual race (#211).
  A swap to another special node between mknodat and the chmod lands the chmod on that node.
  Closing it needs an fd-based chmod with a device and inode check.
- devices_need_privilege decides privilege from the owner of /proc/self.
  It is wrong in a user namespace with uid 0 mapped but no CAP_MKNOD.
  A CAP_MKNOD probe would be the correct signal.
- The root branch of special_files_through_mknod has never run in CI.
  It needs a root runner.
- The Core model Entry has no rdev, so Core losing a device number is invisible to that model.

## Concerns: process and tooling

- PR 175 (nextest sharding) merged although the measured gain was inside noise.
  Zee made that call, so it is recorded as a decision, not an error.
  The macOS job is still the long pole at about 1100 s.
- The repo has no branch protection.
  Nothing mechanically gates a merge, so the check aggregate is the only gate and must be complete.
  PR 213 adds linux-fuse to it.
- Several builders skipped or could not run ship-aftercare and the no-mistakes step.
  no-mistakes is not initialised in this repo, so that rule cannot be met as written.
- The treehouse pool filled to 16 of 16 with leases from earlier sessions.
  Most were finished work with merged PRs.
  Leases need an owner and an expiry, or a periodic sweep that returns clean merged ones.
- treehouse double-leased one slot to two builders once.
  One builder committed onto another builder's branch before noticing.
  Builders now verify branch and cleanliness after leasing.
- treehouse return prompts for confirmation and fails with no stdin.
  Agents must pass --force, which is only safe after checking the contents.
- treehouse status from a nested pool directory shows the same global list, so finding which lease is mine needs a branch search.
- In zsh a variable named path is tied to PATH.
  A loop using it broke every command in the shell.
  Scripts run by agents should avoid that name.
- The destructive-command and first-write hooks fire repeatedly even after the facts are presented.
  They cost several round trips and pushed some work onto less direct commands.
- A background agent kept reporting that it could not tell which lease was its.
  It held none, and it needed an explicit stop.
  Agent prompts should say up front whether a lease is expected.

## Concerns: repository hygiene

- progress/plan.json carries a diff of about 1500 lines that is not committed on main.
  The tracker is the lead's working file, so losing it would lose the decision log.
  It is on the PR 206 branch, but main does not have it.
- docs/reviews holds a large pile of critic reports.
  PR 206 is the vehicle, and it is still open.
- docs/v1-core.md and docs/v1-meta.md edits from the primary checkout were moved to a stash.
  They were written against an older base and conflicted with origin.
  The stash is named pre-main-update-2.
- Seven review and handoff docs existed only inside returned worktrees.
  They were copied into the primary checkout as untracked files and are not on any branch.
- Local main had an unpushed commit that was only a docs evidence file.
  It is preserved on backup/main-9874afa and its content is on the PR 206 branch.

## Concerns: follow-ups the critics raised and nobody filed

- PR 205: the fallocate_modes_reach_the_kernel SKIP path passes silently on CI and should panic when CI is set.
- PR 205: the len and range checks should run before open_rw.
- PR 205: the symlink and macOS branches of fallocate have no unit test.
- PR 208: the g2 corpus file drifts by one comment line per rep.
- PR 208: compare.py does not check that cowfs_ctl is in rebuilt_units.
- PR 210: hash_tree is unchanged by special files and Bad in runner.rs does not forward mknod.
  The critic judged both as not correctness gaps.

## Ideas, not scheduled

- Run only the tests affected by a PR using nextest rdeps filtersets.
  Feasibility numbers are in the tracker item ci-changed-tests.
  It is queued behind PR 213 and nothing has been built.
- Make the g3 accepted-divergence list the single place where known client limits live.
  Then #108, #109 and #204 each have one entry with evidence, and the harness cannot drift from the docs.
- Add a small script that audits every treehouse lease against its PR and branch state and prints which are safe to return.
  This replaces the manual audit done today.
- Commit a standing fallocate mode for the fsx matrix, and allow a per-host fsx digest.
- Rotate CACHY_OS_PASS, which the old pause note still lists.

## Concerns added later in the run

- Critics blocked two PRs that CI had passed.
  PR 222 (changed-crate test selection) would have skipped the cowfs-treehouse tests when cli, daemon or core changed.
  PR 220 (swap recovery) had two data-loss paths: a target ending in .tmp, and two long targets sharing a 200-character staging name.
  CI green is not evidence for this class of change, so a fresh-context critic stays mandatory for core and CI changes.
- Tests that pass on the builder's own Mac can fail on the GitHub macOS runner.
  The two PR 230 shutdown tests passed 100 of 100 locally and failed on CI because they depended on host socket-buffer sizes.
  Timing and socket tests need explicit buffer sizes or the server's own counters, and one CI run before declaring done.
- Issue 121 is still unexplained.
  One CI sample returned wait_ms=1687, about 110 ms before the grace end, with no terminal frame.
  The bytes received were not logged, so an early kill by the server is not excluded.
- The changed-tests selector (PR 222) is only as complete as its model of non-cargo runtime edges.
  A binary reached through a PATH lookup or a new helper name would still be missed by the scan guard.
  Each trial arm was a single sample on shared runners with a large macOS queue wait.
- The g3 PATH_MAX overlay (PR 233) edits a copy of the suite's misc.sh for both arms.
  It must stay restricted to the exact known client condition, or it could hide a real pathconf regression on the Linux arm.
  g3 will still be FAIL because of the one established regression (open/17.t #2, issue 204).
  Whether to add an accepted-established-regression class with explicit sign-off is Zee's decision.
- A read-only security review of the NFS adapter was cut off by a safety classifier before it wrote any test.
  Its two likely BLOCK findings (the MOUNT EXPORT procedure leaks the export path, and the one-shot mount gate re-admits a later MNT) are reasoned only until the hardening builder reproduces them.
- Issue 123 needs two decisions from Zee: a wire change so base_refresh can publish a built tree, and whether the companion mounts snapshots itself.
  The memo is docs/warm-base-123-decision-20261009.md.
- The daemon serialises base_refresh per name, but promote takes a different lock.
  A same-name promote racing a base_refresh is not prevented.
  This comes from reading the code and is not tested.
- Agent slots are capped at 6 concurrent subagents.
  Resumed builders count against the cap, so backfilling a slot sometimes fails and has to wait for the next completion.
- Several PRs were reported as "waiting for CI" by builders whose CI later failed on a different job.
  The lead has to re-read CI on the exact head before every merge, not trust the builder's last message.

## Where each PR stands

- Merged on instruction: 175, 206, 207, 209, 212, 219.
- Merged after a critic: 201, 203, 205, 208, 210, 213, 214, 215, 216, 217, 229, 231.
- Merged with only a green CI, test-only: 230.
- Waiting on a re-critic or on fixes: 220, 222, 233.
- Still building: the NFS mount-gate hardening (issue 43) and the FUSE torn-read investigation (issue 45).
