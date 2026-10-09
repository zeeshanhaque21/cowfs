# Issue #43: separate-adapter namespace regression - real-Core correction and blocker

Corrects `translate43-separate-adapter-namespace-regression.md` (historical, preserved as written).
Independent audit: `docs/reviews/pr145-separate-adapter-regression-wbuddy-review.md`.
Exact head audited: `f5a492d419d37ac1096fcbfaca247ca436d7885c`, branch `test/nfs-separate-adapter-namespace-43`, worktree slot-7.
Test blob sha256 `89e9433766eaf3cc198db8ca6d2c77449fbec302cd2ec28ddc3930750bf03d91` (matches receipt and branch, re-verified).
Primary checkout `9874afae288b51159738f4b5f4a243bd3c822856`; protected dirty progress (`docs/v1-core.md`, `progress/`, untracked `docs/reviews/`) untouched.

## What the old receipt overstated (corrected)

- The claim "drives that topology directly at the public `Adapter::new` seam" is true only for the adapter boundary, not the Core namespace seam.
- Old "covered property" listed `peak == 1` and `!overlapped` (bounded `wait_peak(2, 1000)`) as pass/fail product requirements. That is a lock-shape proxy, not a user-visible outcome, and is over-specified.
- Old "Exact topology under test" called `SharedBackend { inner: MemVfs }` the truth of the topology. `MemVfs` has no per-snapshot `sc.ns` lock and its root is writable, so it cannot distinguish "per-adapter lock insufficient over a real per-snapshot namespace lock" from "no shared lock exists anywhere".

## Actual topology under test (verified from source)

- One `Arc<dyn Vfs>` (`SharedBackend` over `common::memfs()`), wrapped by two `Adapter::new` instances.
- `Adapter::new` builds a fresh `PerIno` `names` map per instance (`crates/cowfs-nfs/src/adapter.rs:228,236-243`), so the two maps are separate by construction.
- Adapter A `mkdir(._doc)` -> `not_a_view` -> `side_of` -> `main_of` -> `peek("doc")` guard read, then held at `self.vfs.mkdir(SIDE)`.
- Adapter B `create(doc)` through its own adapter; A released after B has run.
- The surrogate is `MemVfs` (`crates/cowfs-vfs-test`), NOT a `Core`/`snapshot_view`.

## Source vs runtime are separate

- SOURCE: `SnapshotView` is the only `Core`-backed `Vfs` (`crates/cowfs-core/src/view.rs`); `Core`'s root handle and every `snapshot_view` share one `SnapCtx.ns` per snapshot.
- The audit's source trace says the adapter guard read (`peek`/`lookup`) sits outside `sc.ns`, so the class-level read/mutation gap plausibly survives into the real `Core`/`snapshot_view` path.
- RUNTIME: no test constructs a real `Core` and two `snapshot_view`s; nothing here executes that path. The Core-seam RED is UNPROVED at runtime.

## Blocker: the required topology cannot be built in this file

- `crates/cowfs-nfs/Cargo.toml` deps: `async-trait`, `blake3`, `cowfs-vfs`, `nfsserve`, `signal-hook`, `thiserror`, `tokio`; dev-deps: `cowfs-vfs-path`, `cowfs-vfs-test`, `tempfile`. No `cowfs-core` (confirmed `Cargo.lock:337-350`).
- `cowfs-vfs-test`, `cowfs-vfs-path`, `cowfs-vfs` neither depend on nor re-export `cowfs-core`.
- `Core`, `Options`, `SnapshotView`, `snapshot_view` live only in `cowfs-core` (`lib.rs:58,189,315`; `view.rs`).
- The only consumers of `Core` + `snapshot_view` are `cowfs-daemon` (`src/backend.rs:492`) and `cowfs-fuse` (`tests/coherence.rs:206`), which own their own manifests.
- `common::serve` and `namespace_race.rs` also serve `common::memfs()`, not a real `Core`, so the audit's alternative route ("as `namespace_race.rs` does") does not reach `snapshot_view` either.
- Task constraint: no manifest/dependency edits. Adding `cowfs-core` as a dev-dependency is therefore out of scope for this lane.

## Honest status

- Test as written: UNCHANGED, blob sha256 as above. No rewrite attempted, because a `MemVfs` variant would be the same surrogate the audit rejected.
- CI run `37536162774` (completed, failure): the intended RED at the outcome assertion `!doc_present_at_mutation` (`separate_adapter_namespace.rs:326`). `linux-fuse` passed.
- The class-level defect is real per the audit's source trace. The real-Core runtime reproduction is BLOCKED, not disproved.
- No production, Meta, CI, manifest, dependency, or accepted-receipt change made.
- Free disk at write: 178 GiB (floor 20 GiB clear). No heavy cargo, no cleanup, no offload performed.

## Precise Core-lane handoff (no new global lock)

- Home for the real-Core regression: a crate that already depends on both `cowfs-core` and `cowfs-nfs` (`cowfs-daemon`, or a new test crate given its own dev-dep by its owner) - not `crates/cowfs-nfs/tests/`.
- Build: `Core::open(tmpdir, Options::default())`, `create_snapshot("s")`, two `Arc<dyn Vfs>` from `core.snapshot_view("s")` (same snapshot id), two `Adapter::new` over them, then the same mkdir-first vs doc-first race.
- Candidate minimal seam (audit): key the namespace lock on the shared snapshot (`SnapCtx.ns`, or a lock `Mount`/`Server` derive from the one `Core`) and route the guard read (`peek`/`lookup`) under it. Not a brand-new global lock, not a per-`Adapter` map.
- Accept only on the user-visible outcome through an actual `Core`: a live sidecar view is not shadowed, or (legal mkdir-first serialisation) a real `._doc` directory plus a truthful guard answer. Keep `peak`/`overlap` as diagnostics only.
- Preserve legal serial outcomes: mkdir-first returning a real directory and doc-first returning the sidecar view are both legitimate; `real_dir_took_name` alone is not a defect.
- Do not integrate unaccepted #142.
