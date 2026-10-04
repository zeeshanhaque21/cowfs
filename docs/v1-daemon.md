# v1 daemon

The daemon is the process that makes cowfs run.
It owns a `Vfs` backend, mounts it with the platform adapter, serves the control API of
`docs/v1-control-api.md`, and exports snapshots at paths a client chooses.
Implementation: `crates/cowfs-daemon/`.
Contracts it implements: `docs/v1-architecture.md` (the `Vfs` trait), `docs/v1-control-api.md` (the protocol), `docs/v1-treehouse.md` (the `mount_snapshot` rule table).

## Known limits

macOS NFS on the core backend is broken until issue #53 is fixed: two end-to-end tests fail as its repro, because a directory the mount created goes stale once the write-back layer commits it. Linux FUSE passes everything.

## The pieces

| Module | What it is |
|---|---|
| `daemon` | The process. Opens the backend, mounts, binds the socket, and shuts all three down in order. |
| `handler` | The `ControlHandler`: the snapshot namespace, the mount and the export registry behind the protocol. |
| `exports` | `mount_snapshot` and `unmount_snapshot`, and the whole rule table. |
| `mounts` | The platform adapter: NFS loopback on macOS, FUSE on Linux, plus signal cleanup and the stale-mount sweep. |
| `holders` | Who holds a path: cwd, open file or lock. `/proc` on Linux, `lsof` on macOS. Never this daemon. |
| `import` | `import` and `base_refresh` on the passthrough backend. |
| `backend` | The seam: `CoreBackend` is the real one, `PathBackend` for a store on another filesystem, `MemBackend` for tests. |

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
4. close the backend.

All synchronous.
The backend goes last because it holds the store lock, and the mount has to be gone before the tree it serves is closed underneath it.
`Core::close` is what releases that lock: it flushes, closes the metadata database and the block store, and reports a failure that dropping the handle could not.
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
    fn usage(&self) -> io::Result<Option<Usage>>;      // blocks and bytes, for status
    fn close(&self) -> io::Result<()>;                  // durable, and release the store lock
    fn ingests_directories(&self) -> bool;              // base_refresh only; import uses ingest
    fn ingest(&self, from: &Path, name: &str, progress: &mut dyn FnMut(u64, u64) -> bool)
        -> CtlResult<Option<Ingested>>;                  // ingest through this backend's writer
    fn fsck(&self) -> io::Result<Option<FsckReport>>;
}
```

`root` is the tree the default mount shows, and its root lists the snapshots as directories.
`snapshot` is one snapshot on its own, which is what an export at a client-chosen path mounts.
`Snapshots` is the control-plane namespace: list, create, remove, swap, rename, promote.

## `CoreBackend`: the real one

`CoreBackend` serves `cowfs-core`: the `Vfs` over the block store and the metadata tree, whose
root already lists the snapshots, so the default mount needs no wrapper and an export mounts a
`SnapshotView`.

Every snapshot operation is the core's own control plane. No tree is ever copied.

| Control API | Core |
|---|---|
| `snapshot_list` | `list_snapshots`, parent id resolved to a name |
| `snapshot_create {name}` | `create_snapshot` |
| `snapshot_create {name, from}` | `fork_snapshot`, which is O(1) |
| `snapshot_rm` | `remove_snapshot`, refused while a handle is open |
| `snapshot_reset {name, from}` | `promote_base(from, name)`: the staged swap, one fork and one rename, with an intent record |
| `snapshot_rename` | `rename_snapshot`, staged the same way |
| `snapshot_promote` | a set in the daemon, because the core does not record base-ness |
| `fsck` | `Core::fsck`, mapped to the protocol's report |
| `gc` | `Core::collector` through the reference gate: a mark-and-sweep over the real store that reclaims packs. See below |
| `import` | `import::ingest`: a reserved staging snapshot, a durable read-back compared byte for byte with the source, then one fork into the name. See below |
| `base_refresh` | refused, see below |

Two of those need a word.

A reset is `promote_base(src, base)`, which is exactly "replace `base` with a fresh clone of
`src`", so it is the core's crash-safe swap rather than a delete and a copy. The core forks
twice, so the parent it records is the staging snapshot it then removes; the daemon reports the
snapshot the client named as `from`, which is what the protocol's `parent` means. The core also
creates the target when it is absent, which a base wants and a reset does not, so the daemon
checks that both names exist first and answers `not_found`.

`promote` is the one thing the daemon keeps rather than asks for. `cowfs-meta`'s `SnapshotInfo`
has no base flag, and `snapshot_promote` says how a snapshot is used rather than what is in it,
so the set lives beside the backend and does not survive a restart. `PathBackend` keeps the same
set for the same reason. If a base has to be durable, it belongs in `cowfs-meta` (#42).

### Refusing to open a damaged store

`Core::open` refuses a store whose recovery report has damage to data a completed sync made
durable, and the daemon surfaces that refusal as it stands. It does not acknowledge the loss:
`Store::acknowledge_corruption` is the operator's decision, and a daemon that took it silently
would be making it on their behalf. The error names the store and says so.

A store that is merely torn past the durable watermark is not damage and opens as normal; the
bytes are cut into sidecars and reported.

### `import` on the core backend

`import` is the one directory-ingest operation the core backend has, and it goes through the core
rather than through the store directory: `crates/cowfs-core/src/import.rs`. The bytes go into a
staging snapshot whose name is reserved (the same one a swap stages into, so a crash leaves
nothing a caller can see), the tree is made durable, read back through the `Vfs` and compared with
the source byte for byte, and only then is the name made visible.

The switch is one fork of the staging snapshot into the requested name, so it is a single root
write. An import of the same content twice therefore costs nothing the second time: the blocks are
already stored, and the report's `stored_bytes` says so (0 for a repeat of identical content).

- Peak extra disk is bounded by construction: the source is opened read-only and no uncompressed copy
  is staged on disk, so the only new bytes are the store's own compressed, deduplicated blocks and
  one 64 KiB buffer (`import::CHUNK`) is in flight per file.
  `import-e2e.sh` measures the read-only claim rather than asserting it: a manifest of the whole
  source tree, taken before the daemon starts, is compared with one taken after the last import, over
  every path, kind, mode, size, SHA-256, symlink target, and mtime, ctime, inode, link count, uid
  and gid. atime is excluded on purpose, because the import reads the source and atime is expected to
  move.
- Symlinks are kept as symlinks and never followed. Anything else that is not a regular file or a
  directory (a fifo, a socket, a device) is refused with `invalid_params`, because the core cannot
  hold one and dropping it silently would make the imported tree differ from the source.
- No directory is special-cased, `.git` included: the source is copied as it is found.
- A name that exists is `already_exists`. An import never replaces a snapshot.
- A mismatch, a cancellation or an error leaves the store exactly as the call found it: the staging
  snapshot is dropped and no snapshot named as requested exists.
- Two concurrent imports naming the same snapshot cannot both write. `import` takes no per-name
  snapshot lock from the control server (unlike `snapshot_rm`, `snapshot_reset` and `mount_snapshot`,
  which do), but it does not need one: `Cowfs` sits behind a single `Mutex` in the backend
  (`CoreSlot` in `backend.rs`), and `ingest` is called with that mutex held for its whole duration,
  from `check_new_name` through `finish_swap`. So the loser of the race does not find a half-written
  staging snapshot, it runs afterwards and gets `already_exists` from `check_new_name`. Across
  processes it is stronger still: the store holds an exclusive `flock` on its `LOCK` file, so a
  second daemon on the same store refuses to open it.
- `base_refresh` is still refused here, because it copies a git worktree into the store directory,
  which means nothing for a backend whose snapshots are trees. The handler answers `unsupported`
  and says to copy the source through the mount instead.

`gc` is wired over the real core: `CoreBackend` registers the run, clones the `Core` out of its
slot, builds a `Collector` with `Core::collector`, and runs the cycle on a scoped thread while the
calling thread forwards progress and polls the client's cancel every 50 ms.
Design, lock order and failure behaviour are in `docs/gc-core-integration.md`.
`CtlResult` is `ok` with a `GcReport` whose `candidate_blocks`/`candidate_bytes` are an upper bound;
a live request that frees nothing because the reference side would not hold still is `busy`, and a
cycle error is `io_error`, so a quiet success is never a silent failure.
`fsck` maps to `Core::fsck` on this backend too.

### What `PathBackend` is not

It is a passthrough: every snapshot is a directory under the store, a clone copies, and there is no block store.
So `gc` and `fsck` answer `unsupported` rather than reporting a number that would be a lie, and `status` counts files and logical bytes rather than blocks.
It exists for a store that already lives on another filesystem, and for tests that need no store.

A `swap` here copies into a staging directory beside the target, then renames the old tree away and the new one in, so a failure part way leaves the old snapshot whole.
That is a real atomicity property, not the same one as the core's: a core swap is one root-pointer write with no copy at all.

`import` follows the migration rules in `docs/design.md`: copy the tree in, re-read the source, hash both with `cowfs_ctl::hash_tree`, and only then report success.
The report carries both root hashes, so a caller can hash the source itself and compare before it swaps a directory for the mount.
This backend has no block store, so its report has no `stored_bytes`: every byte is written once and stored once only because the store is a normal filesystem.
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
| `expect_no_holders` defaults to true, under the same lock as the export | the framework's per-snapshot lock, through `HolderGuard` |
| the export is atomic and a rejected one leaves no trace | the registry lock, and `a_refused_export_leaves_no_trace_and_a_good_one_then_works` |
| a crash mid-export leaves the old export or none | the same lock plus the startup sweep |
| the daemon owns the lifetime | `unmount_all`, called from `shutdown_mount_tree` |

The containment check is a plain component-by-component walk, not an `O_NOFOLLOW` `openat` chain, so a link swapped in between the check and the mount is still a hole.
Closing that needs `openat2` with `RESOLVE_BENEATH`, which is a follow-up, and it is listed in the requests at the end of this document.

`unmount_snapshot` mirrors it: `path` must be one the daemon exported, and it is `busy` with nothing changed while a holder is inside it.

### Who a holder is

A holder is a process inside the snapshot **as a client sees it**, which is a directory under the
default mount, not a path in the store.
On the core backend there is no per-snapshot directory in the store at all, so scanning the store
would report no holder for any snapshot and `expect_no_holders` would be decoration.
`Exports` therefore holds the mount point and scans `mount/{name}`, which is also what `ps`
scans, so the two agree.

It is never this daemon.
The daemon holds the store and the mount it serves by definition, so counting it would make
every `expect_no_holders` check `busy` the moment the store had been read once.
`holders::scan` drops this process's own pid.

The two facts together are why the end-to-end export test passes on the core backend: on the
passthrough backend the daemon's own open descriptors under `store/base` were reported as the
holder of `base`.

### The holder check and the lock

`expect_no_holders` runs under the framework's per-snapshot lock, the same one `snapshot_rm` and
`snapshot_reset` use, so a holder cannot appear between the check and the export.
`cowfs-ctl`'s dispatch builds the `HolderGuard`, checks, and calls the handler; the handler takes
`guard.lock()` across its own check and the export.
`handler_conformance` checks that, and the daemon's handler passes it.

## Caching and the invalidator

On Linux the mount runs in `MountMode::Shared` with a one second attribute cache, so nothing the kernel holds is older than a second, and every control-plane change calls `Invalidator::invalidate_all` after it is visible through the `Vfs`.
The invalidator is never called from inside a `Vfs` method the adapter is running: invalidating a name takes directory locks that a syscall waiting on the request may hold, which would deadlock the mount.
The handler calls it from the control thread, after the change.

On macOS there is no invalidator to call, and the NFS client re-reads on its own `actimeo` of 120 seconds, so a control-plane change is visible to a reader within that.
That is the adapter's measured default, not a daemon decision, and it is why a snapshot reset is not immediately visible through the mount on macOS.
It is also why the end-to-end tests check a reset by exporting the snapshot at a fresh path and reading that, rather than reading the default mount.

## Running it

```sh
cowfs-daemon --store DIR --mount PATH [--socket PATH] [--export-root DIR] [--backend core|path]
```

`--backend core` is the default: it is the real one.
`--backend path` serves a directory of a native filesystem instead.

`--export-root` is what makes `mount_snapshot` usable: without one, every path is refused, which is the safe default.

`cowfs serve --store DIR --mount PATH` is the same daemon, through `cowfs_daemon::open_handler` from the CLI's `Backend` trait, so the CLI owns the control server and the signal handling and the handler owns the mount.

## Tests

- `cargo test -p cowfs-daemon` runs the unit tests: one test per row of the rule table, `cowfs_ctl::handler_conformance` against the handler, and the core backend's own namespace, error codes, usage, and store-lock tests.
- `cargo test -p cowfs-daemon --test end_to_end -- --ignored --test-threads=1` is the real run: a daemon process, a real mount, the real CLI.
  It is `#[ignore]`d because it needs a mount adapter and because two real mounts of the same adapter cannot run beside each other.
  On the core backend it writes a multi-MiB file and 300 small ones through the mount, clones, writes into the clone, resets it back, SIGTERMs and reopens, then SIGKILLs and reopens again, checking the fsynced bytes are there and the stale mount was swept.
  It also damages a pack and checks the next start refuses to open the store and says why.
- On Linux with FUSE all five pass.
  On macOS with the NFS loopback the two that `mkdir a/b` on a directory the mount just created fail with `Stale NFS file handle`, because `cowfs-core` releases a created file's virtual inode alias as soon as its create commits while the stateless NFS client still holds the handle (#53).
  They are left failing rather than weakened: that is the repro.

## Requests for other crates

### `cowfs-ctl`

1. None outstanding. `mount_snapshot {name, path, expect_no_holders?}` and `unmount_snapshot {path}` are methods, listed in `hello.methods`, strict in `params`, and their holder check runs under the per-snapshot lock the framework keeps.
2. The containment walk in `Exports` is `symlink_metadata` per component, not `openat2(RESOLVE_BENEATH)`.
   Closing it needs `unsafe_code`, which this crate denies, so it belongs in `cowfs-ctl` next to the socket code.

### `cowfs-core`

1. An alias release that a stateless adapter cannot survive (#53).
   `maybe_drop_alias` drops a file's virtual inode number once its create commits and nobody holds a reference, but `cowfs-nfs` calls `forget` the moment it hands the attribute out, because it has no handle table.
   The client keeps a handle for a number that is now stale, so `mkdir a` then `mkdir a/b` fails on the mount.
   Either the core keeps an alias while the inode is reachable, or the adapter holds a reference per live handle.
   This also decides what an `Ino` means to a stateless client, which the `cowfs-vfs` `forget` contract currently cannot express.
2. `SnapshotInfo` has no base flag, so `snapshot_promote` is daemon-side state that a restart forgets (#42).
3. `import` needs a writer that goes through the `Vfs`, not the store directory.
