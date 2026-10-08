# #43 Translate namespace race: adapter lock, and the residual it cannot close

Status: WIP, draft PR open, not reviewed, not merged.
Branch: `fix/nfs-translate-namespace-race-43`, based on verified remote main `89353e17e5085000711dc428e834f9cc41840a1f`.
This document is the canonical receipt for this work.
The merged PR #111 receipt `docs/verification/ready-43.md` is unchanged.

## What was asked

Close the pre-existing namespace race left open by the #43 translate review.
`Translate` mode reads a name and then mutates the directory in a separate `Vfs` call, with no lock across the pair.
The narrow responsible seam is the owned `cowfs-nfs` `Translate` source plus a focused Rust test.
Nothing outside `cowfs-nfs` may change.
Do not fix a hypothesis.
Do not weaken an assertion, add a retry, or ignore a failure to go green.
Distinguish same-adapter concurrency from an outside-backend mutation, and do not claim a mutex fixes a direct path-`Vfs` write.
If the atomic condition cannot be met without a broader seam, stop and propose the correct layer.

## The race, stated precisely

`Adapter::not_a_view` calls `side_of`, which calls `peek`, which is `Vfs::lookup`.
The mutation that follows is a separate `Vfs::mkdir`/`symlink`/`link`/`create` call.
On the reviewed head there is no lock across that pair.
So two requests that both go through the adapter can interleave inside one directory's name space:

- request A: `mkdir(dir, "._doc")`. The guard reads the directory, sees no `doc`, and passes.
- request B: `create(dir, "doc")`. It makes `doc`.
- A's mutation then lands, and a real directory takes `._doc`.
- The view for `doc` is shadowed for the rest of the mount, and the attribute channel is dead.

This is reachable from one ordinary client.
The server accepts many connections and, within one connection, dispatches up to `max_in_flight` requests concurrently.
See `crates/nfsserve/src/tcp.rs`, `Limits::max_in_flight` default 32.
Every request runs on `tokio::task::spawn_blocking` (`Adapter::run`), on a four-worker runtime (`MountOptions`/`Server::start`).
So two in-flight RPCs run on separate blocking threads at the same time.

## The fix (adapter-reachable case)

One lock per directory, held across the guard read and the mutation for every name-space operation in the adapter.
`PerIno` (the existing per-inode weak-reference lock map) is reused, so writers of different directories do not wait for each other.
A new `Adapter.names: Mutex<PerIno>` field holds it.
`with_names(dir, f)` locks one directory for the whole guard-and-mutate sequence.
`with_two_names(a, b, f)` locks two directories in a fixed `Ino` order for `rename`, so two renames cannot take them in opposite orders and stop each other forever.
The `create`-style methods that treat an existing sidecar as a write keep their existing return behavior.
Only the lock scope changed, with the `durable_or` barrier and the `purge_sidecars` arms preserved.
In `remove` and `rmdir` the `translating` check moved inside the lock too, so a concurrent `create` of the same name cannot flip it between the check and the removal.

Files changed:

| file | change |
| --- | --- |
| `crates/cowfs-nfs/src/adapter.rs` | add `names` field, `with_names`, `with_two_names`; wrap `create`, `create_exclusive`, `mkdir`, `symlink`, `link`, `remove`, `rmdir`, `rename`; move the `translating` check inside the lock in `remove` and `rmdir` |
| `crates/cowfs-nfs/src/sidecar.rs` | `PerIno::of` is now `pub(crate)` so the adapter can reuse the same weak map |
| `crates/cowfs-nfs/tests/namespace_race.rs` | new test file: raw-NFS sample, adapter lock-contract test, refused-directory test |

No deadlock path exists.
The work under the lock is synchronous `Vfs` calls.
`side_create`, `side_remove`, and `purge_sidecars` never re-enter the adapter, and `with_two_names` returns through `with_names` when both directories are the same inode.

## The RED, on unmodified current main

Fixture: `crates/cowfs-nfs/tests/namespace_race.rs`.
A `Vfs` wrapper records how many guarded name-space operations the adapter runs at once for the root directory (`WatchVfs`).
It holds the first `mkdir` of an armed name at a barrier.
Two adapter calls are driven the way a client does.
The overlap probe is a bounded wait on the recorded high-water depth, not a fixed sleep, so a correct adapter that never overlaps simply runs the wait to its deadline.

On unmodified main `89353e17`, `cargo test -p cowfs-nfs --test namespace_race -- --test-threads=1 --nocapture`:

```
test a_sidecar_name_never_becomes_a_real_object_under_raw_nfs ... NFS mkdir(._doc)=0 create(doc)=0 real_dir_took_the_name=true peak_depth_while_held=2 peak_depth=2 names=["._doc", "doc"]
test the_guard_and_its_mutation_are_one_step ... ADAPTER peak_depth_while_held=2 peak_depth=2
test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
```

`peak_depth=2`: the guard read of `mkdir(._doc)` and the second request's name-space change are on the backend at the same time.
That is the window a real directory takes the view name in.

## The GREEN, with the fix

Same command on the fix:

```
test a_refused_directory_leaves_the_name_free ... REFUSE mkdir(._doc) over a live view: Some(13), ._doc real dir=false
test a_sidecar_name_never_becomes_a_real_object_under_raw_nfs ... NFS mkdir(._doc)=0 create(doc)=0 real_dir_took_the_name=true peak_depth_while_held=1 peak_depth=1 names=["._doc", "doc"]
test the_guard_and_its_mutation_are_one_step ... ADAPTER peak_depth_while_held=1 peak_depth=1
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.01s
```

`peak_depth=1`: the second request cannot enter the adapter until the first leaves.

Read the raw-NFS line honestly.
On the fixed tree `._doc` is a real directory and `doc` exists, because the lock serialised the two requests and `mkdir(._doc)` ran first, when `doc` was absent.
That is a legitimate serial outcome, not shadowing by an interleaved race.
The fix-distinguishing signal is `peak_depth` 2 to 1, not the final name set.

## What the fix does not close: the residual

The #43 critic's residual is a **direct `Vfs::create` of the main name injected inside the adapter's own `vfs.mkdir` call**, which bypasses the adapter.
See `bench/out/translate43-critic/tests/translate43_critic.rs`, `RaceVfs::maybe_race`.
An adapter lock only serialises writers that go through that adapter, so it cannot see a writer that holds the same `Vfs` by another path.

This was reproduced against the fixed tree to record the boundary, with the critic's shape ported directly (`bench/out/ready43-race/direct-injection-probe.rs`):

```
INJECT mkdir(._doc)=None real_dir_took_the_name=true
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

`real_dir_took_the_name=true`: the residual still reproduces with the lock in place.
So this fix **does not close the documented residual**, and this document does not claim it does.

## Why the residual needs a broader seam, not a bigger adapter lock

The same logical namespace can be written through more than one `Vfs` handle:

- `crates/cowfs-daemon/src/backend.rs` `root()` returns `Arc::new(c.clone()) as Arc<dyn Vfs>`, and `snapshot()` returns `cowfs_core::Core::snapshot_view(name)`.
  Both are independent handles over the same store.
- The daemon control plane and the mount therefore reach the same namespace through different `Vfs` instances.

No lock inside one adapter can serialise a writer that never calls that adapter.
The atomic condition ("the guard read and the mutation see the same directory state") has to be enforced **below** the adapter, where all writers meet.

Proposed narrow responsible layer:

The guard must become one atomic operation at the shared layer, not a read-then-write in the adapter.
Two shapes, both in `cowfs-vfs` plus its implementations:

1. A `Vfs` primitive that creates a name only if a companion name is absent, for example `mkdir_unless(dir, name, forbidden, mode)`.
   Its contract is that no writer can observe a state where `name` exists and `forbidden` exists, for any writer that uses the primitive.
   The adapter then does not peek and mutate; it makes one call.
2. Or an explicit per-directory name-space lock exposed by `Vfs`, which every writer that can change the namespace takes, including the control plane and snapshot views.

Either shape is a real seam change and is out of scope for this owned-`cowfs-nfs` task.
This document names it rather than approximating it in the adapter, which would be the silent weakening the task forbids.

## Gates

Exact head of this branch, all on the Apple M3 Max dev host:

| gate | command | result |
| --- | --- | --- |
| unit and protocol | `cargo test -p cowfs-nfs` | 155 passed, 0 failed, 17 test binaries and doc-tests all ok |
| format | `cargo fmt -p cowfs-nfs -- --check` | rc 0 |
| lint | `cargo clippy -p cowfs-nfs --all-targets -- -D warnings` | rc 0 |
| RED fixture on main | `cargo test -p cowfs-nfs --test namespace_race` | FAILED, 1 passed; 2 failed |
| GREEN fixture on fix | same | ok, 3 passed; 0 failed |

Toolchain: `rustc 1.99.0 (b940084d7 2026-09-28)`, `cargo 1.99.0`, `rustfmt 1.10.0-stable`.

Raw logs live in the lease under `bench/out/ready43-race/` (gitignored): `main-red.log`, `fixed-green.log`, `direct-injection-probe.log`, `full-suite.log`.
The primary-checkout copies are the source of truth for this receipt; the lease logs are the run artifacts.

## Reachability and limits (not claimed)

- The raw-NFS sample uses the in-process private server over loopback and two real connections with `one_shot_mount: false`.
  A second connection is refused by default `one_shot_mount: true`; this test turns it off on purpose.
- No system mount, daemon, shared store, SSH, or device host was used.
- This shows an adapter-reachable race at the protocol boundary.
  It does not show a macOS kernel reaching the corrupting path; the same limit as PR #111 applies, and whether the kernel reaches it is not claimed here.
- The direct-writer residual is not closed and is not claimed closed.
