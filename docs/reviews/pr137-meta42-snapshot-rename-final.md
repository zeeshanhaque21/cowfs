# PR #137 review: BLOCK, the rename overwrites the flushed root with the stale one and frees the old tree in the same transaction

Reviewer lane: `.treehouse-build-train/.treehouse/cowfs-7c1bf8/6/cowfs`, held slot 6.
Lease verified before any write: branch `review/gc-root-mark-retention-82`, HEAD `b4b55ab`, working tree showing only the six pre-existing untracked `docs/reviews/*.md` files from other lanes, no process of mine running.
Matched the expected state, so the lane was used.

Head reviewed: `924c16bd14c17b0716b19cdf4a0e02e913144a39`.
Base reviewed: `93cfef94457a989d031cb6b0a475ac4edbdb85ef`, which is current `origin/main` and the merge base.
Canonical receipt under review: `docs/verification/evidence/meta42-snapshot-rename.md`, sha256 `93c4b1317b7d9445ced3f7d1316e159b3cca472cbee6172c0a81e2b9a8883c0b`, identical in the MAIN primary checkout and in the blob inside the head.

Review date: 2026-10-05.
This document: canonical PRIMARY copy, `docs/reviews/pr137-meta42-snapshot-rename-final.md`.

## Verdict

BLOCK.

The new API is shaped correctly and every other invariant in the brief holds in the source.
One invariant does not: a rename of a snapshot that has unflushed writes writes the flushed root and then overwrites it with the pre-flush root, in the same transaction that frees the old tree.
The durable row is left pointing at a node that transaction deleted.
Under `cowfs_meta::Options::default()`, which is exactly what `cowfs-core` uses, that is reachable from an ordinary create-then-write-then-rename.

Two more findings, both independent of my own execution.

The test that is named for this exact property does not exercise it, because its own options force the write to be committed before the rename.
And the branch is red on CI at both Linux and macOS, at clippy, before any test ran, so the receipt's test results are not supported by this head.

Runtime proof is blocked by the artifact budget and I did not run it.
The counterexample below is source-level with a complete line chain.
The one piece of executed evidence in this report is the author's own CI run, which I read rather than reproduced.

## Budget gate: blocked, and not waived

Measured before any archive and before any compile, on the lease, in `bench/out` as a whole:

| Quantity | Measured |
| --- | --- |
| `bench/out` physical bytes, `st_blocks * 512` over the whole tree | 38,529,695,744 |
| `bench/out` physical size | **35.884 GiB** |
| cap | 8 GiB |
| free on `/` | 295.8 GiB |
| disk floor | 20 GiB |

35.884 GiB is already 4.5 times the cap before I added anything.
The instruction is to stop heavy work when `bench/out` is already at or above 8 GiB, and to do a read-only source audit with no cargo invocation and no cache deletion.
That is what this review is.
I created no archive, no `CARGO_TARGET_DIR` and no fixture.
I deleted nothing: no prior log, report, source, fixture or any worker's cache.

Operational finding, not a code failure, and I am not claiming the gate passed.
The author reported `bench/out` at 8.2 GiB before a heavy run, added a further 903 MiB, and did not stop.
The tree is now at 35.884 GiB, so other lanes have added roughly 27 GiB since.
Whatever holds this directory is not enforcing the cap, and the cap as written is not per-lane and not per-folder, so "cap the new folder" is not available as a reading.
Free disk of 295.8 GiB is not a reason to proceed; the cap is on `bench/out`, not on the filesystem.

Consequence for this review: I claim no test result of my own.
Every count in the receipt is attributed to the author and is contradicted by CI, not confirmed by me.

## Finding 1, blocking: the rename writes the stale root over the flushed one and frees the old tree

`crates/cowfs-meta/src/db.rs` at `924c16b`. All line numbers are that file at that commit.

The commit function first flushes every dirty tree, then applies the extra.

Flush loop, `db.rs:531` to `546`:

```rust
for (id, e) in &s.snaps {
    if Some(*id) == removed || !e.tree.is_dirty() {
        continue;
    }
    let root = e.tree.write(&mut w)?;
    if root != e.info.root {
        w.add_ref(root);
        w.drop_ref(e.info.root);
        let info = SnapshotInfo { root, ..e.info.clone() };
        snaps.insert(id.0, encode_snap(&info).as_slice())?;   // writes the NEW root
    }
    new_roots.push((*id, root));
}
```

Then the rename arm, `db.rs:600` to `610`:

```rust
Extra::Rename { id, name } => {
    let e = s.snaps.get(id).ok_or(Error::NoSuchSnapshot)?;   // s is the SESSION, not the table
    let mut info = e.info.clone();                          // e.info.root is still the OLD root
    info.name = (*name).to_string();
    snaps.insert(id.0, encode_snap(&info).as_slice())?;     // overwrites the row with the OLD root
    names.remove(e.info.name.as_str())?;
    names.insert(*name, id.0)?;
}
```

`s` at line 604 is the `Session`, and the session's copy of `info.root` is only brought forward at `db.rs:630` to `635`, after the write transaction has already committed at `db.rs:620`.
So when the rename target's tree was dirty in this same commit, line 607 writes the same `SNAPSHOTS` key a second time with the pre-flush root.
`insert` overwrites, so the durable row ends on the old root.

The reference counts then move against that root. `w.settle()` at `db.rs:612`, with `NodeWriter::settle` at `ptree.rs:765` to `818`:
`delta[R_old] = -1` from line 538, `cur = refs[R_old] = 1`, so `new = 0`, which is not below zero and not above zero, so lines 788 to 803 run: the node's bytes are removed, its `REFS` entry is removed, and its children are decremented recursively.
The entire old tree is deleted in this transaction.
`delta[R_new] = +1` leaves the new tree holding a count that nothing points at.

End state, durable: the row for that `SnapshotId` names the new name and the old root, and the old root no longer exists.
End state, in the live session: `db.rs:630` to `635` set `info.root = R_new` and reset the tree, then `db.rs:660` to `671` set the new name, so the open handle reports `R_new`.
The session and the table disagree permanently.

### Why it is reachable with the default options

`is_dirty` is `matches!(self.root, Kid::Mem(_))` at `ptree.rs:434`, so a tree is dirty as soon as any write lands and stays dirty until a commit resets it.
The auto-commit that would clear it is gated at `db.rs:813`:

```rust
if self.opts.ack == Ack::Applied {
    let due = s.pending_ops >= self.opts.sync_every_ops || ... ;
    if due { self.commit(s, Extra::None, false, true) }
}
```

`Ack::Applied` is the default, at `db.rs:70`, and `cowfs_meta::Options::default()` at `db.rs:112` to `123` sets `sync_every_ops: 256`, `sync_interval: 1s`, `max_pending_bytes: 32 MiB`.
`crates/cowfs-core/src/inner.rs:58` builds meta's options as `cowfs_meta::Options::default()`, so the real consumer has exactly this configuration.

Therefore one create, one batch or one `create` call, then a rename, leaves `pending_ops = 1 < 256`, the dirty-tree window open, and reaches the trace above.
With `background: true` the window is bounded by the one-second timer; with `background: false`, which `cowfs-core/src/swap.rs:292` uses, it is bounded only by the op count.
`Ack::Durable` is not a vector, because `db.rs:847` to `851` waits for durability before the write returns, so that path flushes the tree itself.

### What it costs

A read of the renamed snapshot's root after a reopen goes through `MemTree::new(info.root, node_max)` at `db.rs:1391` and then `get_stored` at `ptree.rs:228`, which calls `src.node(&id)?`.
A missing node is `Error::Corrupt("missing tree node {id}")` at `ptree.rs:46` and `ptree.rs:220`.
`Inner::note` at `db.rs:365` to `369` sets `poisoned` on `Corrupt`, and `check_writable` at `db.rs:415` to `419` then refuses every further write on that handle with "handle refuses writes after detecting corruption; reopen and run check()".
So one rename of a snapshot with pending writes makes the snapshot unreadable and the handle write-poisoned, and the writes the user made are orphaned in a tree no row references.
`check()` would also report it, because `check.rs:101` to `122` walks every snapshot root and compares the reachable graph against `refs`, and `check.rs:137` then checks the root itself.

### The asymmetry that shows this is an oversight

`Extra::Add` gets this right. At `db.rs:551` to `553` it resolves the root through `new_roots`, the roots the flush loop just wrote, and only falls back to the session copy:

```rust
Some(src) => new_roots.iter().find(|(i, _)| i == src).map(|(_, r)| *r)
                 .or_else(|| s.snaps.get(src).map(|e| e.info.root))
```

The author knew the session copy of `info.root` is stale for this purpose in `Add` and did not carry it to `Rename`.
`check.rs:524` to `530` uses the same correct pattern, computing the root fresh from the tree and building the row from it.

### Smallest fix, for the author to judge

Resolve the row's root the way `Add` does: take `R_new` from `new_roots` for `id` when the flush loop produced one, and fall back to `e.info.root` otherwise.
Or reject the rename with an error when the target's tree is dirty, which is safe but loses the "a rename is a commit like any other" property the receipt advertises.
Either is a two-line change at `db.rs:604` to `607`.
I am not applying it.

## Finding 2, blocking: the test named for Finding 1 never runs against a dirty snapshot

`crates/cowfs-meta/tests/snapshot_rename.rs:310`, `a_rename_of_a_dirty_snapshot_keeps_its_uncommitted_writes`, and its helper `opts()` at line 28:

```rust
Options { node_size: 512, sync_every_ops: 1, background: false, ..Options::default() }
```

`ack` is not overridden, so it is `Ack::Applied`, and `sync_every_ops` is `1`.
At line 319 the test does `s.create(ROOT_INO, b"dirty", 0o644)`.
That is one applied op, so `s.pending_ops` becomes `1`, and `1 >= 1` satisfies `due` at `db.rs:814`, so the inline commit at `db.rs:825` runs before `rename_snapshot` is ever called at line 322.
That commit flushes the tree and `db.rs:633` calls `e.tree.reset(root)`, so the tree is clean when the rename runs, the flush loop skips it at `db.rs:532`, and the rename arm at `db.rs:604` reads a session copy whose root already equals the durable row.

The test therefore passes on a clean tree, and the property it is named for is never exercised.
Its comment at lines 316 and 317 states the opposite, and the receipt repeats it: line 45 claims "the same transaction still writes the roots of any dirty snapshot", and line 99 lists this test as covering it.

Fixing `opts()` alone is not enough, because `sync_every_ops: 1` is what every other test in the file relies on to make its setup deterministic.
The test needs its own options with `sync_every_ops` high enough that the write stays pending, or `background: false` with a large `sync_every_ops`, so the rename really is the next durable commit.
Under the author's current options the same fix that repairs Finding 1 would still pass this test, which is why the coverage gap and the defect have to be closed together.

## Finding 3, blocking: the branch does not build under the repo's own CI gate

The one CI snapshot I took, on the exact head `924c16b`:

| Check | Status | Conclusion | Started | Completed |
| --- | --- | --- | --- | --- |
| `check (ubuntu-latest)` | COMPLETED | **FAILURE** | 2026-10-05T21:12:51Z | 2026-10-05T21:13:33Z |
| `check (macos-latest)` | COMPLETED | **FAILURE** | 2026-10-05T21:17:48Z | 2026-10-05T21:18:29Z |
| `linux-fuse` | COMPLETED | SUCCESS | 2026-10-05T21:12:27Z | 2026-10-05T21:16:10Z |

Run `37374411493`, job `111979239211` on Ubuntu and `111979238955` on macOS.
Both fail at the same step, `cargo clippy --workspace --all-targets -- -D warnings`, before any test target runs:

```
error: unused variable: `inos`
   --> crates/cowfs-meta/tests/snapshot_rename.rs:215:20
    |
215 |     let (id, root, inos) = populated(&m);
    |                    ^^^^ help: if this is intentional, prefix it with an underscore: `_inos`
    = note: `-D unused-variables` implied by `-D warnings`
error: could not compile `cowfs-meta` (test "snapshot_rename") due to 1 previous error
```

There is not a single `test result:` line in either log, because `cargo test` never started.
So on this head no test in this PR has ever run on CI.

The receipt claims "fmt 0, clippy 0" and eight new tests passing twice, plus lib 16, health 7, recovery 40 four times, critic 12, posix 16, model 2, crash 2 with one ignored.
None of those is supported by the branch as committed.
They may well be true of the author's local tree; they are not true of `924c16b`, and `fmt` and `clippy` are demonstrably not zero.
I am not claiming the tests are wrong, only that they have not been shown to run.

## What holds, checked rather than assumed

These are the parts of the brief that do hold at this head, and the fix for Finding 1 does not need to touch any of them.

- **API shape.** `Meta::rename_snapshot(&self, id: SnapshotId, new_name: &str) -> Result<()>` at `db.rs:1548`, delegating to `Inner::rename_snapshot` at `db.rs:940`.
- **No schema, no dependency, no consumer change.** The delta is exactly three paths: `db.rs` at +86, the new test file, and the receipt. No `Cargo.toml` and no `Cargo.lock` line moves; `git diff` restricted to those paths is empty. `db.rs` blob moves from `b08ed7f7e204a60625124b13415f471ec82800b8` to `22a6015ed311e1eedff08e3475e89c6790a21a1b`. No table, column or constant is added.
- **One transaction, so no half names.** `db.rs:517` opens a single write transaction and `db.rs:620` is the only commit. All three writes for a rename, at `607`, `608` and `609`, are inside it, so a failure anywhere aborts all of them together.
- **Existing destination refused, never replaced.** `db.rs:497` to `500` returns `Error::SnapshotExists` when `s.names.get(name)` maps to a different id. The other snapshot's id, root and row are untouched, and the arm never runs.
- **Missing id refused with no mutation.** `db.rs:492` to `494` returns `Error::NoSuchSnapshot` before `run_hook` and before the transaction opens.
- **Same-name semantics are explicit and cheap.** `db.rs:505` to `506` returns `Ok(None)`, which is before `self.run_hook()` at `511` and before `begin_write` at `517`, so a rename to the current name writes nothing and does not even fire the durability hook.
- **Name validation matches create exactly.** `db.rs:941` to `943` is `name.is_empty() || name.len() > usize::from(u16::MAX)`, character for character `add_snapshot` at `db.rs:918` to `920`. A rename cannot mint a name `new_snapshot` would have refused. Meta does not apply the Core `cowfs-snapname` rule, and the receipt is explicit that unifying it is request 2, not this request. An explicit lower-level policy is fine and I make no bypass claim.
- **Writeability and hook checks are consistent with `Add`.** `rename_snapshot` calls the same `commit` with the same `check_writable` at `db.rs:458` and the same `run_hook` at `db.rs:511`.
- **`before_sync` is pre-publication only, and the receipt says so.** `run_hook` at `db.rs:511` precedes `begin_write` at `db.rs:517`, so a hook failure means no transaction is opened at all. The receipt's line 156 correctly declines any universal rollback claim. That honesty is right and I did not test it, because I could not run anything.
- **The session mirror is updated only after the commit.** `db.rs:660` to `671` runs after `wtx.commit()?` at `620`, so no reader ever sees a half-applied rename through the in-memory path. `Meta::snapshots()` at `db.rs:1498` reads the session.
- **A retained handle keeps working.** The `SnapshotId` does not change, so `Snapshot` handles taken before the rename stay valid and report the new name, which is what `db.rs:664` to `670` arranges.
- **Reopen holds for a clean tree.** Both tables are written in one transaction, so `Meta::open` rebuilds a consistent `snaps` and `names` pair. This is the invariant Finding 1 breaks for a dirty tree.
- **Reservation high-water mark, reap queue and `next_snapshot` are untouched by a rename.** The arm at `db.rs:600` to `610` writes only `SNAPSHOTS` and `SNAP_NAMES`. `meta.insert("ino_reserved", ...)` at `db.rs:618` is the existing unconditional write, and `REAP` and `next_reap` are only touched by `Extra::Remove` at `db.rs:596` to `598`.
- **Id aliasing cannot occur.** A rename reuses the same `id`; only `Extra::Add` allocates from `s.next_snapshot` at `db.rs:579`, and the arm does not touch `next_snapshot`, so a rename can never mint an id that a later create reuses.
- **The Core fork's double-id change is claimed only as source.** The receipt does not claim a runtime result for it and I did not produce one. Core still renames through `src/swap.rs`; the receipt says so at line 44 and in the API docs at `db.rs:1544` to `1546`.

## Request 1 scope, honestly bounded

This is a scoped API PASS candidate for the metadata layer, and it is not that today.
`Meta::rename_snapshot` has no consumer: `cowfs-core` still stages its own rename through `src/swap.rs`, so no behaviour outside `cowfs-meta` changes.
Nothing about mount, export or namespace behaviour changes by merging this.
Issue #42 as a whole stays open, with its other requests unwritten and one, the shared name rule, already delivered in a separate change.
Request 1 alone would be a small, clean addition once Finding 1 is fixed, Finding 2 actually tests it, and the branch builds.

## Limitations and uncertainty, stated plainly

- **No runtime proof of my own.** The budget gate stopped me before any compile. Findings 1 and 2 are source-level, with the full line chain given so they can be checked or reproduced.
- Finding 1's refcount outcome depends on `refs[R_old]` being `1` at the moment of the rename, which is the ordinary case for a snapshot that was never forked from. A forked-from parent would leave `R_old` above `1`, in which case the row is still wrong but the old tree is not freed; the data loss is then silent staleness instead of a dangling root. I did not test the forked case.
- The exact error a caller sees on the first read after reopen is derived from `ptree.rs:46` and `ptree.rs:220`; I did not execute it.
- I did not run any test in the repository, at this head or at the base, so I make no claim about the 16, 7, 40, 12, 16, 2, 2 or 1-ignored counts the receipt reports.
- The path the receipt wants strengthened, real `cowfs-store` bytes read from the refs before and after a rename and after a reopen, is the right test and I could not add it. If Finding 1 is fixed, that probe would be the one that catches it, because it reads the bytes rather than trusting the row.
- CI on this head is red, and I read one snapshot. Nothing was polled, rerun, dispatched or reconfigured.
- `no-mistakes` is uninitialized in this lane and was not initialized. Browser unverified.
- Misakanet is local-only here and was not consulted; no local memory store was reachable in this lane.

## Scope discipline

Issue #42, request 1 only: the new metadata rename API.
No new issue, no new feature, no audit matrix, no new task.
No production edit, no test edit, no source patch by me anywhere.
No archive and no compile, because the budget gate said stop.
No checkout, branch change or commit in the lease.
No lease acquired, returned, reset, stashed, pruned or destroyed.
No signal, restart, sudo, install, unmount or store operation.
Nothing deleted, including every prior critic report, prior artifact tree and any other lane's cache.
The #134 reports `803ea5c5`, `ccc5eafc` and `4c3450f5` and their artifact trees are untouched.
The shared daemon PID 15263 with start time `Sat Oct 3 20:44:29 2026`, the Linux host, and every store, mount and job were never contacted.
Concurrent lanes untouched: READY6 on the store hole flag and metadata types, READY1's #136 documentation correction at `5723216`, READY3's clock observation.
Files I own for this review: this document, `docs/reviews/pr137-meta42-snapshot-rename-final.md`, and nothing else.

## What has to happen before this is a PASS

1. Fix `db.rs:604` to `607` so the row's root comes from `new_roots`, not from the session copy, or refuse a rename whose target tree is dirty.
2. Give the dirty-snapshot test options that leave the write pending, so it tests a dirty tree, and add a probe that reads real `cowfs-store` bytes from the refs after the rename and after a reopen.
3. Fix `tests/snapshot_rename.rs:215` so `clippy --all-targets -D warnings` passes, and get all three CI checks green.
4. Correct receipt lines 45 and 99 once the dirty case is genuinely covered.

Nothing was fixed, merged, marked ready or closed.
PR #137 stays a draft, issue #42 stays open, and no new task was created.