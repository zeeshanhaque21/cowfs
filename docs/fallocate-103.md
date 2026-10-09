# fallocate through the Vfs (issue 103)

## Problem

The FUSE adapter answers `ENOTSUP` to every `fallocate` mode (`crates/cowfs-fuse/src/fs.rs`).
`Vfs` has no method that can express allocate, punch or zero-range, so the adapter cannot do better on its own.
The mounted fsx gate (g4) therefore measures no hole coverage: the matrix `bench/fsx-gate/fallocate-matrix.py` shows 75 of 75 modes `ENOTSUP` on cowfs where ext4 answers `ok` (`docs/reviews/mounted-fsx-g4-final.md`).
Emulating from the adapter is unsafe.
Mode 0 past EOF through getattr plus setattr(size) can shrink a concurrent extension and lose data.
Punch and zero-range by writing zeros are not atomic, allocate storage, and defeat the hole coverage g4 wants.

## Interface

```rust
fn fallocate(&self, ino: Ino, mode: FallocMode, offset: u64, len: u64) -> Result<Attr> { Err(Error::NotSupported) }

#[non_exhaustive]
pub enum FallocMode { Allocate, KeepSize, PunchHole, ZeroRange, ZeroRangeKeepSize }
```

The default returns `NotSupported`, so PathVfs, the NFS adapter and test wrappers keep building and keep answering `EOPNOTSUPP`.
A wrapper that forwards every method by hand must forward this one too, or it silently hides the capability.
Slice B and C list the wrappers (`SnapshotView`, the conformance `Keep`) and forward them.
The enum has five variants, not the four first proposed, because Linux treats `ZERO_RANGE` with and without `KEEP_SIZE` differently, and the matrix probes both.
The kernel bit `PUNCH_HOLE` always travels with `KEEP_SIZE`, so there is no punch variant that changes the size.

## Modes

All ranges are `[offset, offset + len)` in bytes.
`len == 0` is `InvalidArgument` in every mode, as on Linux.

| Mode | Kernel bits | Effect |
|---|---|---|
| Allocate | 0 | Size becomes `max(size, offset + len)`. Existing bytes are untouched. New bytes read as zeros. |
| KeepSize | `KEEP_SIZE` | Validates the inode and range, changes no data and no size. |
| PunchHole | `PUNCH_HOLE\|KEEP_SIZE` | The part of the range inside the file reads as zeros and holds no storage. Size is unchanged. |
| ZeroRange | `ZERO_RANGE` | Like PunchHole, and size becomes `max(size, offset + len)`. |
| ZeroRangeKeepSize | `ZERO_RANGE\|KEEP_SIZE` | Like PunchHole. Size is unchanged. |

cowfs has no preallocation.
Allocate is therefore sparse-aware: it only promises that the file is at least that long and that a later write inside the range cannot fail for lack of space that this call could have reserved, which is no promise at all here.
It never consumes space, so `st_blocks` does not grow, and it never returns `NoSpace` except for a range past the maximum file size.
This is the documented deviation from ext4, where mode 0 reserves blocks.
Zero-range and punch are the same operation on cowfs, because both mean "reads as zeros" and a hole is the cheapest way to say it.
This is a second deviation from ext4, where ZERO_RANGE preallocates blocks for the range.
On cowfs `st_blocks` drops after a zero-range where ext4's would not, and the zero_range rows of a matrix re-run will differ from ext4 on blocks.
The matrix script records blocks only when it prepares each file, not after the call, so it does not show the difference; it is expected, not a regression.

Time updates are a cowfs contract, not a claim about ext4 or xfs, which differ on which modes touch mtime.
PunchHole and both ZeroRange modes change content, so they set mtime and ctime, as the `Vfs` trait already says of content changes.
Allocate and KeepSize set ctime; Allocate also sets mtime when the size grows.
Only the content-changing rule is asserted at `Level::Cowfs`; the Allocate and KeepSize rules are documented, not checked, because native filesystems disagree.
A failed call changes nothing.

## Core semantics

The operation runs under the inode write lock, as `setattr(size)` does, so it is atomic against concurrent writes, truncates and other fallocates.
The size decision (`max(size, end)`) is made under that lock, which removes the shrink-a-concurrent-extension race.
The sequence for a punch of `[a, b)`:

1. Clamp `b` to the file size.
   If `a >= b` there is nothing to punch.
2. Flush the file's dirty extents, as truncate does, so the chunk list is the whole truth.
3. Bytes in `[total, size)` are an implicit hole already.
   Clamp `b` to the chunk-covered total for the rewrite.
4. Replace the chunk refs overlapping `[a, b)` with: the head of the first overlapped chunk, `hole_refs(b - a)`, the tail of the last overlapped chunk.
5. A hole ref overlapped at an edge keeps its remaining part as a hole ref and reads nothing from the store.
6. A stored chunk overlapped at an edge is read, verified with `check_len`, cut, and its surviving bytes re-chunked with the store's FastCDC through `put_piece`, as truncate does.
7. A chunk fully inside the range is dropped without being read.
8. Publish a new `Arc<Chunks>`, update the size when the mode extends, update times, queue the content for the snapshot context.

An edge that lands exactly on a chunk boundary costs no read and no write.
Adjacent hole refs are allowed and read correctly; merging them is an optimisation, not a requirement.
Extension (Allocate, ZeroRange past EOF) only raises `size`; the new bytes are the implicit trailing hole.

### Snapshots and clones

Chunk lists are values and blocks are immutable and content addressed.
A punch builds a new chunk list for the live file and never rewrites a block, so a snapshot or clone that shares the old list still reads the old bytes.
The test is explicit: write, snapshot, punch the live file, read both, and require the snapshot unchanged.
A snapshot view that is read-only answers `ReadOnly`, by the same check the other mutating operations use.

### GC and fsck

A hole ref carries the all-zero id, which GC already filters (`cowfs-gc` drops `HOLE` when marking) and which `is_hole` recognises in core.
Punching leaves blocks unreferenced by the live file; they are freed by the next GC cycle, exactly as after truncate or unlink.
The slice B test runs fsck-style reference checks over a file with punched holes: every non-hole ref resolves, and no hole ref has length zero or above `HOLE_MAX`.

### Space and quota

Punch only lowers stored bytes, so `st_blocks` (`used_bytes`) drops at once and `statfs` free space returns when GC runs.
The one way a punch consumes space is re-chunking the surviving bytes at an unaligned edge, at most two chunks.
If the store or a limit refuses those puts, the call returns `NoSpace` (`ENOSPC`) and the file is unchanged, because the new list is built before it is published.

## Error mapping

| Condition | Vfs error | errno |
|---|---|---|
| `len == 0` | InvalidArgument | EINVAL |
| range beyond the maximum file size, or `offset + len` overflows | NoSpace today | ENOSPC |
| unknown inode | Stale | ESTALE |
| directory | IsDir | EISDIR |
| symlink | InvalidArgument | EINVAL |
| read-only snapshot | ReadOnly | EROFS |
| damaged block at an edge | Corrupt | EIO |
| no store room for the edge re-chunk | NoSpace | ENOSPC |
| backend without support (default) | NotSupported | ENOTSUP (= EOPNOTSUPP on Linux) |

Linux returns `EFBIG` for a range past the maximum size.
`write` and `setattr` already return `NoSpace` for it, and another builder is adding a `FileTooBig` error.
Until it lands, fallocate matches `write`; when it lands, all three move together.
The FUSE adapter rejects negative offset or length with `EINVAL` before calling the Vfs.
The FUSE adapter answers `EOPNOTSUPP` for collapse-range, insert-range, unshare-range, any unknown bit, and the invalid pairs the kernel itself refuses (`PUNCH_HOLE` without `KEEP_SIZE`, `PUNCH_HOLE` with `ZERO_RANGE`).

## Other backends

MemVfs is the reference and gets all five modes, with a `Pages::punch` that drops whole pages and zeroes partial ones.
PathVfs (slice D, only if the g4 control needs it) calls `libc::fallocate` with the mode bits on Linux and reports `NotSupported` elsewhere.
It must not use `posix_fallocate`: that is mode 0 only, and glibc emulates it by writing zeros where the host lacks support, which is the non-atomic, space-allocating behaviour rejected above.
The NFS adapter keeps the default: NFS v3 has no fallocate.

## Tests

Slice A (cowfs-vfs and cowfs-vfs-test): conformance checks, run on MemVfs, with four MemVfs faults so a wrong backend fails: `PunchNoop` and `PunchChangesSize` (caught by the punch check), `AllocateShrinks` (allocate check), `ZeroRangeNoExtend` (zero-range check).
Checks: punch reads zeros and keeps size, zero-range with and without keep, allocate extends and never shrinks (sequentially: allocate inside a longer file leaves the size alone), keep-size past EOF keeps the size, errors, times (`Level::Cowfs`), and a seeded random sequence against a byte model.
MemVfs has one coarse lock, so a two-thread check there could not fail for the right reason; the real interleaving of allocate against growing writes is tested on Core in slice B, where the atomicity lives.
The checks are `Level::Portable`, not `Posix`, because PathVfs answers `NotSupported` until slice D.
The only runs of the whole suite on a backend without fallocate are `crates/cowfs-core/tests/conformance.rs` (the `conformance_tests!` skip list and the `SuiteOptions::skip` in `run_all_prints_a_table`); the PathVfs native and mount runs are manual, ignored and non-strict.
Core skips the new checks by name with the reason "fallocate not implemented yet (#103 slice B)", so the skip is visible and slice B removes it.
Slice B: the same checks on Core, plus the snapshot-unchanged test, hole-ref integrity after a punch, `st_blocks` drop, and edge cases at chunk boundaries.
Slice C: first read the kernel's `fuse_file_fallocate` for the CI and cachyos kernel versions to learn which mode masks reach userspace, because a filtered `ZERO_RANGE` would keep those matrix rows `EOPNOTSUPP` whatever cowfs does.
Slice C: a Linux-only mount test in `crates/cowfs-fuse/tests/mount.rs` that calls `fallocate(1)` or `libc::fallocate` for every mode and reads back, and the matrix re-run.

## What stays out

Collapse-range, insert-range and unshare-range stay `EOPNOTSUPP`.
Real preallocation and space reservation stay out; cowfs has none.
Merging adjacent hole refs, trimming trailing holes and dropping dirty extents instead of flushing are optimisations left for a measured need.
`copy_file_range` and `lseek(SEEK_HOLE/SEEK_DATA)` are separate issues.
