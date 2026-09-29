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

- Mark-and-sweep from snapshot roots, on demand and on a schedule.
- Marking is incremental: subtrees already marked are skipped.
- Each block records a last-accessed time, kept in memory and flushed in batches so reads do not become writes.
- Sweep candidates are unmarked blocks older than a threshold.
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
3. Zero data loss in crash-injection tests.

## Spikes

- Corpus dedup measurement.
- `.git` stored inside the filesystem, with a passthrough overlay as fallback.
- `mmap` and file locking over the NFS loopback.
- Treehouse process detection on the mount.
- How much rustc and Cargo output stays byte-identical across slots at different absolute paths.
