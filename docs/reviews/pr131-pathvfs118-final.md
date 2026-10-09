# PR 131 final independent review: the #118 test-only delivery

Reviewer verdict: **PASS**, with one non-blocking documentation defect recorded below.
This review did not merge, did not push, did not edit source, and did not lease, return, prune or
destroy anything.
It recommends merge and stops there.

## Identities under review

| item | value | verified |
| --- | --- | --- |
| PR | https://github.com/zeeshanhaque21/cowfs/pull/131 | yes |
| head | `b7d1009d667c58bb13c5717bad32c981430b34d7` | yes |
| base | `00065ce75dcd554e1fb4bb084d1c70b2e2a21a87` | yes |
| base is `main` at PR open | `test(nfs): integrate reviewed server requirements harness (#113)` | yes |
| merge base of base and head | `00065ce75dcd554e1fb4bb084d1c70b2e2a21a87`, identical to base, so no rebase is pending | yes |
| commits in the PR | `78c5db55795abcf903f46106a6ddcdb57df1ec0b` (test), `b7d1009` (delivery record) | yes |
| PR state | OPEN, not merged, not draft, `mergeable_state=clean`, 0 reviews, 0 comments | yes |
| issue #118 | **OPEN** | yes |
| issue #120 | **OPEN** | yes |
| current `main` reviewed against | `dadc5241779c97e04ee5262824da6769dc0a6204`, 129 PRs merged | yes |
| lease used | `/Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/14/cowfs`, branch `review/mounted-fsx-g4` at `f816b5e9` | idle before use, see below |

The branch label in the assignment was `priorreview/mounted-fsx-g4`.
No branch of that name exists in this repository.
The assigned path resolves to a registered worktree (`.git/worktrees/cowfs56`) on branch
`review/mounted-fsx-g4`, and that path carries the previous `mounted-fsx-g4` review artifacts, so the
path is the authoritative identity and the label appears to be informal.
This is recorded rather than silently reconciled.

### Lease ownership check, performed before any use

- branch `review/mounted-fsx-g4`, HEAD `f816b5e96624967f16a28444a0631ddc9672892b`
- tracked state clean: `git diff --stat HEAD` empty
- no `index.lock`, no `MERGE_HEAD`, `CHERRY_PICK_HEAD`, or `REBASE_HEAD`
- no process holding that path, no `cargo`, `rustc`, or `cowfs-vfs` process on the host
- last reflog movement was a checkout on 2026-10-04, before this review
- only untracked content is the prior reviewer's `bench/out/fsx-g4-repair-critic/` and three
  `docs/reviews/mounted-fsx-g4-*.md` and `docs/reviews/pr102-final-delivery.md` files, left untouched

Verdict: idle and unclaimed, so used read-only apart from this review's own
`bench/out/pathvfs118-final-critic/**` output directory.

## Diff scope

Exactly what the delivery record claims, nothing more.

```
M  crates/cowfs-vfs-path/src/tests.rs              +51 / -1
A  docs/verification/evidence/pathvfs118-delivery.md  +168
2 files changed, 219 insertions(+), 1 deletion(-)
```

The `tests.rs` change is confined to the single test commit `78c5db5`; the delivery record is the
whole of `b7d1009`.
No production source is touched.

### Production seams unchanged, verified at four revisions

`crates/cowfs-vfs-path/src/table.rs` and `crates/cowfs-vfs-path/src/sys.rs` are byte-identical at the
patch's old base `951045f`, at the PR base `00065ce`, at the PR head `b7d1009`, and at the current
`main` `dadc524`:

| file | `951045f` | `00065ce` | `b7d1009` | `dadc524` |
| --- | --- | --- | --- | --- |
| `table.rs` | `d61ca4226accb9db1bd15d739c0ccda43d8276d0` | same | same | same |
| `sys.rs` | `e94ebf03015fb557c4d953e4215382f092babcff` | same | same | same |
| `tests.rs` | `90de21aa99f2...` | `90de21aa99f2...` | `3577e61a56cd2f6f8f1decfe5ff1be0e4304110d` | `90de21aa99f2...` |

No commit between the PR base and current `main` touches any of the three seams.
`main` gained 2 commits since the PR base.

## Source binding: the precondition reads the fields the adapter keys on

This is the load-bearing claim in the patch, so it was checked against `table.rs` directly.

`table.rs` holds the cache key as

```rust
/// The directory's (mtime, ctime, size, links) when the listing was read: a change made by
pub listing_stamp: Option<(i64, u32, i64, u32, u64, u64)>,
```

and builds it in `readdir_page` from exactly six values, in this order:

```rust
let dstamp = sys::fstat(open.file.as_fd()).map_err(io_err)?;
let stamp = (dstamp.mtime.0, dstamp.mtime.1, dstamp.ctime.0, dstamp.ctime.1, dstamp.size, dstamp.nlink);
let restart = cookie == 0
    || self.nodes.get(&dir)
        .is_none_or(|n| n.listing.is_none() || n.listing_stamp != Some(stamp));
```

The test helper added by this PR returns the same six fields in the same order:
`(mtime, mtime_nsec, ctime, ctime_nsec, size, nlink)`.
The test's comment "the six fields `table.rs` builds its readdir cache key from" is accurate.

Because the resume in the test uses a nonzero cookie taken from the first page, `cookie == 0` does not
force a restart, so the continuation depends on `n.listing_stamp != Some(stamp)`.
The asserted precondition is therefore precisely the left-hand input of the production invalidation
predicate, not a proxy for it.

## The precondition cannot hide a real adapter failure

Four independent reasons, each checked rather than assumed.

1. **The adapter is unchanged.** `table.rs` and `sys.rs` are the same bytes at old base, PR base, PR
   head and current `main`. This PR cannot alter adapter behaviour, so it cannot convert an adapter
   failure into a pass.
2. **The precondition is asserted, never assumed.** `assert_ne!(cached, changed, ...)` runs before the
   listing resumes and fails loudly if the stamp did not move. A host that cannot make the change
   observable gets a failure, not a skip.
3. **The real mutation still happens first.** The external `write("e")` and `remove_file("a")` are
   unchanged and still precede the forced mtime write. The patch adds visibility, it does not
   substitute a synthetic event for the real one.
4. **Detection power is unaffected by the forced stamp.** With a no-op invalidator the stamp value is
   irrelevant, because the comparison that consumes it is gone. The mutant therefore still serves the
   stale cached listing and still fails the expected-entries assertion. Forcing the stamp cannot make
   the mutant pass.

The 118 test-portability fix and the 120 production gap stay separated: the test comment names #120 and
states that whether a same-tick external change must also be seen is a separate property.
#120 remains open and is not closed by this PR.

### The mutation is bounded and explicit, not a sleep

`force_observable_mtime` opens the directory and applies one `futimens`-equivalent write that sets
mtime to exactly one day before its current value, via
`File::set_times(FileTimes::new().set_modified(target))`.
It is a single bounded attempt followed by a hard assert, not a sleep, not a poll, and not an
unbounded loop.
Because the target is a fixed 86400 s in the past and the fixture directory was created moments
earlier, the write cannot be a no-op; if the filesystem rejects it, `.expect("set the directory mtime")`
panics rather than passing.

### Observed stamp movement, both hosts

| host | before | after | fields that moved |
| --- | --- | --- | --- |
| macOS APFS, this review | `(1791219182, 310955842, 1791219182, 310955842, 192, 6)` | `(1791132782, 0, 1791219182, 311232553, 192, 6)` | mtime back 86400, mtime nsec to 0, ctime nsec |
| macOS APFS, author | `(1791217778, 702781627, 1791217778, 702781627, 192, 6)` | `(1791131378, 0, 1791217778, 703390838, 192, 6)` | mtime back 86400, mtime nsec to 0, ctime nsec |
| Linux ext2/ext3 TMPDIR, author | `(1791217927, 467100813, 1791217927, 467100813, 4096, 2)` | `(1791131527, 0, 1791217927, 467100813, 4096, 2)` | mtime back 86400, mtime nsec to 0, ctime unchanged |

This review reproduces the author's macOS stamp movement exactly in shape and in the 86400 s delta.
On both hosts `size` is unchanged across the external create and removal, so only a timestamp
distinguishes the two states.
On the Linux filesystem the explicit write moved `mtime` but left `ctime` entirely unchanged, which is
a useful data point for #120: on that filesystem class `ctime` is not a usable change signal and only
`mtime` carries the change.

## Contract not weakened

Read from the shipped file at head, not from the diff alone.

- No `#[ignore]`, no `#[cfg]` gate, no early `return`, no `should_panic`.
- Every adapter call still uses `.expect(...)`; none is retried and none is made conditional.
- The paging loop is the original loop, unchanged.
- The expected-entries assertion still requires `[a, b, c, d, e]` and is reached; only its failure
  message gained the clause about the stamp.
- The fresh-from-the-start assertion still requires `[b, c, d, e]`, unchanged.
- The single change to an existing line in the whole patch is that message string: `-1 / +2`.
- The public test name is unchanged: `tests::readdir_sees_a_name_created_outside_the_vfs_while_a_listing_is_paged`.

Two cosmetic asymmetries, neither affecting validity: the helper types the nanosecond fields as `i64`
while `table.rs` uses `u32`, and the helper reads with `symlink_metadata` while `table.rs` uses
`sys::fstat` on an open directory descriptor. The compared values are identical and the assertion only
compares the tuple against itself, and the fixture is a real directory, not a symlink.

## Independent macOS sample, run by this review

One foreground invocation through the documented
`.treehouse-ready-wave/mac-heavy.lock` recipe, which is `fcntl.flock` based, so the pre-existing
0-byte lock file does not itself mean held and no holder process existed.
Command shape, dispatch doc lines 61 to 68.
Free disk 328 GiB against a 20 GiB floor.
No shared mounted resource, native APFS fixture only.
Fresh `git archive` of the exact head, so no copied target or mtime cache was seeded.
Isolated `CARGO_TARGET_DIR`, private `TMPDIR`, every cargo command `--locked`.
635 files extracted, then source binding re-verified inside the extracted tree before any run was
credited: `tests.rs` blob `3577e61a56cd` and sha256 `8d4fc0013c6d99af`, `table.rs` blob `d61ca4226acc`
and sha256 `e6a75bb0186188ca`, mutant marker count 0, stamp comparison present 1, `PRECONDITION` lines 1.
The run aborts if the target test is missing from `--list`.

| step | command | exit | result |
| --- | --- | --- | --- |
| list | `cargo test --locked -p cowfs-vfs-path --lib -- --list` | 0 | 33 tests listed, target present |
| 1 | `cargo test --locked -p cowfs-vfs-path --lib tests::readdir_sees_a_name_created_outside_the_vfs_while_a_listing_is_paged -- --exact --nocapture` | 0 | `1 passed; 0 failed; 0 ignored; 0 measured; 32 filtered out` |
| 2 | `cargo test --locked -p cowfs-vfs-path --lib` | 0 | `33 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out` |
| 3 | `cargo fmt --all -- --check` | 0 | clean, zero bytes of output |
| 4 | `cargo clippy --locked -p cowfs-vfs-path --all-targets -- -D warnings` | 0 | clean, no warnings, `-D warnings` in force |

Artifact footprint 0.12 GiB against the 8 GiB cap.
Host Darwin 25.6.0 arm64, rustc 1.99.0, cargo 1.99.0.
Logs at `bench/out/pathvfs118-final-critic/run.log` in the assigned lease.
This is a fresh run by this review and is not a carried result.

## Author receipts, verified rather than accepted

| claim | verification |
| --- | --- |
| `docs/verification/evidence/pathvfs118-delivery.md` sha256 `a5e36d49...` | recomputed at head and in the primary checkout, exact match |
| `pathvfs118-diagnosis.md` sha256 `fc73a654...` | recomputed in the primary checkout, exact match, unchanged |
| `pathvfs118-test-repair.md` sha256 `7eb5b1dc...` | recomputed in the primary checkout, exact match, unchanged |
| saved patch sha256 `efcd6f4e...` | recomputed, exact match, 3599 bytes, non-empty |
| mutant patch sha256 `aef69fa8...` | recomputed, exact match, touches `table.rs` only |
| Linux source archive sha256 `019ba52a...` | recomputed on `src-patched-78c5db5.tar.gz`, exact match |
| archive member `tests.rs` | extracted from the tarball, blob `3577e61a56cd`, sha256 `8d4fc0013c6d99af`, matching the branch |
| archive member `table.rs` and `sys.rs` | blobs `d61ca4226acc` and `e94ebf03015f`, the production bytes |
| mutant marker absent from the archive | 0 occurrences in the tarball's `tests.rs` |
| private no-op invalidator confined to the private variant | `src-mutant/table.rs` blob `de56bfeea9b0`, differing from production by exactly the removal of `n.listing_stamp != Some(stamp)`, carrying an explicit MUTANT marker; `src-mutant/sys.rs` identical to production; `src-mutant/tests.rs` identical to the shipping file |
| macOS binary identity | the archived test binary contains the new test name, the precondition message, the `set the directory mtime` string and the new final message; zero MUTANT markers and zero occurrences of the old message string |
| Linux receipts are raw | `linux-logs/run.log` shows `1 passed; 32 filtered out` and `SUITE_EXIT=0` with `33 passed`; `suite.log` shows `33 passed`; `fmt.log` and `list.err` are zero bytes; `clippy.log` ends at `Finished dev profile` with no warnings |

Linux carry accepted.
The raw Linux logs exist and their source binding is proven by the archive hash and the extracted
member blobs, so the author's Linux head proof is carried rather than re-run.
No fresh remote workload was needed and none was requested.

The macOS receipts are documentary rather than raw: the commands and their results are recorded in the
delivery record, and the compiled binary is proven to come from the shipping source, but there is no
archived macOS run log.
That gap is closed by the independent macOS sample above, which was run from a fresh archive of the
exact head.

## OLD / NEW / MUTANT matrix, carried on verified identities

No fresh matrix was run, and none is demanded, because every responsible blob identity holds:

| seam | old base `951045f` | saved tree that produced the matrix | this PR head |
| --- | --- | --- | --- |
| `tests.rs` | `90de21aa99f2` (OLD case) | `3577e61a56cd` (NEW and MUTANT) | `3577e61a56cd` |
| `table.rs` | `d61ca4226acc` | `d61ca4226acc` | `d61ca4226acc` |
| `sys.rs` | `e94ebf03015f` | `e94ebf03015f` | `e94ebf03015f` |

The file this PR ships is the exact file the NEW and MUTANT cases ran, so the mutant detection was
measured against the shipping bytes and does not depend on the old test.
Carried results: OLD exit 101, NEW exit 0 with a proven changed stamp, no-op invalidator MUTANT exit
101, plus the 33-test scoped suite, fmt and clippy.
These are carried numbers, reported as carried, and are not presented as a run performed by this
review.

The original no-op invalidator exists only as a private variant under the author's
`bench/out/pathvfs118/repair/` and is not in the PR, not in the branch, and not in the archive that was
built and tested.

## Integration against current main

Exact head into exact current `main`, no `FETCH_HEAD` and no working-tree state involved.

- `git merge-tree --write-tree dadc5241779c97e04ee5262824da6769dc0a6204 b7d1009d667c58bb13c5717bad32c981430b34d7` exits 0 with result tree `f801bf61480060b7e147caf14b74a516a9c8bce2` and reports no conflicting paths.
- PR base `00065ce` is an ancestor of `dadc524`, so the base is fast-forwardable and no rebase is needed.
- No `main` commit since the base touches `tests.rs`, `table.rs` or `sys.rs`, so the
  byte-identical-seam argument still holds against `dadc524` and not only against `00065ce`.
- The merge would introduce exactly the one test-file change; `tests.rs` at `dadc524` is still the old
  `90de21aa`.

No combined-tree runtime PASS is claimed.
Nothing was built or run at the merged tree, so this review asserts a clean merge and unchanged seams,
and nothing more.

## CI truth at this snapshot

One snapshot, taken once, of the check runs on the exact head `b7d1009d667c58bb13c5717bad32c981430b34d7`.
No polling, no rerun, no dispatch, no runner change, no workflow edit.

`total_count=3`

| check run | status | conclusion |
| --- | --- | --- |
| `check (ubuntu-latest)` | completed | success |
| `check (macos-latest)` | completed | success |
| `linux-fuse` | completed | success |

All three green at the moment of this snapshot, so this is a positive green observation, not a pending
one and not a zeros-means-unconfigured inference.
`ci.yml` at the exact head is active and unfiltered for pull requests: `on: push: branches: [main]` and
`on: pull_request` with no path or label filter, matrix `[ubuntu-latest, macos-latest]`, plus the
`linux-fuse` workflow. Checks are configured and ran.

Recorded precisely: the legacy combined commit-status API for this head returns
`state=pending total_count=0 statuses=0`, which is that API's default when no legacy status contexts
exist at all, not a pending check run. The check-runs API above is the authoritative Actions result.

## Closing references

Merging this PR will not close anything, which is the intended behaviour.

- GraphQL `closingIssuesReferences` for PR 131: `totalCount = 0`, empty node list.
- A regex scan of the whole PR body for a closing keyword adjacent to an issue number finds no match.
  The only such tokens are `Refs #118.` and `Refs #118, #120`.
- `#118` otherwise appears as "Delivers the already-prepared test-only patch for #118" and `#120` as
  "stays open in #120", "does not fix it, does not claim to".
- Issue #118 is OPEN and issue #120 is OPEN, confirmed by direct API read.

## Finding, non-blocking

`docs/verification/evidence/pathvfs118-delivery.md`, lines 149 to 151, contradicts itself and
misdescribes the shipped code.

Line 149 and 150 read "the test performs extra external mutations until the stamp actually changes,
then asserts it, rather than polling one unchanged event", and then line 151 in the same paragraph
states "there is no arbitrary sleep, **no retry loop**".

The shipped code does one explicit bounded mutation, `force_observable_mtime`, and then one hard
`assert_ne!`. There is no loop and no iteration. The PR body describes the same code correctly with
"no sleep or retry was added", so the delivery record is the outlier.

This is a documentation-accuracy defect, not a validity defect.
The code is stronger than the record claims, because a single deterministic write either moves the
stamp or panics, whereas an iterative loop could in principle exit without reaching the assertion.
It does not affect any test outcome, any production behaviour, or the merge decision, and it is
recorded here rather than edited because this review owns no source path in the branch.

Suggested wording for the next revision of that paragraph, for the author or the merger to apply:

> the test performs one extra external mutation, an explicit write of a directory mtime a day into the
> past, which either moves the stamp or fails loudly, and then asserts the stamp actually changed.
> There is no sleep, no retry loop, no `#[ignore]`, no `cfg` gate, no skipped assertion and no
> threshold weakening.

## Explicitly not claimed

- No repair of the #120 same-tick coherence gap, which stays open.
- No full POSIX, kernel, mount or production acceptance claim.
- No combined-tree runtime PASS at the merged tree; only a clean `merge-tree` and unchanged seams.
- No fresh OLD/NEW/MUTANT matrix; the carried numbers are labelled carried and were not re-run.
- No fresh Linux workload; the author's Linux head proof was carried on verified raw logs and proven
  source binding.
- No evidence that the author's macOS receipts were re-run by this author; the independent macOS
  sample in this report is this review's own run.

## Verdict

**PASS. Merge PR 131 at head `b7d1009d667c58bb13c5717bad32c981430b34d7` into current `main`.**

The delivery is test-only, its precondition is bound to the adapter's own six-field cache key and
asserted rather than assumed, the adapter bytes are unchanged across four revisions, the contract is
not weakened, CI is green on the exact head, the merge is clean against current `main`, and nothing is
auto-closed by merging.
The one finding is a wording contradiction inside the delivery record, worth correcting in the same
change window but not worth blocking on.

This review stops here.
No merge, no next issue.
