# PR145 separate-adapter namespace regression: independent validity audit

Exact head audited: `f5a492d419d37ac1096fcbfaca247ca436d7885c` (test-only PR #145, base `main`).
Test file: `crates/cowfs-nfs/tests/separate_adapter_namespace.rs`, 370 lines, sha256 `89e9433766eaf3cc198db8ca6d2c77449fbec302cd2ec28ddc3930750bf03d91` (matches the receipt, verified against the branch blob).
Prior review carried forward: `docs/reviews/pr143-rpc-outcome-channel-final-wbuddy-review.md` (F5).
Primary docs read: `docs/design.md`, `docs/verification/evidence/translate43-separate-adapter-namespace-regression.md`.
Origin/main at audit: `1580e69b9d987f63c07b2430f8c0b4547ecd8622` (PR #143 merge). Primary checkout HEAD `9874afae`; branch blob read via `git show f5a492d:...`, not the primary tree.

## Actual CI run (completed)

Run `37536162774`, workflow `ci`, conclusion `failure`.
Jobs: `112517503112` check (ubuntu-latest) `failure`; `112517503381` check (macos-latest) `failure`; `112517503434` linux-fuse `success`.
Both check jobs fail on the same test; linux-fuse passing shows the failure is the workspace unit test, **not** a FUSE/proxy artifact.
Raw signature (macOS): `SEPARATE-ADAPTER mkdir(._doc)_ok=true create(doc)_ok=true overlapped=true
doc_present_at_mutation=true real_dir_took_the_name=true depth_while_held=2 peak_depth=2`.
Panic: `crates/cowfs-nfs/tests/separate_adapter_namespace.rs:326:5`, message "adapter B created doc while adapter A was still inside its guarded sidecar mutation".
`the_two_adapters_share_one_namespace ... ok`; result `1 passed; 1 failed`.

## Raw assertion meaning (corrected reading)

Line 326 is the **first** assertion: `!doc_present_at_mutation` (the user-visible fact that `doc` exists inside A's guard window).
The `backend.peak() == 1` assertion (line 331) and `!overlapped` (line 338) **never execute** on RED, because 326 panics first.
So the observed RED is the outcome assertion, not a bare lock-shape assertion. On GREEN the fixture would still demand `peak == 1` and `!overlapped`, which are lock-shape proxies, not user-visible outcomes.
`real_dir_took_the_name` is reported, not asserted (line 341-345); on a correctly serialised arrangement A runs first, sees `doc` absent, and a real `._doc` is legitimate.

## Source trace of the real topology

Product path: `CoreBackend::snapshot` -> `Core::snapshot_view(name)` -> `SnapshotView { core, root }` (`crates/cowfs-daemon/src/backend.rs:490-492`, `crates/cowfs-core/src/view.rs:14-45`).
Each export is its own `Mounted::mount(vfs, path)` -> per-export `Adapter` (`crates/cowfs-daemon/src/exports.rs:113-140`); two exports of one snapshot therefore reach one `Core` through two `Adapter` instances.
Guard read: `Adapter::mkdir` -> `not_a_view` -> `side_of` -> `translating` -> `peek` -> `vfs.lookup` (`adapter.rs:627-641`, `sidecar.rs:142-151`), then mutation `vfs.mkdir`.
`Adapter::new` builds a fresh `PerIno` map **per instance** (`adapter.rs:224-234`); `PerIno` holds per-`Ino` `Weak<Mutex<()>>` (`sidecar.rs:105-121`). Two adapters cannot share it by construction.
Core side: `SnapshotView::mkdir` -> `Core::mkdir` -> `inner.make` (`vfs_impl.rs:40-46`) -> `let _ns = sc.ns.lk();` (`ns.rs:163-169`), a **per-snapshot** lock looked up by snapshot id.
Critically, the guard read `Core::lookup` -> `op_lookup` (`ns.rs:60-72`) does **not** take `sc.ns`.
So even in the real `Core`/`snapshot_view` topology the read-then-write window between the guard read and the mutation is not closed by the Core namespace lock. The defect class is real, not an artifact of the surrogate.

## The three separate questions

1. True product defect: **confirmed, class-level.** Entering the guard window across two adapters can leave a real object shadowing a live sidecar view; a correct arrangement serialises the read against the foreign mutation. This is not a settled topology-agnostic global-serialisation requirement, but the read/mutation atomicity gap is genuine at the Core seam.
2. Test over-specification: **present.** `peak == 1` and `!overlapped` encode a lock-shape (`the backend never saw two guarded ops at once`) rather than the user-visible outcome. A minimal correct fix (guard read and mutation atomic per snapshot namespace) would satisfy `!doc_present_at_mutation` but need not guarantee `peak == 1` measured at the backend, because the fixture counts backend depth, not the protected invariant. The test must not promote `peak == 1` into a product requirement that blocks legal concurrency on unrelated directories.
3. Topology gap: **present and unproved.** The fixture's two adapters wrap `SharedBackend { inner: MemVfs }` (`common::memfs()`, `memvfs.rs:223-234`), not a `Core`/`snapshot_view`. `MemVfs` has no `sc.ns` lock, its root is writable, and `Core::make(parent=ROOT_INO)` is `ReadOnly` (`ns.rs:166`) while the fixture mkdirs at the MemVfs root. The surrogate cannot distinguish "per-adapter lock insufficient over a real per-snapshot namespace lock" from "no shared lock exists anywhere". The `root handle vs snapshot_view` coverage the receipt claims remains **unproved**; no test constructs a `Core` and two `snapshot_view`s.

## Is MemVfs predictive?

For the **adapter-level** property the surrogate is defensible: the accepted single-adapter fixture `namespace_race.rs` also uses `common::memfs()` and was accepted (prior review F1-F4 resolved). For the **Core namespace seam** the surrogate is not predictive: it never creates a `sc.ns`, so it cannot show whether a shared Core lock would close the window, which is the whole point of the #43 handoff.

## Fix-boundary guidance

Do not accept the handoff's "global Core lock" as the fix without first pinning the user-visible illegal outcome.
The responsible boundary is the guard read plus the mutation for one name in one directory needing to be atomic against every other adapter reaching the same snapshot namespace.
The minimal candidate seam is keying the namespace lock on the shared snapshot (the existing `SnapCtx.ns`, or a lock `Mount`/`Server` derive from the shared `Core`), and routing the guard read (`peek`/`lookup`) under it, not a brand-new global lock and not a per-`Adapter` map.
Only after the illegal outcome (`doc` in tree while `._doc` sidecar mutation is in flight, or a real `._doc` directory shadowing a live sidecar view) is reproduced through an actual `Core` with two `snapshot_view`s should the fix be accepted.

## Required fixture correction before this can gate #43

Replace the `MemVfs` surrogate in the failing test with a real `Core` and two `snapshot_view`s cloned from it (or drive the same race through two raw NFS mounts on one exported snapshot, as `namespace_race.rs` does for the single-adapter case), so the assertion is on a user-visible outcome (a live sidecar view is not shadowed; a legal mkdir-first serialisation returns a real directory and a truthful guard answer). Keep `peak`/`overlapped` as diagnostics, not pass/fail product requirements.

## Operational evidence gaps (do not invalidate independent CI)

Receipt resource facts state free disk 223 GiB and worktree `target` 2.2G; that is observed headroom, not a projected-peak preflight. A projected peak under the mac-heavy lock was not measured; compliance is **unverified**, recorded separately from the CI evidence.
Local heavy cargo ran under the mac-heavy lock; the CI failure above is independent of that lock.

## Verdict

- Red test equals defect: partially - the failing outcome assertion pins a real class-level gap, but the test as written is **over-specified** and evaluated on a **topology surrogate**.
- Real bug: `!doc_present_at_mutation` class is genuine at the adapter guard-read/mutation seam and, per source trace, survives into the real `Core`/`snapshot_view` path because `op_lookup` is outside `sc.ns`.
- Not accepted as a #43 gate as-is: topology gap (no `Core`/`snapshot_view` construction) and `peak == 1` over-specification must be fixed first.
- Scope discipline: stay within #43; no new global-serialisation requirement; no whole-issue closure from this PR.
- Full report sha256 recorded on write. Historical receipts immutable; this file is the only artifact produced.
