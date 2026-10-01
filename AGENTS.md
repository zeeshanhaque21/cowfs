# cowfs agent instructions

cowfs is a cross-platform, userspace, content-addressed virtual filesystem.
It gives AI agents working trees where every byte (source, build output, dependencies) is stored once, with O(1) writable snapshots.
Repo: https://github.com/zeeshanhaque21/cowfs (public, Apache-2.0).
Read `docs/design.md` before doing anything.
It is the source of truth for scope and decisions.
Do not re-litigate settled decisions in it.
If you think one is wrong, say so with evidence from a spike and open an issue.

## Status

Design phase.
No code yet.
17 open issues: `spike:` #1-#6 and `v1:` #7-#17.
Do the spikes before any v1 work.
Spike #1 (dedup ratio on the real corpus) comes first and gates everything else.
Track all work as GitHub issues: `gh-axi issue list --repo zeeshanhaque21/cowfs`.

## Settled decisions (summary)

- Rust core library, OS-agnostic.
- Content-defined chunking (FastCDC, about 64 KiB average), BLAKE3, zstd after chunking, hash verified on read.
- Merkle tree, O(1) writable snapshots, redb metadata, append-only packs, crash-consistent.
- GC is mark-and-sweep from snapshot roots.
  Last-accessed time is a sweep-candidate hint only, never the sole reason to free a block.
- Linux uses FUSE.
  macOS uses an in-process NFS loopback, falling back to FUSE-T.
  macFUSE is avoided.
- One local store per host, shared across repos.
- Single-user semantics, no encryption, no Windows, no shared multi-machine store in v1.
- Treehouse (kunchenguid/treehouse v3.1.0) is the day-1 consumer.
  Mode (a) is transparent.
  Mode (b) is snapshot-native warm-base slots.

## Success criteria

1. Dedup ratio measured on the real corpus before writing filesystem code.
2. Build overhead within 1.5x of native on a representative `cargo build` and on `git status` for a large tree.
3. Zero data loss in crash-injection tests.

## Corpus rules (spike #1)

- The corpus is the treehouse pools under `~/.treehouse/*` plus one Node project.
- Treat the pools as read-only.
  Slots are leased to other agents that are working in them right now.
  Never write to, reset, return, prune, or destroy a slot, and never run `treehouse return`, `prune`, or `destroy`.
- The Mac disk is about 91% full.
  Do not copy the corpus.
  Stream files through the chunker and keep only the chunk index (hashes and sizes) plus totals.
- Files change under you.
  Skip or retry files that vanish mid-read, and record how many.
- Report sample size, what was excluded, and a do-nothing baseline (raw bytes, and bytes after per-file compression only).
  A lexical or inferential shortcut is a hypothesis, not a finding.

## Engineering rules

- Reproduce a bug end to end, the way a real user hits it, before fixing it.
- If a bug survives two fixes, the loop is wrong.
  Build a falsifiable spike instead of a third fix (`debug-spike` skill).
- Before any full, expensive, or batch run, test a representative small sample end to end and validate the real deliverable.
  A component smoke test does not count.
- Fix lint errors and flaky or failing tests when you see them, whoever caused them.
- Weigh quality, simplicity, robustness, and long-term maintainability far above development cost.
- Do not add features, abstractions, or error handling beyond what the task requires.
- Default to no code comments.
  Comment only a non-obvious why, in one short line.
- Verify before any irreversible action (kill, delete, overwrite, drop).
  Check whether a process buffers before killing it.
  Long-running jobs must append and flush per item and resume from a partial file.
- Load generators and other background loops (CPU burners, sleep-then-kill jobs, poll loops) run in the foreground under `trap '...' EXIT`, or write a PID file the next step kills. A tool timeout kills the shell and orphans them; on 2026-10-01 eight orphaned burners ran 9 hours at 100% CPU each.
- Never leave a VM mount or FUSE mount wedged: unmount it and verify with `mount` before ending a turn. A stuck mount can hang the whole OrbStack machine in "stopping".
- Every wait loop must exit on failure as well as success, and must have a no-progress timeout measured in minutes.

## Writing

- Never use an em dash.
  Use a plain dash.
- In long Markdown files, put each full sentence on its own line.
- Never hand-edit `CHANGELOG.md` or any auto-generated file.

## Where files go

- All work stays inside this directory or its treehouse worktree.
  That covers clones, builds, logs, research notes, fixtures, and benchmark output.
- `/tmp` is only for throwaway files deleted in the same step.
- Gitignore large binaries, corpora, and third-party clones.
  Never commit benchmark corpora or store data.
- When delegating, give subagents exact project-relative output paths and restate the rules they need.
  Subagents inherit none of these.

## Git and GitHub

- Never add a `Co-Authored-By` trailer or any agent name as co-author.
- Push WIP commits immediately.
  Do not hold local work waiting on a long build.
- Use `gh-axi` for GitHub.
- The `gh` login is `zeeshanhaque21`.
  The SSH key on this machine authenticates as a different account, so `github.com` over SSH is deliberately blocked.
  Always use HTTPS remotes (`https://github.com/zeeshanhaque21/cowfs.git`).
- After every PR push, run the `ship-aftercare` skill to re-verify the PR body and image links.
- Use treehouse for worktree leases.
  Release a lease only with `treehouse return`.
- After merging PRs or substantial changes, run `detect_changes` so the codebase-memory graph stays fresh.

## Tools

- `codebase-memory` for code exploration, search, and tracing.
  Fall back to grep only for string literals, config values, and non-code files.
- `context-mode` for large or unpredictable output.
- Shell commands go through `rtk`.
  Use `rtk read`, `rtk rg`, `rtk git`, and similar.
- `obscura` for web research.
  `context7` for library documentation.
- Downloads use `aria2c` with `-o <name>`.

## Subagents

- Spawn on Sonnet.
- Every subagent prompt must require: context-mode for large or unpredictable output, codebase-memory-mcp for code exploration, rtk for shell commands, caveman ultra mode for prose, ponytail full mode for code.
- Subagents inherit none of these rules, so restate them in each prompt.
- Give each subagent exact project-relative paths it owns, and forbid touching other files.
- Give the exact operational recipe (server restart, mount, PID verification), not "read the skill".

## Environment

- Dev machine is an Apple M3 Max Mac.
  APFS, macOS 26, about 178 GiB free.
- Linux targets: moonscape (`moonscape@192.168.68.119`, Debian aarch64, btrfs and zfs tools installed) and the cachyos box (`zeeshan@100.122.64.51`, RTX 4060 laptop, may be unreachable).
  Read the `cachyos-gpu` and `environment-traps` skills before using them.
- A Bash timeout kills the whole process group.
  Launch daemons and long jobs so they survive it, and read the `environment-traps` skill first.
