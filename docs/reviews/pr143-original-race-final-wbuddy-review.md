# PR143 original-race final: independent read-only wbuddy review

Status: read-only source + receipt + artifact + one CI-API audit. No local build, test, probe, cleanup, or lease action by this reviewer.
Target: PR143 head `8c3f81683276ae7857530ea2e9e9d0420642a3ea` on `fix/nfs-translate-namespace-race-43`.
Parent: `11e5c136abff8afee1bbd1c10a9e71202f307ed3` (the per-directory-lock commit). Grandparent / base main: `89353e17e5085000711dc428e834f9cc41840a1f`.
Scope: existing item #43 only. No scope additions.
Old receipts and the prior review are immutable; this is the only new file.

## Pins (verified this session)

| item | value | how |
|---|---|---|
| head | `8c3f81683276ae7857530ea2e9e9d0420642a3ea` | `git rev-parse` + `git ls-remote origin` |
| remote branch tip | `8c3f8168…` | `git ls-remote` (match) |
| tree | `9274921f1474572089f356df868bf6ef389a0193` | `git rev-parse ^{tree}` |
| parent | `11e5c136abff8afee1bbd1c10a9e71202f307ed3` | `git log %P` |
| parent of parent | `89353e17…` (main) | ancestry check YES |
| test blob | `fdb683059372ac1179eaae81bab1993c2dca35a6` | `git rev-parse 8c3f816:…namespace_race.rs` |
| adapter blob | `1987ece5d8a354693b6af88ec9b3160968e8b8f7` | same |
| sidecar blob | `5f5e7d54f84706f13449dc5fac39b4ebbc13f00d` | same |
| CI run | `37522657769`, workflow `ci`, event `pull_request` | `gh-axi` + GitHub API |
| run head_sha | `8c3f81683276ae7857530ea2e9e9d0420642a3ea` | GitHub API (exact match) |
| run conclusion | success; jobs linux-fuse, check(ubuntu), check(macos) all success | `gh-axi run view` |

The branch is a two-commit ladder: `89353e17` (main) -> `11e5c136` (adds adapter lock) -> `8c3f816` (adds/changes the fixture). `crates/cowfs-nfs/tests/namespace_race.rs` does not exist on `89353e17` (git says `does not exist`), so the receipt's "old" run is the new file ported into a detached main worktree, not a file that ever shipped on main.

## Verdict

FIX HALF-VERIFIED; FIXTURE NOT ACCEPTED AS A CLOSING REGRESSION FOR #43; CHANNEL-BYTE ACCEPTANCE FAILS; #43 STAYS OPEN.

- The adapter per-directory lock is real and source-correct: it genuinely serializes adapter-reachable namespace writers for one directory, exactly the seam #43 named.
- The new fixture is a genuine real-RPC RED on unmodified main and GREEN on the head, but the GREEN is conditional in a way that lets the vulnerable namespace outcome pass unasserted on the fixed tree. This is the "conditional on the observed flag" shape the task forbids as a substitute for namespace/byte invariants.
- The receipt/PR body retract the direct-`Vfs`-writer residual as unreachable. The retraction's source premise is substantially correct (traced below), but the probe offered as proof is weaker than the receipt claims.
- The `10004` status the receipt and PR body call `NFS3ERR_IO` is `NFS3ERR_NOTSUPP`. Mislabel confirmed against source.
- A race-only merge could be accepted separately, but #43 cannot be closed: the channel/bytes acceptance (`10004` on a sidecar write) is a real behavior gap on both trees.

## 1. The fixture's real structure (source)

`crates/cowfs-nfs/tests/namespace_race.rs` @ `fdb6830` (426 lines, 3 tests).

Raw test `a_sidecar_name_never_becomes_a_real_object_under_raw_nfs`:

- `watch.arm(MAIN)` where `MAIN = b"doc"`.
- Thread 1: `mkdir(._doc)` over NFS conn 1. Its guard `not_a_view(._doc)` -> `side_of(._doc)` -> `translating` peeks `._doc`, then `main_of(._doc)` calls `peek("doc")` = `WatchVfs::lookup(ROOT, "doc")`. The hold fires there, after the backend answered. This is inside the adapter's check-then-write window, no injection below `Vfs`. Correct placement.
- Main thread waits `wait_reached(10)`; then thread 2 runs `create(doc)` on NFS conn 2.
- `overlapped = watch.wait_peak(2, 1000)` (bounded).
- `doc_present_at_mutation = inner.lookup(ROOT_INO, MAIN).is_ok()` sampled on the main thread while thread 1 is held.
- `release()`, join both.
- Asserts: L290 window opened; L327 `watch.peak()==1`; L332 `!overlapped`; **L339-344 conditional** `if doc_present_at_mutation { assert!(!real, "a real directory took the live view name…") }`; **L347-353 conditional** `if real { assert_ne!(ch, OK, "…usable attribute view") }`.

Two facts decide everything, and both are only ever checked conditionally.

## 2. What the artifact logs actually show (author's own files)

From `bench/out/ready43-race/` in the READY7 worktree (read only, not modified):

`main-red-final.log`:
```
NFS mkdir(._doc)=0 create(doc)=0 overlapped=false doc_present_at_mutation=true real_dir_took_the_name=true doc_exists=true depth_while_held=1 peak_depth=1 names=["._doc", "doc"]
panicked at namespace_race.rs:343: a real directory took the live view name: the guard read stale state
test result: FAILED. 2 passed; 1 failed
```

`fixed-green-final.log`:
```
NFS mkdir(._doc)=0 create(doc)=0 overlapped=false doc_present_at_mutation=false real_dir_took_the_name=true doc_exists=true depth_while_held=0 peak_depth=1 names=["._doc", "doc"]
test result: ok. 3 passed; 0 failed
```

Read them together, precisely:

- On main, `peak_depth=1` and `overlapped=false`: the L327/L332 depth assertions **pass on main**. The old review's "peak 2 -> 1 is the discriminator" no longer holds for these final runs.
- On both trees `real_dir_took_the_name=true`: `._doc` **is** a real directory in both. So the final namespace is identical across OLD and NEW.
- The only difference is `doc_present_at_mutation` (true on main, false on fixed).
- On main the failure is L340, inside `if doc_present_at_mutation`. On fixed L340 is **skipped**, and the test passes with `real=true` because nothing unconditional checks the namespace.

So the GREEN on the fixed tree holds only because the flag that would force the namespace assertion is false. That is the conditional-substitution pattern. The final name set `["._doc","doc"]` is identical on both trees; no assertion distinguishes legal-serial-fallback from shadow *except* the flag, and the flag is precisely what the fix flips.

## 3. Is the flag a sound proxy, or a vacuous skip?

The flag is `inner.lookup(ROOT, "doc").is_ok()` sampled before release.

- On the fixed adapter, thread 2's `create(doc)` blocks on `with_names(ROOT)` held by thread 1, so `doc` is absent -> flag false. This is the *intended* reason.
- The flag can also be false for an *unintended* reason: if thread 2's `create(doc)` failed instead of blocking, `doc` would likewise be absent, the flag false, L340 skipped, and the test would pass even if `._doc` had taken the live name. **The raw test never asserts the `create(doc)` status.** L312-313 destructure `let (created, _fh) = created.join().unwrap();` and `created` appears only in the `eprintln!`; there is no `assert_eq!(created, OK)` in the raw test (unlike the *adapter* test which does assert it at L388). So a false `doc_present_at_mutation` cannot be distinguished from "the second request failed" by the test.

That is the concrete counterexample the task asked for:

> If `create(doc)` returns non-OK while `._doc` takes the name, the flag reads false, the namespace assertion is skipped, and the test reports green on a tree that still permits the shadow.

The fix does not do this (thread 2 blocks, then succeeds), but the *fixture does not enforce it*. The passing GREEN is therefore not proof the shadow is impossible; it is proof the observed flag was false on that run.

## 4. Determinism of the orchestration

- The window is opened by a barrier (`wait_reached`), not a sleep. Good.
- `wait_peak(2, 1000)` is bounded and failure-aware: on a serialized adapter it returns false at the deadline rather than hanging. Good.
- `hold_if_armed` has a 10s deadline and returns (does not deadlock) if released never fires. Good.
- No `.await` is held; the whole adapter closure runs under `spawn_blocking` (`CowNfs::run`). No lock-held-across-await.
- The two joins are unconditional, so both RPCs always finish.

So the harness is deterministic and bounded, and there is no deadlock-avoidance bypass in the fixture itself. This part is sound.

## 5. Does the test fail OLD and pass NEW for the original reachable bug?

- RED on unmodified main: yes, real. `mkdir(._doc)`'s guard read stale (`doc` absent at the read), `doc` landed, the held mutation ran, `._doc` became a real directory, L340 fired. This is the documented #43 shape over real RPCs.
- Adapter vs raw: the new file adds a genuinely protocol-level raw-NFS test (two real connections, real `spawn_blocking` dispatch), not merely the adapter-direct depth test the old review faulted. That is a real improvement over `11e5c136`'s fixture.
- GREEN on head: the pass does not assert the namespace invariant on the path the fixed code takes. The head's own artifact shows `real=true`, and the test's pass does not contradict a shadow; it only shows the flag was false.

Conclusion: the fixture is a true RED for the adapter-reachable race, but it is **not** a closing regression, because it cannot fail the fixed tree on a namespace or byte invariant, only on a flag it also uses to skip that invariant. Per the task's rule, a conditional on the observed flag must not replace the namespace/channel/bytes invariants. It does.

## 6. Observation atomicity and relevance

`doc_present_at_mutation` is sampled by the *test* thread against the store directly, not by the adapter and not under any lock. It is a point-in-time proxy for "thread 2 got in." It is relevant to the held operation (sampled while thread 1 is provably still held at the barrier), so it is not a random racy flag. But it is not an atomic coupled read of "the guard's answer" with "the mutation about to run," and it does not record the `create(doc)` status. Its relevance is real; its completeness is not.

## 7. Retraction of the direct-`Vfs`-writer residual: traced

The receipt now says the earlier "direct `Vfs` writer" framing was wrong because all namespace ops share `sc.ns` and no caller can reach below `Vfs`. Source trace:

- `crates/cowfs-core/src/queue.rs:219-240`: `struct SnapCtx { …, pub(crate) ns: Mutex<()>, … }`.
- `crates/cowfs-core/src/inner.rs:165` `by_id: HashMap<u64, Arc<SnapCtx>>`; `:244` `snapctx(ino) = snapctx_id(snap_of(ino))`; `:240-242` `by_id.get(id).cloned()`. So one `Arc<SnapCtx>` per snapshot id, shared by every handle that resolves an inode of that snapshot.
- `crates/cowfs-core/src/lib.rs:154` `#[derive(Clone)] pub struct Core { inner: Arc<Inner>, _guard }`; `:315` `snapshot_view` returns `SnapshotView::new(self.clone(), pack(sc.id, ROOT_INO))`. So `Core::root()` (`Arc::new(c.clone())`) and a `SnapshotView` over the same snapshot share `Inner`, hence share `by_id`, hence share the same `sc` and the same `sc.ns`.
- `crates/cowfs-core/src/ns.rs`: `make:169`, `op_link:272`, `op_unlink:322`, `op_rmdir:384/395`, `op_rename:503` all take `sc.ns` across the existence check and the mutation. `op_lookup:60` does not (read-only).

Verdict on the retraction premise: **correct for one snapshot.** Any writer that goes through `cowfs-core` for a given snapshot is serialized by that snapshot's `sc.ns`, regardless of which `Vfs` handle it used. Two snapshots have two `sc`s, so cross-snapshot aliases are not serialized, but two aliases *within one snapshot* are.

### Weakness in the offered proof (`two-handle-probe`)

`bench/out/ready43-race/two-handle-probe.rs` does `let b = a.clone()` where `a` is one `Arc<dyn Vfs>` over one `SnapshotView`. `b` is the same handle, so this proves intra-snapshot atomicity only, which `sc.ns` already guarantees by construction. It does **not** exercise the case the receipt cites (`Core::root()` handle vs `snapshot_view` handle), and `a_single_snapshot_view_is_never_shadowed_by_a_directory` is a sequential mkdir-over-a-file check, not a race. The 400-round `both=0 neither=0` result is consistent with the source but is not independent evidence for the cross-handle claim. Also, the probe file is untracked scratch (not in the head tree), so it is not a permanent regression.
Note also `make` returns `Error::ReadOnly` for `parent == ROOT_INO` (`ns.rs:165-167`), so the metadata root is not a mutable namespace writer at all.

Net: the retraction is source-supported; the probe is not the proof the receipt implies. Do not accept `400 / both=0 / neither=0` as universality or as byte/channel proof; the source trace is the basis, and it covers same-snapshot only.

## 8. Channel/bytes: the `10004` mislabel and the standing failure

`channel-io-main.log`: `CONTROL create(doc)=0 create(._doc)=0 write=10004`.
`channel-io-fixed.log`: `CONTROL-FIXED create(doc)=0 create(._doc)=0 write=10004`.
Identical on both trees, no race.

Status meaning, against source:
- `crates/nfsserve/src/nfs.rs:109` `NFS3ERR_IO = 5`.
- `crates/nfsserve/src/nfs.rs:171` `NFS3ERR_NOTSUPP = 10004`.

So `10004` is `NFS3ERR_NOTSUPP`, **not** `NFS3ERR_IO`. The receipt (`translate43-original-race-regression.md:85`) and the PR body both name it `NFS3ERR_IO`; that label is wrong.

Attribution: `crates/cowfs-nfs/src/sidecar.rs:314-317` in `side_write` returns `NFS3ERR_NOTSUPP` when the written buffer is not a plausible AppleDouble prefix; `is_plausible_prefix` (`crates/cowfs-nfs/src/appledouble.rs:90-95`) rejects bytes whose header magic/version do not match. The channel test writes `[0;8]` (all zeros), which is not a plausible prefix, so the refusal is deliberate and deterministic. It is a refusal of implausible sidecar bytes, not an I/O error, and it reproduces on both main and head with no race.

Classification: this sits inside #43's channel/byte acceptance (the whole point of Translate is a working `._doc` attribute channel). It is not fixed by and not fixable by the namespace lock. It needs its own ticket, but #43 cannot be declared passed while the channel-byte acceptance fails.

## 9. CI audit (one bounded read-only run)

`37522657769`, workflow `ci`, event `pull_request`, branch `fix/nfs-translate-namespace-race-43`.

- Head: GitHub API `.head_sha` = `8c3f81683276ae7857530ea2e9e9d0420642a3ea`. Exact match to the claimed head.
- Conclusion: success. Jobs: `linux-fuse` success, `check (ubuntu-latest)` success, `check (macos-latest)` success.
- Workflow on the head (`.github/workflows/ci.yml`): the `check` matrix runs `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, then `cargo test --workspace`. `cargo test --workspace` includes `crates/cowfs-nfs`, so the `check` legs do compile and run `namespace_race` on both OSes. `linux-fuse` runs only `cowfs-vfs-path`/`cowfs-fuse` and never builds `cowfs-nfs`; its success is not NFS evidence. fmt and clippy are covered by the green `check` legs (step 5 and step 6 of the ubuntu job both completed success).
- Coverage limit (honest): `gh-axi run view --log` truncates to the last ~20000 chars, which landed in the macos bench-harness tail; I could not fetch the fixture's per-test lines without downloading artifacts (out of scope). So CI confirms the workspace suite passed on `8c3f816` on both OSes, but I did not independently read the `a_sidecar_name…` pass line from CI. That is a missing, not a passed, check.

No polling, rerun, dispatch, workflow change, or artifact download performed.

## 10. Resource preflight and cleanup claims

- No resource-preflight receipt (8 GiB cap / 20 GiB floor / projected compile peak) was found under `.treehouse-ready-wave/`. Only `mac-heavy.lock`, three patch files, and a fixture file are present. The task required auditing that preflight *prior* to expensive runs; it is not in the receipt set. Missing.
- The author says the throwaway main worktree and its target were removed. Observed: `bench/out/ready43-race/mainwt` is absent (consistent); `target/ready43` still exists; `target/ready43-main` is absent. I did not witness the removal and found no deletion receipt. **Provenance label: unknown.** Do not retroactively bless the cleanup; only note it is consistent with observation.
- Author's GREEN receipt binds to `8c3f816` by claim plus my source match; the log files carry no embedded commit hash (same limitation the prior review noted).

## Findings

| id | severity | finding |
|---|---|---|
| G1 | blocking-for-claim | The raw fixture's namespace assertion (L339-344) and channel assertion (L347-353) are both conditional. On the fixed tree the artifact shows `real_dir_took_the_name=true` and the test passes only because `doc_present_at_mutation=false` skips L340. The passing GREEN is not a namespace/byte invariant. |
| G2 | high | The raw test never asserts the `create(doc)` NFS status. A non-OK second create yields the same `doc_present_at_mutation=false` and the same skip, so the fixture can go green on a tree that still allows the shadow. Concrete counterexample given in section 3. |
| G3 | high | `10004` is `NFS3ERR_NOTSUPP` (`nfs.rs:171`), not `NFS3ERR_IO` (`nfs.rs:109`). Receipt line 85 and PR body mislabel it. The refusal is `side_write`'s deliberate implausible-prefix guard (`sidecar.rs:314-317`), present on main and head with no race. Channel/byte acceptance fails; #43 cannot be closed. |
| G4 | medium | The `two-handle-probe` cited as proof of store atomicity clones one handle (`let b = a.clone()`), not `root()` vs `snapshot_view`. It proves intra-snapshot atomicity only, which `sc.ns` guarantees by construction. The cross-handle claim rests on the source trace, not the probe. |
| G5 | medium | No resource-preflight receipt (cap/floor/compile-peak) exists for the expensive runs. Missing evidence. |
| G6 | low | `mainwt`/`target/ready43-main` absent (consistent with the removal claim) but no deletion receipt; `target/ready43` still present. Provenance of cleanup: unknown. Logs embed no commit hash. |
| G7 | info | CI `37522657769` is pinned to `8c3f816` and passed on both OSes including `cargo test --workspace`; the NFS fixture per-test lines were not independently read from CI. fmt/clippy covered by green `check` steps. |
| G8 | info | The direct-`Vfs`-writer residual retraction is source-supported for same-snapshot writers; the metadata root rejects `make` (ReadOnly). The prior review's F1 is fairly corrected in principle, but G4 weakens the offered proof. |

## What remains open on existing #43

1. A permanent regression whose pass requires an **unconditional** namespace and byte invariant on the fixed tree, not a flag that gates the very assertion at issue. Candidate: assert `create(doc)==OK` unconditionally AND assert the invariant "if `doc` exists, `._doc` is not a real directory OR its attribute channel serves the live view" in a form that fails the shadow regardless of the flag.
2. The channel/bytes defect: whole-file write to an existing sidecar over a live view returns `NFS3ERR_NOTSUPP` deterministically. Needs its own ticket and its own acceptance.
3. `Root()-handle vs snapshot_view-handle` atomicity: source-supported but not exercised by the probe; either accept the source trace explicitly or add a real two-handle regression.

## Merge / closure recommendation

- A **race-only** merge of the adapter lock (`11e5c136` + a strengthened fixture) can proceed on its own merits; the lock is real and correct for adapter-reachable writers, and CI on `8c3f816` is green on both OSes.
- **#43 must stay open.** Do not record a whole-#43 pass. The channel-byte acceptance (`10004`) fails on both trees, and the current fixture does not close the namespace residual because its decisive assertions are conditional on the flag the fix flips.

## Scope compliance

Read-only. No checkout/edit/build/test/probe/cleanup/offload/lease/acquire-return-prune-destroy/signal/restart/SSH/install/waiver/history-rewrite/source-write/issue-edit/merge/close. GitHub read via `gh-axi` and one API call; no polling, rerun, dispatch, or artifact download. The READY7 worktree was listed, never entered as a build target. The prior review and old receipts were read, not modified. Only this file was written, in MAIN primary checkout, not committed.
