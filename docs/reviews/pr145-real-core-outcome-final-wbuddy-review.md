# PR145 real-Core outcome: independent validity audit (final)

PR #145 `test(nfs): separate-adapter namespace regression for #43`; branch `test/nfs-separate-adapter-namespace-43`.
Full SHA audited: `fc98b429313ddec7ff79df90141dcc3b22070cef` (== `refs/pull/145/head` == remote branch head); base parent `f5a492d419d37ac1096fcbfaca247ca436d7885c`.
Verdict: SOURCE HYPOTHESIS ONLY. No runtime proof at this head; the new test has no independent CI, and current-head macOS CI is RED on clippy, not on the test.

Settled contract (not a new global serialization policy): two serial products exist.
mkdir-first: A's guard read of `doc` was absent, so `._doc` is a real directory; final `{._doc = real dir, doc = regular file}`.
doc-first: B created `doc` first, so A's guard sees it and `not_a_view` refuses; final `{._doc = live sidecar view of doc, doc = regular file}`.
Illegal: the final state fits NEITHER serial product, or an operation result misdescribes the state it ran against.

CI facts (one bounded completed-log audit of the current head).
Run `37541923711` head_sha `fc98b42` is current head. macOS job `112536718211` completed: failure.
It failed in step `cargo clippy --workspace --all-targets -- -D warnings`, which precedes `cargo test`; the `cargo test --workspace` step is `skipped`.
Exact completed-log error: `error: manual implementation of .is_multiple_of()` at `crates/cowfs-daemon/tests/separate_adapter_namespace.rs:81:15` (`while self.0.len() % 4 != 0`).
`clippy::manual_is_multiple_of` is implied by `-D warnings`, so `could not compile cowfs-daemon (test "separate_adapter_namespace")`.
Consequence: the new four-test file never executed in this CI on any runner.
ubuntu `cargo test --workspace` passed (94 binaries; no daemon `separate_adapter_namespace` binary) and never compiles the file: `cowfs-nfs` is `cfg(target_os = "macos")` only and the file is `#![cfg(target_os = "macos")]`.
The earlier failed run `37536162774` is head_sha `f5a492d` (the parent), not evidence about this head.
The local "RED, 5/5" receipt is a local run, not independent CI, and cannot stand in for a CI result that does not exist.

Runtime proof at fc98b42: absent.
The new test prints `REAL-CORE ...`; a completed-CI search of the current-head macOS log found `REAL-CORE` 0 times and all four new test names 0 times.
So there is no CI observation of `guard_saw_main`, `main_at_mutation`, `real_dir_took_the_name`, `overlap`, or `peak` at this head; the claim rests on an unshipped local run that cannot be reproduced.

Topology (genuine, unlike the earlier nfs fixture).
The file builds a real `Core::open` -> `create_snapshot("s")` -> two `snapshot_view("s")` of the SAME snapshot -> two `Server::start` -> two raw-NFS `Client`s.
`crates/cowfs-daemon/Cargo.toml` gates `cowfs-nfs` under `[target.'cfg(target_os = "macos")'.dependencies]`.
Premise tests pin real facts: two adapters share one snapshot namespace (same `fileid3` for one name), `Core::mkdir(ROOT_INO)` is `ReadOnly` while `snapshot_view().mkdir(ROOT_INO)` succeeds, and the sidecar channel round-trips valid bytes and refuses junk with `NOTSUPP`.
`SnapshotView` maps `ROOT_INO` to the snapshot root (`crates/cowfs-core/src/view.rs:25-34`), so a real directory under `._doc` is a genuine product outcome.
This is a genuine topology; it is not the `MemVfs` surrogate the earlier PR145 review rejected, and the root-facade-vs-view distinction is asserted, not merely claimed.

The outcome assertion can forbid a legal result.
The illegal branch fires on `main_at_mutation == true`, where `main_at_mutation` is an OBSERVER probe: the `GuardedView` wrapper calls `inner.lookup(parent, MAIN)` at the instant its `mkdir(SIDE)` runs.
That probe is not a result of any operation in the history; it is a third-party read injected at one instant.
The locally observed final state `{._doc = real dir, doc = regular file}` equals the mkdir-first serial product, so the namespace is linearizable to mkdir-first; only the mid-operation observer makes it "illegal".
A `mkdir(._doc)` request legitimately spans `[guard read, mutation]`, and concurrent `create(doc)` may land inside that span while A's op is still correctly ordered before it at A's guard read.
Whether this is a true anomaly depends on A's linearization point; the test fixes it at the mutation via the observer, assuming an internal ordering rather than showing a misordered product result.
Missing to settle it: the actual serial product RPC statuses, final namespaces and types, handles and `fileid`s, and `doc`/channel bytes, compared against the observed run.
Without that, `guard_saw_main=false` with `main_at_mutation=true` is a SOURCE hypothesis that the guard read and the mutation do not share `SnapCtx.ns`, not a proven non-linearizable outcome.

Source trace: seam exists, runtime path unproven.
`Core::op_lookup` (`crates/cowfs-core/src/ns.rs:60`) does `self.dir(parent)` then `dent_lookup` with no `sc.ns.lk()`.
`Core::make` (`crates/cowfs-core/src/ns.rs:163`) takes `let _ns = sc.ns.lk()` before the existence check and insert.
`SnapCtx.ns` is one `Mutex<()>` per mounted snapshot (`crates/cowfs-core/src/queue.rs:223`), shared by every `snapshot_view` of that snapshot.
The `Adapter` guard is `with_names` over `self.names: Mutex<PerIno>` (`crates/cowfs-nfs/src/adapter.rs:203-232, 640`), per instance; two adapters over one snapshot hold disjoint `PerIno` maps.
So the guard read (`op_lookup`, no `ns`) and the mutation (`make`, holds `ns`) take different locks, and the two-adapter case has no shared adapter lock either.
Real structural asymmetry at the Core seam, but not proof the end-to-end product misorders; the missing independent run is the proof.
Do not treat the historical "class-level defect" hypothesis as runtime proof.

Minimal responsible production seam (only if a defect is later established): `Core::op_lookup` must take `sc.ns.lk()` for the snapshot it reads so the guard read is atomic with `make` on the same snapshot namespace.
No new global serialization, no daemon-level lock, no adapter-wide lock is implied or required; no code edits here.

Operational compliance gaps (recorded separately, artifacts preserved): the receipt records free 222 GiB and target growth under the 8 GiB cap.
Those are OBSERVED headroom and growth, not a projected compile-peak preflight; neither the heavy lock nor a small target bounds a projected peak, so the compile-peak cap is UNVERIFIED.
Independent of the CI result; nothing is cleaned up, archived, or altered.

Verdict:
- Source: the guard-read/mutation lock asymmetry is real at `Core::op_lookup` vs `Core::make` (`ns.rs:60`, `ns.rs:163`; `queue.rs:223`).
- Runtime: NOT established at `fc98b42`. No independent CI ran the new test; current-head macOS CI failed on clippy at `separate_adapter_namespace.rs:81` before `cargo test`.
- Serial counterexample: NOT shown. The observed final state equals the mkdir-first serial product and is linearizable to it; the "illegal" label rests on a mid-operation observer, not a misordered product result.
- Illegal final result: NOT shown.
- Scope: fixed issue-68 scope; no new locks, no new requirements, no whole-#43 acceptance, no code edits.
- Full SHA audited: `fc98b429313ddec7ff79df90141dcc3b22070cef`.
