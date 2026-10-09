# Issue 43: dead-server handling, end-to-end evidence (macOS NFS loopback)

Date: 2026-10-08.
Host: this Mac (Darwin 25.6.0), NFS mounts need no sudo.
Binaries: debug builds of cowfs-daemon and cowfs at worktree HEAD a93a736 (ci/parallel-tests-v2), rebuilt just before the run.
Script: a zsh harness (scratchpad exp.sh) that does every filesystem op under a 20 s perl alarm cap.
Private paths per run: store, mount, socket dir (0700) and export root under `<worktree>/.exp43-<mode>/`.
No source change was made. No PR was opened.

## Procedure per run

1. Start `cowfs-daemon --store S --mount M --socket SOCK --export-root R --backend core`.
2. `cowfs --socket SOCK snapshot create base` (the mount root is read-only; writes go into a snapshot directory).
3. Write `base/f.txt` and a 5 MB random `base/big.bin`, record sha256 of both.
4. Send the signal under test to the daemon.
5. Record the mount table, the socket file, and `ls`, `stat`, `touch` on the mountpoint (20 s cap).
6. Start the daemon again on the same paths, record its stderr, time to serve, and re-checksum both files.
7. SIGTERM the second daemon and record the final state.

## Results

| Signal | Daemon exit | Mount table after | Socket after | ls / touch on mountpoint | Restart | Checksums |
|---|---|---|---|---|---|---|
| SIGTERM | 0.13 s | mount removed | removed | ls ok in 0.05 s (plain directory) | serves in 0.3 s | identical |
| SIGINT | 0.14 s | mount removed | removed | ls ok in 0.03 s | serves in 0.2 s | identical |
| SIGKILL (run A) | 0.13 s | mount STILL LISTED | left on disk | ls hangs, hit the 20 s cap (rc 142); touch hangs, 20 s cap; stat answered from cache in 0.06 s | log: "swept a stale mount", serves after 4.8 s | identical |
| SIGKILL (run B) | 0.13 s | still listed | left | ls 20.05 s cap, touch 20.07 s cap | swept, serves after 4.7 s | identical |
| SIGKILL (run C) | 0.13 s | still listed | left | ls 20.08 s cap, touch 20.07 s cap | swept, serves after 1.4 s | identical |
| SIGKILL (run D) | 0.13 s | still listed | left | ls 20.05 s cap, touch 20.05 s cap | swept, serves after 1.95 s | identical |

Second-daemon SIGTERM after each restart: exits in 0.13 to 0.30 s, mount gone, socket gone.
The exact log line on restart is `cowfs-daemon: swept a stale mount at <mountpoint>`.

## Findings

The issue 43 symptom is real and reproduced: after a SIGKILL the mount stays listed and `ls` and `touch` block for 20 s or more (capped by my alarm, not by the kernel).
The wiring works: the next daemon start sweeps the stale mount via `prepare_platform` and serves the same store with both files intact (sha256 identical, 4 of 4 clean SIGKILL runs).
The daemon's own ordered SIGTERM and SIGINT handler unmounts cleanly, removes the control socket, and exits in about 0.13 s.
Sweep cost on restart is 1.4 to 4.8 s (nfsstat plus forced unmount), so a user sees the hang until they restart the daemon; nothing clears the stale mount without a restart.

## Anomaly, not reproduced

The first SIGKILL attempt used a flawed harness: it wrote to the read-only mount root so no data was written, its readiness check was only "mount listed and socket exists" (both already true from the stale state), and it appended all logs to one file.
In that attempt the second daemon logged no "swept" line, the stale mount stayed listed after its SIGTERM, and the (empty) checksum comparison printed OK vacuously.
I could not tell whether daemon 2 died, hung in the sweep, or was never ready.
Four correct runs with a per-start log and a real readiness probe (`cowfs status`) did not repeat it.
Treated as a harness artifact, labelled unverified, no fix attempted. Not enough evidence for a defect.

## Cleanup verification

After all runs: `mount | grep -c exp43` = 0, no cowfs-daemon process with an exp43 path, experiment directories removed.
Pre-existing daemons of other agents and the user (pids 15263, 19125, 81318) were not touched.
One earlier stale mount from the flawed attempt was removed with `umount -f` after confirming its path was inside `.exp43-kill`.

## Limits

Debug build, single host, 5 MB of data, one mount, background load from other agents.
The 20 s alarm caps the measured hang, so the true unmitigated hang duration is not measured.
The `|| echo` fallbacks in the harness never fired because the pipeline status was sed's, so "no mount" is inferred from empty output.
