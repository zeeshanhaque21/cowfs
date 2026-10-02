# v1 daemon

The daemon is the process that makes cowfs run.
It owns a `Vfs` backend, mounts it with the platform adapter, serves the control API of
`docs/v1-control-api.md`, and exports snapshots at paths a client chooses.
Implementation: `crates/cowfs-daemon/`.
Contracts it implements: `docs/v1-architecture.md` (the `Vfs` trait), `docs/v1-control-api.md` (the protocol), `docs/v1-treehouse.md` (the `mount_snapshot` rule table).

## The pieces

| Module | What it is |
|---|---|
| `daemon` | The process. Opens the backend, mounts, binds the socket, and shuts all three down in order. |
| `handler` | The `ControlHandler`: the snapshot namespace, the mount and the export registry behind the protocol. |
| `exports` | `mount_snapshot` and `unmount_snapshot`, and the whole rule table. |
| `mounts` | The platform adapter: NFS loopback on macOS, FUSE on Linux, plus signal cleanup and the stale-mount sweep. |
| `holders` | Who holds a path: cwd, open file or lock. `/proc` on Linux, `lsof` on macOS. |
| `import` | `import` and `base_refresh` on a passthrough backend. |
| `backend` | The seam: `PathBackend` today, `MemBackend` for tests, `cowfs-core` later. |

`Daemon::start` is the whole startup, in this order:

1. `prepare_platform`: sweep mounts of ours whose server is gone.
   The sweep is what keeps a `kill -9`'d daemon from leaving a mount that hangs every `ls` on it for twenty seconds or more on macOS 26.
2. Open the backend and canonicalise the store.
3. Build the export registry, with the mount point and the store as forbidden targets.
4. Mount the default view.
5. Bind the control socket.

The mount is made before the socket, so a bind failure unmounts rather than leaving a live mount with nothing to serve it.

## Shutdown order

A `shutdown` request, SIGTERM or SIGINT all end the same way, and the order is fixed:

1. unmount every export, in the order they were created,
2. unmount the default mount,
3. stop the control server and remove the socket,
4. drop the backend.

All synchronous.
`shutdown_mount_tree` takes the mount out of an `Option` before unmounting it, so a signal and a `shutdown` request racing each other cannot double-unmount, and the second one finds nothing to do.
A second signal exits at once with code 130: an operator who signals twice wants the process gone, and a daemon that will not stop is worse than one that stops untidily.

`install_backstop_signal_cleanup` exposes the platform adapters' own
`install_signal_cleanup` for a process that mounts and does nothing else.
The daemon does not install it: it unmounts and then calls `process::exit(128 + sig)` from its
own signal thread, which races the ordered shutdown above and usually wins it, leaving the
control socket on disk.
The daemon has its own handler, and `cowfs-ctl` removes a stale socket at the next start, so the
backstop is not needed for correctness.

## The backend seam

```rust
pub trait Backend: Send + Sync + fmt::Debug {
    fn root(&self) -> io::Result<Arc<dyn Vfs>>;
    fn snapshot(&self, name: &str) -> io::Result<Arc<dyn Vfs>>;
    fn store_path(&self) -> &Path;
    fn snapshots(&self) -> &dyn Snapshots;
}
```

`root` is the tree the default mount shows, and its root lists the snapshots as directories.
`snapshot` is one snapshot on its own, which is what an export at a client-chosen path mounts.
`Snapshots` is the control-plane namespace: list, create, remove, swap, rename, promote.

`cowfs-core` implements the same two traits.
Nothing above them changes when it lands, because the handler only ever talks to the traits.

### What `PathBackend` is not

It is a passthrough: every snapshot is a directory under the store, a clone copies, and there is no block store.
So `gc` and `fsck` answer `unsupported` rather than reporting a number that would be a lie, and `status` counts files and logical bytes rather than blocks.
Nothing is deduplicated: every byte is written once and stored once only because the store is an ordinary filesystem.
The dedup, the O(1) snapshots and the content addressing arrive with `cowfs-core`.

A `swap` here copies into a staging directory beside the target, then renames the old tree away and the new one in, so a failure part way leaves the old snapshot whole.
That is a real atomicity property, not the same one as the core's: a core swap is one root-pointer write with no copy at all.

`import` follows the migration rules in `docs/design.md`: copy the tree in, re-read the source, hash both with `cowfs_ctl::hash_tree`, and only then report success.
The report carries both root hashes, so a caller can hash the source itself and compare before it swaps a directory for the mount.
`base_refresh` checks out the ref with `git worktree add` rather than trusting a working tree, and records the real commit, so `base status` compares commits instead of comparing nothing.

## mount_snapshot

A client-chosen path is a mount primitive, so it is a capability and the server keeps it.
Every rule in the table in `docs/v1-treehouse.md` is enforced in `exports::Exports::check`, whatever the client asks for, and each has a test named after it.

| Rule | Where it is checked |
|---|---|
| absolute, at most 4096 bytes, no control characters | `validate_abs_path` from `cowfs-ctl` |
| inside a configured export root, at least three components below it | `root_for` and `MIN_COMPONENTS_BELOW_ROOT` |
| no component is a symlink, resolved from the root down, and owned by this uid | `check_components`, component by component with `symlink_metadata`, never one `realpath` at the end |
| no `..`, and the target is not a symlink | the `split('/')` check and the `symlink_metadata` arm |
| absent, or an empty directory | the `symlink_metadata` and `read_dir` arms |
| not the mount point, an ancestor of it, or the store | the `forbidden` loop, before containment, so the refusal names the real reason |
| the export root is private to this uid and not group or other writable | `check_root` |
| `name` passes `validate_snapshot_name` and the snapshot exists | the first two arms of `check` |
| one snapshot, one export, and one export per path | the two `live` arms at the end of `check` |
| `expect_no_holders` defaults to true, under the same lock as the export | the registry lock, held across the whole call |
| the export is atomic and a rejected one leaves no trace | the lock, and `a_refused_export_leaves_no_trace_and_a_good_one_then_works` |
| a crash mid-export leaves the old export or none | the same lock plus the startup sweep |
| the daemon owns the lifetime | `unmount_all`, called from `shutdown_mount_tree` |

The containment check is a plain component-by-component walk, not an `O_NOFOLLOW` `openat` chain, so a link swapped in between the check and the mount is still a hole.
Closing that needs `openat2` with `RESOLVE_BENEATH`, which is a follow-up, and it is listed in the requests at the end of this document.

`unmount_snapshot` mirrors it: `path` must be one the daemon exported, and it is `busy` with nothing changed while a holder is inside it.

The protocol has no `mount_snapshot` method yet, which is gap 1 in `docs/v1-treehouse.md`.
Until it lands, `Handler::mount_snapshot` and `Handler::unmount_snapshot` are the entry points, and the end-to-end test drives those against a live daemon.

## Caching and the invalidator

On Linux the mount runs in `MountMode::Shared` with a one second attribute cache, so nothing the kernel holds is older than a second, and every control-plane change calls `Invalidator::invalidate_all` after it is visible through the `Vfs`.
The invalidator is never called from inside a `Vfs` method the adapter is running: invalidating a name takes directory locks that a syscall waiting on the request may hold, which would deadlock the mount.
The handler calls it from the control thread, after the change.

On macOS there is no invalidator to call, and the NFS client re-reads on its own `actimeo` of 120 seconds, so a control-plane change is visible to a reader within that.
That is the adapter's measured default, not a daemon decision, and it is why a snapshot reset is not immediately visible through the mount on macOS.

## Running it

```sh
cowfs-daemon --store DIR --mount PATH [--socket PATH] [--export-root DIR]
```

`--export-root` is what makes `mount_snapshot` usable: without one, every path is refused, which is the safe default.

`cowfs serve --store DIR --mount PATH` is the same daemon, through `cowfs_daemon::open_handler` from the CLI's `Backend` trait, so the CLI owns the control server and the signal handling and the handler owns the mount.

## Tests

- `cargo test -p cowfs-daemon` runs the unit tests, including one test per row of the rule table and `cowfs_ctl::handler_conformance` against the handler.
- `cargo test -p cowfs-daemon --test end_to_end -- --ignored --test-threads=1` is the real run: a daemon process, a real mount, the real CLI, `SIGTERM` leaving nothing behind, and a `SIGKILL` whose stale mount the next start sweeps.
  It is `#[ignore]`d because it needs a mount adapter and because two real mounts of the same adapter cannot run beside each other.

## Requests for other crates

### `cowfs-ctl`

1. `mount_snapshot {name, path, expect_no_holders?}` and `unmount_snapshot {path}`, returning `mount_info` and `ok`, with `params` strict as `snapshot_rm` is.
   Both are new methods, which the evolution rules already allow within major version 1, and a client learns they exist from `hello.methods`.
2. `expect_no_holders` on those two must be evaluated under the same per-snapshot lock the framework already keeps, the way it is for `snapshot_rm` and `snapshot_reset`.
   The registry in `Exports` has its own lock today, so the two are not yet the same lock and a holder can appear between the framework's check and the export.
3. `Handler::mount_snapshot` and `Handler::unmount_snapshot` are the code the dispatch arms would call; nothing in them needs to change.
