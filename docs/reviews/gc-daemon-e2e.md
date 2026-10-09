# Independent review: PR #78 daemon gc end-to-end verification

Reviewer: fresh native Sonnet 5.5, read-only on source, harness and docs.
Audited head: `a5ba8d043af6740a4d4cf4c09930bc4c02111e5e` (branch `verify/gc-daemon-e2e`, slot 14).
Production GC source under test: `ea958947b437a089c760b7f4ee381c57702c81d8` (PR #76 head).

## Verdict

- GC behavior through the real user path: **PASS**. Independently reproduced, every number matches the builder's.
- PR #78 as a mergeable verification artifact: **BLOCK**, on three fixable items (B1, B2, B3 below).
- Merge order: PR #78 targets `main` (`97f6bbe`) but its branch contains all of PR #76. PR #76 must land first, then PR #78 is retargeted or rebased so it carries only the harness and docs.

## Source and binary identity

- `git diff ea95894 HEAD -- crates Cargo.toml Cargo.lock .github` is empty. `HEAD:crates` and `ea95894:crates` are both tree `961952e19a389a9c60e2c16190dd354e006664ef`.
- PR #78 changes exactly three files vs PR #76: `scripts/verify-gc-daemon.py`, `docs/verification/gc-daemon-e2e.md`, `docs/v1-daemon.md`.
- Builder binaries: `cowfs` `3f1b96c5...ba520`, `cowfs-daemon` `fbc86249...25009`. They equal the `env` record of run4, are built after commit `ea95894`, and are newer than every file under `crates/`.
- My independent build (`cargo build --locked --offline`, own target dir `bench/out/gc-e2e-critic/target`): `cowfs` `45e256c5...d529`, `cowfs-daemon` `a01be926...5925`. These differ from the builder's because rustc embeds paths and the target dir differs. They are not comparable byte for byte, so the identity claim rests on the tree hash plus build timestamps.
- Seed helper built from my own copy of the seed source: `7827eb64799da0c0d72517838ac3fa16b82e03fc7170f82a7948610b345d1854`. Bit-identical to the builder's.
- Production defaults, read from source: `crates/cowfs-daemon/src/daemon.rs:227` opens `CoreBackend::open(&store, cowfs_core::Options::default())`. `backend.rs:201` passes `cowfs_gc::Options::default()` (`dead_ratio` 0.5, `min_dead_bytes` 8 MiB, `io_budget_bytes` 2 GiB, `reclaim` true). `cowfs-store/src/types.rs:19` default `max_pack_size` is 256 MiB. The daemon command line in my run has only `--store --mount --socket --backend core`.
- The 16 MiB `max_pack_size` is set only inside the seeder's own `Core::open`, which then calls `Core::close`. The daemon never sees it.

## Independent reproduction (new run root, `bench/out/gc-e2e-critic/run1`, 16 records, 0 failed)

One private 16 MiB-pack fixture, unmodified daemon and CLI, fresh socket under `/private/tmp/cowfs-gc-critic-<pid>/`, NFS loopback mount, no privileges.

| Observation | Value |
|---|---|
| Packs before | `pack-00000000.cpk` 16,759,743 (sealed) and `pack-00000001.cpk` 12,205,006 (last, active on reopen); total 28,964,749 |
| Dry run | `candidate_blocks` 368, `candidate_bytes` 15,136,450, `freed_*` 0 |
| Dry-run invariant | sha256 of every file under the store dir (packs, redb meta, hints) before vs after: **0 files changed** (stronger than the builder's size-only check) |
| Live gc | `freed_blocks` 202, `freed_bytes` 16,759,743; `pack-00000000.cpk` unlinked, new `pack-00000002.cpk` 1,623,293 |
| Packs after | 12,205,006 + 1,623,293 = 13,828,299 |
| fsck (same daemon, and again on fresh daemon) | ok, 0 problems, 193 blocks, 2 snapshots, `bytes_checked` 13,828,299 equals the on-disk pack total |
| Mounted readback | `keep000`, `keep001`, `live000`, `live001`: SHA-256 equal before gc, after gc on the same mount, and after shutdown plus a **fresh daemon on a new mount path**; BLAKE3 equals the seeder's declared digest in all three reads. Python `blake3` was present and used, not skipped |
| Teardown | `shutdown` rc 0, daemon exit 0, mount gone, socket gone, both times |
| Fresh `nfsstat -m` | the new mount is `localhost:/cowfs-...` NFS, no native fallback |

Eligibility: `candidate_bytes` only counts packs that are neither active nor at or above the mark epoch (`crates/cowfs-gc/src/lib.rs:383-393`). A positive dry-run `candidate_bytes` therefore proves the dead root blocks sit in a sealed, eligible pack. Only pack 0 was unlinked.

I also ran the **committed** builder harness unchanged with `--work bench/out/gc-e2e-critic/harness-run`: `records 63, failed 0`, `env.head` = `a5ba8d0...`, identical reclaim numbers. I did not touch the builder's `run*` or `seed-crate` directories.

## Gross versus net: `freed_bytes` is gross

- `GcReport.freed_bytes` = sum of what `discard_pack` returns, which is the unlinked pack's file length (`crates/cowfs-store/src/compact.rs`, `fs::metadata(&path).len()`). Contract (`docs/gc-core-integration.md:121`): "bytes on disk freed by unlinked packs". Gross by contract.
- Observed: gross 16,759,743; net physical drop 15,136,450 (= `candidate_bytes`); difference 1,623,293 = the rewritten pack that now holds the live records. My run recorded `gross_minus_net == new_pack_bytes` and `net == candidate_bytes`, both true.
- So the CLI line `freed 202 blocks (16.0 MiB)` overstates the space saved by 10.7% here. The control-plane `GcReport` has no `bytes_copied`, so a user cannot net it out.
- `freed_bytes` (16.76 MB) exceeds `candidate_bytes` (15.14 MB), while `candidate_bytes` is documented as "the most a cycle can free". Under gross accounting that bound is false.
- This is **not** a correctness failure against PR #76's written contract, so it does not block the GC. It is a semantics defect to fix separately in PR #76 or a follow-up: report net (`freed_bytes - bytes_copied`) or add a `bytes_copied` / net field to the control `GcReport`, and reword the `candidate_bytes` bound. A minimal repro is this fixture.
- PR #78 must not present 16,759,743 as space saved. Its report and body list `freed_bytes` and the delta side by side with no gross/net label. See B2.

## Blocking items for PR #78

**B1. The headline reclaim evidence is not reproducible from the PR.**
`bench/out/gc-daemon-e2e/seed-crate/` is gitignored (`/bench/out/`), `git ls-files | grep seed-crate` is empty, and the harness contains no template for it. `build_seed_helper` only runs `cargo build` on that manifest, so on a clean checkout it raises and the whole reclaim phase cannot run. The docs and PR body say the harness "builds the private seed helper on first run", which is false off the builder's machine. The seed `Cargo.lock` is also untracked, and its `blake3 = "1"` is unpinned there. Fix: track the seed source (for example `scripts/gc-fixture-seed/` with its `Cargo.lock`), or have the harness write it from a template.

**B2. Label gross versus net.** Add the gross/net distinction above to `docs/verification/gc-daemon-e2e.md` and the PR body, and assert `freed_bytes - new_pack_bytes == physical delta` in the harness so it is a gate, not prose.

**B3. Evidence and claim hygiene.**
- `gc.dry_run.no_pack_change` is vacuous: `packs_before` and `packs_after_dry` are both measured after the dry run (`scripts/verify-gc-daemon.py:883-885`), so it compares a value to itself.
- The `reclaim.*` dry-run check compares file sizes only, not content.
- The BLAKE3 cross-checks use `b3_now is None or ...`. If the python `blake3` module is absent they pass silently, and the record does not say whether BLAKE3 ran.
- Several records are bare booleans with no digest (`reclaim.reopen_survivors_match_source`, `cancel.daemon_survives`).
- The builder's reopen check re-reads only `keep000/001`, which live in the active pack that gc never rewrites. The `live` files, the ones actually copied into the new pack, are re-read only on the pre-reopen mount, which may be served from the NFS client cache. My run closes this gap (fresh mount, all four files, BLAKE3 match), and fsck covers the blocks, but the PR's harness does not.
- run4's `env.head` is `99c7c23` while the PR body claims `a5ba8d0`. run4 was produced from an uncommitted script. My run of the committed script gives `a5ba8d0` and 63/0, so the claim is true after the fact, but the cited evidence file does not support it.
- The PR body says the `docs/v1-daemon.md` fix is "already in the base of this branch". It is not: the file differs from `ea95894` by 13 lines (the "gc not wired" text), so PR #76 left it stale and PR #78 corrects it.

## Cleanup guard and process hygiene (audited)

- `kill_verified` signals a pid only when its `ps` command line carries the fixture's exact socket and store paths, and the selftest proves it refuses a foreign pid. No `pkill`. The daemon is started with `start_new_session=True`. Evidence records are append, flush, fsync per step. The harness shuts the daemon down through the CLI and waits for exit before any kill, so there is no unflushed state at kill time.
- Wait loops are bounded, and daemon-ready also exits on a dead child or `FATAL` / `panic` in the log.
- Weak spots, not blocking: the stub and `--backend path` daemons have no unmount sweep if their own shutdown fails, and `shutil.rmtree(args.work)` wipes whatever `--work` is given.
- After both of my runs: 0 mounts matching `gc-e2e-critic` or `gc-daemon-e2e`, 0 `/private/tmp/cowfs-gc*` directories, no stray processes, `git status` clean in slot 14. The shared daemon (pid 11068, `~/.cowfs/store`) was never addressed.

## Gaps that remain open (stated, not claimed)

- **Cancellation:** the SIGINT -> exit 130 -> progress -> daemon alive test runs on the **stub** backend. It is control-plane only. Real-core `gc` cancellation through the daemon is not exercised.
- **Crash:** no SIGKILL or power-loss injection through the full stack. Graceful shutdown only.
- **Platform:** macOS NFS only. No Linux FUSE run.
- **Pack size:** the sealed packs come from a fixture seeded with `max_pack_size` 16 MiB, not from organic growth of a 256 MiB default store.
- **Partial reclaim:** one cycle reclaimed 202 of 368 dead blocks. The other 166 (11,729,603 dead bytes) were in pack 1, which was the active pack. After restart that pack is sealed, and a fresh-daemon dry run reports `candidate_bytes` 11,729,603. A second cycle would reclaim it. Not a defect, but the user-visible effect of "gc" is staged.
- **Unexplained:** the live records copied out of pack 0 total 1,623,293 B, while the two `live` files are 1 MiB. I did not decompose the extra ~0.57 MB (likely tree or metadata blocks). fsck and cold reads are clean.
- No throughput or timing claim was made or checked.

## Artifacts

- `bench/out/gc-e2e-critic/run1/records.jsonl`: 16 records, 0 failed. Daemon logs alongside.
- `bench/out/gc-e2e-critic/harness-run/evidence/records.jsonl`: 63 records, 0 failed, head `a5ba8d0`.
- `bench/out/gc-e2e-critic/critic_run.py`, `build.log`, `critic.out`, `harness-run.out`: reviewer script and logs, all gitignored.
- Builder files untouched: `scripts/`, `docs/verification/`, `bench/out/gc-daemon-e2e/**`.
