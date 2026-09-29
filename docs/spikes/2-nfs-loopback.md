# Spike 2: mmap and file locking over the macOS NFS loopback

Issue: #2.
Tool: `spikes/nfs-loopback`.
Run date: 2026-09-29, macOS 26 on an Apple M3 Max, APFS backing store.

## Result

The macOS NFS loopback is viable as the mount mechanism, with three conditions.
The unprivileged mount, `mmap`, locking, git, and cargo debug and release builds all work.
Clean builds are within 1.5x after fixing the server.
Incremental rebuilds are 2.6x to 3.8x and probably structural.

Server: a passthrough NFSv3 server built on `nfsserve` 0.11 that mirrors a backing directory.
Mount: `mount_nfs -o locallocks,vers=3,tcp,rsize=131072,actimeo=120,port=11111,mountport=11111 localhost:/ <dir>`, run as the normal user.

## What works

- The mount needs no sudo.
- Test battery `test_mount.py`: 24 of 24 pass.
  It covers roundtrip and stat, directory operations, 5,000-entry readdir, a 256 MiB hash roundtrip, symlinks, `O_EXCL`, atomic rename over an existing file, unlink-while-open, hardlinks, chmod and utimes, truncate, `mmap` (read-only, shared plus msync, private copy-on-write, grow), flock and fcntl (exclusive, shared), git init/add/commit/status/gc/fsck, and a small cargo build.
- `mmap` writes with no msync, with msync, and with fsync were re-read from a fresh process: 0 mismatches in 100 files each, on the mount and on native.
- Cargo debug builds work, including incremental compilation and a rebuild after a source edit.
- clang and rustc produce correctly signed, runnable binaries on the mount.
- Hardlinks work end to end after patching `nfsserve` (see below).
  Backing store: 644 hardlinked files after a debug build, identical to native APFS (644).
  Per-file `lstat` on the mount agrees with the backing store for all 644.

## Conditions

0. **The `nfsserve` library needed four fixes.**
   LINK support, a lock-contention fix in `TransactionTracker`, a readdir cookie fix, and a path-map fix (see Performance and Bugs).
   The vendored copy carries them all.

1. **`locallocks` is required.**
   With `nolocks`, every flock and fcntl call fails with `EOPNOTSUPP`, and rustc's incremental compilation aborts because it cannot take its session lock.
   With `locallocks`, locks are enforced by the client kernel only.
   That is enough for one client on one machine, which is the v1 model.
   It gives no cross-mount or cross-process-namespace guarantees beyond that.
2. **The NFS server library must support LINK.**
   `nfsserve` 0.11 does not implement `NFSPROC3_LINK`, so `link(2)` failed with `EBADRPC` (errno 76).
   Rustc incremental compilation hardlinks its `.o` files.
   In the spike #1 corpus, the hardlinks were in lumen debug and wasm-dev `incremental/` directories.
   The size-comparison sample was 3 lumen slots (slots 3 and 4 were re-checked for locations), so this attribution is from a sample.
   I vendored `nfsserve` under `spikes/nfs-loopback/vendor/nfsserve` (BSD-3-Clause, license kept) and added `LINK` with a default `NOTSUPP` trait method.
   The patch is 104 added lines in `nfs_handlers.rs` and `vfs.rs`.
   Upstream contribution or a maintained fork is a v1 decision.
3. **AppleDouble sidecars are unresolved.**
   The macOS client writes `._name` files next to files that carry extended attributes or resource forks.
   The spike server hides `._*` entries in readdir, which is a hack.
   A real store needs a deliberate policy: store them as ordinary files, or map them to xattrs.

## Performance

### First measurement: 2.5x to 4.3x on clean builds, 9x to 26x on incremental

Same crate (about 60 dependencies), this Mac, n=2 per row, naive passthrough server.
This first result showed the criterion was missed and did not say why.

### Where the time went (Opus profiling pass, original server)

- One clean debug build: about 39,000 NFS operations, 28.5s of summed server latency inside 23.4s of wall time.
  The client keeps only 1.5 to 2 requests in flight.
- A 10s `sample` of the server showed about 94% of samples waiting on one lock inside `nfsserve`'s `TransactionTracker`.
  On every request it scanned its whole retransmission map (60s of entries) while holding a global mutex, so cost grew with the square of the operation count.
- The second contention point was our path map.
  Renaming any file rescanned the whole map, which never shrinks.
- Server CPU during the build was 21.5s, against 16.3s user and 5.4s sys for cargo.

### After fixing the server

Ratio is NFS time divided by native time, median and range, paired per run and interleaved.
The machine was not idle: load average was 17 to 100 (CI runners and other rustc builds), and native debug clean itself moved between 5.5s and 11s.
Treat spreads as wide.

| Variant | n | Debug clean | Release clean | One-line-edit rebuild |
|---|---|---|---|---|
| Original server | 3 | 5.41 [1.89-7.62] | 3.44 [3.03-5.23] | 19.5 [13.2-29.4] |
| Tracker fix | 3 | 1.05 [0.93-1.37] | 1.18 [1.08-1.36] | 3.09 [2.27-3.10] |
| Final tree, one server for 4 runs | 4 | 1.07 [0.95-1.31] | 0.98 [0.92-1.09] | 3.79 [2.56-7.34] |
| Final tree without the rename fast path, 4 runs | 4 | 1.58 [1.26-1.81] | 1.44 [1.35-1.66] | 2.80 [2.16-3.19] |

- Clean builds are now about native speed, so most of the first gap was a library bug and not the NFS protocol.
- The last row shows the path map growing: NFS times rose run over run (release 12.8s to 15.0s) without the rename fast path.
- Mount options `rsize/wsize=1 MiB` and `actimeo=3600` did not beat the baseline mount beyond noise (n=3).
- `nordirplus` was not tested because plain READDIR in `nfsserve` ignored the page cookie.

### Incremental rebuild is still 2.6x to 3.8x, and probably structural

A one-line-edit rebuild is 13,000 to 26,000 operations, mostly LOOKUP.
A lookup of a missing file cost 45 to 69 microseconds over the loopback against about 3 microseconds native (n=3 by 3,000).
The server's share is about 10 to 20 microseconds.
The rest is the macOS NFS client, TCP loopback and RPC.
About 20,000 operations at 50 microseconds with 1.8 in flight gives about 0.55s, which matches the observed gap of about 0.6s.
This is the agent's inference from one microbenchmark.
It is not a proven root cause, and I have not verified it independently.
If it holds, the incremental case cannot reach 1.5x by server tuning alone, and this is the workload agents hit most.
The seeded-build measurement (spike #3 scope) tests exactly this case.

### Still open

- readdir re-reads and re-sorts the whole directory per page: 23 to 71 ms per page on a 10,000 to 15,000 entry `deps/`.
- The path map never shrinks, and directory rename still scans all of it.
- A real cowfs backend adds hashing, zstd and a redb lookup per operation.
  That is reasoning only, not measured.
  It mostly lands on read, write and create, but any per-lookup redb transaction adds directly to the incremental cost.

## Bugs found on the way

- **`nfsserve` readdir cookie bug (correctness).**
  The library used the inode number as the page cookie, and the server resumed a listing at the first entry with that inode.
  Hardlinks to one file in the same directory share an inode, so a page boundary on such an entry restarted the listing from the wrong place.
  rustc incremental builds create exactly this pattern in `deps/`.
  Effect: duplicates and errors on listing, `rm -rf target` failing, and later builds failing with "can't find crate".
  The Opus agent reported 6 of 6 reproductions.
  I confirmed it independently with `test_hardlink_readdir.py` (4,000 adjacent hardlink pairs).
  Old server: run 1 listed 8062 of 8000 entries with 8000 unique and `rmtree` failed, and runs 2 to 4 hung on the corrupted leftover directory.
  Fixed server: 3 of 3 pass with 8000 of 8000 unique and clean removal.
  My first, smaller version of the test (600 files, 200 links, non-adjacent) also passed against the old server, so it proved nothing.
  I strengthened it before recording this result.

- Rename fix-up in the path map appended a trailing slash when the renamed entry was the path itself.
  This made `write` fail with `ENOTDIR` and left zero-length files.
  It showed up as `ar`, `ranlib`, `libtool` and rustc output being empty 22 to 34 times out of 40, and as cargo build scripts being killed with SIGKILL.
  Fixed, and the same tools then failed 0 of 40 times.
  Note this was my server bug, not an NFS client limitation.
- `find -links +1` on the mount reported 343 hardlinked files where the backing store and per-file `lstat` show 644.
  Attributes returned inside readdir replies carry a stale `nlink`.
  Tools that read `nlink` through readdir attributes will undercount until the file is stat-ed.
  I did not check whether `actimeo` or readdirplus settings change this.

## Not tested

- `pjdfstest`, `fsx`, `xfstests`: not run.
- All timing was taken on a heavily loaded machine (load 17 to 100), n=3 to 4.
- The tracker fix was not A/B tested alone under matched load.
- Unlink of the original name followed by access through an already-open or cached filehandle to a remaining link.
  The server keeps the id-to-path entry for the first name, so a stale handle may fail.
- Whether the macOS client rejects `link()` on a directory before it reaches the server (the result was `EPERM`).
- FUSE-T as the fallback: not evaluated, since the NFS route works.
- Behaviour on a real cowfs store rather than a passthrough of APFS.
- Crash behaviour, concurrent clients, and a full multi-hour agent workload.
