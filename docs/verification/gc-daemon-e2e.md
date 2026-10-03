# Verification: daemon `gc` end to end over the real core

Independent verification of PR #76 (`Refs #10`), production tree `crates/` at
`fe899f9f425d40d5db36d13c8400ef3438676879` (build head `f5011574383dbe4d65bd2a5995cfff9b2d168b3d`,
a recorded proxy for the binary origin, not cryptographic proof).
This report is produced by `scripts/verify-gc-daemon.py` and is not part of the production change.

This is the **current** production head: `f501157` builds on `89b202c`, which built on `69ae448`, which
replaced `ea958947` (the head that carried the PR #76 root-walk identity data-loss race). The
verification below was re-run against `f501157` on the actual committed crate tree.

This report makes **no claim that the production GC is safe**, and no claim about the `f501157`
lookup/walk race beyond what is recorded here. Source safety is separately and independently reviewed
(in parallel, native, pending); this document reports only the end-to-end behavior it measured. The
`f501157` head must pass that source review before this PR is rebased or merged.

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

Current run (head `7c08311`, production crates `f501157`):

```
records: 72   failed: 0
env.harness_head (actual run label): 7c083110fd06079ab7688c7a6d3a14116fbfcbc1
production tree:                      fe899f9f425d40d5db36d13c8400ef3438676879   (crates/ at HEAD)
build head (recorded proxy):          f5011574383dbe4d65bd2a5995cfff9b2d168b3d   (newest commit touching crates/)
script sha256:                        a51259070b54bb852e7de5a16bf492fa5ecc8a81672c12292492e53470c0bff2
binaries:  cowfs fcc58209c48cbfc2ea9b3e57dd55b588658992e3c85e406260abf1a40a10217a
           daemon bbf278481199488bbb03b201a47e1e5d3fb20a83fa201e99eddd4ea07ae92989
```

Built from this exact committed crate tree with `cargo build -p cowfs-cli -p cowfs-daemon -p cowfs-gc`
into the worktree's own `target/` (isolated by worktree, not a borrowed target). The `harness_head`
label `7c08311` is the commit this script actually ran from; `build head` is a **recorded proxy**, not
cryptographic proof the binary came from exactly that tree (rustc embeds build paths). The
verification applies to the **production tree** id `fe899f9f`. The harness script sha256
(`a5125907...`) is unchanged across the `4b9044a`, `42e9182`, and `7c08311` runs, so the same harness
produced all three.

Full evidence: `bench/out/gc-daemon-e2e/run-f501157/evidence/records.jsonl` (append, flush, fsync per
step).

### Prior run (`89b202c`, superseded, kept for provenance)

The same harness was previously run against `89b202c` (`production tree 73a22952`, head label
`42e9182`), producing the same fixture numbers; evidence at
`bench/out/gc-daemon-e2e/run-89b202c/evidence/records.jsonl`. That head carried the marks-cache
version fix (`COWMARK1` -> `COWMARK2`); `f501157` adds the lookup/walk `NoSuchSnapshot` skip and
retains the cache versioning.

### Prior run (`69ae448`, superseded, kept for provenance)

The same harness was previously run against `69ae448` (`production tree f4f54aee`, head label
`4b9044a`), producing the same fixture numbers. That run is superseded by the current one but not
rewritten; evidence at `bench/out/gc-daemon-e2e/run-safe-head/evidence/records.jsonl`.

### Historical run6 (superseded, kept for provenance)

An earlier run was made against the now-unsafe production head `ea958947`:

```
records: 72   failed: 0
env.harness_head (stale label):  f170dc177e0792e828dca02ef9adddfaf1665750
script sha256 (content identity): a51259070b54bb852e7de5a16bf492fa5ecc8a81672c12292492e53470c0bff2
committed blob at 8cab23a:        e5e275a550a4f0cfa6de7951dee673d27cb0647c
production tree:                  961952e19a389a9c60e2c16190dd354e006664ef   (crates/ at HEAD, == ea958947)
build head:                       ea958947b437a089c760b7f4ee381c57702c81d8   (newest commit touching crates/)
```

The `env.harness_head` label inside run6 is `f170dc1`, the **parent** of the commit that carried
those bytes. The script `sha256` `a5125907...` equals the `8cab23a` blob
(`git rev-parse 8cab23a:scripts/verify-gc-daemon.py` = `e5e275a5...`), so that run is bound by content
to the committed script even though the recorded label is stale. The JSONL evidence is historical and
is **not** rewritten. `ea958947` is the unsafe head and its numbers are superseded by the current run;
they are retained only to show the same fixture behavior before the fix.

`production tree` is the git tree id of `crates/` (the code under test); `build head` is the newest
commit that touched `crates/`. `build head` is a **recorded proxy**, not cryptographic proof that the
binary was built from exactly that tree (rustc embeds build paths, so binaries are not
byte-comparable across target dirs); the independent reviewer's fresh build from the clean archive is
the separate source-and-behavior proof. The verification applies to the **production tree** id; it
does not claim any uncommitted working tree was tested.

Historical full evidence: `bench/out/gc-daemon-e2e/run6/evidence/records.jsonl`. An independent
reviewer's clean-archive run against the **committed** script is at
`bench/out/gc-e2e-repairs-critic/archive/bench/out/run-clean/` (72/0); the builder's earlier
clean-checkout run at `bench/out/gc-daemon-e2e/clean-checkout/` used the `f170dc1` script (71/0). See
Reproduction.

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

All numbers in this section are from the **current run** (`harness_head 7c08311`, production crates
`f501157`); the prior `89b202c` and `69ae448` runs and the historical run6 on `ea958947` produced the
same fixture numbers.

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

Measured on the current run (and run6), one private 16 MiB-pack fixture, unmodified binaries:

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
change) and tracked as issue #81 (fixed by reporting net, or adding a `bytes_copied` field), separate
from PR #76's blocking root-walk race.

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
  tracked as issue #81, separate from PR #76's blocking race, not fixed in this verification.
- No organic growth to the production 256 MiB default pack size. The fixture seals 16 MiB packs; the
  daemon still opens with the real default options.
- No production edits to `cowfs-core`, `cowfs-gc`, `cowfs-daemon`, root manifests, or workflows.
- No Linux harness run; the harness is macOS-only here.
- **No certification of the `f501157` lookup/walk race.** This harness uses a **quiescent** fixture: it
  does not remove a snapshot concurrently with a walk. `f501157` adds a skip for `NoSuchSnapshot`
  raised in the lookup->walk window (and propagates every other error). Verifying that concurrent-vanish
  counterexample, and the old-cache-poison upgrade path, is the separate source reviewer's scope; this
  document reports only that the current quiescent run is clean on this head.
- **No `test-hooks` feature upstream; hook source unchanged across heads.** `grep test-hooks` over the
  `f501157` tree returns nothing, `crates/cowfs-core/Cargo.toml` has no `[features]`, and `cowfs-gc`
  depends on `cowfs-core` only as a `[dev-dependencies]` entry. The hook-relevant source (`gate.rs`,
  `gc_barrier_window.rs`, `fsops.rs`, both `Cargo.toml`s, `store/src/fsio.rs`) is **byte-identical**
  between `89b202c` and `f501157`, so the earlier external compile proof carries on this head.
- **The `test-hooks` feature is gone and the hazardous gate seam is `cfg(test)`-only (on `89b202c`
  and unchanged on `f501157`).**
  The earlier `69ae448` build had the `cowfs-core` `test-hooks` feature unified on (because
  `crates/cowfs-gc/Cargo.toml` depended on `cowfs-core` with that feature and the daemon links
  `cowfs-gc`), so the fail-open barrier-removal seam was compiled into the daemon. On `89b202c` this
  is repaired: `crates/cowfs-core/Cargo.toml` has no `[features]` and no `test-hooks`, `cowfs-gc`
  depends on `cowfs-core` only as a `[dev-dependencies]` entry, and `grep test-hooks` across the tree
  returns nothing. The gate seam is gated on `#[cfg(test)]` (`crates/cowfs-core/src/gate.rs:12`,
  `gc_barrier_window.rs`), so `remove_barrier` and the parked-thread probe are **absent** from the
  production daemon binary. A residual `crates/cowfs-core/src/fsops.rs` seam remains: it is `pub
  #[doc(hidden)]`, always compiled, and holds global process state, so it is **callable by an
  in-process library embedder** (its `set_fault`/`arm`/`disarm`/`trace_take` compile from an external
  crate). It is **not remotely invokable** through this daemon: `fsops` has no reference in
  `cowfs-daemon`, `cowfs-ctl` or `cowfs-cli`, and the control protocol has no fault or arm verb; in
  the daemon's and any production path it is simply never armed. It is **not** the fail-open barrier
  seam: an armed fault makes `sync_file`/`sync_dir` return errors. One path, `crates/cowfs-core/src/swap.rs:57`,
  discards a directory-sync error (`let _ = ...sync_dir`), so an armed fault there is swallowed rather
  than surfaced; this is a static source observation and no new risk was reproduced. The string
  `injected sync fault on` is present in the binary because the module always compiles. Stated this
  way rather than as universal production unreachability: the correct claim is "not invoked by the
  daemon or any production path; public library API".

## Known nonblocking issues (not fixed this pass)

- `gc.dry_run.no_pack_change_negative_control` is mislabeled: it only re-reads the state hash and
  confirms stability across a second identical read, which is not a negative control. The real
  mutation control is `reclaim.dry_run_mutation_detected`.
- Early-failure leak: `main()` creates `/private/tmp/cowfs-gc-e2e-<pid>/` before `selftest()` and
  removes it only in a later `finally`, so a failing selftest (for example a missing `blake3` module)
  can leave a persistent `/private/tmp` directory.

Both are harness hygiene, not evidence-integrity defects. They are not fixed in this pass so the
72-record run above is not invalidated by a script change; fixing them requires its own regression
proof and re-run.

## Combined-graph caveat (documentation only, not this harness's build)

A static reviewer observed that `cargo check --workspace --all-targets` reports the **non-test**
`cowfs-store` lib feature `fault-injection` in the daemon-bin compile graph. Cause, confirmed here:
`crates/cowfs-store/Cargo.toml` carries a **self dev-dependency**
(`cowfs-store = { path = ".", features = ["fault-injection"] }`), and the workspace is
`resolver = "2"`. When a workspace/`--all-targets` invocation brings dev-dependency edges into the
graph, that feature is unified onto the **non-test** store lib (`cargo tree -p cowfs-store -e
features` shows `cowfs-store feature "fault-injection"` active even for the lib). The seamed code is
`#[cfg(feature = "fault-injection")]` (`crates/cowfs-store/src/fsio.rs`), reads `C7D_EXIT_FILE` /
`C7D_EXIT_LEN` / `C7D_EXIT_SYNC_N` / `C7D_EXIT_BOUNDARY_N`, and calls `std::process::exit(77)`.
Facts, stated not overclaimed:

- It is **pre-existing** (the store manifest is unchanged by `89b202c`) and is **not the barrier hook
  and not the `fsops` seam**.
- It is **feature-gated, not target-gated**: any build that excludes dev-dependency edges (the plain
  `cargo build -p cowfs-cli -p cowfs-daemon -p cowfs-gc` binaries this harness ran) omits it, and
  `C7D_EXIT` appears **0** times in those daemon binaries. This document's proof is the **default
  daemon build**; the `--all-targets`/workspace fault configuration is a **different graph** and was
  **not built or executed** here.
- It was read from cargo's resolved feature graph and source, **not** from an independently executed
  binary in that configuration, and **no new data-loss claim** is made from it.
- It is a crash-injection seam, not a reference-barrier fail-open.

## Independent source-side notes (separate reviewer, scope stated)

A fresh native reviewer independently reproduced this harness's behavior on `89b202c`: a small
14-record actual-reclaim run and the full 72-record run, both 0 failed, with all four reopen files
matching independently regenerated fixture digests on a fresh NFS mount, plus compile-level proof
that `Core::set_gate_fault` is not reachable (`E0599`), the gate module is private (`E0603`), and no
`test-hooks` feature exists. That review certifies **artifact and end-to-end behavior only**; it
explicitly does **not** certify production source safety, which remains the separate source critic's
gate. The review report is a local, untracked artifact (`docs/reviews/gc-daemon-final-source.md`), not
durable GitHub evidence; it is not linked as a URL. That review predates `f501157`; a targeted
re-review of the `f501157` race fix is required and is not claimed here.

## Source fixes these heads carry (not exercised by this harness)

`89b202c` bumps the marks-cache format magic `COWMARK1` -> `COWMARK2`, so a `mark.bin` written by a
pre-fix collector is never trusted and every root is walked in full (the fail-open root-walk cache
hazard). This harness always builds a **fresh** store per run and never reads or writes a marks cache,
so a legacy `COWMARK1` file cannot arise here and no harness cache-format compatibility change is
needed. Verifying the cache-version logic itself is the source critic's scope, not this end-to-end
harness; this document makes no claim about it beyond recording that the source carries it.

`f501157` adds the lookup/walk `NoSuchSnapshot` skip and its regression tests (bounded race fixtures,
`Refs #83`). This harness's fixture is **quiescent** and never removes a snapshot during a walk, so it
does not exercise that window; the skip, its propagation of all other errors, and the bounded race
fixtures are the source critic's scope.

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

Current head `7c08311`: exported the committed tree (`git archive 7c08311 | tar -x`) into
`bench/out/gc-daemon-e2e/run-f501157/clean-archive/`, confirmed the seed source is present from
tracked files only, and built it `--locked --offline` (76s, reusing the local registry cache). The
three tracked seed files hash identically in the archive and the worktree
(`Cargo.toml` `629b6746...`, `Cargo.lock` `275e7f2c...`, `src/main.rs` `157430268d...`), so the seed
is reproducible from tracked source alone on this head. Only `scripts/gc-fixture-seed/target/` is
ignored; there is no ignored seed **source** dependency.

Earlier heads `42e9182` (crates `89b202c`) and `4b9044a` (production crates `69ae448`) were archived
into `bench/out/gc-daemon-e2e/run-89b202c/clean-archive/` and
`bench/out/gc-daemon-e2e/run-safe-head/clean-archive/` and built `--locked --offline` (64s, 36s) with
the same three source hashes.

Two earlier clean-source runs used different scripts:

- **Builder run (71 records).** Exported `f170dc1` (`git archive f170dc1 | tar -x`) into a fresh
  directory, confirmed the seed source is present from tracked files only, and built it
  `--locked --offline` (49s). Running the harness from that tree gave **71 records, 0 failed** with
  the reclaim numbers below. Its script `sha256` is `05ca05d5...` (`f170dc1`), which predates the
  `selftest.digest_detects_corruption` check. Artifacts under
  `bench/out/gc-daemon-e2e/clean-checkout/` (gitignored).
- **Independent reviewer run (72 records).** A fresh native reviewer exported the committed `8cab23a`
  tree, rebuilt the binaries from the byte-identical `crates/`, and ran the **committed** script
  (blob `e5e275a5...`, `sha256` `a5125907...`, identical in the archive and `git show 8cab23a:`) to
  **72 records, 0 failed**. This is the first 72/72 clean-source run of the committed script.
  Artifacts under `bench/out/gc-e2e-repairs-critic/archive/bench/out/run-clean/` (gitignored).

Both runs give identical reclaim numbers (`gross 16,759,743 - net 15,136,450 = 1,623,293`;
`pack-00000000.cpk` unlinked; all four reopen files matching fixture digests). The builder did not
produce a 72-record clean-checkout run; that result belongs to the independent reviewer.

Expected output ends with `"failed": 0`. Evidence is written under
`bench/out/gc-daemon-e2e/run/evidence/`.
