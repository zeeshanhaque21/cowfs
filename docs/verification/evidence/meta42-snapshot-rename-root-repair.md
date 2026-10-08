# meta42-snapshot-rename-root-repair: the real red is recorded, the rename arm resolves the flushed root, two new cases are written and unexecuted

Lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, READY5, held throughout.
Red head: `9d1e5ef66d55798da08780296a05d6601468611f` on `fix/meta-snapshot-rename-42`, verified clean and equal to the remote before any edit, and still the parent of this work.
Immutable, preserved unedited: `93c4b1317b7d9445ced3f7d1316e159b3cca472cbee6172c0a81e2b9a8883c0b`, `a390aaf4b84bcd6a36117b854faa54710543e4c32c20e868185ff08a390be811`, `471a07310e793016763a1f77e4e195bfeca89dcf1613d84fe50b238a6355b0ec`, `b66ea61b14d2507ea0d9eb1975c0cd40e40c774a132f0271296818970a8d824e`.

## The red, read out of the actual CI logs

Run `37376415288`, `head_sha` `9d1e5ef66d55798da08780296a05d6601468611f`, created `2026-10-05T21:31:34Z`, conclusion `failure`.
I fetched all three job logs and read the failure in them rather than taking a summary for it.

**`check (ubuntu-latest)`, job `111986390388`, started `21:31:45Z`, completed `21:39:08Z`, conclusion `failure`.**
The failing target is bound by the log itself: `Running tests/snapshot_rename.rs (target/debug/deps/snapshot_rename-9d5ff8fd5fa17170)`.
`thread 'a_rename_commits_a_pending_tree_change_and_leaves_the_row_readable' panicked at crates/cowfs-meta/tests/snapshot_rename.rs:372:5:`
`assertion 'left != right' failed: the committed row must carry the flushed root, not the pre-flush one`
`left` and `right` are byte-for-byte the same 32 values, `[56, 49, 247, 189, 66, 119, 124, 95, 90, 118, 158, 65, 211, 130, 82, 97, 173, 159, 152, 217, 47, 100, 146, 154, 143, 175, 27, 255, 60, 151, 32, 18]`.
`test result: FAILED. 7 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s`.
`error: test failed, to rerun pass '-p cowfs-meta --test snapshot_rename'`.

**`check (macos-latest)`, job `111986390434`, started `21:31:42Z`, completed `21:39:57Z`, conclusion `failure`.**
Same test, same line `372:5`, same message, same equality, `test result: FAILED. 7 passed; 1 failed`.
The hash values differ from Ubuntu's, which is expected because the roots come from fresh `Timestamp::now()` calls per run and are not fixed by the test.
Bound to its own binary: `Running tests/snapshot_rename.rs (target/debug/deps/snapshot_rename-e8fea655cdbc51c8)`.

**`linux-fuse`, job `111986389988`, started `21:31:40Z`, completed `21:36:40Z`, conclusion `success`.**
`test result: ok. 40 passed; 0 failed` and `test result: ok. 11 passed; 0 failed`.

Two facts about the barrier that the logs settle on their own.
The `cargo fmt --all --check` step ran and did not fail the run.
The `cargo clippy --workspace --all-targets -- -D warnings` step ran and reached `Finished 'dev' profile [unoptimized + debuginfo] target(s) in 11.00s`, so the clippy barrier is gone and the test target actually executed, which is why there is a `test result:` line at all.

Captured raw, append-only, in `bench/out/meta42-snapshot-rename-root-repair/`:

| Log | sha256 |
|---|---|
| `ci-37376415288-ubuntu-111986390388.log` | `b3ddb278447f6d280600d35b9f9f99b7a13b4daa6d8ce862577e36de76b456d9` |
| `ci-37376415288-macos-111986390434.log` | `37d1a16172512896f3e9f3a3ecd820ea36a52310f84dd4e73f59f5b79c8b1b35` |
| `ci-37376415288-linux-fuse-111986389988.log` | `2d4b96412c7e50987935338fda69a259fc5af01b1694bcfdf681180d989b7d55` |

## What the red actually proves, and what it does not

It proves the **durable row** carries the pre-flush root, on two platforms, in a real run.

It does **not** prove a missing-node failure.
The assertion at `372:5` compares the reopened row's root to the root read before the pending write, and it aborts there.
The lookup that walks the tree from that root, and `check()`, were never reached in either log, so no missing-node error and no `check()` failure was observed.
Those remain predictions from the source, and they stay predictions until a run gets past line 372.

That is the whole finding, and it is enough: the row is wrong, so the tree the rename's own transaction settled is unreachable from the row.

## The fix

One arm, `crates/cowfs-meta/src/db.rs`, the `Extra::Rename` match arm. Eleven added lines, no line removed, nothing else in the file touched.

```rust
let e = s.snaps.get(id).ok_or(Error::NoSuchSnapshot)?;
// The flush loop above may have written this snapshot's dirty tree and
// moved its root, while this session copy still holds the pre-flush root.
// Resolve the root through new_roots the way Extra::Add does, or this
// insert writes the pre-flush root back over the flushed one and the
// freed tree's successor is unreachable from the row.
let root = new_roots
    .iter()
    .find(|(i, _)| i == id)
    .map(|(_, r)| *r)
    .unwrap_or(e.info.root);
let mut info = e.info.clone();
info.root = root;
info.name = (*name).to_string();
```

It reuses the seam that was already in this file: `Extra::Add` resolves its source root through `new_roots` at `db.rs:551-557` for exactly this reason, and falls back to the session copy when the source was clean. No new mechanism, no framework, no schema, no on-disk change, no dependency, no lock line.
`new_roots` is the same `Vec<(SnapshotId, NodeId)>` the flush loop pushes into, so the lookup is over data this transaction just wrote.

Deliberately unchanged, and verified unchanged in the diff: the `id` is still the key, so `SnapshotId`, inode numbers, generation, creation time and parent all come from the same `e.info.clone()`; `names.remove(e.info.name.as_str())` still reads the **old** name off the session entry, not the clone, so the old key is the one removed; the name collision refusal still happens in the validation block before this arm; the session mirror update at `db.rs:641-646` and the hook still run after `wtx.commit()`; the ref add and drop are still the flush loop's, so the refcount and `settle` are untouched; the reservation high-water mark, the reap queue and `next_snapshot` are untouched.

When the tree was not dirty, `new_roots` has no entry for this id and `root` falls back to `e.info.root`, which is the value already in the row, so the clean case behaves exactly as before.

## Two new cases, written and not executed

The existing dirty regression is **byte unchanged**.
That was deliberate and load-bearing: the old failure and the new result have to be comparable, and the red is pinned to `snapshot_rename.rs:372:5`.
The only edit to existing lines was the module doc comment, rewritten in the same three lines so that line 372 still holds the same `assert_ne!` in the same place, which I checked after the edit.
Line 485 is still the end of the previous last test, and the two new tests are appended after it.

The file now has ten tests.

**`a_rename_keeps_real_file_bytes_readable_after_a_drop_and_reopen`.**
This is the real store readback that was owed.
512 KiB of deterministic, irregular bytes are ingested into a real `cowfs_store::Store` with `ingest_bytes`, which is what produces genuine content-defined chunk refs, and the test asserts the result is more than one chunk so it cannot degenerate into a single-block check.
The refs are attached through the public `Tx::set_content`, so the chunks in the tree point at real blocks and not at placeholder or zero ids.
The rename happens with a pending unsynced `create` in the tree, then everything is dropped and the store reopened.
The survivor check walks `Snapshot::chunks`, asserts the count is unchanged, asserts the chunk lengths still cover the original length exactly, asserts each id is not the empty-content id, asserts each id is still present in the store via `contains`, asserts each id is the same block rather than a rewritten one, and concatenates `store.get` for every block and compares the result to the original body byte for byte.
That is the property the row assertion cannot provide: the bytes themselves, read back through the renamed snapshot's root after a real close and reopen.

**`a_rename_of_a_dirty_snapshot_keeps_a_forked_old_root_live_and_the_renamed_one_fresh`.**
The shared-root variant, using the existing `Snapshot::fork` seam at `db.rs:1692`.
The fork goes through `add_snapshot`, so `Extra::Add` bumps the reference on the shared root at `db.rs:578`, and the test asserts the fork starts on the base root.
The base is then dirtied and renamed while the fork stays put.
Here the rename's `drop_ref` only decrements, so the old tree is not freed and a stale row would not show up as a missing node.
It shows up as stale content, which is what the test asserts: the fork still resolves its original root, still has its own files, and does **not** gain the write that was pending on the base, while the renamed base has that write and keeps what it already had.
`check()` is asserted at the end.

Both cases use the existing `opts_pending()` fixture, the one that leaves `sync_every_ops` at the default 256 with `Ack::Applied`, which is the configuration `cowfs_core::inner` opens meta with.

## What was not verified locally, stated plainly

**Nothing was compiled, formatted, linted or run locally.**
No cargo invocation of any kind, no new target directory, no archive, no build, no offload, no cache deletion, no `ready-40` removal, no cap waiver.
So for the two new cases the compiled status is **none**, and the following are all owed to CI and are **not** claimed:

1. **That they compile.** They are the first tests in this repo to drive `cowfs_store` from a `cowfs-meta` integration test, so the API usage is verified by reading `crates/cowfs-store/src/{lib,store,types,chunk}.rs`, `crates/cowfs-meta/src/tx.rs` and `crates/cowfs-meta/src/db.rs`, and not by a compiler.
2. **That `cargo fmt --all --check` passes on them.** Hand-written Rust that has never been through rustfmt is the single most likely reason this run goes red, and it would be red for a formatting reason rather than a behavioural one. I did not run `cargo fmt` because the instruction forbids local cargo, so this is a known open risk rather than a surprise.
3. **That the fix makes the existing regression pass.** The fix is derived from the source chain and the CI red, and it is the same resolution `Extra::Add` already uses, but no run has shown the old failure turning into a pass.
4. **That the bytes case passes**, and specifically that concatenating `store.get` over the chunk refs reproduces the body exactly. That relies on one block holding exactly one chunk, which is what `ingest_bytes` produces; the length-coverage assertion before the byte comparison exists so that if the assumption is wrong the failure says so directly.
5. **The missing-node and `check()` behaviour past line 372**, which is still unobserved.

**No pass of any kind is claimed from this turn.**

The build budget remains blocked exactly as recorded in `b66ea61b`: `bench/out` measures 20.862 GiB against an 8 GiB cap, and after removing every eligible compiler cache file the floor is 11.839 GiB, so the cap is unreachable without touching `bench/out/ready-40`, which holds another lane's mutation-golden source snapshots and receipts and is therefore protected. Free space is not the gate and does not waive it. `bench/out` is unchanged at 20.862 GiB, nothing was deleted, and every binary, source archive, golden file, log, receipt and prebuilt test executable is intact.

## State

PR #137 stays a **draft** and stays **BLOCKED** until this head has a real runtime pass on both platforms, a fresh independent review, and green CI.
Issue #42 stays open, no new issue was created, and this does not complete request 1 or the issue: the API still has no consumer, `cowfs-core` still stages its own rename, and nothing outside `cowfs-meta` changes behaviour.
No browser verification was done. `no-mistakes` is uninitialized in this lane and was not initialized. Misakanet is local-only here and was not consulted.