# Verification: daemon `gc` end to end over the real core

Independent verification of PR #76 (`Refs #10`), production tree `crates/` at
`961952e19a389a9c60e2c16190dd354e006664ef` (build head `ea958947b437a089c760b7f4ee381c57702c81d8`).
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
records: 72   failed: 0
harness head:     f170dc177e0792e828dca02ef9adddfaf1665750
production tree:  961952e19a389a9c60e2c16190dd354e006664ef   (crates/ at HEAD)
build head:       ea958947b437a089c760b7f4ee381c57702c81d8   (newest commit touching crates/)
```

The three SHAs are recorded separately on purpose. `harness head` is the commit this script ran
from; `production tree` is the git tree id of `crates/` (the code under test); `build head` is the
newest commit that touched `crates/`, the best available signal for what the shipped binaries were
built from (rustc embeds build paths, so binaries are not byte-comparable across target dirs). The
verification applies to the **production tree** id; it does not claim that any uncommitted working
tree was tested.

Full evidence: `bench/out/gc-daemon-e2e/run6/evidence/records.jsonl` (append, flush, fsync per step),
and an independent clean-checkout run at `bench/out/gc-daemon-e2e/clean-checkout/` (see
Reproduction).

| Step | Proves |
|---|---|
| `selftest.no_daemon_exit3` | a socket no daemon serves gives exit 3 and `{"error":{"code":"not_running"}}` |
| `selftest.kill_refuses_foreign_pid` | the harness refuses to signal a pid that is not this fixture |
| `selftest.long_socket_rejected` | an over-long socket path is rejected with `SUN_LEN`, not silently used |
| `selftest.state_hash_detects_change` | the store-state sensor used by the dry-run gate actually moves when content changes |
| `selftest.blake3_available`, `selftest.digest_detects_corruption` | BLAKE3 is present (a required prerequisite) and the file digests move on a corrupted byte |
| `selftest.seed_source_present` | the tracked seed source (manifest, `Cargo.lock`, `src/main.rs`) is present, so a clean checkout can rebuild it |
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

The fix is a **fixture**, not a product change. `scripts/gc-fixture-seed/` (tracked: `Cargo.toml`,
`Cargo.lock`, `src/main.rs`) uses the supported `cowfs_core`/`cowfs_store` API to build a closed
store with a small `max_pack_size` so many packs seal, a live snapshot, a removed snapshot whose dead
records alone clear the default thresholds, and survivors. It then closes the `Core` fully. The
harness opens that store with the **unmodified** daemon, whose `CoreBackend::open` still uses
`cowfs_core::Options::default()` (256 MiB packs) and `cowfs_gc::Options::default()`.

Measured on run6, one private 16 MiB-pack fixture, unmodified binaries:

```
gc --dry-run   candidate_blocks 368  candidate_bytes 15,136,450  freed_bytes 0   packs unchanged
gc (live)      candidate_blocks 368  candidate_bytes 15,136,450  freed_blocks 202
               freed_bytes (GROSS) 16,759,743
```

### Gross versus net

`freed_bytes` is **gross**: it is the file length of each unlinked pack
(`crates/cowfs-store/src/compact.rs`, `fs::metadata(&path).len()`), and the written contract
(`docs/gc-core-integration.md`) calls it "bytes on disk freed by unlinked packs". It is **not** the
space saved, because the surviving live records are rewritten into a new pack:

| Quantity | Value |
|---|---|
| `freed_bytes` (gross, unlinked pack length) | 16,759,743 |
| physical net drop (on-disk pack total before -> after) | 15,136,450 |
| rewritten pack `pack-00000002.cpk` (now holds the live records) | 1,623,293 |

`gross - net = 1,623,293 = new_pack_bytes`, asserted by `reclaim.gross_minus_net_equals_rewrite`.
The net drop equals `candidate_bytes` (15,136,450), the dead bytes that were genuinely freed.

Note this also means `freed_bytes` (16.76 MB) exceeds the documented "most a cycle can free"
`candidate_bytes` (15.14 MB). Under gross accounting that bound does not hold. This is a
**semantics defect in the product, not in this fixture**: the control-plane `GcReport` carries no
`bytes_copied`, so a caller cannot net `freed_bytes` out. It is out of scope here (no production
change) and should be fixed by reporting net, or adding a `bytes_copied` field, in a follow-up
against PR #76.

| Check | Evidence |
|---|---|
| `reclaim.dry_run_reports_candidates` | `candidate_bytes = 15,136,450` (> 0) |
| `reclaim.dry_run_no_pack_change` | store content hash equal before and after the dry run (independent samples), pack sizes unchanged |
| `reclaim.dry_run_mutation_detected` | the same sensor moves when one byte under the store is changed (negative control) |
| `reclaim.gc_freed_bytes_positive` | gross `freed_bytes = 16,759,743`, `freed_blocks = 202`, `dry_run = false` |
| `reclaim.gross_minus_net_equals_rewrite` | `gross 16,759,743 - net 15,136,450 = 1,623,293 = new_pack_bytes` |
| `reclaim.physical_pack_bytes_dropped` | `pack-00000000.cpk` (16,759,743 B) **unlinked**; on-disk total 28,964,749 -> 13,828,299, net delta 15,136,450 |
| `reclaim.referenced_snapshot_untouched` | the `live` snapshot's files sit in the candidate pack and are preserved: SHA-256 unchanged and BLAKE3 matches the fixture |
| `reclaim.survivors_unchanged_after_gc` | `keep000`/`keep001` readback unchanged (SHA-256, both sides) and BLAKE3 equals the fixture-declared digest |
| `reclaim.fsck_clean` | after the reclaim: `ok = true`, `problems = []`, 193 blocks checked |
| `reclaim.reopen_survivors_match_source` | after shutdown and reopen on a **fresh daemon and new mount path**, all four files (`keep000`/`keep001` and the rewritten-pack `live000`/`live001`) match their SHA-256 and fixture BLAKE3 |

The BLAKE3 cross-check is a **required** prerequisite: `blake3_file` raises if the python `blake3`
module is missing, and the harness records `selftest.blake3_available`, so an absent module fails
loudly instead of silently passing a weaker check. The reopen check re-reads the `live` files, which
are the ones gc actually copied into the rewritten pack, on a new mount path so the bytes cannot be
served from the first mount's NFS client cache.

The `live` snapshot is the negative control: it is written **before** the dead payload, so its blocks
land in the same pack as the dead records. A reclaim that freed referenced data would corrupt them.
They read back byte-identical instead, and their BLAKE3 equals the fixture hash.

The fixture's `max_pack_size` (16 MiB) is the only non-default knob, and it is a supported
`cowfs_store::Options` field set by the fixture's own private crate. The production default of
256 MiB is left untouched. The daemon always opens with the real default (`CoreBackend::open` passes
`cowfs_core::Options::default()` and `cowfs_gc::Options::default()`), so a real 256 MiB-default store
behaves the same way once it has a sealed, dead-dominated pack. This report does not claim to have
grown a store to 256 MiB organically.

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
- No real-core `gc` cancellation. The cancel path above is control-plane only.
- No fix for the gross `freed_bytes` accounting (`candidate_bytes` bound and missing `bytes_copied`);
  recorded here as a product semantics gap, fixed in a follow-up, not in this verification.
- No organic growth to the production 256 MiB default pack size. The fixture seals 16 MiB packs; the
  daemon still opens with the real default options.
- No production edits to `cowfs-core`, `cowfs-gc`, `cowfs-daemon`, root manifests, or workflows.

## Reproduction

```sh
cargo build -p cowfs-cli -p cowfs-daemon -p cowfs-gc
python3 scripts/verify-gc-daemon.py
```

The seed source is **tracked** under `scripts/gc-fixture-seed/` (manifest, `Cargo.lock`,
`src/main.rs`), so a clean checkout can rebuild it: the harness runs
`cargo build --release --locked --offline --manifest-path scripts/gc-fixture-seed/Cargo.toml` on
first run and records the seed source SHA-256 and the helper binary SHA-256. `--offline` uses the
local cargo registry cache; if a pinned dependency is not cached the build fails and the harness
records the missing prerequisite rather than fetching. The python `blake3` module is required
(`python3 -m pip install blake3`).

### Clean-checkout proof

Exported the committed tree (`git archive <head> | tar -x`) into a fresh directory, confirmed the seed
source is present from tracked files only, and built it `--locked --offline` (49s). Running the
harness from that clean tree against the same production binaries gave `72/72 (0 failed)` with
identical reclaim numbers (`gross 16,759,743 - net 15,136,450 = 1,623,293`; `pack-00000000.cpk`
unlinked; all four reopen files matching). Artifacts under
`bench/out/gc-daemon-e2e/clean-checkout/` (gitignored).

Expected output ends with `"failed": 0`. Evidence is written under
`bench/out/gc-daemon-e2e/run/evidence/`.
