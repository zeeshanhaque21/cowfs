# Issue #43: separate-adapter namespace regression (root handle vs `snapshot_view`)

Lane: READY wave, worktree slot-7, branch `test/nfs-separate-adapter-namespace-43`.
New head: `f5a492d419d37ac1096fcbfaca247ca436d7885c` (pushed to `origin/test/nfs-separate-adapter-namespace-43`).
Draft PR: #145, https://github.com/zeeshanhaque21/cowfs/pull/145, base `main`, `Refs #43`.
Deliverable: new test file `crates/cowfs-nfs/tests/separate_adapter_namespace.rs`, 370 lines, sha256 `89e9433766eaf3cc198db8ca6d2c77449fbec302cd2ec28ddc3930750bf03d91`.

## What this covers

The permanent regression for the topology #143 did not cover.
`#143` added a per-directory lock to serialise a guard read against the mutation it guards, proven by the accepted single-adapter fixture `crates/cowfs-nfs/tests/namespace_race.rs`.
That lock lives on the `Adapter` instance.
The product does not always use one adapter: the daemon serves the core root on the default mount and exports a snapshot at a client-chosen path, and each export is its own `Mount` -> `Server` -> `Adapter` over the same `Core` (`crates/cowfs-daemon/src/exports.rs:113` `mount_snapshot` over `backend.snapshot(name)`; `crates/cowfs-daemon/src/backend.rs:490` `snapshot` -> `Core::snapshot_view`).
Two adapters reach one snapshot namespace through two `PerIno` lock maps that are separate by construction (`Adapter::new` builds a fresh map per instance, `crates/cowfs-nfs/src/adapter.rs:228`).

## Exact topology under test

- One backing `Vfs` (`SharedBackend` over `common::memfs()`), wrapped by TWO separate `Adapter::new` instances.
- Wrapper sits at the `Vfs` boundary the two adapters share, not below it.
  It counts guarded name-space operations inside either adapter for the root and holds adapter A's sidecar mutation at a barrier.
  No injection below `Vfs`, no fabricated backend.
- Adapter A runs `mkdir(._doc)` through the public `Adapter::mkdir` (`adapter.rs:667`) -> `not_a_view` (`:628`) -> `side_of` -> `main_of` -> `peek("doc")` guard read, then stops at its `self.vfs.mkdir(SIDE)` mutation.
- Adapter B runs `create(doc)` through the public `Adapter::create` (`adapter.rs:503`) on the other adapter, then A is released.
- The window is deterministic, not timing-dependent: B's `create` is released only after A signals it reached the sidecar mutation, exactly the interleaving a lock shared between the two adapters would forbid.

### Property asserted (topology-agnostic lock contract)

- `!doc_present_at_mutation`: B did not land inside A's guard window.
- `peak_depth == 1`: the backend never saw two guarded name-space operations at once.
- `!overlapped` (bounded `wait_peak(2, 1000)`, exits on timeout): same fact, time-bounded.
- Premise test `the_two_adapters_share_one_namespace`: a create through one adapter is visible to the other with the same inode id, so the lock test cannot pass on disjoint state.

`real_dir_took_the_name` is reported, not asserted.
On a correctly serialised arrangement A runs first, sees `doc` absent, and a real `._doc` directory is a legitimate outcome.
That value alone does not separate the topologies; the lock-contract assertions do.

## Source-or-actual RED status

Confirmed RED on the exact merged `origin/main` `1580e69b9d987f63c07b2430f8c0b4547ecd8622`, which already contains the #143 fix (`11e5c13` lock-fix commit merged via PR #143).

Local source run on the real `origin/main` tree:

```
SEPARATE-ADAPTER mkdir(._doc)_ok=true create(doc)_ok=true overlapped=true doc_present_at_mutation=true real_dir_took_the_name=true depth_while_held=2 peak_depth=2
thread 'two_adapters_over_one_namespace_do_not_both_enter_the_guard' panicked at crates/cowfs-nfs/tests/separate_adapter_namespace.rs:326:5
exit 101
```

ACTUAL CI (completed run `37536162774`, `check (ubuntu-latest)`, job `112517503112`), same signature at `crates/cowfs-nfs/tests/separate_adapter_namespace.rs:326:5`:

```
Running tests/separate_adapter_namespace.rs
test the_two_adapters_share_one_namespace ... ok
test two_adapters_over_one_namespace_do_not_both_enter_the_guard ... FAILED
SEPARATE-ADAPTER mkdir(._doc)_ok=true create(doc)_ok=true overlapped=true doc_present_at_mutation=true real_dir_took_the_name=true depth_while_held=2 peak_depth=2
error: test failed, to rerun pass `-p cowfs-nfs --test separate_adapter_namespace`
```

The failing test is the ONLY test that failed in `cargo test --workspace`.
`cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` both passed on CI.
The failure is the intended RED regression on unfixed main, not an accidental break of another test.

## CI check breakdown (run `37536162774`, completed)

- `check (ubuntu-latest)` job `112517503112`: FAIL, `cargo test --workspace` exit 101, the RED test above.
- `check (macos-latest)` job `112517503381`: FAIL, `cargo test --workspace`, same RED test.
- `linux-fuse` job `112517503434`: PASS.

## Gates (local, under the mac-heavy lock)

- `cargo fmt --all --check`: pass.
- `cargo clippy -p cowfs-nfs --all-targets -- -D warnings`: pass.
- `cargo test -p cowfs-nfs --test namespace_race`: 5 passed, 0 failed (accepted fixture unchanged, not edited).
- `cargo test -p cowfs-nfs --test separate_adapter_namespace`: RED on main as above (1 passed, 1 failed).

## Resource facts

- Worktree: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/7/cowfs`, branch `test/nfs-separate-adapter-namespace-43` at `f5a492d`, working tree clean.
- Remote: `https://github.com/zeeshanhaque21/cowfs.git` (HTTPS; SSH blocked by design).
- Free disk at time of write: 223 GiB (floor 20 GiB never approached).
- Worktree `target`: 2.2G (cap 8 GiB never approached).
- No local cleanup, offload, or space freeing performed.
- Heavy cargo ran under `.treehouse-ready-wave/run_with_lock.py` with `.treehouse-ready-wave/mac-heavy.lock`.

## Ship-aftercare

PR #145 body was corrupted on open: literal `\n`, and a `mount(8)` command output injected where inline-code spans were (shell substitution of backticked names).
Body rewritten from a file (`--body-file`, no shell interpolation) and verified via `gh api pulls/145 --jq .body`: 0 literal `\n`, 41 real newlines, 0 `/dev/disk` output leak, code fences intact.
No evidence images in this PR (test-only, evidence is text), so the image-repair step is not applicable.

## Handoff: the fix needs a Core/daemon seam

This PR is the regression only.
Making it green requires a lock shared across the adapters that serve one `Core`, not a per-`Adapter` lock.
That is a change at the `Core` root-handle / `snapshot_view` seam (or a shared lock introduced where `Mount`/`Server` are constructed per export), which is a production change outside this test's scope.
Not attempted here.

## Scope

Test only.
Owns `crates/cowfs-nfs/tests/separate_adapter_namespace.rs` (new).
Does not edit `crates/cowfs-nfs/tests/namespace_race.rs`, `crates/cowfs-nfs/src/adapter.rs`, any manifest, dependency, CI, or production file.
No `cowfs-core` dependency added (`cowfs-core` is not a dev-dependency of `cowfs-nfs` and manifests cannot change); the shared backend is a local `Vfs` fixture.

## Not claimed

This receipt does NOT close #43.
It pins one uncovered topology (separate adapters over one namespace) as a permanent RED regression with an actual-CI reproduction.
`#43` remains open until the shared-lock fix lands and this test goes green.
