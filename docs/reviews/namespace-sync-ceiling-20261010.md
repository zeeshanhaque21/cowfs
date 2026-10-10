# Namespace sync ceiling experiment, 2026-10-10

## Question

On a cowfs NFS mount (macOS), every operation that changes the namespace costs tens of milliseconds, while stat and read are in microseconds.
Code reading pointed at `Adapter::durable()` in `crates/cowfs-nfs/src/adapter.rs`, which calls `sync_namespace` before every reply that changes a name or attribute (added for #90).
This experiment measures the ceiling: how fast metadata gets if that per-operation sync is removed entirely.
It does not propose or land a fix.

## Setup

- Commit: `65a551957134c361c70fd7c843ef265a2327dc7c` (main), fresh clone in a `mktemp -d` scratch directory.
- U is the unmodified build.
- P is the same commit with the patch below, and nothing else changed.
- Both were built with `cargo build --release -p cowfs-daemon -p cowfs-cli`.
- Each daemon was private: `cowfs-daemon --store <S>/store --mount <S>/mnt --socket <S>/sock/daemon.sock --backend core`, with `<S>` under the scratch directory, never under `~/.cowfs`.
- Only one private daemon ran at a time.
- The live daemon (pid 10824), `~/.cowfs/mnt` and its socket were never touched.
- Each store was fresh, and the benchmark wrote into a snapshot made with `cowfs snapshot create bench`.
- AppleDouble mode was the default (`Hide`).
- Machine: M3 Max Mac, macOS (Darwin 25.6.0), internal SSD, about 238 GB free throughout.

### The patch (P)

```diff
--- a/crates/cowfs-nfs/src/adapter.rs
+++ b/crates/cowfs-nfs/src/adapter.rs
@@ -515,7 +515,8 @@ impl Adapter {
     /// its old name after the daemon died. The reply is the one chance to fix that, and it costs
     /// file data nothing: a dirty file stays unstable until its own COMMIT.
     fn durable(&self, ino: Ino) -> NfsResult<()> {
-        self.vfs.sync_namespace(ino).map_err(stat)
+        let _ = ino; // CEILING EXPERIMENT: skip sync_namespace
+        Ok(())
     }
```

The patched code was never pushed to any branch.
A third build, P-diag, added one `eprintln!` in `Adapter::commit` on top of P, to log which inode each COMMIT targets.
It was used only for the bottleneck diagnosis below, never for timing.

## Micro-benchmark

Tool: `microbench.py <dir> <N>` (the same script used for the live measurement this morning).
Each cell is microseconds per operation, run 1 / run 2.
Order of runs: U, native, P, P, native, U, each doing N=200 and then N=1000.
Native is the same script on APFS, in a directory under the same `$TMPDIR` scratch (the same volume as the stores), not `/tmp` itself.
P run 1 at N=1000 had `sample` attached to the daemon for part of the run.

### N=200

| operation | U (us/op) | P (us/op) | native (us/op) |
|---|---|---|---|
| mkdir | 40,798 / 44,406 | 13,823 / 11,536 | 50 / 38 |
| create+write 4 KB+close | 54,136 / 51,375 | 22,588 / 22,721 | 81 / 70 |
| stat | 14 / 4 | 11 / 3 | 2 / 2 |
| open+read 4 KB+close | 165 / 128 | 150 / 95 | 16 / 15 |
| rename | 46,243 / 13,998 | 594 / 573 | 68 / 61 |
| create+write 1 MB+close (40 files) | 52,560 / 55,585 | 23,802 / 22,657 | 147 / 152 |
| create+write 4 KB+fsync (100) | 58,117 / 52,827 | 22,496 / 22,013 | 115 / 75 |
| listdir x20 | 400 / 139 | 112 / 236 | 185 / 183 |
| unlink | 11,678 / 10,872 | 408 / 452 | 36 / 38 |

### N=1000

| operation | U (us/op) | P (us/op) | native (us/op) |
|---|---|---|---|
| mkdir | 42,613 / 43,717 | 12,050 / 12,217 | 40 / 41 |
| create+write 4 KB+close | 55,529 / 56,512 | 23,478 / 23,662 | 68 / 68 |
| stat | 4 / 7 | 3 / 9 | 3 / 2 |
| open+read 4 KB+close | 75 / 83 | 72 / 85 | 16 / 16 |
| rename | 59,339 / 59,753 | 7,789 / 6,371 | 66 / 63 |
| create+write 1 MB+close (40 files) | 57,261 / 55,535 | 24,876 / 23,419 | 150 / 146 |
| create+write 4 KB+fsync (100) | 60,918 / 53,463 | 23,441 / 23,115 | 80 / 78 |
| listdir x20 | 1,092 / 492 | 458 / 539 | 699 / 670 |
| unlink | 11,824 / 11,995 | 364 / 386 | 39 / 40 |

### Reading the table

- Removing the per-operation sync cuts mkdir about 3.5x (43 ms to 12 ms), create about 2.4x (55 ms to 23 ms), unlink about 30x (12 ms to 0.4 ms) and rename 8x to 80x.
- P is still far from native: mkdir is 12 ms against 0.04 ms, and create is 23 ms against 0.07 ms, about 300x slower.
- So the hypothesis that removing the per-op sync restores near-native metadata speed is false.
- The U numbers on a fresh private store (43 ms mkdir, 55 ms create) are about half of this morning's live mount numbers (95 ms, 115 ms).
- The live store is larger and busier, and that difference was not investigated.
- P rename grows with directory size: 0.6 ms at N=200, 6 to 8 ms at N=1000, while unlink stays flat at 0.4 ms.
- That growth was not investigated.

## Next bottleneck on P: the COMMIT behind every new entry

P is still slower than 5 ms per create and mkdir, so the daemon was sampled (`sample <pid> 5`, five samples: two during the benchmark, one during a mkdir-only loop, three during a slot rebuild).

### What the stacks show

During the mkdir phase almost every busy daemon thread was in the same chain:

```
NFSFileSystem::commit (nfsserve COMMIT handler)
 > Adapter::commit
 > cowfs_core::Inner::op_fsync > finish_sync
 > cowfs_meta::Inner::sync > Inner::commit
 > redb::WriteTransaction::commit > TransactionalMemory::commit
 > PagedCachedFile::flush > std::fs::File::sync_all > fcntl (F_FULLFSYNC)
```

In a 3 s sample of a mkdir-only loop (400 mkdirs, 12.6 ms each), `Adapter::commit` had 1938 samples and `Adapter::mkdir` had 2.
Each redb commit showed two `sync_all` calls, roughly equal in weight.

### Why a mkdir sends a COMMIT

The client RPC counters (`nfsstat -c`) for 201 mkdirs went up by about 201 each of Mkdir, Create, Write, Commit and Setattr.
These are global counters, so the live mount contributes noise, but the exact match with 201 is the signal.
Every new file and directory on the mount carries a `com.apple.provenance` extended attribute (visible with `ls -l@`).
The macOS NFS client stores that attribute in a `._name` AppleDouble file: create, write 4 KB, COMMIT.
In the default `Hide` mode cowfs stores that sidecar as an ordinary hidden file, so its COMMIT reaches `op_fsync` and a full metadata commit.
P-diag confirmed it: three mkdirs and one 10-byte file produced five COMMITs on 4096-byte regular files and one on the 10-byte file.
That the 4096-byte files are the `._` sidecars is inferred from their size, their count and the counters; P-diag printed `side=false` for them because `Hide` mode stores them as plain inodes rather than translated sidecars.

This explains the P numbers:

- mkdir is about 12 ms: one sidecar COMMIT, one metadata commit with two F_FULLFSYNC.
- create+write+close is about 23 ms regardless of size: the file's own COMMIT on close plus its sidecar's COMMIT.
- create+write+fsync costs the same as without fsync, because close already committed.

### During a real rebuild

Three samples during the P slot rebuild, summed over threads (samples, about 1 ms each):

| frame | build sample 1 | 2 | 3 |
|---|---|---|---|
| `Adapter::lookup` | 8,833 | 7,867 | 5,081 |
| `Adapter::readdir` | 7,586 | 9,812 | 14,117 |
| `Adapter::commit` > `op_fsync` | 4,623 | 4,018 | 3,210 |
| `__fcntl` (F_FULLFSYNC) | 2,569 | 2,614 | 2,342 |

In sample 3, 10,619 readdir samples and 5,036 lookup samples were blocked in `RwLock::lock_contended` under `Snapshot::readdir` and `Snapshot::lookup`.
Getattr, write and remove were blocked in the same way.
Commit threads appear under `Inner::commit_batch > Inner::wlock` and in `sync_all`.
The hypothesis from the stacks, not verified with a lock trace, is that the metadata commit holds the snapshot lock across F_FULLFSYNC, so every reader waits behind every COMMIT.
The client sent about 26,700 COMMITs during the 451 s P rebuild (global counter, so it includes some live mount noise).
At about 12 ms each, if they serialize, that is roughly 320 s of the 451 s.
This is an estimate, not a measurement.

## Real rebuild in a slot

Recipe: fresh native clone at the same commit, `cargo test --no-run --workspace` natively (19 s, 113 Compiling lines, not the expected 2 minutes), `cowfs import <base> --name exp-base`, `cowfs snapshot create <slot> --from exp-base`, append `// ceiling test` to `crates/cowfs-store/src/lib.rs` in the slot, then time `cargo test --no-run --workspace` inside the slot.
Every run recompiled 7 crates (cowfs-store, cowfs-meta, cowfs-gc, cowfs-core, cowfs-daemon, cowfs-fuse, cowfs-cli).

| variant | wall time | result |
|---|---|---|
| native, in place | 8.4 s | ok |
| P, slot 1 | 391 s | failed: `ld` EEXIST (see below) |
| P, slot 2 | 415 s | ok |
| P, slot 3 (with `sample` attached) | 451 s | ok |
| U, slot 1 | 835 s | failed: `ld` EEXIST (see below) |
| U, slot 2 | pending | running at time of this commit |

Known U value for this edit from earlier runs: 1250 to 1650 s.
P and U imported identical bases: 37,222 files, 4.5 GiB.
A first U import (59,326 files) was discarded before use because the native control build had modified that base.
Import took 28 s into P and 35 s for that discarded U import.

So P cuts the rebuild by roughly 2x to 3x, and is still about 50x slower than native.

## Tradeoff demo: what the sync buys

Each repetition: create a file in the mount and fsync it, rename it, `fsync` the parent directory (`os.fsync` on an `os.open(dir)` descriptor), then `kill -9` the private daemon after checking its command line, restart the daemon on the same store, and look for both names.

| variant | rename survived | rename lost (file back at old name) |
|---|---|---|
| U | 5 of 5 | 0 of 5 |
| P | 0 of 5 | 5 of 5 |

In every P repetition the file was intact under its old name, and the new name did not exist.
This is the #90 failure: on macOS a directory fsync sends no COMMIT, so without the per-operation sync an acknowledged rename is lost when the daemon dies.
Removing the sync outright is not acceptable; any faster design must keep this 5 of 5.

## Surprises

- `ld` failed with `open() failed, errno=17 (File exists)` for the test binary it was writing, in 2 of 5 slot rebuilds, once on U and once on P.
- After the failure, `ls` showed that the output path did not exist.
- It happens on the unmodified daemon, so it is not caused by the patch; it looks like a create-after-unlink race on the mount and deserves its own issue.
- The provenance sidecar doubles the cost of every file create and is the whole cost of a mkdir on P; it is invisible on the mount in `Hide` mode.

## What could not be verified

- That the readers in the rebuild samples wait on the very lock held across F_FULLFSYNC (stack-derived only, no lock trace).
- The share of rebuild time spent in COMMIT (estimate from a global counter and a per-COMMIT cost, not measured).
- Why P rename slows down with directory size.
- Why the live mount is about twice as slow as a fresh private store.
- Whether `Translate` AppleDouble mode, which stores no sidecar inodes, removes the sidecar COMMIT; it was not tested.
- Variance: two runs per cell only, and the rebuild runs are single runs per slot.

## Scratch and teardown

All builds, stores, mounts and logs were in a `mktemp -d` directory and were deleted after a clean `cowfs shutdown` of the private daemon.
No `umount -f` was used.
