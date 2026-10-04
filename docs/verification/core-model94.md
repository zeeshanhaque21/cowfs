# Verification: core model #94, the dentry of an elided unlink

Independent reproduction and fix for issue #94, on lease 16, branch `fix/core-model-94`, base
`ceb96c67033cbf97f79267d6af7db3fa204d77d1` (which is `origin/main`).

`cowfs-core/src` was byte-identical to `origin/main` before this change; the bug is in Core, not in
the model test, and not in the PR #79 change that surfaced it.

## Baseline: the exact failing seed

The seed from hosted CI run `37225470351` was pinned into `crates/cowfs-core/tests/model.proptest-regressions`
unchanged, and run exactly as CI runs it.

```
cc 9aa30bfa88a2438194d3b5ae7af55c7e2a59ff8a233abfe1cd0ddbec9d213900
# shrinks to ops = [Create([2], 0), Fork(0), Unlink([2]), Create([2], 0), Unlink([2]), DropCaches, Create([2], 0)]
```

On the unmodified base, both model tests fail with the divergence the issue reports:

```
cargo test -p cowfs-core --test model core_matches_memvfs_with_tiny_caches
assertion `left == right` failed: snapshot s0 result differs at op 59: Create([2], 35544)
  left: Err(Exists)
 right: Ok([])

cargo test -p cowfs-core --test model core_matches_memvfs_with_default_options
assertion `left == right` failed: snapshot s0 result differs at op 59: Create([2], 35544)
  left: Err(Exists)
 right: Ok([])
```

The second run matters: **the issue's "tiny caches only" reading is wrong.** The same seed fails
under the default options, so the trigger is the `DropCaches` operation, not the cache size. A small
cache only makes it easier to hit, because `drop_caches` evicts every clean entry regardless of size.

## Root cause

Reproduced end to end through the public `Core` API, with the model's operation sequence and nothing
else. No proptest, no differential oracle needed to see the mechanism.

The minimized sequence, with `Create([2])` meaning `create` of the single-component name `b'c'` in a
snapshot root:

1. `create f` in `s0`, then `fork_snapshot(s0, s1)`. `s1` inherits `f`; meta names it.
2. `unlink f` in `s1`. The node is a meta inode, so `Queue::try_elide` cannot cancel anything and an
   `Op::Unlink` stays **queued**. Meta still names `f`.
3. `create f` in `s1` succeeds, because the negative dentry from step 2 is authoritative while
   uncommitted. This queues an `Op::Create`.
4. `unlink f` in `s1` again. Now the *create* is elided, and the elide path wrote the negative dentry
   with `seq: 0`:

   ```rust
   // crates/cowfs-core/src/ns.rs, before
   if last && q.try_elide(&cn) {
       q.touch(&pn);
       drop(q);
       self.ctr.elided.fetch_add(1, Ordering::Relaxed);
       self.dents.put(parent, name, None, 0);   // <-- claims meta already agrees
       return Ok(());
   }
   ```

   `DCache::shrink_all` and `DCache::shrink` keep exactly the entries with `seq > flushed`, so a
   `seq: 0` entry is clean: evictable, and understood to reflect what meta already holds.
5. `drop_caches` therefore evicts the only record that the name is gone.
6. The next `create f` misses the cache, reads meta, meta still names `f`, and answers `Err(Exists)`.

Eliding a create says nothing about the earlier queued unlink of the same name, which meta has never
been told about. The elide path asserted a commit that had not happened.

The fork is what makes this reachable: the forked side inherits a name from meta, so its first unlink
is a real queued removal of something meta knows. Without the fork, a create-then-unlink pair elides
with nothing left in meta, and `seq: 0` happens to be true.

Measured baseline state, printed from a scratch harness before the fix (removed afterwards; the
behavior is now pinned by `crates/cowfs-core/tests/elide_dentry.rs`):

```
after 2nd unlink: elided=1 pending=2
drop_caches=true create -> Err(Exists)
final names=[]
```

`elided=1` confirms the elide path was the one taken. `pending=2` confirms work was still queued.

## The fix

`crates/cowfs-core/src/ns.rs`, both elide paths (`op_unlink` and `op_rmdir`): record the seq the
elided unlink actually reached instead of `0`.

```rust
let seq = q.touch(&pn);
drop(q);
self.ctr.elided.fetch_add(1, Ordering::Relaxed);
self.dents.put(parent, name, None, seq);
```

The entry is now dirty for exactly as long as the removal is uncommitted, so no cache drop, shrink or
eviction can lose it. Once the batch commits, `flushed` reaches that seq, the entry becomes clean, and
it is dropped like any other entry that meta now agrees with. Nothing else changes: the same elide
still happens, `Stats::elided` still counts it, and the entry is still a negative entry.

Scope note: this is the namespace dentry write in the elide path only. It is deliberately not the
fsync or namespace-barrier code in `ns.rs`/`io.rs`, which a concurrent lease also owns; see the
overlap section below.

## Proof

Old fail, new pass, on the CI seed exactly as CI runs it:

| run | base `ceb96c6` | with the fix |
|---|---|---|
| `core_matches_memvfs_with_tiny_caches` | FAILED, `left: Err(Exists) right: Ok([])` | passed |
| `core_matches_memvfs_with_default_options` | FAILED, same divergence | passed |

Bounded neighbor seeds. `PROPTEST_CASES` scales the case count and proptest picks a fresh random seed
on every run (no `PROPTEST_SEED` is set), so these are five distinct seed sets, not one repeated:

| run | cases | result |
|---|---|---|
| 1 | 32 | 2 passed, 0 failed |
| 2 | 64 | 2 passed, 0 failed |
| 3 | 96 | 2 passed, 0 failed |
| 4 | 128 | 2 passed, 0 failed |
| 5 | 160 | 2 passed, 0 failed |

Whole crate, after the fix: `cargo test -p cowfs-core` gives **276 passed, 0 failed** across every
test target, including the model tests, `fsck`, `invariant`, `crash`, `kill9` and the snapshot and
namespace suites.

`cargo fmt --all -- --check` clean. `cargo clippy -p cowfs-core --all-targets` clean.

## The regression test

`crates/cowfs-core/tests/elide_dentry.rs`, five tests over the public `Core` API:

- `create_after_an_elided_unlink_sees_the_name_as_free`: the exact sequence, then asserts the true
  expected state rather than only the absence of an error: `lookup` is `NotFound`, `create` succeeds,
  the listing is exactly `["f"]`, the content reads back as `third`, `nlink` is 1, `check()` passes and
  `fsck` is clean.
- `create_after_an_elided_unlink_survives_a_reopen`: same sequence through `sync`, then a real
  `Core::open` of the same directory. Asserts the name, the content and `fsck` after the reopen, and
  that the reopened session hands out a new inode number.
- `an_unlinked_name_stays_free_after_a_reopen_of_the_whole_sequence`: the sequence is committed, so
  meta itself has dropped the name, and the name is still free after a reopen.
- `a_name_that_is_still_present_still_answers_exists`: the valid `Exists` case is preserved. A live
  name still answers `Exists`. Unlinking one of two hard links frees that name only, so a following
  `create` yields a different inode and the surviving link keeps `nlink` 1. This was checked against
  `MemVfs` (`crates/cowfs-vfs-test`), which deletes by name with no nlink gate, rather than assumed.
- `the_same_sequence_under_default_options`: the issue's "tiny caches only" reading, pinned as a test
  so it cannot be reintroduced silently.

The test was checked to be non-vacuous: with the fix stashed and only `ns.rs` reverted,
`create_after_an_elided_unlink_sees_the_name_as_free` and `the_same_sequence_under_default_options`
both fail, and pass again once it is restored.

## Overlap with concurrent leases

`fix/nfs-namespace-durability-90` (native lease 13) touches `crates/cowfs-core/src/inner.rs`,
`io.rs` and `vfs_impl.rs` only. `git diff --name-only origin/main origin/fix/nfs-namespace-durability-90
-- crates/cowfs-core/src/ns.rs` is empty, so the namespace file this fix changes is untouched by that
lease and the two do not overlap in the same file. Both changes are in `cowfs-core`, so integration
should still be sequenced rather than merged blind.

No `CHANGELOG.md` edit, no workflow edit, no merge.
