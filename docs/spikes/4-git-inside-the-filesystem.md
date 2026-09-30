# Spike 4: `.git` inside the filesystem

Issue: #4.
Scripts: `spikes/nfs-loopback/spike4/`.
Raw data: `spikes/nfs-loopback/out/spike4/results.json` (git-ignored).
Run date: 2026-09-29, macOS NFS loopback with the tuned passthrough server from spike 2.

## Result

Putting `.git` on the mount costs almost nothing for read-only stat workloads and 3.9x for index and object writes.
A native `.git` overlay fixes most of that write cost, but not the working-tree cost.
Checkout, worktree add and cold status are 9x to 11x native in both layouts, so those come from working-tree file operations and not from `.git` placement.
The spike also found a write race in the passthrough server that dropped writes to read-only-mode files, which I have reproduced and fixed (see Server bugs).

## Setup

- Repo: `ai-tools/OmniRoute`, 12,591 tracked files, 805 MiB pack.
  Each layout also had 5,000 ignored files added as `node_modules`.
- Layouts, run interleaved in rotating order:
  - N: everything native.
  - M: working tree and `.git` both on the NFS mount (the design default).
  - O: working tree on the mount, `.git` native via `--separate-git-dir` (the overlay fallback).
- The server was a copy of the tuned binary, md5 `873006a9...`, on its own port.
  Timed batches held the shared CPU lock.
- Ratios are median divided by the N median, n=5 unless stated.
  Machine load was 15 to 62 for most cells, and the summarizer excluded any run with load1 above 30.
  During the gc, repack and pack-objects loops load reached 150 to 330, and those loops were used for correctness only.

## Timings (ratio to native)

| Workload | N seconds | M | O |
|---|---|---|---|
| `git status`, warm | 0.082 | 1.34x | 1.32x |
| `status` after `update-index --really-refresh` | 0.080 | 1.35x | 1.30x |
| `update-index --really-refresh` alone | 0.024 | 2.5x | 2.5x |
| `status --ignored` | 0.109 | 1.20x | 1.12x |
| `log --oneline -n 2000` | 0.036 | 1.10x | 1.21x |
| `diff --stat` between commits, 9,885 files (n=4 and 3) | 1.13 | 1.03x | 1.02x |
| `diff --stat` on a clean tree | 0.022 | 2.6x | 2.6x |
| `status` cold (after a server restart) | 0.136 | 6.6x | 7.0x |
| Modify 200 files | 0.008 | 5.5x | 6.2x |
| `add -A` after touching 200 files | 0.199 | 3.9x | 1.1x |
| `commit` | 0.053 | 3.9x | 1.7x |
| `checkout`, forward, 9,885 files | 1.35 | 9.3x | 8.8x |
| `checkout`, back | 1.70 | 11.2x | 10.3x |
| `worktree add` | 1.59 | 11.5x | 10.5x |
| `worktree remove` | 0.90 | 1.7x | 1.6x |
| Status in the new worktree, first / warm | 0.43 / 0.10 | 1.42x / 1.31x | 1.40x / 1.10x |

I checked these ratios against the raw JSON and they match.
The worktree and diffstat cells have n=3 to 4 because several runs were excluded for load.

- Read-only stat workloads (status, log, diff between commits) are 1.0x to 1.35x in both layouts.
- Index and object writes are where `.git` placement matters: the overlay takes `add -A` from 3.9x to 1.1x and `commit` from 3.9x to 1.7x.
- Checkout, worktree add and cold status cost the same in M and O.
- Git settings: the untracked cache cuts M status from 0.112s to 0.082s but native improves more, so the ratio worsens to 2.45x.
  `preloadindex`, `checkStat=minimal`, `feature.manyFiles` and `splitIndex` gave no gain within noise.
  The fsmonitor daemon refuses a network mount.

## Correctness

- `git fsck --full` was clean on N, M and O, and HEAD, tree and `status --porcelain` matched across layouts on a toy repo and on OmniRoute.
- 20 repeated commits gave equal HEADs on every layout.
- `kill -9` during `add` and during `commit` (5 runs each per layout, test processes only): fsck and status stayed clean on every layout.
  Every layout, native included, left a stale `index.lock` and `tmp_obj_*` files, and the checker removed the lock before re-running, so the re-run success only shows recovery after lock cleanup.
- A stress loop of 200 worktree commits plus 100 same-tree commits (n=3) left no stale locks and fsck was clean everywhere.
  Lock-contention failures happened on every layout, native included.
  The stress timings were flagged by load and are not reported.
- `git worktree add` from a main repo on the mount works.
- `git gc` and `git repack -adf` on M never completed cleanly on the unfixed server, so the "gc plus fsck clean on M" requirement was not met on that run.
  After the write-race fix below, a small test repo (44 MiB of incompressible objects) passed `repack -adf` 8 of 8, `gc`, and `fsck --full` before and after.
  The large OmniRoute repo was not re-run.

## Server bugs found

1. **Writes to read-only-mode files failed, now fixed.**
   Files created with mode 0444 or 0400 failed with EACCES at `fsync` about half the time on 8 MiB writes (agent: 14 of 20).
   Git writes every pack and idx as 0444, and `git pack-objects` failed 8 of 8 on M (rc 128, "sha1 file write error"), loudly and never with silent corruption in that run.
   I reproduced it independently: 6 of 20 for 0444 and 9 of 20 for 0400 failed, 0 of 20 for 0644 and 0 of 20 for a native 0444 control.
   Cause, hypothesised by the agent and confirmed by my fix: `open_for_write` flipped permissions to writable, opened, then restored them, with no lock.
   Parallel NFS WRITE requests raced, so one could restore the mode between another's flip and open (EACCES), or read the already-flipped mode as the original (mode drift).
   Fix: serialize that path with a mutex and retry a plain open first.
   After the fix, 0 of 80 creates failed (0444 and 0400, 40 each), the final modes stayed 0444 and 0400, the 25-check battery passed (a read-only-mode check was added), and `git repack -adf` passed 8 of 8 on a test repo.
   This is a passthrough artifact: a real cowfs backend stores modes as metadata and does not need the hack.
   The requirement stays: the server must let the file's owner write a file created with a read-only mode.
2. **One corrupt pack index on M, unexplained.**
   After a gc and repack retry loop, one `.idx` failed its own SHA-1 trailer, with 101 zeroed runs at offset 180,224 and above.
   The `.pack` was valid, and the bytes were identical on the mount and in the backing store.
   `fsck` exited 14 with many broken links.
   The agent repaired it with `git index-pack`.
   It was not reproduced: 8 `pack-objects` runs all failed loudly.
   The exit code of the run that produced it was lost.
   The race in bug 1 could plausibly drop WRITE requests, but that is not confirmed as the cause.
   Treat it as unexplained data corruption until a long soak with the fixed server shows none.
3. **Readdir cookie is still position-based.**
   An incremental scan-and-unlink loop left 1,225 of 2,500 entries on the mount (native 0).
   I reproduced it: 1,186 of 2,500 left on the mount, 0 native.
   `rm -rf` and `shutil.rmtree` still worked in my run because they list the directory first.
   The agent reports `git worktree remove` failing on M and O with "Directory not empty" in 5 of 5 runs, but my worktree measurement above shows `worktree remove` completing, so I did not confirm that failure.
4. **AppleDouble sidecars pile up in the backing store**: 32,496 on M and 19,948 on O, after utime, chmod and `O_EXCL` creates.
   `git fsck` run against the backing directory sees them as bad refs.

## Harness note

`git clone --no-hardlinks` copies the source's commit-graph chain, and a `repack -ad` then `fsck` fails on that stale graph even on a native control.
The chains were deleted from the test repos and that check was dropped.

## Overlay decision

- The overlay is worth adopting only if index and object write latency stays over budget in the real backend, and it does not fix checkout, worktree add or cold status.
- It removes exposure to bugs 1 and 2, but those are server bugs and are fixed or tracked regardless.
- Treehouse mode (b) creates slots by snapshot clone and not by checkout or `git worktree add`, so it avoids the 9x to 11x costs.
  Mode (a) pays them.
- A passthrough server has no hashing, zstd or redb cost, so real cowfs numbers will be worse.

## Not verified

- gc, repack and fsck timings on M and O were not measured.
  N was n=2 at load 10 to 57: `gc` 30s and 1.9s, `repack -adf` 12s to 13s, fsck 21s to 32s.
- The corrupt `.idx` (bug 2) is unexplained.
- The write-race fix was verified on the 44 MiB test repo and on 80 direct creates, not on the 805 MiB OmniRoute repo.
- Real-repo stress timings.
- The 200-commit stress on a `.git` file pointing into `.git/worktrees/` for layout O.
