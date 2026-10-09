# PR143 NFS namespace race: independent wbuddy review

Status: read-only source + existing-artifact audit. No local build, test, or probe run by this reviewer.
Target: PR143 head `11e5c136abff8afee1bbd1c10a9e71202f307ed3` (== remote `refs/pull/143/head`), base and parent `89353e17e5085000711dc428e834f9cc41840a1f` (== remote `refs/heads/main`).
Branch: `fix/nfs-translate-namespace-race-43`. Draft, open, unmerged.
Assigned scope: `cowfs-nfs` only. Review file: this one.

## Verdict

ADAPTER-ONLY SUBSET PASS. ORIGINAL DIRECT-WRITER RESIDUAL NOT FIXED. CI PENDING.

- The per-directory lock is real, correctly scoped, and demonstrably closes the window **for writers that enter through the adapter**. Source supports this and the author's artifact logs prove it (old red, new green, same assertion).
- The original issue-43 residual named in the task (a direct `Vfs` writer, or a second `Adapter`/`Vfs` handle over the same store) is NOT closed. It reproduces on the fixed tree, and the PR body and receipt say so honestly.
- CI for this exact head is **in progress** at audit time: no pass verdict exists. Do not read `linux-fuse` success as NFS fixture evidence.

## What was verified (pins)

| item | value |
|---|---|
| head commit | `11e5c136abff8afee1bbd1c10a9e71202f307ed3` |
| remote `refs/pull/143/head` | `11e5c136…` (match) |
| parent / base | `89353e17e5085000711dc428e834f9cc41840a1f` |
| remote `refs/heads/main` | `89353e17…` (match) |
| adapter.rs blob | sha256 `55a2ecacb77d4455b5a4bc6bc44d2a9171aa86c62ca5fd783a5e1d36237d5f0a` |
| sidecar.rs blob | sha256 `5e2b5821923f4e67e51211caa15750b868d379c89e543c405f26b3a544846da1` |
| tests/namespace_race.rs blob | sha256 `ce4a2de38b3ae69d3a…cff255fb7` (prefix/suffix captured; full digest not readable from the tool output) |
| receipt blob | sha256 `0f4cc6a898539c83198ca084fb7c59be3285eee394634b83bc5cda5114ecb80b` |

All source facts below come from `git show 11e5c136:…` (immutable pinned blobs). The codebase-memory graph indexes local MAIN `93cfef9` and does not contain this remote head, so it was used for location only.

## Source binding: the lock as written

`crates/cowfs-nfs/src/adapter.rs`

- Field (line 214): `names: Mutex<PerIno>`.
- `with_names` (640-646): `let l = lock(&self.names).of(dir); let g = lock(&l); let out = f(); drop(g); out`.
  The `names` map mutex is taken only to fetch/clone the per-directory `Arc<Mutex<()>>`, then released before the per-directory mutex is acquired. No map-hold-over-guard inversion.
- `with_two_names` (650-665): for two distinct dirs, takes both per-dir mutexes in a fixed ascending-`Ino` order; identical dirs delegate to `with_names`. This is the anti-deadlock ordering the task asked to confirm, and it is present.
- Work under the lock is synchronous `Vfs` calls. The async boundary is `CowNfs::run` (991-1000), which dispatches the whole synchronous closure via `tokio::task::spawn_blocking`. No lock is held across an `.await`. No reentrancy: `side_create`/`side_remove` in sidecar.rs take `sidecar_locks` (a different `PerIno`, sidecar.rs:106), never `with_names`, so no nested acquisition of the same lock class.

Guarded public namespace mutators (all enter `with_names` or `with_two_names`):

| method | line | lock |
|---|---|---|
| `create` | 503 | `with_names` (512) |
| `create_exclusive` | 565 | `with_names` (573) |
| `mkdir` | 667 | `with_names` (671) |
| `symlink` | 684 | `with_names` (699) |
| `link` | 708 | `with_names` (716) |
| `remove` | 740 | `with_names` (744) |
| `rmdir` | 789 | `with_names` (793) |
| `rename` | 819 | `with_two_names` (831) |

`lookup` (379), `read` (459), `write` (475), `getattr` (368), `setattr` (436), `readdir` (883) do not mutate names and are intentionally unguarded. Correct.

`PerIno` (sidecar.rs:106-122) holds `Weak` refs and prunes entries past 4096 when the last strong ref drops. A guard keeps its `Arc` alive for the whole call, so the per-dir mutex cannot vanish mid-critical-section. Lifetime is correct.

## What the tests actually assert (this is the weak point)

`crates/cowfs-nfs/tests/namespace_race.rs`, three tests, 388 lines.

`a_sidecar_name_never_becomes_a_real_object_under_raw_nfs` (258-315): real RPCs over two TCP connections to the in-process NFSv3 server. Two assertions carry it:

1. line 299: `assert_eq!(created, OK)` - the concurrent `create(doc)` succeeded.
2. lines 300-304: `assert_eq!(watch.peak(), 1)` - **the load-bearing assertion. It is a concurrency-depth metric, not a namespace or bytes check.**

The final-namespace / attribute-channel check is conditional (lines 307-314): `if !real { … assert_eq!(ch, OK, "the attribute channel for doc is dead") }`. If `._doc` became a real directory (`real == true`), that assertion is **skipped entirely**. Bytes are never read back.

`the_guard_and_its_mutation_are_one_step` (322-360): adapter-direct, `assert_eq!(watch.peak(), 1)` again. Same metric, no namespace check.

`a_refused_directory_leaves_the_name_free` (364-387): asserts `mkdir(._doc)` is refused while `doc` exists, `._doc` is not left real, `doc` survives. This one does assert namespace state, but it exercises the sequential refuse path, not the window.

`WatchVfs` (52-135): `peak` is monotonic (`fetch_max`), `wait_peak` is bounded. A `peak()==1` assertion therefore only means "the two sampled operations never overlapped at the recording point" - it is a proxy for the lock contract, and on the fixed tree it is exactly the assertion that flips 2 -> 1.

## Author artifacts: what they prove and what they do not

Artifacts live in the READY7 worktree, `bench/out/ready43-race/` (project-relative to `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/7/cowfs/`):

| file | sha256 | size |
|---|---|---|
| fixed-green.log | `fd963950…40a450a8` | 830 B |
| main-red.log | `0a104407…5a93b24a` | 1629 B |
| old-head-red.log | `9a52d71b…e2922d49` | 1416 B |
| direct-injection-probe.log | `375a2422…5db346f30` | 554 B |
| direct-injection-probe.rs | `e6bb3989…3dfe14ae` | 5252 B |
| full-suite.log | `0dc832e4…50bc4502` | 15849 B |
| clippy.log | `a284def0…1256bb3f3` | 210 B |

Old vs new assertion readings (same test file, different code under it):

- `old-head-red.log`: old file `namespace_race.rs` (2 tests, `a_directory_never_takes_a_name...` / `the_attribute_channel_survives...`) FAILED 0/2. The raw-residual test printed `mkdir(._doc)=0 real_dir_took_the_name=true`, i.e. `._doc` became a real dir and the attribute channel died (line 257, `left: 17 right: 0`).
- `main-red.log`: new file on unfixed main, `running 3 tests`, `test result: FAILED. 1 passed; 2 failed`. Raw-NFS test printed `real_dir_took_the_name=true peak_depth_while_held=2 peak_depth=2 names=["._doc","doc"]` and failed at line 300 `left: 2 right: 1`. Adapter test failed at line 355, same 2 vs 1.
- `fixed-green.log`: new file on the fixed head, `running 3 tests`, all ok (3 passed). Raw-NFS test printed `real_dir_took_the_name=true peak_depth_while_held=1 peak_depth=1 names=["._doc","doc"]`.

Read those three lines together. On the fixed tree the raw-NFS test still shows `real_dir_took_the_name=true` and final `names=["._doc","doc"]` - `._doc` IS a real directory in the final namespace. The test passes solely because `peak_depth` went 2 -> 1 and the `if !real` guard skipped the channel assertion. That is the author's own artifact confirming the narrow metric, not namespace repair. It is consistent with the honest PR body and receipt; it is not evidence the residual is gone.

`direct-injection-probe.log` is the smoking gun for the residual: it compiles `tests/namespace_race_direct.rs` and prints `INJECT mkdir(._doc)=None real_dir_took_the_name=true` with `test … ok`. That probe is a **separate untracked file** - `git ls-tree -r 11e5c136` has no `namespace_race_direct`, and the worktree `git status` is clean (the file was removed after the run). It reproduces exactly the task's described shape: a direct `Vfs` writer injected inside the adapter's own `vfs.mkdir` call, bypassing the adapter, still shadows the real name on the fixed tree. The per-directory mutex lives inside one `Adapter`; a writer that never touches that `Adapter` is invisible to it.

`full-suite.log`: 17 suites, **155 passed, 0 failed, 0 ignored** by the reviewer's count of `test result:` lines. `clippy.log`: `cargo clippy` on `cowfs-nfs` clean, finished, no warnings. `cargo fmt` output is not a separate artifact here; treat fmt as PENDING/unproven from these logs.

Artifact caveats (honest):
- Logs carry no embedded commit hash. They bind to the head only via the author's claim; the pinned source I read matches the code the logs compile, but the logs themselves are not cryptographically pinned to `11e5c136`.
- `fixed-green.log` shows `real_dir_took_the_name=true`, so it is self-evidence against the stronger reading of "fixed".

## Existing-43 residual: NOT FIXED

The residual is **any namespace writer that reaches the store without passing through this `Adapter` instance**. Named instances from source:

1. **Direct `Vfs` writer.** `crates/cowfs-daemon/src/backend.rs` `root()` returns `Arc::new(c.clone()) as Arc<dyn Vfs>`; `snapshot()` returns `c.snapshot_view(name)`. These are independent `Vfs` handles over the same underlying store. A caller holding either can `create`/`mkdir` the shadowed name with no lock. Reproduced by `direct-injection-probe.rs`.
2. **Second `Adapter` instance** over the same store. `names: Mutex<PerIno>` is per-`Adapter` (adapter.rs:214). Two adapters hold two maps, so two per-directory mutexes for the same `Ino`. Serialization is per-instance, not global. A second mount, or a second `CowNfs::new` in-process, defeats it.
3. **Snapshot alias of the same underlying directory.** Distinct packed `Ino` values that resolve to the same store directory map to different `PerIno` entries and different mutexes.

The task's framing - do not accept a different `peak_depth` RED/GREEN as closure of the original race - is correct. The peak-depth flip is exactly what the fix changes; the original direct-writer outcome (`._doc` real, `ISDIR` on the create channel) is unchanged.

## Deadlock / correctness review of the chosen layer (source)

- Lock ordering: `with_two_names` orders by `Ino` ascending; two renames cannot invert. Verified.
- Map mutex vs guard mutex: map released before guard taken (`with_names` 641-642; `with_two_names` 655-659). No inversion. Verified.
- Async: all guard bodies run under `spawn_blocking` (`run` 991-1000). No guard held across `.await`. Verified.
- Reentrancy: `side_create`/`side_create_exclusive`/`side_remove` use the other `PerIno` (`sidecar_locks`). No same-class nested acquisition. `creating`/`translating` call `peek` which calls `Vfs::lookup`, not the adapter. Verified.
- Weak-ref lifetime: guard holds a strong `Arc` for the call duration; `PerIno::of` upgrades or inserts. No use-after-drop. Verified.
- The one semantic asymmetry: `lookup` (379-405) reads `translating` - hence `peek` - **without** the directory lock. A concurrent adapter `create` under `with_names` can still change the same directory's names while a `lookup` samples it. This is inherent to a write-side lock and is not the reported bug, but it means "one step" holds for write-vs-write, not read-vs-write.

## Minimal responsible shared-layer proposal (not implemented here)

The residual's root is that the invariant "a name read and the mutation that follows are one step" is enforced **inside one `Adapter`** while the mutation surface is the shared store. The minimal shared layer that would cover the adapter-only subset and the residual together is a lock owned by the store/`Vfs` layer keyed by the real directory identity:

- A per-store `PerDirKey` lock manager in `cowfs-vfs` (or the daemon backend that hands out `root()`/`snapshot()` handles), keyed by the resolved directory inode, not by the adapter's `Ino` view.
- `Adapter::with_names` / `with_two_names` acquire from that shared manager instead of the private `names: Mutex<PerIno>`, so a direct `Vfs` writer and a second adapter over the same directory take the same lock.
- Writers that bypass the adapter entirely must either route through the same manager or the manager must sit at the `Vfs` mutation entry points (`create`/`mkdir` - the `namespace`-mutating `Vfs` methods).

Graph sites for that layer (locate-only; graph lacks this remote head):

- `backend.rs::root` / `backend.rs::snapshot` - the two-handle split.
- `Adapter::with_names` / `with_two_names` (adapter.rs:640/650) - current private map.
- `PerIno` (sidecar.rs:106) - the reusable per-key weak-lock shape to promote to the shared layer.

No implementation in this review. Scope stays `cowfs-nfs` review + the proposal above.

## CI

Run `37519197301`, workflow `ci`, event `pull_request`, branch `fix/nfs-translate-namespace-race-43`, head `11e5c136`:

| job | id | status |
|---|---|---|
| linux-fuse | 112459860895 | completed, success |
| check (macos-latest) | 112459861299 | **in_progress** |
| check (ubuntu-latest) | 112459861404 | **in_progress** |

Verdict: PENDING for the OS legs that build and run `cowfs-nfs`. `linux-fuse` runs only `cargo test -p cowfs-vfs-path --test native` and `-p cowfs-fuse`; it never builds `cowfs-core` or `cowfs-nfs`, so its success is **not** NFS fixture evidence. One CI read performed; no polling, no rerun, no dispatch.

## Findings

| id | severity | finding |
|---|---|---|
| F1 | blocking-for-claim | ORIGINAL direct-writer residual NOT FIXED. Per-adapter lock cannot see a direct `Vfs` writer or a second adapter. Reproduced by the author's own `direct-injection-probe.log` (`INJECT mkdir(._doc)=None real_dir_took_the_name=true`) on the fixed tree. |
| F2 | high | Raw-NFS test passes on the fixed tree while `real_dir_took_the_name=true` and `names=["._doc","doc"]`. Load-bearing assertion is `peak()==1` (depth metric), and the namespace/channel check is skipped by `if !real` (lines 307-314). No bytes assertion. The test does not assert the final namespace is uncorrupted. |
| F3 | medium | `lookup` reads `translating`/`peek` without the directory lock (adapter.rs:379-405). Read-vs-write on one directory is not serialized; only write-vs-write is. |
| F4 | low | Artifact logs embed no commit hash; binding to `11e5c136` rests on the author's claim plus my source match. `cargo fmt` has no artifact; fmt is unproven from these logs. |
| F5 | info | PR body and receipt are honest: they name the residual, the adapter-only scope, and "not reviewed, not merged". This review does not dispute the honesty, only the sufficiency of the fix for the stated original residual. |

## Scope compliance

- Read-only. No local cargo/build/test/probe/archive/target/cleanup/offload/waiver. No checkout/reset/stash/rebase/force-push. No lease/signal/daemon/store/mount change. No CI rerun/dispatch/commit/push/merge/new-issue.
- Only `cowfs-nfs` files reviewed. Only this file written, in MAIN primary checkout.
- Old receipts/reviews left immutable.
- Independent of the author: the narrow depth test passing is not treated as closure of the original race.

## Residual risk / open

- Direct-writer and multi-adapter residual remain open; fix is required for the original issue-43 target.
- CI pass-or-pending: PENDING (macos `check` and ubuntu `check` in progress).
- `linux-fuse` success is not NFS fixture proof.
