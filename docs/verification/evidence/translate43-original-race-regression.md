# #43 Translate namespace race: the original guard-window regression, RED on main

Status: WIP, draft PR #143 open, not reviewed, not merged.
Branch: `fix/nfs-translate-namespace-race-43`.
New commit under test: `8c3f81683276ae7857530ea2e9e9d0420642a3ea`.
Baseline: unmodified main `89353e17e5085000711dc428e834f9cc41840a1f`.
This document is the NEW canonical receipt for the residual regression.
The older receipt `docs/verification/evidence/translate43-existing-race-repair.md` is unchanged and its adapter-lock findings still stand, except where this document corrects the residual framing (see "Correction to the earlier framing").

## What was asked

The PR #143 review rejected closure because the shipped GREEN raw test only proved an adapter-subset mutex, and its main RED was a `depth 2 vs 1` instrumentation count, not the original direct-writer failure.
The task was to produce a permanent, deterministic RED fixture for the ACTUAL residual over the real public raw NFS flow, capture the runtime OLDFAIL with a named assertion on unmodified main, and only then decide whether the minimal fix stays in owned `cowfs-nfs` paths or must stop.

## The original fixture, ported

The original acceptance contract is the critic fixture `residual_the_guard_window_is_not_closed`: while the adapter's `mkdir(._doc)` is between its guard read and its write, the main name `doc` appears, and the review says a real directory can then take `._doc`.
The port makes the window deterministic over real raw NFS, with no injection below `Vfs`:

1. `WatchVfs::lookup` answers the backend read and then holds, so the `mkdir(._doc)` guard has its answer in hand.
2. A second NFS connection runs `create(doc)` as a real RPC.
3. The hold is released, so the held mutation runs on the answer read before `doc` appeared.

The fixture records whether `doc` was in the tree at the moment the held mutation was allowed to run (`doc_present_at_mutation`).
That fact decides the outcome:

- `doc_present_at_mutation = true`: the guard read stale state, so no real object may take the live view name.
- `doc_present_at_mutation = false`: the `mkdir` was the serial winner, and a real `._doc` directory is the correct macOS fallback for a name that had no main file yet.

The assertion is unconditional given the recorded fact, so a run that produces the shadow cannot pass by skipping it.
This corrects the review's complaint about the old `if !real { ... }` channel check, which skipped exactly when the damage occurred.

## OLDFAIL: runtime RED on unmodified main, named assertion

Command (in a detached worktree at `89353e17` with only the new test copied in):

```
cargo test -p cowfs-nfs --test namespace_race -- --test-threads=1 --nocapture
```

Named failing assertion and observed values:

```
NFS mkdir(._doc)=0 create(doc)=0 overlapped=false doc_present_at_mutation=true real_dir_took_the_name=true doc_exists=true names=["._doc", "doc"]
thread panicked at namespace_race.rs:343:
  a real directory took the live view name: the guard read stale state
test result: FAILED. 2 passed; 1 failed
```

This is the original defect on the real request flow: the guard read `doc` as absent, `doc` then landed, the held mutation still ran, and a real directory took the live view name.

## GREEN on the fixed branch, same test

Command (branch head `8c3f816`, `CARGO_TARGET_DIR=target/ready43`):

```
cargo test -p cowfs-nfs --test namespace_race -- --test-threads=1 --nocapture
```

Observed:

```
NFS mkdir(._doc)=0 create(doc)=0 overlapped=false doc_present_at_mutation=false real_dir_took_the_name=true doc_exists=true names=["._doc", "doc"]
test result: ok. 3 passed; 0 failed
```

`doc_present_at_mutation=false`: the adapter lock serialised the second request out of the window, so the guard's answer was current and the real `._doc` is the legal fallback, not a shadow.
The real-dir branch asserts the channel is not reported as a usable view.

## Correction to the earlier framing

The earlier receipt and PR body claimed a "direct `Vfs` writer" residual that an adapter lock cannot cover.
That framing is wrong, and the evidence is direct:

- Every `cowfs-core` namespace op (`make`, `op_link`, `op_unlink`, `op_rmdir`, `op_rename` in `crates/cowfs-core/src/ns.rs`) takes `sc.ns` and holds it across the existence check and the mutation.
- A handle the daemon hands out is a `Core` clone or a `snapshot_view`, both sharing the same `sc.ns`.
- Scratch probe (not committed, log at `bench/out/ready43-race/two-handle-probe.log`): 400 concurrent rounds of `create(doc)` against `mkdir(doc)` on one real snapshot view gave `a_won=28 b_won=372 both=0 neither=0`. Exactly one winner every round.

So no caller can reach below `Vfs`, and the store check-and-mutate is already atomic.
The earlier critic fixture reached below `Vfs` with a raw `inner.create`, which no caller and no NFS request can perform.
The residual that is real, caller-reachable, and reproduced above is adapter-level only, and the adapter lock closes it.

## Separate defect, not fixed here

`create(doc)` then `create(._doc)` then `write` returns `NFS3ERR_IO` (`10004`) on both main and this branch with no race at all.
Logs: `bench/out/ready43-race/channel-io-main.log` and `channel-io-fixed.log`.
It is a pre-existing `._doc` channel-write defect, independent of the namespace race, and is not part of this fix.

## Gates on `8c3f816`

| gate | result |
| --- | --- |
| `cargo test -p cowfs-nfs` (local, `target/ready43`) | 51 unit + all integration targets passed, 0 failed |
| `cargo fmt --all -- --check` | rc 0 |
| `cargo clippy -p cowfs-nfs --all-targets -- -D warnings` | rc 0 |
| GitHub Actions run 37522657769 (`ci`, head `8c3f816`) | success: linux-fuse, check (ubuntu-latest), check (macos-latest) |

The CI run is a genuine-code run on the pushed head, not a local execution.
It confirms the fixture compiles and the suite passes on both target OSes.

## Scope

Changed: `crates/cowfs-nfs/tests/namespace_race.rs` only.
Preserved: the existing adapter lock in `crates/cowfs-nfs/src/adapter.rs` and the `pub(crate)` on `PerIno::of`.
No Core, daemon, manifest, CI, or store change.

## Evidence files

- `bench/out/ready43-race/main-red-final.log`: RED on `89353e17`.
- `bench/out/ready43-race/fixed-green-final.log`: GREEN on `8c3f816`.
- `bench/out/ready43-race/two-handle-probe.rs` and `.log`: store atomicity probe.
- `bench/out/ready43-race/channel-io-main.log` and `channel-io-fixed.log`: the separate channel-write defect.
- `docs/reviews/pr143-nfs-namespace-race-wbuddy-review.md`: the review that drives this work.
