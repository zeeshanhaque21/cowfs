# Evidence for issue #90, NFS namespace durability

Tracked proof for PR 96.
Every number below came from a run of the harness whose blob is listed under `source-binding`,
against the fixture binaries whose sha256 is listed beside it.
The raw per-rep logs are gitignored and stay local under `bench/out/`; they are named as paths,
not quoted as though they were public.

## commit

```
c6445476e6c9882e975638f2e6d829a7dd304443
```

`origin/main` was `46b0f269d5bef4a2c204c25f5b3015da601d3beb` when this branch merged it.
`ceb96c67033cbf97f79267d6af7db3fa204d77d1`, the real merge-base, is an ancestor of this head.

## source-binding

Blob ids at that commit, for the files the receipts depend on.

| file | blob |
|---|---|
| `crates/cowfs-daemon/tests/namespace_durability.rs` | `24c8d59d909a84ea8d67d1782ea59be47a1a2482` |
| `crates/cowfs-daemon/tests/namespace_durability_gate.rs` | `1ee68f6ae230e78b1bc2703d3ebbc438017d7acd` |
| `crates/cowfs-nfs/src/adapter.rs` | `b2b54c4b210eca7ee425e6de530728e2f4cabff0` |
| `crates/cowfs-core/src/inner.rs` | `ce9e2aabbfff0c72dd32c07414b1d183e7beec74` |
| `crates/cowfs-core/src/ns.rs` | `5a1024c115d51806131c6f6cc9ff99f5f20e6588` |
| `crates/cowfs-core/tests/elide_dentry.rs` | `1df33880c23b6250fde053673e8a4048994c7b05` |
| `crates/cowfs-core/tests/model.proptest-regressions` | `caadf6bac71585c0fa4cffd95e25ebec34db194f` |

The last two are the issue 94 fix that came from `main`, byte-identical to `main`.
The elide regression seed `9aa30bfa88a2438194d3b5ae7af55c7e2a59ff8a233abfe1cd0ddbec9d213900` is
still in `model.proptest-regressions` and still re-run by `cargo test -p cowfs-core --test model`.

| fixture binary | sha256 |
|---|---|
| `target/debug/cowfs-daemon` at this commit | `cbb755b7d47a6bbd209db6014f666941543775949aa133d2f7c6b810641b7e3d` |
| `target/debug/cowfs` at this commit | `94570994234b6bb9720c5ab529145257485dcd9d8798a2c5811652e53b8acbfc` |
| `target/debug/cowfs-daemon` with every barrier removed | `9fda2a955845af00219310ac57f3374d37f40100bd50437730928fefaab9c5f2` |

The pre-fix column was produced by the same harness against the third binary, built with every
barrier call removed from `adapter.rs`.
Removing those call sites is exactly the pre-fix behaviour, because nothing else reaches
`sync_namespace`.

## Recipe

One rep is a private store under `bench/out/durability90/`, a real `cowfs-daemon --backend
core`, the real `cowfs` CLI, a real NFSv3 loopback mount, and a snapshot created through the
control socket.
The file is written and `fsync`ed before the rename, so the only uncommitted thing at the kill
is the rename itself.
The caller then does what its row says, `SIGKILL` reaches a pid this harness started 2 to 6 ms
later, and the same store is reopened by a fresh daemon on a fresh mount.
A rep passes only when the new name reads back with the same bytes, the old name is gone, and
`cowfs fsck` reports clean.

## Results

| after `rename` | before the fix | at this commit |
|---|---|---|
| `fsync-parent-dir` | new name lost in 3/3 | kept 3/3 |
| `fsync-read-only-fd` | new name lost in 3/3 | kept 3/3 |
| `no-sync-at-all` | new name lost in 3/3 | kept 3/3 |
| `write-fsync-sibling` | new name lost in 0/3 | kept 3/3 |

Every rep on both sides ran `cowfs fsck` and every one reported clean.

```
ok: 2 blocks (382 B), 1 snapshots checked
ok: 3 blocks (446 B), 1 snapshots checked
```

The `write-fsync-sibling` row is the control the issue measured as already working.
It passes on both sides, which is what makes the other three a client problem and not a store
problem: the rename was always committable on this daemon, and the syncs the issue names simply
never crossed the wire.
The `no-sync-at-all` row is a bare rename with no caller sync at all, which the old transport
lost and this one keeps, because the acknowledgement is now the barrier.

## Independent critic

Critic slot 6 ran its own harness on its own tree and reported 24 readbacks: 24 clean `fsck`, 15
keeping the new name, 9 with the old name back.
Its per-case verdicts were `fsync-parent-dir` 0/3 then 3/3, `fsync-read-only-fd` 0/3 then 3/3,
`no-sync-at-all` 0/3 then 3/3, and the dirty-write control 3/3 on both sides.

Source: `docs/reviews/namespace-durability90-final.md` in critic slot 6's worktree, read in
full.
Its harness was not reused here and its verdicts were not inherited.
The two runs agree on the direction and differ on pids and absolute timings, as two hosts should.

## Native APFS control, and what it does not show

The same recipe on this host's own filesystem, with the worker `SIGKILL`ed instead of a daemon:
the new name survives.

**That is not offered as a durability result.**
A process kill does not touch the page cache, so APFS keeps an unsynced rename and this control
cannot fail.
What it establishes is narrower and worth stating exactly: the recipe, the readback, the byte
comparison and the cleanup are sound, so a name lost on the cowfs side is the filesystem losing
it and not the harness looking in the wrong place.

## Limits

- `SIGKILL` is a process crash, not power loss.
  Every readback above is equally consistent with the host page cache.
  This shows the name survived a process death, not that the bytes reached the platter.
- The cost figures in `docs/nfs-namespace-durability90.md` are one observed distribution from
  one busy host in a debug build.
  They are not an upper bound and not a benchmark.
- `docs/design.md` success criterion 2, build overhead within 1.5x of native, is not measured
  here and stays open.
- Mid-`gc` crash, concurrent writers, reclamation, and `shutdown` as a crash boundary are not
  sampled by any of these runs.

## Raw logs, local only

Gitignored, on the machine that produced them, not public proof:

- `bench/out/durability90/results.jsonl`: one line per rep at this commit, appended and flushed
  per rep.
- `bench/out/durability90-repair/results-baseline.jsonl`: the same harness against the pre-fix
  daemon.
- `bench/out/durability90-repair/repair-*.log`: one full run per case at this commit.
- `bench/out/durability90-repair/prefix-*.log`: the same cases against the pre-fix daemon.
- `bench/out/durability90-repair/gate-*.log`: the CI gate runs, including the two deliberate
  mutations that fail it.

