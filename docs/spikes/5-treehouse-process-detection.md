# Spike 5: treehouse process detection on the mount

Issue: #5.
Scripts: `spikes/nfs-loopback/spike5/`.
Run date: 2026-09-29, treehouse v3.1.0, macOS 26, tuned NFS loopback server from spike 2.

## Result

`treehouse return` works on a slot that lives on the NFS mount for every case treehouse can detect at all.
It has two gaps.
One of them is worse on the mount than on native disk.

## How treehouse finds lingering processes

I read the source at tag v3.1.0 and confirmed the key file myself.

- `internal/process/detect.go`, `FindProcessesInWorktree`: lists every pid with `gopsutil`, reads each process's working directory with `p.Cwd()`, and keeps the process if its cwd resolves to a path inside the slot.
- On darwin the cwd read is `proc_pidinfo(PROC_PIDVNODEPATHINFO)`.
- It uses no `lsof`, no open file descriptors, no locks, and ignores process groups and sessions.
- `terminate.go` and `terminate_unix.go` send SIGTERM, poll with `kill(pid, 0)`, then send SIGKILL after a grace period (2s in `cmd/get.go`, `killLingeringProcesses`).
  It then re-scans and returns an error if survivors remain.

## Cases

Each case leased a slot with `get --lease`, spawned detached fixtures, then ran `return --force`.
One run per case, so timings are indicative only.

| Case | Native | Mount |
|---|---|---|
| a. cwd at slot root | pass | pass |
| b. cwd in a nested subdirectory | pass | pass |
| c. process chdir'd out, holds an open fd on an untracked file | not detected | not detected, slot left dirty |
| d. process chdir'd out, holds a flock in the slot | not detected | not detected, slot left dirty |
| e. setsid child with cwd in the slot | pass | pass |
| e2. setsid child with cwd outside the slot | child survives | child survives |
| f. ignores SIGTERM | pass, escalates to SIGKILL (2.5s) | pass (2.34s) |
| g. clean return, then a second `get` | pass, same slot | pass, same slot |
| h. interactive `get`, background `sleep`, exit | pass | pass |

### The mount-only failure in cases c and d

Return killed nothing, and the reset unlinked the file while a process still held it.
The macOS NFS client silly-renamed the file to `.nfs.<id>` inside the slot.
`return` still exited 0 and printed "returned to pool".
`treehouse status` then showed the slot as `dirty`, and the next `get` skipped it and took another slot.
On native disk the same cases leave a clean slot and an orphan process.
After the leftover processes were killed and the mount was remounted, all slots read `available` and no `.nfs*` entries remained.
Whether the `.nfs` file vanished at kill time or at unmount time was not verified.

### umount

After the clean returns (a, b, e, f, g), plain `umount` succeeded.
With the c, d and e2 processes alive it failed with "Resource busy", and it succeeded once the process holding the flock (d) was killed, so d alone is enough to block it.
c alone was not tested.

## lsof and cwd lookup (n=3, machine load about 30 to 70)

| Measurement | Native | Mount |
|---|---|---|
| `lsof +D`, 3-file slot | 0.29s to 0.36s | 0.30s to 0.43s |
| `lsof +D`, 10,000-file tree | 0.41s to 0.43s | 0.67s to 0.86s |
| `lsof +D`, 10,000 files, cold after remount (n=1) | not run | 0.50s |
| `lsof -a -d cwd -p <pid>` | 0.05s | 0.07s to 0.11s |
| `proc_pidinfo` cwd via ctypes (n=200) | median 6.5 to 6.7 microseconds | median 7.4 to 7.6 microseconds |

Both `lsof` and `proc_pidinfo` report a cwd on the NFS mount correctly (200 of 200 lookups on each side), with no hang and no meaningful penalty.

## Proposed changes

- The cwd-only detection needs no change for the common agent case (a, b, e, f, g, h).
- Upstream to treehouse: also treat open file descriptors and flocks as lingering.
  A `proc_pidinfo(PROC_PIDLISTFDS)` scan or an `lsof +D` pass fixes c and d on native disk too.
- In cowfs: the silly-rename dirt is NFS specific, so cowfs needs its own answer for c and d.
  Options are a control-API list of processes with open fds on the mount that runs before `return`, or making `.nfs*` entries invisible to `git status`.
- The design says to propose upstream and fork only if that is rejected.
  Nothing else in treehouse needs a provisioner change for the cwd case.

## Safety and integrity check

The real pools under `~/.treehouse` are leased to other agents.
Every sandboxed treehouse command used a sandbox `HOME`, `TREEHOUSE_ROOT` and an explicit `--root`, with a guard that aborted on any real-pool path in the output.
The agent never ran `update`, `prune` or `destroy`.
Before and after: 17 of 18 pool state files were unchanged.
`lumen-a7cbb6/treehouse-state.json` changed, and I confirmed it is still being written now, with lease holder `lumen-m14-builder`, while none of our processes were running.
`update-check.json` also changed, because every treehouse invocation refreshes that cache, including `--help` calls by other agents.

## Not verified

- Linux behaviour: `gopsutil` reads `/proc` there and may differ.
- Case c alone as a `umount` blocker.
- One run per case.
- A mount that has gone stale.
- Whether the `.nfs` file disappears at kill time or at unmount time.
