# Spike 3 (Linux half): FUSE passthrough versus native

Issue: #3.
Scripts and server source: `spikes/nfs-loopback/spike3/`.
Raw data: `spikes/nfs-loopback/out/spike3/results.json` and `vm/*.jsonl` (git-ignored).
Run date: 2026-09-29.
The macOS half is in `docs/spikes/2-nfs-loopback.md`.

## Result

A Linux FUSE passthrough is within the 1.5x budget for clean builds, incremental rebuilds, no-op builds, test-compiles and seeded builds, once it uses long cache timeouts.
Bulk small-file metadata operations (`cp -a`, `find`, `rm -rf`, `git status`, `git add` and `commit`) are over the budget at 2x to 3x.
This is a passthrough server with no hashing, zstd or redb behind it.

## Setup

- Target: an OrbStack VM `cowfs-spike3`, Debian bookworm arm64, 15 vCPU, 16 GB, kernel 7.0.14-orbstack, `cargo -j4`.
- Native side: the VM root filesystem, which is btrfs mounted `nodatacow,nodatasum`, not ext4.
  An ext4 loop-image comparison was attempted and discarded (VM load reached 20 and Mac load was above 30), so there are no ext4 numbers.
- Server: Rust, `fuser` 0.15.1 with `default-features = false`.
  It keeps an inode table of `O_PATH` file descriptors keyed by `st_ino`, uses real file descriptors for open files, and uses symlink-aware `utimensat` and `fchownat`.
  It does not implement xattr, mknod, fallocate or copy_file_range.
- Unprivileged mounting works in the VM: `fusermount3` is setuid and `/dev/fuse` is 0666.
- Absolute times are VM times.
  Virtualization applies to both sides, so ratios remain comparable within this VM.
- Crates: X is a copy of `spikes/dedup-corpus`, Y is a copy of `spikes/nfs-loopback`.
  The large tree is 7,777 files (fetched registry sources plus X and Y), not a real repo with 5,000 to 30,000 tracked files.

## Correctness

Identical for the baseline, tuned and final server configurations:
- `test_mount.py`: 24 of 24 without edits.
- Hardlink readdir: 8,000 of 8,000 unique, clean removal.
- `mmap_race.py`: 0 mismatches in 100 for each of nosync, msync and fsync.
- Extra checks all pass: nofollow `utimensat` and `lchown` on valid and dangling symlinks (target mtime unchanged), `fstat` on an unlinked open file (nlink 0), `O_APPEND`, rename over a directory, `find -links`, `statfs`, and a POSIX lock blocking another process.
- flock and fcntl locks are handled by the kernel locally, so the server does nothing for them.
- The X binary built natively and through FUSE gives the same `report.json`.
  `index.bin` and `slots.jsonl` differ, and the difference was not investigated.

## Final numbers

Final options: `--ttl 3600 --neg --keep-cache`.
Ratio is FUSE time divided by native time, paired median.
n=5 (n=25 for `git status`, n=15 for `find`).
I checked these ratios against the raw JSON.
For the final configuration, all 26 cells had 0 runs with Mac load above 30.
VM load was 0.8 to 3.2.

| Workload | Native s | FUSE s | Ratio | Within 1.5x |
|---|---|---|---|---|
| X clean debug | 5.45 | 6.11 | 1.13 [1.08-1.14] | yes |
| X clean release | 9.88 | 10.28 | 1.03 | yes |
| Y clean debug | 5.30 | 6.05 | 1.14 | yes |
| No-op build, X / Y | 0.042 / 0.023 | 0.044 / 0.033 | 1.07 / 1.48 | yes / borderline |
| One-line-edit rebuild, X / Y | 0.33 / 0.50 | 0.39 / 0.58 | 1.19 / 1.16 | yes |
| Test-compile after edit, X / Y | 0.17 / 0.16 | 0.19 / 0.18 | 1.10 / 1.12 | yes |
| Seeded first build, X / Y | 0.042 / 0.040 | 0.047 / 0.042 | 1.12 / 1.11 | yes |
| Seeding by `cp -a`, X / Y | 0.118 / 0.136 | 0.183 / 0.236 | 1.63 / 1.66 | no |
| `git status`, 7,777 files | 0.022 | 0.048 | 2.75 [1.8-7.6] | no |
| `git add` and `commit` | 1.51 | 3.43 | 2.20 | no |
| `cp -a` of the tree | 0.22 | 0.45 | 2.03 | no |
| `find` | 0.020 | 0.051 | 2.83 | no |
| `rm -rf` of the tree | 0.167 | 0.544 | 3.18 | no |
| 256 MiB write plus fsync / cold read / warm read | 0.24 / 0.115 / 0.059 | 0.30 / 0.17 / 0.05 | 1.27 / 1.45 / 0.90 | yes |

- A copied warm base built Fresh: 34 of 34 (X) and 41 of 41 (Y) units, 0 rustc runs, on both sides.
- Several ratios rest on tiny absolute gaps (milliseconds for no-op, `git status` and `find`), and builds only take 6 to 10s in this VM.

### Comparison with the macOS NFS loopback

The same kinds of workloads on the tuned macOS NFS server:

| Workload | macOS NFS loopback | Linux FUSE |
|---|---|---|
| Clean build | 0.9x to 1.2x | 1.03x to 1.14x |
| One-line-edit rebuild | 2.6x to 3.2x | 1.16x to 1.19x |
| No-op build | 2.5x to 3.4x | 1.07x to 1.48x |
| Missing-name lookup, uncached | 45 to 69 microseconds | 10.3 microseconds |
| Missing-name lookup, repeated, negative cache | not tested | 1.3 microseconds |

This is a cross-platform comparison and is not controlled: different operating system, kernel, filesystem (APFS against btrfs `nodatacow`), a VM, and different server code.
It does suggest that most of the macOS warm-build gap comes from the macOS NFS client and not from the passthrough design.
The mount options on the NFS side were left at the spike 2 defaults.

## Option tuning

One variable at a time against a same-session baseline, n=3 to 5.

| Option | Effect | Kept |
|---|---|---|
| `--entry-ttl 3600` | existing-name lookup 6.3 down to 1.0 | yes |
| `--attr-ttl 3600` | `git status` 3.1 against 7.4 | folded into `--ttl` |
| `--neg` (negative cache) | repeated missing name 9.0 down to 0.8 | yes |
| `--keep-cache` | warm read 2.54 down to 1.00, seed `cp` 2.26 down to 1.59 | yes |
| worker threads (4, 8) | no-op build 2.4 to 2.5 against 1.22, lookup 15 to 34 microseconds against 6 | no |
| `--writeback` | no consistent gain | no |
| max_background, congestion threshold, max_write, max_readahead, max_read | within noise | no |
| `--rdplus` | no help | no |
| cache dir, cache symlinks | bimodal, no clear gain | no |
| `--direct-io` | warm read 1.38 but no build gain, and shared writable mmap under it was not tested | no |
| splice | `fuser` 0.15.1 cannot set it | not tested |

The default options (`f_base`) were worse on cached-metadata cells: seed `cp` 2.38, `git status` 2.81, `find` 3.35, `rm -rf` 4.09, warm read 2.28.
Those runs are confounded: 19 of 26 cells had Mac load above 30, and native times drifted by up to 2x between sessions.
Compare ratios only within a configuration.

## Floor versus fixable

- **Floor:** an uncached FUSE round trip costs about 6.0 microseconds even with a server that does no work (`--null-lookup` answers ENOENT immediately, n=5 by 3000), against about 2 microseconds native including Python overhead.
- **Fixable but small:** the server's own work adds about 1.5 to 3 microseconds. Worker threads make it worse because the handoff costs 15 to 34 microseconds.
- **Removed by caching:** long entry and attribute timeouts plus negative caching remove the round trips for repeated lookups (ratio 1.0).
- **What remains:** bulk metadata operations scale with operation count times 6 to 10 microseconds.
  Only caching or batching reduces them.
- **Condition on caching:** the 3600s timeouts and `keep-cache` are only valid if the mount is the sole writer.
  That fits the single-user model, but cowfs snapshot and control-API operations that change the tree behind the mount would need to invalidate the kernel caches.

## Not verified

- The ext4 comparison (discarded), so the native baseline is btrfs `nodatacow,nodatasum`, which may be faster than default btrfs or ext4.
- A real repo with 5,000 to 30,000 tracked files.
- A real cowfs backend (hashing, zstd, redb).
- Mac load stayed at 150 to 370 for over 35 minutes at the end of the session, so no reruns were possible.
  One `git` auto-gc contaminated the first baseline git cells, and later runs used `gc.auto=0`.
- moonscape: stages 0 to 2 (feasibility, build, correctness) ran there before the redirect, with 24 of 24 checks and load 0.86 to 1.34 on 4 cores.
  No benchmarks were run there, and its directory has been removed.
