# Issue 171: which acceptance rows are met on main, and what remains

Date: 2026-10-09.
Tree checked: `origin/main` at `fb64023efd6beaa53692a33019e24b7dc587f9be`.
Source of the rows: the issue 171 body, taken from `docs/reviews/issue17-status-20261008.md`.

## Verdict

Four of the five rows are met on current main, each with a runtime result.
One row, older kernels, is unmet and needs a decision from Zee before any code.
So the issue should stay open, or be narrowed to the kernel row, rather than closed.
No code changed for this pass, so there is no PR.

## Row 1: CI runs the isolation tests

Met.
PR #172 (`b567508`) added the `linux-namespaces` job, and `ef5b264` made the required `check` job depend on it.
The job is `.github/workflows/ci.yml:121-148`: it sets `kernel.apparmor_restrict_unprivileged_userns=0`, runs `bench/test_namespaces.py`, then fails unless all 9 Isolation tests passed and the only skip is `test_off_linux_is_unmeasurable`.
Runtime proof: CI run 37922319309 on main at `fb64023`, job `linux-namespaces` (113792940511), concluded success with `Ran 17 tests` and `OK (skipped=1)`, and its gate step passed.

## Row 2: a cargo-level canonical build

Met for the measured scope.
Zee decided on 2026-10-08 that the byte-identical promise covers cargo builds.
PR #180 (`bbdc9f5`) makes the canonical route set `CARGO_INCREMENTAL=0`: `crates/cowfs-treehouse/src/mode_b.rs:638-646`.
The regression test is `the_canonical_build_command_sets_cargo_incremental_off` in `crates/cowfs-treehouse/tests/canonical.rs:642`.
Measurement: `docs/verification/evidence/cargo171.md`, and 6 of 6 identical builds on btrfs, ext4 and XFS in `docs/verification/evidence/namespaces171-matrix.md`.
The control with `CARGO_INCREMENTAL=1` differed in 81 paths every time, so the identity is not a harness artefact.

## Row 3: core backend `base_refresh`, re-verify after PR #163

Met, re-verified today on the cachyos box.
PR #163 merged on 2026-10-09 at 00:00 UTC.
The earlier lease run used the path backend only, because the core refused `base_refresh` then.

Run:

- Host: cachyos, kernel 7.2.8-2-cachyos, unprivileged, scratch `/mnt/docs/Projects/cowfs-ns171`.
- Tree: `git archive origin/main` at `fb64023`, built with `cargo build -p cowfs-cli -p cowfs-treehouse -p cowfs-daemon`.
- Script: `docs/verification/evidence/namespaces171/lease171.sh` with only two changes, the workspace paths and `--backend "$BACKEND"` in place of `--backend path`, run as `lease171b.sh <out> 3 core`.
- The store held `meta.redb` and `store`, the core layout; the path backend keeps one directory per snapshot instead.
- Treehouse: the v3.1.2 release binary already checksum-verified in the matrix run.

Result:

- 3 companion-driven real leases, `cowfs-treehouse base refresh --build 'cargo build --workspace' --canonical C --ns-helper scripts/cowfs-ns-run.sh` with no `--slot`, all exited 0 with `"built_in_slot": true`.
- Refresh 1 reported `previous_commit: null`.
  Refreshes 2 and 3 reported `previous_commit` equal to the commit, so the replace path that PR #163 added ran twice, not only a first ingest.
- `base status` afterwards: `snapshot repo-48b32e-base`, `base_commit` equal to `head_commit` (`652f4ff`), `fresh: true`.
- Each leased `target` had 43 files, and the 3 manifests were identical: 1 distinct manifest of 3, 0 differing paths.
- The native control lease, built at its own path without the helper, differed in the same 15 path-embedding files as on the path backend.
- Host mounts other than the daemon's own were unchanged, the canonical directory was empty before and after, and the FUSE mount was gone after shutdown.
- The 4 slots were returned with `treehouse return --force` under the run's own root, and `treehouse status` then showed all 4 available.

## Row 4: a real treehouse lease run on Linux

Met by PR #182 (`29f6e9a`), see `docs/verification/evidence/namespaces171-matrix.md`, section "Row 3: a real treehouse lease".
Today's core run above is a second real lease run, on the core backend.

## Row 5: btrfs, XFS and older kernels

btrfs and XFS: met by PR #182, 17 tests ran and passed on btrfs, ext4 and XFS, with 6 of 6 identical cargo builds on each.
Older kernels: unmet.
The only kernels measured are 6.12 (moonscape) and 7.2.8 (cachyos).
The cachyos box has one kernel and no root for this work, so it cannot boot an older one.

## Decision needed from Zee: older kernels

1. Pick a floor.
   The issue does not say how old a kernel must be.
2. The bounded way to get one is a second hosted-runner image in the `linux-namespaces` job, for example `ubuntu-22.04`, as a matrix entry.
   Probe first, on that image: `uname -r`, whether `kernel.apparmor_restrict_unprivileged_userns` exists, and whether `unshare -Ur -m true` works without the sysctl step.
   The current job's `sudo sysctl -w` line would fail on an image without that key, so it would need to tolerate a missing key.
   The kernel version of that image is not stated here because it was not measured.
3. `.github/workflows/ci.yml` is being edited by the PR 222 agent today, so this change was not made here to avoid a conflicting edit.
4. Anything older than the hosted images needs a VM with a chosen kernel, which needs root or a new host.

## Not covered, and not an issue 171 row

- A leased slot that is itself a cowfs snapshot on the FUSE mount; today's slots, like the matrix run's, were plain treehouse worktrees on btrfs.
- Release builds, registry dependencies, incremental rebuilds over an existing `target`, and N above 6.
- The `cargo171.md` label that calls `/mnt/docs` ext4; on cachyos it is btrfs, as the matrix doc already reports.

## Process notes

- The treehouse worktree lease for this task did not complete in 8 minutes.
  Several `treehouse status` and `get` calls from other agents were hung at the same time, and an `ls` under `~/.cowfs/mnt/base/.treehouse/` was also hung, which points at the pool's cowfs mount rather than at treehouse.
  This was not investigated further, and the lease request was stopped; no branch or slot was created.
- Box scratch `/mnt/docs/Projects/cowfs-ns171` was removed after the evidence was copied off.
  No other agent's processes or directories were touched.
