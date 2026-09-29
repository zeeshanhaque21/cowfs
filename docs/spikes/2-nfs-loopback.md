# Spike 2: mmap and file locking over the macOS NFS loopback

Issue: #2.
Tool: `spikes/nfs-loopback`.
Run date: 2026-09-29, macOS 26 on an Apple M3 Max, APFS backing store.

## Result

The macOS NFS loopback is viable as the mount mechanism, with three conditions.
The unprivileged mount, `mmap`, locking, git, and cargo debug and release builds all work.
Performance is far outside the 1.5x criterion and is not resolved by this spike.

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

## Performance is not within the bar

Same crate (about 60 dependencies), clean build, this Mac, n=2 per row:

| Build | Native APFS | NFS loopback | Ratio |
|---|---|---|---|
| release, clean | 8.4s, 9.2s | 23.3s, 31.0s | 2.5x to 3.7x |
| debug, clean | 7.5s, 8.0s | 27.8s, 34.3s | 3.7x to 4.3x |
| debug, incremental rebuild after a one-line edit | 0.3s, 0.3s | 7.7s, 2.8s | 9x to 26x |

The success criterion is within 1.5x.
This spike does not meet it, and does not show where the time goes.
The server is a naive passthrough: a global path-map mutex, synchronous `std::fs` calls on the async runtime, no readdir batching, and a full re-sort of every directory on each readdir page.
So the numbers are an upper bound on loopback overhead for this server, not a floor for NFS in general.
Spike #3 needs to profile before any conclusion about NFS versus FUSE-T.
The n=2 spread in the incremental row (7.7s vs 2.8s) is large, and variance was not characterised.

## Bugs found on the way

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
- Server-side LINK tracing: not enabled, so there is no log of the LINK calls themselves.
- Unlink of the original name followed by access through an already-open or cached filehandle to a remaining link.
  The server keeps the id-to-path entry for the first name, so a stale handle may fail.
- Whether the macOS client rejects `link()` on a directory before it reaches the server (the result was `EPERM`).
- FUSE-T as the fallback: not evaluated, since the NFS route works.
- Behaviour on a real cowfs store rather than a passthrough of APFS.
- Crash behaviour, concurrent clients, and a full multi-hour agent workload.
