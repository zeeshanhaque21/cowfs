# Verification: daemon `gc` end to end over the real core

Independent verification of PR #76 (`Refs #10`), head `ea958947b437a089c760b7f4ee381c57702c81d8`.
This report is produced by `scripts/verify-gc-daemon.py` and is not part of the production change.

## What was verified, and by what

The harness drives the **real deployed user path**, not the library API:

- `cowfs-daemon --store <private> --mount <private> --socket <private> --backend core`
- `cowfs --socket <private> --json status | import | snapshot rm | gc [--dry-run] | fsck | shutdown`

Private fixtures only: store and mount live under `bench/out/gc-daemon-e2e/` (gitignored), the
control socket lives in a short, mode-0700 directory under `/private/tmp` because the leased
worktree path exceeds macOS `sun_path`. No shared socket, store, mount, lease, or workload is
touched. The shared daemon on `~/.cowfs` is never addressed; the harness refuses to signal any pid
whose command line does not carry this run's exact socket and store.

## Result

```
records: 37   failed: 0
head:    ea958947b437a089c760b7f4ee381c57702c81d8
```

Full evidence: `bench/out/gc-daemon-e2e/run/evidence/records.jsonl` (append, flush, fsync per step).

| Step | Proves |
|---|---|
| `selftest.no_daemon_exit3` | a socket no daemon serves gives exit 3 and `{"error":{"code":"not_running"}}` |
| `selftest.kill_refuses_foreign_pid` | the harness refuses to signal a pid that is not this fixture |
| `selftest.long_socket_rejected` | an over-long socket path is rejected with `SUN_LEN`, not silently used |
| `daemon.ready` + `mount.is_nfs` | a private daemon mounts a private store over the macOS NFS loopback, no privileges |
| `import.keep.verified`, `import.drop.verified` | two deterministic incompressible trees ingest through the real writer and verify by hash |
| `mount.survivor_matches_source_before_gc` | the survivor read through the mount equals the **source** file hash taken on disk |
| `status.two_snapshots`, `status.one_snapshot_after_rm` | snapshot count follows import and `snapshot rm` |
| `gc.dry_run`, `gc.dry_run.no_pack_change` | dry run reports candidates, frees nothing, and rewrites/unlinks no pack data |
| `gc.live_run` | live gc answers with a `GcReport` |
| `gc.observed_numbers` | records the real candidate/freed numbers (see below) |
| `mount.survivor_matches_source_after_gc` | the survivor is byte-identical after both gc runs |
| `fsck.clean` | `fsck` over the core backend is clean over the surviving snapshot |
| `cancel.sigint_exit_130_bounded` | SIGINT to a running `gc` client exits 130 in bounded time |
| `cancel.progress_frame_seen` | real JSON progress frames (`{"progress":{...phase:"mark"...}}`) were emitted |
| `cancel.daemon_survives` | the daemon stays healthy after a cancelled client |
| `path_backend.gc_unsupported` | a private non-store backend answers `unsupported` for `gc`, exit 1 |
| `shutdown.*`, `reopen.*` | the private daemon shuts down, its mount is gone, the exact store reopens in a fresh daemon, and the survivor still matches source |

## The honest limit: no byte reclaim from a single active pack

Over the real CLI/daemon, `cowfs-daemon --backend core` opens the store with
`cowfs_core::Options::default()` and `cowfs_gc::Options::default()`. The defaults are
`min_dead_bytes = 8 MiB` and `dead_ratio = 0.5`. `CoreBackend::run_gc` passes them straight to
`Core::collector`.

A store produced by a few-MiB fixture is a **single pack**, and `cowfs-gc` skips the active pack and
every pack at or above the mark epoch before it ever checks the dead threshold:

```rust
// crates/cowfs-gc/src/lib.rs, candidate pass
if info.active || Some(info.id) >= epoch.map(|(p, _)| p) {
    r.skip(info.id, SkipReason::Active);
    continue;
}
if plan.dead_bytes < self.opts.min_dead_bytes
    || plan.dead_ratio() < self.opts.dead_ratio {
    r.skip(info.id, SkipReason::BelowThreshold);
    continue;
}
```

`candidate_bytes` is `plan.record_bytes()` of *eligible* pack plans, so it is structurally 0 when the
only pack is active. Concretely, in this run:

```json
{"candidate_blocks":41,"candidate_bytes":0,"dry_run":false,"freed_blocks":0,"freed_bytes":0}
```

`candidate_blocks` (41) comes from `store_blocks - live_blocks` and is independent of pack
eligibility, so the report can show dead blocks while `candidate_bytes` stays 0. A larger probe
(18.5 MiB store, 134 dead blocks in the one active pack) gave the same shape: `candidate_bytes: 0`,
nothing freed.

The library-level reclamation *is* proven, but only under injected options the CLI cannot set:
`crates/cowfs-daemon/src/handler.rs::gc_over_the_core_reclaims_dead_packs_and_survivors_still_read`
uses `max_pack_size: 96 << 10`, `dead_ratio: 0.0`, `min_dead_bytes: 1` so that many small packs
exist and none of them is the epoch pack. That is reachable from `CoreBackend::open_with_gc`, which
the CLI and the daemon binary never call.

So the verified statement is:

- The daemon `gc` request **is wired and answers correctly** over the real core: dry run, live run,
  progress frames, cancel, `busy`/`io_error` discipline, and survivor readback all behave.
- The user-facing binary **cannot free bytes from a small single-pack store**, and `candidate_bytes`
  reads 0 for such a store. That is a real gap in the CLI/daemon path, and it is a finding, not a
  pass. Closing it needs either a daemon-reachable knob for the collector thresholds and a pack
  roll, or a collector rule that can rewrite the active pack safely - a production decision, out of
  scope here.

## What was deliberately not done

- No throughput or timing claim. No benchmark.
- No claim about the collector's sweep semantics from the stub backend. The stub cancel test proves
  only the client signal -> cancel frame -> exit-130 lifecycle.
- No full-stack crash injection. The daemon was never SIGKILLed in this pass.
- No production edits to `cowfs-core`, `cowfs-gc`, `cowfs-daemon`, root manifests, or workflows.

## Reproduction

```sh
cargo build -p cowfs-cli -p cowfs-daemon -p cowfs-gc
python3 scripts/verify-gc-daemon.py
```

Expected output ends with `"failed": 0`. Evidence is written under
`bench/out/gc-daemon-e2e/run/evidence/`.
