# Repair proof for issue #90

What the independent integration review of PR 96 found, what each finding turned out to be, and
what was measured after the repair.

Reviewer: critic slot 6, `docs/reviews/namespace-durability90-integration-final.md`, seven findings.
Its report is read-only and is not edited by this branch.

The review's own summary was that the real crash deliverable passed and that the mechanism, the
locking, the store-before-metadata ordering and the root scope all held.
The seven findings are below in the reviewer's order, each with the actual effect, the change, and
the check that distinguishes the change from the code it replaced.

## Finding 1, `durable_or` barriers unconditionally and the documentation said it did not

`durable_or` evaluates `self.durable(dir)` as the first element of the tuple it matches, with no
short-circuit, so `setattr` reaching it with an error result still pays the full barrier.
The documentation in `docs/nfs-namespace-durability90.md` claimed a refused `setattr` owes no
barrier.
It does.

**What was decided.** The unconditional barrier is correct and stays.
It is not "nothing to do because this RPC changed nothing": it commits whatever the snapshot already
had queued, so a refusal arriving on top of earlier uncommitted writes still discharges them.
A short-circuit would leave those queued writes behind, which is the bug this whole change is about.
The cost is one metadata sync on a refusal.
So the fix is to the claim, not the behaviour: the helper doc, the `setattr` call site, the design
doc and the README now say the barrier is unconditional and why, and a test asserts it.

## Finding 2, the precedence regression could not observe what it claimed

The original regression injected `Error::Io` for the attribute step and `Error::Io` for the
barrier.
`nfsstat` maps both to `NFS3ERR_IO`, so the reply was `NFS3ERR_IO` whichever order the helper used,
and the assertion passed either way.
The test was green and proved nothing.

**The fix.** The attribute fault is now `Error::PermissionDenied`, which `nfsstat` maps to
`NFS3ERR_ACCES`.
A reply carrying `NFS3ERR_ACCES` is distinguishable from one carrying `NFS3ERR_IO`, so precedence
becomes observable.
Three routes are asserted: both healthy gives one barrier and the reported attributes; attribute
fails with a healthy barrier gives one barrier and `NFS3ERR_ACCES`, so a caller can still tell an
attribute problem from a durability one; both fail gives `NFS3ERR_IO`, because a status reading as
"that did not happen" would be a lie about a name that exists.

**Mutation evidence.** Replacing the helper with one that swallows the barrier's status when the
caller's step also failed fails the third assertion with `left: 13, right: 5`, `13` being
`NFS3ERR_ACCES` and `5` being `NFS3ERR_IO`.
An arm-order-only mutant was also tried and does not change behaviour: when the barrier fails the
tuple is `(Err, Err)` and both orders reach the barrier arm.
That mutant is not claimed as a check, because it is not a behaviour.

## Finding 3, `rmdir` had the same early-`?` shape the `create` arms had

In Hide mode `rmdir` calls `purge_sidecars`, which unlinks real sidecar names, then retries the
`rmdir`.
Both the purge and the retry were followed by `?`, so a failure in either returned before the
barrier while sidecar names had already been removed.

This is not a claimed production bug: no crash measurement reproduced a name lost through this
path.
It is the same code shape that was a real defect in `create`, and it owes a barrier.

**The fix.** The arm yields an `NfsResult` instead of propagating, and the whole result goes through
`durable_or`, so every exit from the point where a name may have been removed owes the barrier.
`reap_if_last` now runs only on the success path, after the barrier is discharged.

**Mutation evidence.** Restoring the `?` on the purge fails `a_purge_that_fails_midway_still_barriers`
with no barrier recorded at all, `left: [], right: [SyncNs(1)]`.
Restoring an early `return` on the failing retry fails the same test the same way.
Both routes are driven through the real NFS adapter over a real socket, in Hide mode, against a
directory holding only sidecars, and the test asserts `unlinks() == 2` before asserting the barrier,
so the barrier assertion cannot pass vacuously.

## Finding 4, the gate's `Drop` signalled bare pids

`Drop` iterated a `Vec<u32>` of pids and sent `SIGKILL` by number, with a `debug_assert!` as the
only guard.
`debug_assert!` is compiled out in release, so the guard did not exist in the profile a CI run
would use.

**The fix.** Each child is recorded at spawn, before any mount wait and before anything can fail,
with its pid, the kernel's start time, its executable and argv, and the store and socket this
fixture owns.
A signal is sent only after that identity is re-read and every field still matches.
A recycled pid, a stranger on the same number, and an unreadable identity are all refused and
reported rather than killed.
Where a child is still owned by this process, it is stopped through its own handle, which needs no
identity at all and cannot hit a stranger.
There is no process group and no `pkill`: one verified pid or nothing.

**Checks.** 16 unit tests, all with owned harmless children: a child carrying this fixture's paths
is recognised and signalled; a live child that does not carry them is refused; a live child whose
recorded start time is off by one second is refused; a pid of 0 with an empty executable is refused;
the bounds are real, in that a 30s child spawned against a 300ms budget returns in under 5s and is
reported unfinished.

## Finding 5, the delete decision failed open

`cowfs_daemon::mounts::is_mounted` returns `false` both when `/sbin/mount` cannot be run and when
`is_listed` fails to match.
`is_listed` matches unescaped `mount` output, so it cannot match a path containing a space, which
`mount` prints as `\040`.
Either case gave `Drop` a `false` and sent it to `force_remove_dir_all` on a path that might still
have been a live mount.

**The fix.** A local tri-state reader in the gate's own test tree: `Mounted`, `Absent`, `Unknown`.
`Unknown` means the table could not be run, did not exit zero, did not finish inside its budget, or
could not be parsed, and it forbids the delete outright; only `Absent` permits one.
Paths are compared by exact equality after a single-pass decode of the four escapes `mount` uses
(`\040`, `\011`, `\012`, `\134`), so a prefix is not a match and a literal `\040` inside a name is
not rescanned into a space.
A table with any unparseable line is `Unknown` for the whole table, because a partial parse cannot
support absence.

`cowfs_daemon::mounts` and `cowfs_nfs::is_listed` belong to other owners, so this is local to the
test tree rather than a change to shared helpers.
It is worth saying what that means: the shared helpers still fail open for anyone else who uses
them, and that is not fixed here.

**Checks.** Empty, truncated and unparseable tables, an exact path, a space, a backslash, a literal
escape sequence, a prefix that must not match, and the delete decision itself asserted directly
rather than inferred.

## Finding 6, the 30s deadline bounded nothing

`umount` ran through `Command::status`, which has no timeout and can block against a wedged kernel.
The 30s deadline started after it returned, so it bounded the wrong thing.

**The fix.** Every external command the teardown runs is spawned and polled against a deadline
created before the spawn, so a slow spawn cannot extend it.
The whole teardown shares one absolute deadline computed once, so a slow step cannot buy a later
step a fresh budget.
An `umount` that does not finish is reported and stopped through its own handle, and the fixture
keeps its store rather than proceeding.

**Checks.** The bounds are tested directly against owned harmless children: a 30s child spawned
against a 300ms budget returns in under 5s and is reported unfinished; `/sbin/mount` listing the
table against a 10s budget is reported finished.
No real mount is unmounted by any of these, and no kernel is wedged.

## Finding 7, the tracked evidence carried no provenance

`results-this-commit.jsonl` had `case`, `rep`, `pid`, `new_name`, `old_name`, `fsck` and nothing
else.
The binding to a commit lived in prose in a README, so the file on its own could not be tied to any
code, and a reader had no way to tell which tree or which binary produced it.
The file was also hand-written into the repository rather than generated by a run.

**The fix, in two halves.**

`crates/cowfs-daemon/tests/evidence/` now generates the receipts.
Every row repeats the revision, the git blob of each source file the outcome depends on, and the
sha256 of both fixture binaries, so a reader of the rows alone is not relying on a manifest having
been shipped alongside.
The revision is read from git at the moment of the run.
A dirty tree is recorded as `code-under-test (uncommitted)` with a sha256 of the diff rather than
presented as committed source, because the two are different and only one is on GitHub.
Nothing here names a commit that did not exist when the rows were written.

`DURABILITY90_REPS` bounds the matrix, so a receipt set can be regenerated from an actual small
measurement instead of re-running the full three-per-case batch.

**What was not regenerated, and why.** The rows in `results-this-commit.jsonl` were produced at
`c6445476e6c9882e975638f2e6d829a7dd304443` and are bound here by that commit, by the blob ids
listed in `README.md` under `source-binding`, and by the file's own sha256

```
5b89dd986049fd7143796fc2d9ecc886a389b293099b9d0ad03194bac6c703b7
```

They are historical rows, not rows produced at the current tip, and this document does not present
them as such.
They have not been re-measured at `21d45f9`, because the shared mac-heavy resource lock was held
for the whole of a single bounded 600s wait and the measurement was not run unlocked.
That is a real gap and it is recorded as one.
`c644547` and `21d45f9` carry the same barrier call sites in `adapter.rs` apart from the `rmdir` arm
fixed in finding 3, which is why the historical rows remain evidence for the mechanism rather than
for the current tree.

## What changed in the source between the reviewed head and this one

Blob ids at `21d45f9c9d628ca4758b9140c8dca742efb04764`.

| file | blob at `02dddf8` | blob at `21d45f9` |
|---|---|---|
| `crates/cowfs-nfs/src/adapter.rs` | `b2b54c4b210eca7ee425e6de530728e2f4cabff0` | `c9ff3862ea1647d2ca3825fc997159cd8b2e0172` |
| `crates/cowfs-daemon/tests/namespace_durability.rs` | `24c8d59d909a84ea8d67d1782ea59be47a1a2482` | `4ca8613bbab6e208e389d3dca2ac86052d3b298d` |
| `crates/cowfs-daemon/tests/namespace_durability_gate.rs` | `1ee68f6ae230e78b1bc2703d3ebbc438017d7acd` | `4bc69a1ceaaa7e257389eb1d14bf2b278992fcbc` |
| `crates/cowfs-core/src/ns.rs` | `5a1024c115d51806131c6f6cc9ff99f5f20e6588` | `5a1024c115d51806131c6f6cc9ff99f5f20e6588` |
| `crates/cowfs-core/tests/model.proptest-regressions` | `caadf6bac71585c0fa4cffd95e25ebec34db194f` | `caadf6bac71585c0fa4cffd95e25ebec34db194f` |
| `crates/cowfs-core/tests/elide_dentry.rs` | `1df33880c23b6250fde053673e8a4048994c7b05` | `1df33880c23b6250fde053673e8a4048994c7b05` |

New files at `21d45f9`, none of which existed at `02dddf8`:

| file | blob |
|---|---|
| `crates/cowfs-daemon/tests/guard/reader.rs` | `23d1092366baf12dc349cc3cee176d4e1b7c2916` |
| `crates/cowfs-daemon/tests/guard/tests.rs` | `a778da481485a454a59614b32a2325d70b213b5c` |
| `crates/cowfs-daemon/tests/evidence/reader.rs` | `9261acfb9ea0fdb49063492acd4adccee853b915` |
| `crates/cowfs-daemon/tests/evidence/tests.rs` | `ecabe9348d1994ba64846b155816f7f6aa7f32a7` |

`ns.rs` and both `cowfs-core` regression files are byte-identical across the three commits, which
is the point: none of the seven findings needed a `Core` change.
The elide seed `9aa30bfa88a2438194d3b5ae7af55c7e2a59ff8a233abfe1cd0ddbec9d213900` is untouched.

## Unresolved, carried forward honestly

- The shared mac-heavy resource lock was held for the whole bounded wait, so no new crash
  measurement was taken at `21d45f9`.
  `README.md` records the historical rows at `c644547` and this document records the gap.
- `cowfs_daemon::mounts::is_mounted` and `cowfs_nfs::is_listed` still fail open for other callers.
  Other owners, not this branch.
- `docs/design.md` success criterion 2, build overhead within 1.5x of native, is not measured and
  stays open.
- Power loss is not covered. Every readback here is a process kill and is equally consistent with the
  host page cache.
- Issue 88, full crash-injection acceptance with durable receipts, is open and separate.
  Success criterion 3 of the design is not claimed closed.