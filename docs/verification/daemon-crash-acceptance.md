# Verification: full-stack daemon crash recovery with durable receipts

Issue: [#88](https://github.com/zeeshanhaque21/cowfs/issues/88).
Harness: `scripts/verify-daemon-crash.py`.
Evidence: `bench/out/crash88/accept-88-final/records.jsonl`, 856 records, 0 failing.

## Result

25 of 25 executions passed: 3 sample cases and 12 matrix configurations at 2 reps each,
plus 2 native controls.
Every one of the 48 durability receipts taken before a `SIGKILL` was byte-identical after
the same private store was reopened by a fresh daemon and a fresh NFS mount.
`fsck` reported 0 problems on all 30 reopened stores.
No tree was torn, no control call failed, and no required snapshot name went missing.

This is scoped evidence over the windows listed below, not a proof that every crash window
is safe.

## What ran against

| | |
|---|---|
| tree | `ceb96c67033cbf97f79267d6af7db3fa204d77d1` (`verify/full-stack-crash-88`, base `ceb96c6`) |
| `cowfs-daemon` | sha256 `4f29fab15f09ac2754c8ee0d4b7b9b0c515b620fbf10b89c6c010c99844085d8` |
| `cowfs` | sha256 `9567bded815291568a523c6d7a5772e9fd997d797da19cd9d628bee56ca0c5b5` |
| transport | real in-process NFSv3 loopback, `cowfs-daemon --backend core` |
| per-case wall clock | 0.2 s to 4.2 s, 60 s total for all 32 case executions |

Binaries are built from the committed tree with
`cargo build -p cowfs-cli -p cowfs-daemon`; the harness records the revision and both
binary digests in its first `source.identity` record so a run is attributable.

## The operation model, read from the source

The harness does not assume which acknowledgements are durable. It reads them off the code
and tests each one.

**An NFS `WRITE` ack is not durable.**
`cowfs_core::io::op_write` writes into the node's in-memory state
(`f.write(off, data)` at `crates/cowfs-core/src/io.rs:100`) and returns.
Nothing is fsynced.

**A control-plane mutating call acks `Applied`, not `Durable`.**
`cowfs_meta::db::Ack` defaults to `Applied` (`crates/cowfs-meta/src/db.rs:113`) and nothing
in the crate tree ever sets `Ack::Durable`: the only assignment is that default. So
`cowfs snapshot create` returning 0 means applied and visible, not durable.
There is also no `sync` request in the control API (`crates/cowfs-ctl/src/types.rs:432`),
so the control plane exposes no durability barrier of its own.

**`os.fsync(fd)` on a file in the mount is a durability receipt.**
The NFS adapter maps `COMMIT` to `fsync(ino, false)` for every handle including the root, and
never to a data-only sync (`crates/cowfs-nfs/src/lib.rs:40`). That reaches:

1. `op_fsync` (`crates/cowfs-core/src/io.rs:277`), which for a file runs `fsync_snapshot`;
2. `flush_snapshot` (`inner.rs:761`), which flushes data first (`flush_data`, so blocks reach
   the pack) and only then commits the namespace (`commit_batch`, `inner.rs:790`);
3. `meta.sync()` (`db.rs:679`), whose `before_sync` hook is `cowfs_core::store_sync_hook`
   (`lib.rs:110`), installed in the one production path that opens a `Core` (`lib.rs:207`);
4. `Store::sync()` (`store.rs:1295`), which `fsync`s the pack file
   (`fsio.rs:160`) and only then advances the watermark (`wm.rs:153`, documented as
   "the caller must already have fsynced the data it describes").

Data durable before namespace durable, and the watermark after the data.
So the promise after `fsync` returns is strong: bytes, the durable watermark, and the
namespace all survive losing the process.

`cowfs shutdown` is the other receipt, via `Core::close`.

Two levels follow, and the harness enforces them differently:

- **Level B, durable.** An `fsync` returned. Bytes and promised names must survive. A miss is
  a hard failure.
- **Level A, applied.** A write or control call returned with no `fsync`. Loss is permitted,
  because `docs/design.md` accepts "bounded loss of recent writes on a crash". The outcome is
  recorded, never failed.

A receipt is only ever level B if the harness issued a sync that measurement showed actually
reaches the server. Two of the three rename variants had to be demoted for exactly this
reason, and the reason is stored in the receipt rather than hidden.

## What each boundary did

| boundary | level | outcome after SIGKILL and fresh reopen |
|---|---|---|
| `write` + `fsync` | B | all files matched, every rep |
| `write`, no `fsync`, kill immediately | A | all 8 applied files survived across 2 reps |
| `write` + `fsync`, plus a sibling `write`+`fsync` | B | both matched |
| `rename` + parent-directory `fsync` | A | **lost in 2 of 2 reps**, reverted to the old name |
| `rename` + `fsync` of a read-only fd | A | **lost in 2 of 2 reps**, reverted to the old name |
| `rename` + a sibling `write`+`fsync` | B | survived in 2 of 2 reps |
| `mmap` write, `msync`, `fsync` | B | matched |
| `snapshot create` fork + `fsync` inside the fork | B | both trees listed, all files matched |
| `snapshot rm` of a fork | A | base's durable bytes intact |
| `gc` cycle then kill | B | survivor matched before and after gc, fsck clean |
| idle daemon, no writes | n/a | reopened clean, fsck 0 problems |

## Findings

### 1. On this transport, a POSIX parent-directory `fsync` after a rename is not a durability receipt

Measured, 2 reps each, kill 2 to 6 ms after the rename:

- `os.fsync(dirfd)` after `os.rename`: the rename was **gone** after the fresh reopen. The
  directory held `orig.bin`, exactly the pre-rename name.
- `os.fsync(fd)` on a read-only descriptor to the renamed file: the rename was **gone**.
- `write`+`fsync` of any file in the same snapshot: the rename **survived**.

This is not a cowfs defect. A file rename is queued as `Op::Rename`
(`crates/cowfs-core/src/ns.rs:557`), so any `fsync` that actually reaches the server drains
the queue and commits it. The evidence is that the third variant survives while the first two
do not. The reading is that this macOS NFS client does not emit a `COMMIT` for a directory
`fsync`, nor for an `fsync` of a descriptor with no dirty pages, so neither call crosses the
wire.

Consequence, and it is a documentation defect rather than a code one: the crate states that
"the name a file was created under is durable when COMMIT returns"
(`crates/cowfs-nfs/src/lib.rs:42`), which is true, but nothing in the tree warns that on this
client a caller cannot produce that `COMMIT` for a bare rename. A caller who renames and then
only `fsync`s the directory, following the POSIX habit, gets no durability at all. Worth an
issue against the transport documentation.

### 2. `SIGKILL` samples process crash, not power loss

This is the limit of the evidence and it is not small.
`SIGKILL` kills a process, not the kernel, so any byte the daemon already `write(2)`ed into a
pack is in the host page cache and outlives the process.
Consequently:

- level A bytes that reached the store survive a `SIGKILL`, which is why the immediate
  kill-race case still showed 4 of 4 surviving;
- level A bytes still only in daemon memory would be lost, and the harness's crash windows
  of 0 to 6 ms sit inside the 500 ms background-flush window
  (`cowfs_core::Options::flush_interval`), so they are the window where this is reachable;
- **power-loss loss of un-fsynced pack bytes cannot be exercised by `SIGKILL` at all.**
  That window is untested here and needs a different method: a fault-injecting block device,
  a VM with a hard reset, or the store's own sync-fault seams.

### 3. The gc case exercises the mark path but frees nothing at this scale

The dead set was made real, not shared: 12,582,912 bytes written into a snapshot of their own,
that snapshot removed, then one `gc` cycle.
Measured report: `candidate_blocks` 155 and 154 across the two reps, `freed_bytes` 0,
`freed_blocks` 0.

Zero freed is the expected result, not a bug: a small fixture lives in the open pack, which
the collector cannot unlink.
So this case establishes that a collect cycle plus a crash does not damage acknowledged
survivors.
It does not establish reclamation.
Real reclamation with sealed packs is covered by `docs/verification/gc-daemon-e2e.md`, which
seeds them; that report is not re-run here.

## Windows sampled, and windows not sampled

Sampled, all with the kill 0 to 6 ms after the last receipt, except where noted:

- after a completed `fsync` of written data (level B);
- after an un-fsynced write, immediately (level A, inside the 500 ms flush window);
- after a queued rename, with and without a real `COMMIT` (level A and level B);
- after an `mmap` write with `msync` and `fsync` (level B);
- after a fork whose name was committed by a later `COMMIT` (level B);
- after a `snapshot rm` (level A);
- after a completed `gc` cycle, 0.49 s and 0.69 s after the last receipt (level B);
- on an idle daemon (control).

Of the level A receipts, 26 survived and 6 were lost: the 4 rename receipts of finding 1,
and the 2 garbage sets the gc case removed on purpose, whose absence is the point of the case.

Not sampled, and each needs a seam this lane is not allowed to add:

- **mid-`gc` crash.** The kill lands after the `gc` ack. No public boundary exposes a point
  inside a collect cycle, so the barrier window and the mark/sweep walk are untested under a
  kill. The library-level seams (`cowfs_gc::Gc::set_between_lookup_and_walk`,
  `cowfs_core::fsops::set_fault`) exist for this and were deliberately not driven from here.
- **power loss.** See finding 2.
- **a crash between the pack `fsync` and the watermark advance**, or between the watermark
  advance and the metadata commit. Both are internal orderings with no public edge.
- **more than two concurrent writers**, and any writer that is not this harness.
- **`cowfs shutdown` as the receipt.** Implemented and exercised at teardown, but not used as
  the crash boundary, because a clean shutdown is not a crash.

## Native (APFS) controls

Two controls, both driven by a real child process, receipts flushed and `fsync`ed beside the
data before the child dies:

| control | result |
|---|---|
| writer writes one fsynced file and one un-fsynced file, then `SIGKILL`s itself | child exited `-9`; the fsynced file matched its source hash; the un-fsynced file survived |
| writer writes, fsyncs, exits 0 (clean-restart control) | child exited `0`; both files matched |

The difference is stated rather than smoothed over: the cowfs daemon has no APFS analogue, so
the process killed here is the *writer*, not a server.
On APFS a returned `write()` is already in the host page cache, so an un-fsynced file
survives.
On cowfs an un-fsynced write can be lost while it is still only in daemon memory.
That is exactly the asymmetry the level A / level B split exists to describe.

## Isolation

Every signal went to a pid this harness spawned, and only after its command line was
confirmed to carry this run's own `--store` and `--socket`
(`kill.verified_target`, 60 occurrences).
The harness refuses to signal anything else: `selftest.kill_refuses_foreign_pid` proves it by
offering it a live `sleep` pid and requiring a refusal.
Unmounting is equally narrow: `unmount.refused_not_our_mount` is recorded whenever the mount
table does not list that exact path.

Verified after the run:

- the shared daemon on pid 15263 has the same pid and the same start time
  (`Sat Oct 3 20:44:29 2026`) as before it, and is the only `cowfs-daemon` running;
- the only `cowfs` mount in the mount table is the shared `~/.cowfs/mnt`; none leaked;
- no `cowfs-crash88-*` socket directory remains.

Fixtures live under `bench/out/crash88/` (gitignored, 39 MB for the whole tree, 24 MB of which
is the two 12 MiB gc garbage sets).
The control socket is the one thing outside the run directory, because a socket path must fit
`sun_path`; each run makes its own `0700` directory under `/private/tmp` and removes it at
teardown, reporting rather than deleting anything it did not create.
No shared store was collected or scanned, no lease returned, no runner touched, no workflow
dispatched.

## Budgets

Declared in the harness before the matrix runs, and exceeding one raises rather than grows:
64 KiB per operation, 16 operations per case, 2 concurrent daemons, 180 s per case, 90 s
without progress.
Only the gc case writes more than 64 KiB, and only enough to pass the collector's own
8 MiB floor.
The store is never grown to the default 256 MiB pack size.

## Fail-closed self-tests

Run before any case; a failure stops the run.

| check | expectation |
|---|---|
| `selftest.ghost_socket_is_not_success` | a socket no daemon serves gives exit 3 and `{"error":{"code":"not_running"}}` |
| `selftest.kill_refuses_foreign_pid` | a pid that is not the fixture is refused, not signalled |
| `selftest.plain_dir_is_not_a_mount` | a plain directory is not treated as a mount |

## Reproducing

```sh
cargo build -p cowfs-cli -p cowfs-daemon
scripts/verify-daemon-crash.py --stage sample            # the small validated sample first
scripts/verify-daemon-crash.py --stage all --reps 2      # what this report describes
scripts/verify-daemon-crash.py --only rename_writefsync  # one boundary
```

`records.jsonl` is appended, flushed and `fsync`ed per record, so a killed run leaves usable
evidence and a rerun skips the cases already marked terminal.

## Two harness bugs found and fixed during this work

Recorded because both produced convincing false results before they were caught.

1. `receipts.durable_items()[-1]` was used to re-file a receipt after a rename. It silently
   re-filed the *last* receipt, not the renamed one, so a case that had actually passed looked
   like a failure with a hash mismatch. Now matched by path
   (`Receipts.repath`), never by position.
2. The sample phase and the matrix phase reused the same store path for the same case, so
   `snapshot create live` failed on the matrix run against the sample run's residue. The store
   path now carries the phase.

The first one is the reason the rename boundary was probed three ways instead of asserted once.