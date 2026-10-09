# PR 130 final independent review: gc dry-run mark-state 122

Scope: final independent review of PR 130 only, at head `c4ed9d7ce3a36f59ec3f58a8167fb110b4b3550e`, base `00065ce75dcd554e1fb4bb084d1c70b2e2a21a87`.
Verdict: scoped PASS.
No critical finding, no test-vacuity defect, no production change.

Reviewer lease: `.treehouse-build-train` slot 6, branch `review/gc-root-mark-retention-82`, held by
`cowfs-gc-retention-82-critic`.
No source, checkout, branch, reset, stash, commit, push, merge, lease return, or lease acquire was performed.

## What was reviewed

Two commits, two files, zero production bytes:

| Commit | Message |
| --- | --- |
| `8a8cee02e3591f30f76b990a06794e84d4fcdad9` | `test(gc): a dry run leaves the recorded set byte-identical` |
| `c4ed9d7ce3a36f59ec3f58a8167fb110b4b3550e` | `docs(gc): the 122 dry-run mark-state repair and its mutation control` |

```
M crates/cowfs-gc/tests/control.rs
A docs/verification/evidence/gc-dry-run122-repair.md
```

`git diff --name-only <base> <head> -- 'crates/*/src/*'` returns **0 files**.
`crates/cowfs-gc/src/lib.rs` is byte-identical between base and head:
`sha256 e4b687ad679d2c0982d88eadcb81bb10704c48d573cd25bcfd8d8819c28308f2` at both.
`crates/cowfs-gc/tests/common/mod.rs` is untouched, so the shared fixture is not modified to suit the new tests.
No `Co-Authored-By` or other agent trailer appears in either commit.

The reviewed test source is `crates/cowfs-gc/tests/control.rs` at
`sha256 d22e00861e91154d74dd16191f815e7dca5fd7b9107e99258a3c2c535f6b62cd`, identical in the PR blob, in the author's worktree, and in my own pristine archive.
The canonical evidence document is at `docs/verification/evidence/gc-dry-run122-repair.md`, not under `docs/reviews/`, and its SHA matches the one I was given:
`sha256 f47cdeb26f409960e9a212f7e07edfdfa5903ddcf7c31a730b82901bb3c05601`, byte-identical in the PR blob and in the primary checkout.

## The corrected premise holds

The original assertion was a tautology, not a cancellation check:

```rust
assert!(!f.gc_dir().join("mark.bin").exists() || true);
```

`|| true` accepts every state, so it rejected nothing.

The corrected premise, which I confirmed against the real collector rather than a truth table, is:

1. A fresh fixture has no `mark.bin` at all.
2. A dry run over a fresh fixture writes none.
3. A real cycle legitimately records a set. On the author's own fixture that is **120 bytes**.
4. A second dry run over that non-empty set reads it (`marked_skipped_roots == 1`) and leaves those 120 bytes byte-identical.

Measured with the byte-identical `body()` and `garbage()` from the reviewed file:

```
P2 fresh_fixture marks = None
P2 first_dryrun marks = None skipped_roots=0
P2 after_real_collect marks = 120 bytes (skipped=0, freed=159132, dry_run=false)
P2 AUTHOR_CLAIM_120_MATCHES = true
P2 second_dryrun marks = Some(120) skipped_roots=1
```

The author's 120-byte figure reproduces exactly.
A second probe with independent fixture content recorded 88 bytes instead, which is expected: the recorded size depends on how many blocks the cycle condemns, and the point is only that the set is present, non-empty, read, and unchanged.

The assertion is non-vacuous in the direction that matters.
`marked_skipped_roots == 1` is discriminating, not constant: the same dry run on a store with no recorded set reports `marked_skipped_roots == 0`, and the recorded set begins with the `COWSMARK3` magic and a real root/block table, so the value comes from collector state rather than from a fixed number.

Both new tests use the public `Gc::open` and `Gc::collect` over a real `Fixture`.
There is no mocked mark path and no injected file path anywhere in the two tests.

## Production early return is correct and behaviour-preserving

`Gc::finish` in `crates/cowfs-gc/src/lib.rs` returns immediately on `self.opts.dry_run`, before `flush_hints` and before `marks.save(&dead)`.
That is the single guard between a dry run and writing the recorded set, and it is correct: a dry run must not consume or overwrite the incremental set a real cycle built.
This PR does not touch it, so the guard's correctness is demonstrated by the tests rather than changed by them.

## Mutation control: the guard is load-bearing

I ran the author's exact mutation myself, because a raw artifact in someone else's tree is not reproducible evidence.

The mutant removes the `dry_run` early return from `Gc::finish` and nothing else:

```
lib.rs head sha256   = e4b687ad679d2c0982d88eadcb81bb10704c48d573cd25bcfd8d8819c28308f2
lib.rs mutant sha256 = 311c7c1faaf2631fd9c625f7c99df4576f494b280b87434aee38201c646fff5f
removed lines        = 3
exactly the guard removal: True
```

The mutant was built from its own fresh `git archive` of the head with its own `CARGO_TARGET_DIR`, never copied from the healthy tree, so no cached artifact or seeded mtime could be mistaken for a rebuild.

Same mutant, same library, two different control files:

| Control file | Result |
| --- | --- |
| Old tautological, restored from base `00065ce` | `10 passed; 0 failed` |
| New reviewed | `FAILED. 10 passed; 1 failed` |

The new guard rejects the mutant and the old one does not.
The failure is the collector-state assertion itself, not a pack-byte or report guard:

```
panicked at crates/cowfs-gc/tests/control.rs:81:5:
assertion `left == right` failed: a dry run leaves the recorded set exactly as it found it
  left: Some([67, 79, 77, 83, 77, 65, 82, 75, 51, ...])
 right: None
```

`left` decodes to `COWSMARK3`, which is exactly the recorded-set bytes the mutant's dry run wrongly wrote into a store that had none.
So the guard is real and it bites on the property it claims to cover.

## Runs

All heavy work ran as one foreground child under the shared `/Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/mac-heavy.lock`, acquired with a 600-second bound and exit 75 rather than running unlocked.
Every wait loop also exits on failure and on a 300-second no-progress condition.
`TMPDIR` pointed inside my owned fixtures directory.
The lock was free with no holders before I started, and acquired in 0.0 s each time.
Disk was 329 GiB free at the start, 328 GiB at the end.
Owned artifacts: 999 MiB, inside the 8 GiB allowance.

Healthy lane, exact samples before the full scoped run, as required:

```
cargo +stable test -p cowfs-gc --test control -- --exact a_dry_run_changes_nothing_and_still_reports
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 10 filtered out

cargo +stable test -p cowfs-gc --test control -- --exact a_dry_run_does_not_consume_a_recorded_set
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 10 filtered out
```

Full scoped suite, on the pristine archive after removing my two probe files:

```
cargo +stable test -p cowfs-gc --test control
running 11 tests
test the_hint_cap_drops_hints_and_says_so ... ok
test a_dry_run_changes_nothing_and_still_reports ... ok
test a_cancel_mid_cycle_leaves_the_store_consistent ... ok
test a_cancel_stops_the_copy_loop_but_finishes_what_it_copied ... ok
test a_dry_run_does_not_consume_a_recorded_set ... ok
test progress_is_streamed_and_ends_at_the_candidate_count ... ok
test a_cancel_stops_the_cycle_and_is_reported ... ok
test a_removed_snapshot_reclaims_exactly_the_reported_bytes ... ok
test a_read_never_writes_and_hints_flush_in_batches ... ok
test a_store_with_corruption_is_refused ... ok
test the_io_budget_bounds_a_cycle ... ok
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.46s
```

Exact counts: 11 tests in `control.rs` at head, 11 passed, 0 failed, 0 ignored.
The base file has 10.
Linters on the same pristine tree:

| Step | rc | Note |
| --- | --- | --- |
| `cargo +stable fmt --all -- --check` | 0 | clean |
| `cargo +stable clippy -p cowfs-gc --tests -- -D warnings` | 0 | clean, `Finished dev profile in 0.14s` |

No ignored test is treated as acceptance.
The author's repeat-11 claim is verified independently, not taken on trust.

## CI truth, one snapshot, no polling

The author's "no checks configured" claim is **contradicted**.
The repository has an active workflow, and checks exist and have run on this head.

`GET /repos/zeeshanhaque21/cowfs/actions/workflows` returns one workflow, `ci` at `.github/workflows/ci.yml`, state `active`.
Its triggers are `push: branches: [main]` and a bare `pull_request:` with **no filters**, so this PR is in scope for it.
It runs `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and the bench unit tests on an ubuntu and a macos matrix.

`GET /commits/c4ed9d7.../check-runs` returns `total_count: 6`, at two snapshots about four minutes apart, with no polling and no rerun:

```
check (ubuntu-latest) | completed | success | 2026-10-05T16:32:24Z
check (macos-latest)  | in_progress | null   | null
linux-fuse            | completed | success | 2026-10-05T16:26:43Z
check (ubuntu-latest) | completed | success | 2026-10-05T16:31:22Z
linux-fuse            | completed | success | 2026-10-05T16:24:58Z
check (macos-latest)  | in_progress | null   | null
```

The combined commit status is `pending` with `total_statuses: 0`.
`mergeable` is `MERGEABLE`; `mergeStateStatus` is `UNSTABLE`, which is consistent with those runs still in progress.
PR metadata: state `OPEN`, draft `false`, base `main` at `00065ce75dcd554e1fb4bb084d1c70b2e2a21a87`, head `c4ed9d7ce3a36f59ec3f58a8167fb110b4b3550e`.

Accurate statement: checks are configured, four completed successfully, and two macos-latest runs were still in progress at both snapshots.
That is a pending state I observed, not an absence of configuration, and not a waiver.
I did not investigate why those runs were still open, did not poll, did not rerun, did not dispatch a workflow, and did not touch runner configuration.
The coordinator owns that read-only investigation.

## PR body and refs

The body is neutral on the tracker item.
It ends `Refs #122`, which is deliberately non-closing.

`closingIssuesReferences` returns `{"nodes": []}`, so nothing closes automatically.
A keyword scan for `fixes #`, `closes #`, and `resolves #` found no match in the body.
Issue 122 stays open until the coordinator decides otherwise.

The body also states the exclusions honestly: test-only scope, `crates/cowfs-gc/src/` untouched, `tests/common/mod.rs` untouched, no physical reclamation, no crash injection, no power cut, no real collector mount, no queued Core cancellation, no g6 or acceptance-criterion claim.

## Integration tree

```
git merge-base 00065ce75dcd554e1fb4bb084d1c70b2e2a21a87 c4ed9d7ce3a36f59ec3f58a8167fb110b4b3550e
00065ce75dcd554e1fb4bb084d1c70b2e2a21a87

git merge-tree --write-tree 00065ce75dcd554e1fb4bb084d1c70b2e2a21a87 c4ed9d7ce3a36f59ec3f58a8167fb110b4b3550e
916e3d13aea3ea0f83890e1e02a194e8ec85314a
rc=0
```

Base is the merge base and the merge is clean, computed against the explicit current `main` SHA rather than `FETCH_HEAD`.
Because the change is source-and-test only and touches no production byte, current `main` compatibility holds by construction, but I make **no combined-runtime PASS claim**: I did not build or run the merged tree, only the head archive.

## Not claimed

Physical reclamation, crash injection, power-loss, Core cancellation, and the g6 acceptance criteria are explicitly **not** claimed and are **not** new work from this PR.
I did not reopen the known flushing or pack bugs, and I did not change any GC implementation.
No known issue is treated as fixed by this test-only change.

## Recommendation

Accept PR 130 as a scoped test-hardening change.

The old assertion was a tautology that could not fail, its premise about the file being absent was wrong for any reused fixture, and the replacement is verified load-bearing against a minimal mutation while passing on the unmutated tree.
The production path is byte-identical, so there is no runtime risk to accept.
Merge is clean against current `main`.

Two items belong to the coordinator, not to me:
The two in-progress `check (macos-latest)` runs, read-only investigation only.
Any decision to close issue 122, which this PR deliberately leaves open.