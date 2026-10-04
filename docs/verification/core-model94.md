# Verification: core model #94, the dentry of an elided unlink

Independent reproduction and fix for issue #94, on lease 16, branch `fix/core-model-94`.

The reproduction was done against `ceb96c67033cbf97f79267d6af7db3fa204d77d1`, which was `origin/main`
when the bug was found.
The branch has since been rebased onto `0915f97`, current `origin/main`, so CI runs against current
main; the rebase touched no file of this change and `crates/cowfs-core/src/ns.rs` carries blob
`5a1024c115d51806131c6f6cc9ff99f5f20e6588` before and after it.

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

Whole crate, after the fix: `cargo test -p cowfs-core` gives **279 passed, 0 failed, 9 ignored** across
every test target, including the model tests, `fsck`, `invariant`, `crash`, `kill9` and the snapshot and
namespace suites.
279 is 276 plus the 3 tests added for the `rmdir` site; the 9 ignored are pre-existing crate-level
ignores, not tests skipped for this change.

Every exit code below was read by running the command on its own, never through a pipeline, after the
false claim in the next section had already been made once:

```
cargo fmt --all -- --check                                exit 0
cargo clippy -p cowfs-core --all-targets -- -D warnings   exit 0
cargo test -p cowfs-core --test model                     2 passed, 0 failed
cargo test -p cowfs-core --test elide_dentry              8 passed, 0 failed
cargo test -p cowfs-core                                  279 passed, 0 failed, 9 ignored
```

Those numbers were re-measured after the rebase onto `0915f97`, with the model seed `9aa30bfa...`
unchanged and `crates/cowfs-core/src/ns.rs` still at blob `5a1024c115d51806131c6f6cc9ff99f5f20e6588`.

## Correction of two claims that were false on head `0d51b38`

An earlier version of this document, written for head `0d51b38cbdd0d709811575020cc9a109048ba325`,
stated `cargo fmt --all -- --check` clean and presented the green local test runs as though the head
was in good shape.
Both statements were wrong, and the mistake is worth recording because the shape of it is easy to
repeat.

**The formatter was red on that head, and CI never ran a test because of it.**

```
CI run 37231298754, workflow ci, event pull_request, branch fix/core-model-94
  check (ubuntu-latest)  failure
  check (macos-latest)   failure
  linux-fuse             success
cargo fmt --all -- --check    -> exit 1
4 hunks, all in crates/cowfs-core/tests/elide_dentry.rs
```

Both failing jobs stopped at the first step, `cargo fmt --all -- --check`, so `cargo test --workspace`
never executed.
Every green test number in the earlier version of this document was therefore local only, and no CI run
had ever exercised the tests.

**Why the local check reported clean anyway.**
The earlier check was written as:

```
cargo fmt --all -- --check 2>&1 | head -30; echo "fmt exit=$?"
```

`$?` there is the exit status of `head`, the last command in the pipeline, not of `cargo fmt`.
`head` succeeded, so the script printed `fmt exit=0` while the formatter was failing.
The formatter never printed a diff, so nothing in the output contradicted the printed status either.

The lesson is narrow and mechanical: check a status by running the command on its own and reading its
own exit code, and never infer a green build from a tool that ran after the one being checked.
The critic's review of `0d51b38` found this on the head; the same check, run directly, now gives
`exit 0` on the repaired head.

**What was and was not green on `0d51b38`.**
The fix logic was reviewed and correct, and the local test runs were genuinely green.
What was not established was that the head could build its CI at all.
A local green run and a red formatter on the same commit are both true, and only one of them was
reported.

## The second patched site was untested

The fix changes two sites, `op_unlink` and `op_rmdir`.
The first version of the test file exercised only `op_unlink`, so nothing in the repository would
catch a regression in the `rmdir` elide path.
That was found by review after the first push, not before it.

The repository now covers both sites: `mkdir` in `s0`, fork, then `rmdir`, `mkdir`, `rmdir` in `s1`,
then a cache drop, then `mkdir` must succeed, with `Stats::elided == 1` asserted so the elide path is
proven to be the one taken.

## The regression test

`crates/cowfs-core/tests/elide_dentry.rs`, eight tests over the public `Core` API.
Four cover the file path (`op_unlink`) and four cover the directory path (`op_rmdir`), which is the
second patched site:

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
- `mkdir_after_an_elided_rmdir_sees_the_name_as_free`: the directory counterpart of the first test.
  `mkdir d` in `s0`, fork, then `rmdir`, `mkdir`, `rmdir` in `s1`, a cache drop, then `mkdir` must
  succeed. Asserts the true state: `lookup` is `NotFound`, the listing is exactly `["d"]`, `s0` still
  lists `["d"]` and is untouched by everything done in `s1`, and after a `sync` plus another cache drop
  the tombstone is gone while the listing is still correct. The second assertion is what distinguishes
  "correct" from "cached forever": the entry is dirty only until the batch commits.
- `a_removed_directory_is_gone_from_meta_after_a_reopen`: the removal is committed, so meta itself has
  dropped the name, and it is still free after a real `Core::open`. `s0` still has its directory.
  `check()` and `fsck` are asserted clean both before and after the reopen.
- `a_directory_that_is_still_present_still_answers_exists`: the directory counterpart of the `Exists`
  control. A live directory name still answers `Exists`, an absent name answers `NotFound`, and a
  directory with an entry answers `NotEmpty`.

The whole file was checked to be non-vacuous against the pre-fix `ns.rs`, with every other file at
this head.
An archive of this head had exactly one file replaced, by `git show ceb96c6:crates/cowfs-core/src/ns.rs`,
and every other file was verified identical to the head by hash:

| build | `elide_dentry` |
|---|---|
| old `ns.rs` from `ceb96c6` | 4 passed, **4 failed** |
| this head | **8 passed**, 0 failed |

The four failures on the old source are `create_after_an_elided_unlink_sees_the_name_as_free`,
`the_same_sequence_under_default_options`, `mkdir_after_an_elided_rmdir_sees_the_name_as_free` and
`a_removed_directory_is_gone_from_meta_after_a_reopen`.
Both patched sites are covered by at least one discriminating test.
The four that pass on both builds are the durability and control tests, which do not depend on the
cache drop: `create_after_an_elided_unlink_survives_a_reopen`,
`an_unlinked_name_stays_free_after_a_reopen_of_the_whole_sequence`, and the two `Exists` controls.
They are worth keeping, but they do not pin this bug and are not claimed to.

## Status of CI on this head

- CI run `37231298754`, head `0d51b38`: **red**, and it failed before any test ran, at
  `cargo fmt --all -- --check`. `cargo test --workspace` did not execute in CI for that head.
- No CI run has yet been green for any head of this PR.
  Every test number in this document is local.
  A CI result on the repaired head is reported only if it is actually observed, and this document does
  not claim a green CI run it has not seen.

## Overlap with concurrent leases

`fix/nfs-namespace-durability-90` (native lease 13, branch `342bfa0`) touches
`crates/cowfs-core/src/inner.rs`, `io.rs`, `vfs_impl.rs`, plus `cowfs-daemon/tests/namespace_durability.rs`,
`cowfs-nfs/src/adapter.rs` and `cowfs-vfs/src/vfs.rs`.
`git diff --name-only origin/main origin/fix/nfs-namespace-durability-90 -- crates/cowfs-core/src/ns.rs`
is empty, so the one file this fix changes is untouched by that lease and there is no textual conflict.

The two are still both `cowfs-core` and both touch namespace durability semantics, so integration must
be sequenced and retested together, in that order, on the exact post-merge head.
The Core seed for that retest is
`9aa30bfa88a2438194d3b5ae7af55c7e2a59ff8a233abfe1cd0ddbec9d213900`, already pinned in
`crates/cowfs-core/tests/model.proptest-regressions`, together with lease 13's own durability suite.

Issue #94 is fixed on `main` only after this PR merges.

No `CHANGELOG.md` edit, no workflow edit, no merge, no lease returned.
