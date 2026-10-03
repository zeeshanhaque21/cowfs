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
records: 63   failed: 0
head:    99c7c23f1cbbffa80fd18bf5488605c7c61498fa
```

Full evidence: `bench/out/gc-daemon-e2e/run4/evidence/records.jsonl` (append, flush, fsync per step).

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
| `gc.observed_numbers` | records the real candidate/freed numbers on a small single-pack store (see below) |
| `mount.survivor_matches_source_after_gc` | the survivor is byte-identical after both gc runs |
| `fsck.clean` | `fsck` over the core backend is clean over the surviving snapshot |
| `cancel.sigint_exit_130_bounded` | SIGINT to a running `gc` client over the stub exits 130 in bounded time |
| `cancel.progress_frame_seen` | real JSON progress frames (`{"progress":{...phase:"mark"...}}`) were emitted |
| `cancel.daemon_survives` | the daemon stays healthy after a cancelled client |
| `path_backend.gc_unsupported` | a private non-store backend answers `unsupported` for `gc`, exit 1 |
| `reclaim.*` | **actual byte reclaim** on a seeded store with sealed eligible dead packs, over the unmodified binary (see below) |
| `shutdown.*`, `reopen.*` | the private daemon shuts down, its mount is gone, the exact store reopens in a fresh daemon, and the survivor still matches source |

## Actual reclamation through the unmodified user path

The acceptance this task set is a real reclaim: `freed_bytes > 0`, a physical pack drop, and every
survivor still byte-identical, driven by the **unmodified** `cowfs-daemon`/`cowfs` binaries with
their own default options.

The obstacle is that the deployed thresholds (`cowfs_gc::Options::default()`: `min_dead_bytes`
8 MiB, `dead_ratio` 0.5) and the skip rules (active pack and every pack at or above the mark epoch
are skipped before the threshold check) mean a store whose only pack is the active one has
`candidate_bytes` 0 by construction. No CLI flag can change that.

The fix is a **fixture**, not a product change. `bench/out/gc-daemon-e2e/seed-crate/` (private,
gitignored) uses the supported `cowfs_core`/`cowfs_store` API to build a closed store with a small
`max_pack_size` so many packs seal, a live snapshot, a removed snapshot whose dead records alone
clear the default thresholds, and survivors. It then closes the `Core` fully. The harness opens that
store with the **unmodified** daemon, whose `CoreBackend::open` still uses
`cowfs_core::Options::default()` (256 MiB packs) and `cowfs_gc::Options::default()`.

Measured on run4:

```
gc --dry-run   candidate_blocks 368  candidate_bytes 15,136,450  freed_bytes 0   packs unchanged
gc (live)      candidate_blocks 368  candidate_bytes 15,136,450  freed_blocks 202  freed_bytes 16,759,743
```

| Check | Evidence |
|---|---|
| `reclaim.dry_run_reports_candidates` | `candidate_bytes = 15,136,450` (> 0), `packs_unchanged = true` |
| `reclaim.gc_freed_bytes_positive` | `freed_bytes = 16,759,743`, `freed_blocks = 202`, `dry_run = false` |
| `reclaim.physical_pack_bytes_dropped` | `pack-00000000.cpk` (16,759,743 B) **unlinked**; on-disk total 28,964,749 -> 13,828,299, delta 15,136,450 |
| `reclaim.referenced_snapshot_untouched` | the `live` snapshot's files sit in the candidate pack and are preserved: `unchanged = true` and BLAKE3 matches the fixture |
| `reclaim.survivors_unchanged_after_gc` | `keep000`/`keep001` readback unchanged (SHA-256, both sides) and BLAKE3 equals the fixture-declared digest |
| `reclaim.fsck_clean` | after the reclaim: `ok = true`, `problems = []`, 193 blocks checked |
| `reclaim.reopen_survivors_match_source` | after shutdown and reopen of the exact store, survivors still match source |

The `live` snapshot is the negative control: it is written **before** the dead payload, so its blocks
land in the same pack as the dead records. A reclaim that freed referenced data would corrupt them.
They read back byte-identical instead, and their BLAKE3 equals the fixture hash.

The fixture's `max_pack_size` (16 MiB) is the only non-default knob, and it is a supported
`cowfs_store::Options` field set by the fixture's own private crate. The production default of
256 MiB is left untouched. A production user store is built by the same writer over time and packs
seal by the same rule, so a store with at least one sealed, dead-dominated pack reclaims exactly as
this fixture does; the threshold values, the dry-run safety, and the default pack size are not
weakened or bypassed.

## The observed limit: no byte reclaim from a small single-active-pack store

This section records a real observation, not a gate. On the small fixture (a few MiB, one pack),
the deployed thresholds and skip rules leave `candidate_bytes` at 0:

```json
{"candidate_blocks":41,"candidate_bytes":0,"dry_run":false,"freed_blocks":0,"freed_bytes":0}
```

`candidate_blocks` (41) comes from `store_blocks - live_blocks` and is independent of pack
eligibility, so the report can show dead blocks while `candidate_bytes` stays 0. A larger probe
(18.5 MiB store, 134 dead blocks in the one active pack) gave the same shape. The cause is
`crates/cowfs-gc/src/lib.rs`:

```rust
// candidate pass
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

This is expected for a single active pack and is not a defect: the collector refuses to rewrite the
pack it is actively appending to. It is recorded as an observation so a future product change would
not fail the harness, and it does not contradict the actual-reclaim result above, which uses a
store that has sealed eligible packs.

## Cancellation: what is proven, and where

The CLI cancel path (`cancel.*`) runs against the **stub** backend. It proves the control-plane
lifecycle only: client SIGINT -> `{"error":{"code":"cancelled"}}` -> exit 130 in bounded time, with
real JSON progress frames, and the daemon surviving. It says nothing about the real collector's
sweep or its cancellation under the core backend. That gap is stated here rather than papered over.

## What was deliberately not done

- No throughput or timing claim beyond the bounded cancel latency. No benchmark.
- No claim about the collector's sweep semantics from the stub backend.
- No full-stack crash injection. The daemon was never SIGKILLed in this pass.
- No production edits to `cowfs-core`, `cowfs-gc`, `cowfs-daemon`, root manifests, or workflows.

## Reproduction

```sh
cargo build -p cowfs-cli -p cowfs-daemon -p cowfs-gc
python3 scripts/verify-gc-daemon.py
```

The harness builds the private seed helper on first run
(`bench/out/gc-daemon-e2e/seed-crate/`, gitignored). Expected output ends with `"failed": 0`.
Evidence is written under `bench/out/gc-daemon-e2e/run/evidence/`.
