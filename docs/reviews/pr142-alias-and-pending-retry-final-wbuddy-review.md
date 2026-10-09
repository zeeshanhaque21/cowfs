# PR 142 alias-contract and pending-retry final review (head `6a0515e6`)

Reviewer: wbuddy (independent, read-only).
Scope: exact-head acceptance audit of PR 142 head `6a0515e6460b9211d8cbf51164b8a1b8ea6960bc` against #42 request 4 and #40.
Method: GitHub API reads (integer-only), `gh run view --log` plaintext, immutable `git show` on fetched objects.
Forbidden and not done: no source edit, no checkout, no cargo/build/test/archive/probe/cleanup, no lease, no runner/dispatch/rerun, no history rewrite, no PR-state edit, no issue action, no main commit/push, no shared-resource change.
Runtime statements are from completed GitHub-hosted CI runs or labelled UNEXECUTED.

## 1. Exact pins

| Object | SHA |
|---|---|
| Head | `6a0515e6460b9211d8cbf51164b8a1b8ea6960bc` |
| Head tree | `2a1f90ec32a5fdf2333875ad5165638dfcd1b27d` |
| Head parent | `b588a0e94bd1165c22edcb4d3062765e1eb723e3` |
| Prev-reviewed head | `67fd0aceb400fd84010527b0ed355ebfb1540f43` -> `b588a0e` -> `6a0515e6` (linear) |
| Merge head (older) | `1a49279253c0c9f31677793b779973a137fb143e` (parents `89d3a9d` + old main `e488a17b`) |
| Remote main (advanced) | `1580e69b9d987f63c07b2430f8c0b4547ecd8622` (merge of `e488a17b` + `7afe7226`, PR 143) |
| PR 142 `base.ref` / `base.sha` | `main` / `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` |
| PR 142 `mergeable` / `mergeable_state` | `true` / `unstable` |
| PR 142 `state` / `draft` / `merged` | `open` / `true` / `false` |
| PR 142 `changed_files` / `+` / `-` | 12 / 874 / 51 |
| Head run | `37530637925` (the only run for this head) |

`git ls-remote` confirms remote main is exactly `1580e69b` and the branch tip is exactly `6a0515e6`.

## 2. Tested-merge compatibility (not assumed)

New main `1580e69b` is **NOT an ancestor** of head `6a0515e6`; merge-base(head, new main) is `e488a17b`. So the head was built on old main, and the CI-test merge tree is `PR-head + e488a17b`, not `PR-head + 1580e69b`.

Compatibility check (explicit, not assumed): PR 143 (`7afe7226`) changed only `crates/cowfs-nfs/tests/namespace_race.rs`. It does not touch `inner.rs`, `ino.rs`, `ns.rs`, `db.rs`, or the alias contract. The `alias.rs` blob is identical on `e488a17b`, `6a0515e6`, and `1580e69b` (`38c37627b57de3400f76fd7db05a4a0c771e0b4d`). Therefore integrating new main would not change the alias verdict below. New main also already inserts one alias per create in `commit_batch`, the same shape this head uses, so the alias-policy surface is consistent between main and the branch.

## 3. Runtime evidence (completed CI, the actual result)

Head run `37530637925` (`event pull_request`) is `completed failure`.

| Job | Conclusion |
|---|---|
| `check (ubuntu-latest)` | failure |
| `check (macos-latest)` | failure |
| `linux-fuse` | success (not Core/Meta proof) |

Steps on both `check` jobs: `cargo fmt --all --check` success, `cargo clippy --workspace --all-targets -- -D warnings` success, `cargo test --workspace` **failure**.

The failure is a hard crash, not an assertion:

```
Running tests/alias.rs (target/debug/deps/alias-...)
running 4 tests
test alias_bytes_at_500k_creates ... ignored, heavy ...
thread 'a_create_past_the_alias_ceiling_is_refused' (6344) has overflowed its stack
fatal runtime error: stack overflow, aborting
error: test failed, to rerun pass `-p cowfs-core --test alias`
Caused by: process didn't exit successfully: ... (signal: 6, SIGABRT: process abort signal)
```

Identical on ubuntu and macos. The prior `0 vs n+1` assertion failures on `67fd0ace` are gone, replaced by an **infinite recursion that aborts the test binary**. Because `cowfs-core` sorts before `cowfs-meta` and the process aborts, the entire `cowfs-meta` suite, `reserved_inode_identity.rs`, and `inode_reservation.rs` again never run.

### 3.1 Root cause: the alias is a genuine self-map, and `load_node` recurses on it

The `b588a0e` change re-enabled aliasing every committed create:

```rust
// crates/cowfs-core/src/inner.rs, commit_batch
for (v, m) in &created {
    al.insert(*v, sc.id, *m);
}
```

`Aliases::insert` (`ino.rs:100`) writes both directions:

```rust
self.fwd.insert(virt, m);                 // fwd[ino]  = bare meta number
if let Ok(pm) = pack(snap, m) {
    self.rev.insert(pm, virt);            // rev[packed] = ino
}
```

On the head, `make` (`ns.rs`) sets each created child to the packed meta number: `let ino = pack(snap, ticket.ino().0)?;`. So in `created`, `v == pack(snap, m)`, and `insert` writes `rev[pack(snap, m)] = v`, i.e. **`rev[ino] = ino`** - a self-map in the reverse direction.

`canon` reads `rev`, and `load_node` recurses on its result:

```rust
// crates/cowfs-core/src/inner.rs, load_node
Id::Meta { snap, m } => {
    let v = self.aliases.rd().canon(snap, m);   // returns ino (self-map)
    if let Some(v) = v {
        return self.nodes.get(&v).map_or_else(|| self.node(v), Ok);   // node(ino) -> load_node(ino)
    }
    (snap, m)
}
```

There is no `v != ino` guard. When the node is not cached, `node(ino)` -> `load_node(ino)` -> `canon` -> `node(ino)` forever. The `alias.rs` ceiling test triggers this exactly: it does `c.forget(a.ino, 1)` (evicts the node), then `c.getattr(*ino)`, which reloads through `node(ino)` and recurses.

Why main does not crash on the same code shape: on main, creates still use **virtual** numbers, so `v != pack(snap, m)` and `canon` returns a different number; `node(v)` loads a distinct node, no recursion. PR 142 changed the child to be the packed meta number, which is what turns the same loop into a self-map. The `b588a0e` reasoning ("it is a real entry, not a self-map") examined only `fwd` and missed `rev`.

Responsible seam: `crates/cowfs-core/src/inner.rs` `commit_batch` (alias insert) combined with `load_node`/`canon` (`inner.rs:300`, `inner.rs:344-346`). The `ns.rs` ceiling enforcement is correct and is not the cause.

This is a **fatal regression that aborts a process**, strictly worse than the assertion failure it replaced.

## 4. Source/runtime verdict on the two claimed fixes

### 4.1 `ns.rs` ceiling enforcement - source-correct

`make` refuses at `self.aliases.rd().len() >= self.opts.alias_limit` before reserving, stores the `"session inode limit reached: {live} inodes are live, the ceiling is {}"` message in `last_error`. This matches the unchanged `alias.rs` contract text. Capacity-after-unlink is covered by the unchanged test (`unlink(r, b"f0")` -> `aliases == 3` -> create succeeds), and the alias is removed on the last unlink (`inner.rs` release path, `nlink == 0`). `op_link` increments `nlink` without adding an alias, so hardlinks do not inflate the count. UNEXECUTED at runtime (the run aborts before the assertion), but source-consistent.

### 4.2 Aliasing every create - source-wrong (the crash)

As shown in 3.1, this is the defect. The claim "not a self-map" is false for `rev`, and `canon`/`load_node` recurse on it.

### 4.3 T13 correction (`6a0515e6`) - source-correct

T13 now:
- asserts the pending create resolves in the session tree (`s.getattr(want)`, `pending.ino == want`, `pending.kind == File`);
- retries with a **different name** `b"b"` and expects `Err(Error::Exists)` (so the refusal is the taken inode number, not the name);
- drops `s`/`m`, reopens with `Meta::open`, and reads through `again.snapshot_by_id(SnapshotId(1)).getattr(want)` expecting `NotFound`.

This matches the retained-pending-tree semantics proven in the prior review (failed `commit()` does not reset `e.tree`; the durable store never saw the create). `snapshot_by_id` exists (`db.rs:1718`). This corrects the defect I found on `67fd0ace`. UNEXECUTED at runtime (cowfs-meta never runs), but the source is now right and uses `want` rather than leaving it unused.

## 5. Unchanged old tests: not weakened

- `crates/cowfs-core/tests/alias.rs`: blob `38c37627`, byte-identical on `e488a17b`, `67fd0ace`, `6a0515e6`, and `1580e69b`. No assertion added, removed, or relaxed. The two failing tests are unchanged main tests.
- `crates/cowfs-meta/tests/inode_reservation.rs`: blob `2f0deab5`, identical base vs head.
- `crates/cowfs-core/tests/reserved_inode_identity.rs`: new on the branch (not on base), 4 identity tests. No old test altered.
- `docs/v1-core.md`: +1 row (`inner::take_reserved | reserved | leaf`). `queue.rs`: `Op::Create` gains `reserved: Option<ReservedIno>`. Both are the approved #42 integration; no contract weakened.

Old virtual control (`ino.rs::virt`) is `#[cfg(test)]`-only and now only exercises `classify`; it is not a production path. The "old virtual control only rejects virtual marks" property holds.

## 6. Missing gates (separate from the runtime verdict)

- F7 bounded alias table / session ceiling: FAILING at runtime via the crash (not just "CI pending").
- Meta reservation/retry suite (T12/T13/T14, 18 db.rs unit tests), `inode_reservation.rs` (11 tests), Core `reserved_inode_identity.rs` (4 tests): UNEXECUTED on this head.
- M5 allocator runtime green (PR-144 lane): pending.
- Whole-#42 consumer/snapshot acceptance: out of scope here and not claimed.

Do not read "cargo fmt and clippy pass" as acceptance. The only runtime signal is `cargo test` failure with a stack-overflow abort.

## 7. Merge verdict

**NOT MERGE_READY. Do not merge.**

Blocker: **RUNTIME FAILURE with a root-caused source defect.** Head run `37530637925` fails `cargo test --workspace` on ubuntu and macos with `fatal runtime error: stack overflow, aborting` in `alias.rs::a_create_past_the_alias_ceiling_is_refused`. Root cause: `inner.rs` `commit_batch` aliases every create, so a reserved create writes `rev[ino] = ino`, and `load_node` (`inner.rs:344-346`) recurses on `canon`'s self-map result with no guard. This is a source-level cause, proven from source plus the CI crash, not a theory.

The `ns.rs` ceiling fix and the T13 correction are source-correct but cannot be credited at runtime because the run aborts beforehand.

Suggested direction (not applied; no source edits by me): do not alias a reserved create in `rev` when `v == pack(snap, m)` (or guard `load_node`/`canon` against a self-map). For reserved creates, the packed number is already canonical, so no reverse entry is needed. This restores `fwd.len()` accounting for the ceiling without introducing the self-map. A fresh CI run is required to prove it; the current head does not.

## 8. Author-receipt accuracy

`docs/verification/evidence/meta42-core-alias-contract-and-pending-retry-correction.md` correctly identifies the `67fd0ace` alias regression and the T13 defect, and marks local execution as forbidden. Two inaccuracies:

1. It asserts the reserved alias "is not [a self-entry] ... `canon` both resolve through it correctly." That is wrong for `rev`; `canon` reads `rev` and the entry is a self-map, which is the crash cause.
2. It cites "Run `37532881115` is the latest" for the fix head. That run does not exist (`gh api .../37532881115` -> `404`). The only run for head `6a0515e6` is `37530637925`, completed failure. The receipt's "PENDING" status is stale; the result is now known and failed.

## 9. Raw evidence pins

- PR 142: head `6a0515e6`, base `e488a17b`, mergeable `true`, state `unstable`, draft `true`, merged `false`.
- Head run `37530637925`: `completed failure`; steps 5 fmt success, 6 clippy success, 7 `cargo test` failure; `alias.rs` stack overflow on ubuntu and macos; `linux-fuse` success.
- `b588a0e` run `37530284116`: `completed failure` on `error[E0599] no method named getattr found for struct db::Meta` (db.rs:2512), which masked the alias recursion; fixed by `6a0515e6`.
- New main `1580e69b` = merge of `e488a17b` + `7afe7226` (PR 143, only `crates/cowfs-nfs/tests/namespace_race.rs`). Not an ancestor of head.
- `alias.rs` blob `38c37627b57de3400f76fd7db05a4a0c771e0b4d` identical on `e488a17b`, `67fd0ace`, `6a0515e6`, `1580e69b`.
- Log source: `gh run view 37530637925 --log`, ANSI-stripped copy used for counting.

No sandbox run occurred; the `context-mode` MCP tools were unavailable this session, so large logs were derived with in-line filters rather than an indexed sandbox.
