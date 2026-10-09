# cowfs design

## Purpose

A cross-platform, userspace, content-addressed virtual filesystem.
Agents work directly on it.
Every byte of source, build output, and dependencies is stored once, whichever tree it appears in.
Treehouse is the day-1 consumer.

Native OS features cannot give this on their own.
APFS clones and btrfs reflinks only share data that was explicitly copied.
btrfs dedup is offline only (bees, duperemove).
ZFS dedup is inline but needs a lot of RAM.
So a hashed block store is the core, and the OS layer is only the mount mechanism.

## Core (Rust, OS-agnostic library)

### Blocks

- Content-defined chunking (FastCDC), about 64 KiB average with min and max bounds.
- Small files are stored whole.
- BLAKE3 hashing.
- zstd compression, applied after chunking, so dedup is computed on uncompressed bytes.
- Hash verified on every read, plus an `fsck` command.

### Tree

- Merkle tree of directories and files.
- A snapshot is a writable, O(1) metadata copy.

### Metadata and durability

- redb, an embedded transactional KV store, for metadata.
- Blocks live in an append-only pack layout.
- Crash-consistent metadata transactions.
- Bounded loss of recent writes on a crash is acceptable.
- A torn or corrupt tree is not.

### Garbage collection

- Mark-and-sweep from snapshot roots.
- Marking is incremental: subtrees already marked are skipped.
- GC runs on demand only today (`cowfs gc`).
  Running it on a schedule is OPEN: nothing in the daemon starts a cycle.
  See `docs/gc-scheduling-10-20261009.md` and issue #10.
- The sweep is per pack, not per block.
  A pack is a candidate when its dead record bytes pass `dead_ratio` and `min_dead_bytes`.
  There is no age threshold on blocks.
  Whether to add one is OPEN (decision D3 in the memo, issue #10).
- Each block is meant to carry a last-accessed time, kept in memory and flushed in batches so reads do not become writes.
  The hint store exists in `cowfs-gc` (`Gc::note_access`, `atime.bin`), but no read path calls it, so the file stays empty and "coldest first" is not active in production.
  Feeding it is OPEN (memo and issue #10).
  `atime.bin` is append-only with no compaction, so it would grow without bound once fed.
- Access time is a hint only.
  It is never the sole reason to free a block, because a cold block may still be referenced by a live snapshot.
- The same timestamps can later drive tiering of cold blocks to slower or remote storage.

### Semantics

- Full POSIX: `mmap`, atomic `rename`, hardlinks, symlinks, xattrs, file locks.
- Single-user model: mode bits preserved, everything owned by the mounter.
- No encryption in v1.

## Mounting

- Linux: FUSE.
- macOS: in-process NFS loopback server, falling back to FUSE-T if the spike shows problems with `mmap` or locking.
  macFUSE is avoided because it needs a kernel extension.
- Layout: one mount with a `/<snapshot>/` directory per clone.
- Linux only: optional per-agent mount namespaces that place every clone at an identical canonical path, so artifacts that embed absolute paths stay byte-identical.
- macOS: normalize paths with compiler flags such as `--remap-path-prefix`.
- Interface: a CLI on top of a Unix-socket control API.
  Commands: `snapshot create/rm`, `gc`, `fsck`, `import`, `base refresh`.

## Scope

- One local store per host, shared across all repos, so dependencies dedup across projects.
- The on-disk format leaves room for a future remote or shared block backend.
- Out of v1: Windows, a shared multi-machine store, encryption.

## Treehouse integration (day 1)

Treehouse keeps a pool of reusable git worktrees.
A slot is an ordinary worktree at `{pool}/{slot}/{repo}`.

### Mode (a): transparent

- The treehouse root and each repo's main checkout live on the cowfs mount.
- Unmodified treehouse creates ordinary worktrees there.
- Block dedup happens underneath.

### Mode (b): snapshot-native

- Slot creation and reset become an O(1) clone of a per-repo warm base that already contains build artifacts.
- Delivered by a `cowfs treehouse` companion and hooks.
- If hooks are insufficient, propose a small provisioner extension upstream.
  Fork only if that is rejected.

### Warm base

- A snapshot of `main` with dependency builds already in `target/`.
- Refreshed on CI success, plus a manual `base refresh`.
- Replaces hand-rolled cache seeding scripts.

### Migration

- Non-destructive `cowfs import <dir>`.
- Ingests slot by slot, verifies by hash, and only then swaps the directory for a mount.
- Peak extra disk stays small.

### Clone lifecycle

- Discard after the work is committed to git.
- Promoting a clone to a base is an explicit operation.

## Success criteria

1. Dedup ratio against a real corpus: the worktree pools plus one Node project.
   Measured before any filesystem code is written.
2. Build overhead within 1.5x of native on a representative `cargo build`, and on `git status` for a large tree.
   Amended after spikes 2, 3 and 18 (decision recorded in issue #18):
   - The 1.5x bar stays for Linux and for macOS clean builds and `git status`.
   - On macOS, warm and incremental builds are measured against an absolute budget instead of a ratio, because the macOS NFS client costs about 50 microseconds per lookup against about 6 on Linux FUSE, and about 1,100 to 1,400 missing-name lookups per rebuild are structural.
   - The provisional macOS budget is that an edit-and-rebuild of the reference crate adds under one second over native, to be confirmed against the real backend.
   - Revisit after v1 is complete: evaluate another macOS mount route (FUSE-T, macFUSE) against this budget (issue tracked as a post-v1 follow-up).
3. Zero data loss in crash-injection tests.

## Spikes

- Corpus dedup measurement.
- `.git` stored inside the filesystem, with a passthrough overlay as fallback.
- `mmap` and file locking over the NFS loopback.
- Treehouse process detection on the mount.
- How much rustc and Cargo output stays byte-identical across slots at different absolute paths.
