# NFS namespace durability (issue #90)

A `rename` on the macOS NFS mount used to be answered from memory, and the caller's follow-up sync
never reached the server.
The name came back at its old name after the daemon died, with `fsck` clean and the file's bytes
intact: an uncommitted name, not corruption.
This is what was measured, what changed, and what is still not promised.

## What was measured

Real `cowfs-daemon --backend core`, the real `cowfs` CLI, a real NFSv3 loopback mount, a private
store under `bench/out/durability90/`, `SIGKILL` of the fixture-owned pid 2 to 6 ms after the
caller returned, and the same store reopened by a fresh daemon on a fresh mount.
The harness is `crates/cowfs-daemon/tests/namespace_durability.rs`; every rep starts, mounts,
kills and mounts again, so the matrix is deliberately three reps per case and no more.

The file is written and `fsync`ed *before* the rename, so the only uncommitted thing left at the
kill is the rename itself.
Each row says what the caller did after `os.rename` returned.

| after `rename` | before | after |
|---|---|---|
| `os.fsync(dirfd)` on the parent | new name **lost**, old name back, 0/3 | kept, 3/3 |
| `os.fsync(fd)` on a read-only fd to the renamed file | new name **lost**, old name back, 0/3 | kept, 3/3 |
| `write` + `fsync` of a sibling file in the same snapshot | kept, 3/3 | kept, 3/3 |
| nothing at all | new name **lost**, old name back, 0/3 | kept, 3/3 |

The third row is the control that was already there.
It proves the rename is committable on this daemon and that the first two syncs simply never cross
the wire, so the loss was never a store or metadata problem.

The native control is in the same test: the identical recipe on APFS, with the worker `SIGKILL`ed
instead of a daemon.
It passes, and it cannot fail on durability, because a process kill does not touch the page cache.
What it establishes is that the recipe, the readback and the cleanup are sound, not that the sync
mattered.
Only a machine crash distinguishes `fsync` from no `fsync` on APFS, which is why the cowfs numbers
above are the load-bearing ones.

Evidence, gitignored: `bench/out/durability90/results-baseline.jsonl`, `results.jsonl`,
`baseline.log`, `fixed.log`.

## Why the caller's own sync could not be enough

`nfsproc3_commit` maps to `fsync(ino, false)`, so COMMIT was always strong enough.
The problem is that macOS does not send COMMIT for the two syncs a caller actually reaches for:

- `fsync` of a **directory** descriptor. There are no dirty pages to write back, so there is
  nothing to commit.
- `fsync` of a **descriptor with no dirty pages**, which is what `fsync` of a read-only fd to an
  already-written file is.

Both return success at the client and produce no RPC.
The real client's counters agree: a `write` + `fsync` of any file in the affected snapshot adds
`write` and `fsync` and a COMMIT, while `rename` and `fsync(parent_dir_fd)` add no COMMIT at all.
Those counters are machine-global across every NFS mount on the host, so they only attribute
cleanly when the host is otherwise idle; the crash result above is the per-mount evidence and does
not depend on them.

There is also no control-plane escape hatch: `cowfs_ctl::Request` has no `sync`, so a caller that
knows the problem cannot ask the daemon for a barrier over the socket.

## What changed

The reply is the last place the server can act, so the adapter acts there.

`Adapter::durable` calls `Vfs::sync_namespace` after every name and attribute change and before the
status goes out: `create`, `create_exclusive`, `mkdir`, `symlink`, `link`, `setattr`, `remove`,
`rmdir` and `rename`, once per RPC, after all of that RPC's own `Vfs` calls have succeeded.
A failure is reported as `NFS3ERR_IO`.
It is never reported as success, and nothing is flushed to paper over it: the applied rename stays
visible, so the caller's retry finds its own work rather than having to undo it, and
`Core::health` reports the failed sync rather than swallowing it.

`WRITE` is untouched.
A stable write still gets the `fsync` of that file that the client asked for, an unstable write
still gets none, and `READ`, `LOOKUP` and `READDIR` are still cheap.
The barrier is at the acknowledgement of a name, which is what keeps a `cargo build` on the mount
from turning into one fsync per write.

`Vfs::sync_namespace` is new, with the default `fsync(ino, false)`, which is at least as strong, so
an implementation that does not care pays nothing and cannot be wrong.
`cowfs-core` implements it as the namespace commit it already makes for `readdir` and `require_empty`,
plus the metadata sync.
`ROOT_INO` still means the whole mount, because a client that asks for the mount is asking for
everything in it; a handle reaches only its own snapshot.

### Why a namespace-only commit cannot name a block that is not there

A namespace batch contains a content operation only for a file whose bytes were already written:
`queue_content` runs after the flush that wrote them, and a file with unflushed bytes has no
content operation queued at all.
`Core::finish_sync` runs `meta.sync`, whose `before_sync` hook is the store's own sync, so the
store is flushed before the metadata that names it.
A durable name therefore never names a block the store has not written.
`a_namespace_barrier_leaves_unrelated_dirty_data_alone` in
`crates/cowfs-core/tests/ns_durability.rs` reads a dirty file back through a barrier and then
through a reopen, so this is checked rather than argued.

## What is still not promised

- **WRITE data is unstable until the client sends COMMIT.** That is NFSv3, and the repair does not
  change it. A caller that wants the bytes durable asks for a stable write or sends COMMIT.
- **A barrier is per snapshot.** `sync_namespace` on a handle makes that snapshot's namespace
  durable and no other, which is what the isolation test asserts from the queue.
- **The cost is one metadata sync and one store sync per namespace RPC.** That is what NFSv3 asks
  for, and every correct NFS server pays it, but it is a real cost for a workload of many small
  metadata operations on one snapshot. Not measured here: this is a correctness repair and the
  machine was busy with other agents' mounts.
- **A failed barrier leaves the namespace applied in memory but not durable.** The caller is told
  `NFS3ERR_IO` and the name is still visible, so a retry is safe. It is not rolled back, because the
  rollback would be a second mutation that could fail the same way.
- **`fsync(ROOT_INO, false)` is still the whole-mount barrier**, so a client that commits the mount
  root gets everything, data included.

## Tests

| test | what it pins |
|---|---|
| `cowfs-daemon` `namespace_durability.rs` | the crash: real mount, private store, `SIGKILL`, fresh daemon, both names and the bytes, 3 reps per case, plus the native control |
| `cowfs-nfs` `ns_durability.rs` | the barrier follows the mutation and names the source directory; every namespace RPC barriers exactly once; `WRITE` and `READ` do not; a failed barrier is `NFS3ERR_IO` and nothing else was flushed |
| `cowfs-core` `ns_durability.rs` | the name survives a reopen after only a namespace barrier; unrelated dirty data is neither flushed nor lost; a handle reaches one snapshot and the root reaches all; a failing sync reports and a retry commits one name |

Run:

```text
cargo test -p cowfs-core --test ns_durability
cargo test -p cowfs-nfs --test ns_durability
cargo test -p cowfs-daemon --test namespace_durability -- --ignored --test-threads=1 --nocapture
```