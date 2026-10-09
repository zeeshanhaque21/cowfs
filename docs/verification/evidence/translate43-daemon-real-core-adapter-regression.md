# translate43 daemon real-Core adapter regression

PR #145 head `fc98b42`, branch `test/nfs-separate-adapter-namespace-43`, draft, `Refs #43`.
Test only. Adds `crates/cowfs-daemon/tests/separate_adapter_namespace.rs` (new) and corrects
`crates/cowfs-nfs/tests/separate_adapter_namespace.rs`. No production, manifest, dep, or Core #142 change.

## Topology (real Core)
One `Core::open` -> `create_snapshot("s")` -> two `snapshot_view("s")` of the SAME snapshot ->
two `Server::start`, each its own `Adapter` over one `Core`, as `exports.rs` `mount_snapshot` ->
`backend.snapshot` does. A minimal raw-NFS client drives real RPCs on two connections
(`cowfs-nfs` is a macOS-only dep here and `nfsserve` types are unreachable, so frames are hand-rolled).
A `Vfs` wrapper holds adapter A's sidecar mutation after A's own guard read of `doc` answered, and
records ground truth at both moments. File is `cfg(target_os = "macos")`.

## Result: RED, deterministic (5/5 runs)
`cargo test -p cowfs-daemon --test separate_adapter_namespace` -> 3 passed, 1 failed.

```
REAL-CORE mkdir(._doc)_st=0 create(doc)_st=0 overlapped=false doc_present_during_window=true
guard_saw_main=false main_at_mutation=true real_dir_took_the_name=true doc_regular=true
depth_while_held=1 peak_depth=1
```

`guard_saw_main=false` while `main_at_mutation=true` is the defect: A's guard read of `doc` returned
absent while `doc` was live at the instant A's mutation ran, and a real `._doc` directory then took
the live sidecar view's name. `Adapter`'s per-directory lock is per instance; two adapters over one
snapshot namespace do not serialise. Likely fix: the guard read (`core.lookup`/`op_lookup`) does not
take `SnapCtx.ns` while the mutation (`core.mkdir` -> `make`) does. That is a Core/NFS change, not
this test. This PR adds no production code.

## Premise tests pass
`the_two_adapters_share_one_snapshot_namespace` (same GETATTR `fileid3` for one name through both),
`the_core_root_facade_is_read_only_for_make` (Core ROOT `mkdir` refused, so writes land in a
`snapshot_view`), `the_sidecar_channel_round_trips_valid_bytes_and_refuses_junk` (valid bytes
round-trip; implausible junk refused with `NOTSUPP` without corrupting the channel).

## nfs-crate fixture corrected
`crates/cowfs-nfs/tests/separate_adapter_namespace.rs`: lock-shape assertions (`peak == 1`,
`!overlapped`, `!doc_present_at_mutation`) demoted to diagnostics; the test now asserts the outcome
branch. `cargo test -p cowfs-nfs --test separate_adapter_namespace` -> 1 passed, 1 failed (outcome RED).

## Gates
`cargo fmt --all --check`: pass. Both test crates run under the wave's exclusive heavy lock
(`mac-heavy.lock`) with `target/ready43`; free 222 GiB (floor 20), target growth well under the 8 GiB cap.
