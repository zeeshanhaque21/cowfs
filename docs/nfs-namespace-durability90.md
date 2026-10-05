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

Tracked evidence, readable on a fresh checkout:
`docs/verification/evidence/namespace90/README.md`, with the per-rep receipts beside it.
The raw logs stay gitignored under `bench/out/` and are named there as paths, not quoted as
though they were public.

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

### An error reply does not get to skip the barrier

The barrier is owed as soon as the change exists, not when the reply is a success.
That includes an RPC that ends in an error after it has already changed something.
`rmdir` in Hide mode is the case that needs saying out loud: it calls `purge_sidecars`, which
unlinks real `._name` sidecars, and then retries the `rmdir`.
If the purge or the retry fails, names have already been removed, so the barrier is still owed even
though the reply is an error.
The arm therefore yields a value that goes through `durable_or` rather than propagating with `?`,
which is the same shape the `create` arms had and the reason the earlier version of them was wrong.
This is not a measured production loss: no crash measurement reproduced a name lost through that
path.
It is a code shape that owes a barrier, fixed on that basis.
`Vfs::create` has made the name before the attribute step runs, so an attribute step that fails
must not return early and leave that name queued and unacknowledged: the caller could not tell
whether the name was there, and if the daemon then died it was not.
Both `create` arms therefore yield a value rather than propagating with `?`, and
`Adapter::durable_or` decides the status when the change's own step and the barrier both fail.

The barrier's `NFS3ERR_IO` wins that case, and the reason is what the status would otherwise mean.
The attribute error reads as "that did not happen", which is a lie about a name that does exist, so
the caller would have no way to learn what state the namespace is in.
The applied change is still not rolled back: a rollback is a second mutation that can fail the same
way, and the reply says plainly that it did not.
A refused mutation is a different question, and the honest answer is that it still pays a barrier.
An earlier revision of this document claimed a refusal owes none, because nothing changed.
That was wrong, in the code and in the prose.
`durable_or` evaluates the barrier as the first element of the tuple it matches, so a `setattr`
that ends in an error still runs the full barrier, and an earlier revision of this file said it did
not.
The barrier is unconditional on purpose.
It is not "nothing to do because this RPC changed nothing": it commits whatever the snapshot
already had queued, so a refusal arriving on top of earlier uncommitted writes still discharges
them, and a short-circuit would leave those queued writes behind, which is the bug this change
exists to fix.
The price is one metadata sync on a refusal, observed around 4.6 ms on the host these numbers came
from.
`create` and `create_exclusive` do return before `durable_or` on a refusal, because they refuse
before anything is mutated, so for those two the old claim happened to hold.

`a_refused_setattr_still_issues_a_namespace_barrier` in
`crates/cowfs-nfs/tests/ns_durability.rs` asserts the issuance and the caller's status, and it is
named for exactly that.
It does not assert a discharge, because the fake `Vfs` it wraps is `MemVfs` and has no queue to
inspect; an earlier name for that test promised a discharge the assertion could not observe.
The discharge claim rests on the structure instead: `Vfs::sync_namespace` reaches
`Inner::sync_ns_snapshot`, which calls `barrier`, which calls `flush_namespace_locked`, which drains
that snapshot's queue.

The two statuses in that precedence are distinct on the wire, which is what makes it checkable.
The attribute fault is `PermissionDenied`, which `nfsstat` maps to `NFS3ERR_ACCES`, and the barrier
fault is `Io`, which maps to `NFS3ERR_IO`.
With a healthy barrier the caller still receives `NFS3ERR_ACCES` and can tell an attribute problem
from a durability one; with both failing it receives `NFS3ERR_IO`.

`a_created_name_is_barriered_even_when_the_attribute_step_fails` in
`crates/cowfs-nfs/tests/ns_durability.rs` records the names the fake `Vfs` was told to create, so it
can tell an error reply from a name that is not there, and counts barriers across all three routes.
It was checked against the old shape, which fails it.

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

## Interaction with the issue 94 elide

A barrier now commits the queue at every namespace acknowledgement, so the queue commits far more
often than it used to.
That changes the window in which an unlink can cancel a queued create, because the elide only
applies to a create the store has not seen yet.
PR #95 fixed that elide and did not merge this branch; this branch did not see that fix, and
neither diff settles the interaction by reading.

It is settled by running both shapes on the integrated tree in
`crates/cowfs-core/tests/ns_durability_elide.rs`:

- The #94 sequence with no barrier between the steps, where the elide fires, followed by a barrier,
  a create and a reopen.
  The name has to be free afterwards, and not reserved by the elided create.
- The same sequence with a barrier after every step, which is what a mount now does.
  The elide cannot fire, because the create has already been committed, and that is the correct
  outcome rather than a regression: the elide only cancels a create the store never saw.
  What matters is that the names and the bytes are right in both snapshots after a cache drop and a
  reopen, and they are.
- An elided `rmdir` under the same pattern, with the barrier after each step.

The elide regression seed `9aa30bfa` from `model.proptest-regressions` is untouched and still
re-run by `cargo test -p cowfs-core --test model`, which passes.

## What is still not promised

- **WRITE data is unstable until the client sends COMMIT.** That is NFSv3, and the repair does not
  change it. A caller that wants the bytes durable asks for a stable write or sends COMMIT.
- **A barrier is per snapshot.** `sync_namespace` on a handle makes that snapshot's namespace
  durable and no other, which is what the isolation test asserts from the queue.
- **The cost is one metadata sync and one store sync per mutating namespace RPC.** Measured by
  `crates/cowfs-core/tests/ns_durability_cost.rs`, 200 reps, debug build, on a host that had other
  agents' NFS mounts busy, so treat these as an upper bound and not as a benchmark:

  | | ms |
  |---|---|
  | rename queued into memory, what it cost before | 0.005 |
  | rename plus the barrier, after | 10.99 |
  | rename plus a COMMIT of a file, what a client that emitted one already paid | 11.97 |
  | a barrier with nothing pending, the floor | 0.007 |
  | one 4 MiB write, for scale | 254.7 |

  This is one observed distribution from one busy host in a debug build.
  It is **not** an upper bound and not a benchmark: a busy host neither establishes nor refutes a
  limit, and 200 reps of one operation is a sample.
  What it does support is the direction, the barrier is not dearer than the protocol's own price
  for the same guarantee and is nearly free when there is nothing to commit.
  An independent critic measured the same shape on a different host at 0.005 / 4.63 / 4.64 / 0.006
  / 108.4 ms, same order of magnitude difference between hosts.
  What has **not** been re-measured is the mount-level build overhead of `docs/design.md` success
  criterion 2, because a `cargo build` on the mount was not run here.
  That is the number a critic should ask for before this merges.
- **A failed barrier leaves the namespace applied in memory but not durable.** The caller is told
  `NFS3ERR_IO` and the name is still visible, so a retry is safe. It is not rolled back, because the
  rollback would be a second mutation that could fail the same way.
- **`fsync(ROOT_INO, false)` is still the whole-mount barrier**, so a client that commits the mount
  root gets everything, data included.

## Tests

| test | what it pins |
|---|---|
| `cowfs-daemon` `namespace_durability.rs` | the crash: real mount, private store, `SIGKILL`, fresh daemon, both names and the bytes, 3 reps per case, plus the native control |
| `cowfs-core` `ns_durability_cost.rs` | labels the price of a barrier against the price of a COMMIT and against a barrier with nothing pending |
| `cowfs-daemon` `namespace_durability_gate.rs` | the CI gate: one non-ignored macOS rep of the two variants the issue measured as lost, with no skip path |
| `cowfs-core` `ns_durability_elide.rs` | the issue 94 elide on the integrated tree, with and without a barrier between the steps |
| `cowfs-nfs` `ns_durability.rs` | the barrier follows the mutation and names the source directory; every namespace RPC barriers exactly once; `WRITE` and `READ` do not; a failed barrier is `NFS3ERR_IO` and nothing else was flushed |
| `cowfs-core` `ns_durability.rs` | the name survives a reopen after only a namespace barrier; unrelated dirty data is neither flushed nor lost; a handle reaches one snapshot and the root reaches all; a failing sync reports and a retry commits one name |

Run:

```text
cargo test -p cowfs-daemon --test namespace_durability_gate
cargo test -p cowfs-core --test ns_durability
cargo test -p cowfs-core --test ns_durability_elide
cargo test -p cowfs-nfs --test ns_durability
cargo test -p cowfs-daemon --test namespace_durability -- --ignored --test-threads=1 --nocapture
```

The first four are not ignored and run in `cargo test --workspace`.
The last is the wide matrix and stays manual: 12 reps, each one a daemon start, mount, kill and
mount again.

## CI

The gate is `crates/cowfs-daemon/tests/namespace_durability_gate.rs`, macOS only and not ignored,
so `cargo test --workspace` runs it on the macOS runner.
It has no skip path.
A host with no NFS client fails naming `mount_nfs` rather than reporting a green run that never
mounted, because a gate that reports success without having mounted is the failure this issue was
about.
It is bounded to one rep of the two variants the issue measured as lost, so it does not turn every
`cargo test` into twelve daemon lifetimes.
It is mutation-checked: with `rename`'s barrier removed it fails on "the new name must survive the
kill".