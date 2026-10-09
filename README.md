# cowfs

A cross-platform, userspace, content-addressed virtual filesystem.
Agents work directly on it.
Every byte of source, build output, and dependencies is stored once, no matter how many trees it appears in.

Status: design phase.
See [docs/design.md](docs/design.md).

## Why

Parallel AI agents each need their own working tree, and each tree grows its own build artifacts.
Thirty near-identical worktrees with `target/` and `node_modules` fill a disk fast.
Copy-on-write clones only share data that was explicitly copied.
Identical bytes produced by separate builds are still duplicated.
cowfs deduplicates at the block level, so identical bytes are stored once regardless of origin.

## Goals

- Block-level deduplication of everything: source, build output, dependencies.
- O(1) writable snapshots, one per agent.
- Full POSIX semantics, including `mmap`, so builds run directly on the mount.
- Linux and macOS first, with an OS-agnostic core.
- Day-1 integration with [treehouse](https://github.com/kunchenguid/treehouse).

## Known limits

- macOS NFS mount: `open(2)` of a fifo fails with `EACCES`, because the macOS NFS client refuses to open any vnode that is not a regular file, directory or symlink.
  The server sees no request, and no server change or mount option helps.
  See issue #204 and [the evidence](docs/verification/evidence/nfs204-fifo-open.md).

## License

Apache-2.0.
Anyone may use, modify, and redistribute this software, provided the copyright and NOTICE attribution are preserved.
See [LICENSE](LICENSE) and [NOTICE](NOTICE).
