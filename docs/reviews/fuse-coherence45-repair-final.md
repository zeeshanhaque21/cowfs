# Delta review: PR #105 repair `06f0695` against my BLOCK at `45fcebf`

Reviewer: ready-wave slot 13, lease `748aec3c71201afdaeb690847dfbab10`, branch `review/fuse-coherence-45`.
Prior review of this PR: [`fuse-coherence45-final.md`](fuse-coherence45-final.md), sha256 `0f666319fcd440f758554fa560a97cf17fa5dc8eb8c154c30566cf287ec0686f`, left untouched.
Reviewed head: `06f06952944a3db82d13baed66cdb0d672f83f75`.
Prior blocked head: `45fcebff5eabdbe97766659cb0c929571f08b17f`.

Verdict: **PASS on both prior blockers**, independently proven, with three documentation-accuracy defects that should be corrected before merge.
No blocker remains.
No production or test-code semantics changed, so my earlier source-level proof carries forward unchanged.

## 1. Provenance

`45fcebf` is an ancestor of `06f0695`, verified with `git merge-base --is-ancestor`.
Exactly one commit separates them.
I fetched the new head with `git fetch --no-tags origin 06f0695...` and read it with `git show` and `git diff`.
No reset, no stash, no force, no checkout of the new head, no source edit, no commit, no push, no merge, no lease return.
My worktree HEAD stayed at `45fcebf` for the whole delta review, and the working tree carries only my two untracked report files.

I could not move HEAD to the new commit without violating the no-reset constraint, so I built the new source from `git archive` of the exact blob into a private immutable fixture at `/home/moonscape/cowfs-ready-wave/task-45-final-review/`, separate from the builder's `task-45-repair` and from my earlier `task-45-review`.

| item | value |
| --- | --- |
| reviewed head | `06f06952944a3db82d13baed66cdb0d672f83f75` |
| prior blocked head | `45fcebff5eabdbe97766659cb0c929571f08b17f` |
| branch base | `46b0f269d5bef4a2c204c25f5b3015da601d3beb` |
| `origin/main` | `724f81c1731bf409497f7062574462df2ca0045b` |
| merge-base with main | `46b0f269d5bef4a2c204c25f5b3015da601d3beb`, branch is 5 commits behind main |
| committed `Cargo.lock` | `0b47cb02a7fe7f6bdf0447286e512d43fae04201df8f3a7e93e97833d45696b9` |
| committed `coherence.rs` | `559147ca8708e6160dbf879895cd1b725fd41cf0f8c9b02464060b393e2604ed` |
| `conformance.rs`, base and head | `7c0c72fc3cd7e9b9299e0c4063474ad8cdb2ca67d98f60bbe4d06a7679b92213`, byte-identical |
| my binary at this head | `coherence-4ae8afeea6fae250`, sha256 `f6505d657a59d15e61fe7f72a77ecbe9da24c3fb919523fb4a0d942296e856e3` |

Builder's canonical documents, in the primary checkout, verified byte-identical to the committed blobs at `06f0695`:

| document | file sha256 | git blob id | bytes match committed |
| --- | --- | --- | --- |
| `docs/verification/ready-45.md` | `9e629bdf9c740c11...` | `499bf2c9211831de0b07130cc19e367b30209878` | yes |
| `docs/verification/evidence/coherence45-repair.md` | `2df1d66f6cc91445...` | `d266d9ef6d9136cf22a90b7d3cd815323a5d040b` | yes |

The file sha256 values and the git blob ids are different numbers for the same bytes because they are different algorithms, sha256 against git's sha1.
The published blob prefixes `499bf2c9` and `d266d9ef` both match what `git rev-parse 06f0695:<path>` returns.
Both canonical documents are therefore published and reviewable, which was the point of the F5 fix.

## 2. The diff is exactly what it claims, and it is comment-only in the test

Four files, and no others:

```
Cargo.lock                                       |  1 +
crates/cowfs-fuse/tests/coherence.rs             | 11 +-
docs/verification/evidence/coherence45-repair.md | 211 ++++++++++++
docs/verification/ready-45.md                    | 411 ++++++++++++++---------
```

**No `crates/*/src/` file is touched anywhere in the diff.** Zero production code changed.

The `coherence.rs` change is **comment-only**, proven rather than asserted.
Stripping every `//` and `//!` line and every blank line from both revisions leaves byte-identical bodies:

```
OLD code-only sha256 : 8f505571542a84b75b6c4d7f17af7430a1f56f4ff69f7dc042603f232adac829
NEW code-only sha256 : 8f505571542a84b75b6c4d7f17af7430a1f56f4ff69f7dc042603f232adac829
CODE BODIES IDENTICAL: true   (8753 bytes each)
```

Both revisions have exactly 4 `#[test]` functions and exactly **0** real `#[ignore]` attributes.
The one `#[ignore]` string in the new file is inside the explanatory doc comment, not an attribute.
`conformance.rs` is byte-identical to base, so the pre-existing `fd5ec1f` skip is untouched and this PR adds no skip, no retry and no serialization.

Because the code bodies are identical, the `Cargo.lock` addition cannot change semantics: it adds a dev-dependency edge to a package that was already compiled for the test.
My earlier source-level findings therefore carry over without re-running them: the flush-threshold proof via public `Stats.dirty_bytes` going 2048 then 0 across 16 full-block writes, mount-versus-`Core` byte equality, correctness after dropping both mount and `Core` and reopening the store, and the mutation firing with `block 0 is torn at rest with no writer running: it holds 34 and [45, 45, 45, 45]`.

## 3. F1, the blocking lockfile defect: FIXED, and proven

The `Cargo.lock` delta is exactly one added line inside the `cowfs-fuse` dependency list, with no version move anywhere:

```
@@ -295,6 +295,7 @@ dependencies = [
 name = "cowfs-fuse"
 version = "0.0.0"
 dependencies = [
+ "cowfs-core",
```

Committed lock hash moved `2ed20c88...` to `0b47cb02...`.

I recorded the fixture's `Cargo.lock` hash **before any cargo command ran**, then ran the `--locked` commands first, so no rebuild could rewrite the lock and hide the defect:

```
PRE_ANY_CARGO_Cargo.lock = 0b47cb02a7fe7f6bdf0447286e512d43fae04201df8f3a7e93e97833d45696b9
F1a_metadata_locked_rc=0        lock_after_metadata=0b47cb02...
F1b_check_locked_rc=0           (no lock error)
                                lock_after_check=0b47cb02...
```

Both commands exit 0 on the clean published source, and the lock hash is identical before and after, so nothing was regenerated behind the measurement.
This is the opposite of the old behaviour, where both exited 101 with `cannot update the lock file ... because --locked was passed to prevent this`.
My prior BLOCK's F1 evidence stands as the "before"; the numbers above are the "after", taken on Linux at this exact head.

Mergeability, checked without integrating anything: `origin/main` is `724f81c`, `main`'s `Cargo.lock` does not yet list `cowfs-core` under `cowfs-fuse`, and a `--dry-run` patch of the one-line delta onto main's lock applies cleanly.
So the repair should not create a `Cargo.lock` conflict at merge time.

## 4. F2, the false-PASS command: FIXED in the docs, verified at this head

The `--ignored` flag is gone from the test's own doc comment, and the comment now warns that adding it filters all four out:

```
//! Run: `cargo test -p cowfs-fuse -j4 --test coherence -- --nocapture --test-threads=1`
//! No `--ignored`: none of these tests is `#[ignore]`d, and adding that flag filters all four out,
//! so it prints `running 0 tests` and still exits 0. Assert `running 4 tests` before believing a
//! green result from this target.
```

I ran both forms against a binary built at this exact head, built from `coherence.rs` hash `559147ca...`:

```
--- OLD command, verbatim ---
B1_ignored_rc=0
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.00s

--- CORRECTED command ---
B2_corrected_rc=0
running 4 tests
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 101.80s
```

All four test names appear in the corrected run's output: `core_view_keeps_aligned_4k_blocks_atomic_while_flushing`, `concurrent_writers_leave_every_block_uniform_and_acknowledged`, `mount_and_native_agree_on_acknowledged_write_visibility`, `native_tmpfs_torn_count_is_reported_for_comparison`.

This closes a real gap in the repair's own evidence.
The builder's before/after block ran on `coherence-a198afafd2e72431`, which the evidence file itself labels as the **first commit `45fcebf`** binary, while labelling `26199e373705ff...` as this revision's.
So the "corrected command passes" half of their F2 demonstration was inherited from the old binary, not run at this head.
It is logically sufficient, because the code bodies are byte-identical, but it was not measured at this revision.
I have now measured it at this revision, on Linux, and it is 4 of 4 passing.

That run also shows the fixture's 20 s cap is real: the mount arm reported `4939 writes` rather than the usual 6000, with 6000 reads and 0 torn, because writers stopped at the cap while readers finished.

## 5. Lint: the substance is right, the printed command does not reproduce

The evidence file claims, at line 173:

```
cargo clippy -p cowfs-fuse --all-targets -- -D warnings       -> rc 0 on the owned surface
```

Run verbatim at this head on Linux, it exits **101**:

```
A1_doc_cmd_rc=101
error: this `if` can be collapsed into the outer `match`
error: could not compile `cowfs-meta` (lib) due to 1 previous error
```

The reason is mechanical and worth stating, because the claim is about scoping.
`-p cowfs-fuse` restricts which package's *targets* are the subject, but the dependency closure is still built, and a trailing `-- -D warnings` applies to everything compiled, including workspace-member dependencies.
So that command is **not** restricted to the owned surface, and `cowfs-meta` fails it.
The command that genuinely expresses the intended claim is `--no-deps`:

```
A3_nodeps_dw_rc=0        cargo clippy -p cowfs-fuse --all-targets --no-deps -- -D warnings
A2_default_rc=0          cargo clippy -p cowfs-fuse --all-targets
A4_workspace_dw_rc=101   cargo clippy --workspace --all-targets -- -D warnings   -> tx.rs:314
```

So the underlying assertion is true: the owned surface is clean, and the only `-D warnings` failure is the single pre-existing `cowfs-meta/src/tx.rs:314` `collapsible_match`, unmodified at base, owned by the #42 lane, and a rustc/clippy 1.95.0 finding that does not fire on CI's pinned stable since main is green.
Only the printed command needs `--no-deps` added.
I am not reporting this as a false pass on the workspace gate, and I did not edit `tx.rs`.

## 6. Three documentation defects to correct before merge

### D1, the tracked-source identity table carries a stale `coherence.rs` hash

`ready-45.md:136`, under the heading "F4: source identity is the tracked revision", lists:

```
- `coherence.rs`: `88577bfdf0ba5c917c9ca3ea64e3f91170d1b908577cf1065d489bb3730dd213`
```

`evidence/coherence45-repair.md:21` repeats it in the table captioned "Tracked source state at this head".

The actual hash at `06f0695` is `559147ca8708e6160dbf879895cd1b725fd41cf0f8c9b02464060b393e2604ed`.
`88577bfd...` is the **previous** commit's hash.
This is the same class of error F4 was raised about, reintroduced in the opposite direction: the table meant to be the authoritative record of tracked state is wrong about the one file this commit actually modified.
A reviewer using that table to confirm "the test file is unchanged" would be misled into thinking the file did not change at all, when its comments did.

The correct fix is to record `559147ca...` as the tracked hash and add that the code body is byte-identical to `88577bfd...`, comments only.
That single sentence makes the point the table currently obscures.

The three other occurrences of `88577bfd...` at `ready-45.md:93` and `evidence:149,156` are the mutation baseline and restored hashes.
Those are correct for a run performed at `45fcebf`, but they are not labelled with the commit they ran at, which is what makes them look like current-state claims.
Label them.

### D2, the "rc 0 on the owned surface" lint line needs `--no-deps`

Covered in section 5.
One word.

### D3, `ready-45.md` has a duplicate experiment row that invites double-counting

`ready-45.md:205` and `:206` are the same experiment listed twice:

```
| repository's own check body, unskipped, builder | tmpfs | `FAIL` line count, no read counter exists | 40 | 11 reps tore, 7 distinct offsets |
| repository's own check body, unskipped, builder | tmpfs | same                                            | 40 | reported alongside the above      |
```

The second row adds no data, and a reader scanning the table can read it as two independent 40-rep tmpfs experiments, which would inflate the apparent sample to 80 reps.
The compact evidence file's equivalent table does not have this duplication, so it is an editing slip in the long document only.
Delete the second row.

None of D1, D2 or D3 affects the correctness of the code, the validity of the scoped conclusion, or any safety property.
They are a stale number, a command missing a flag, and a duplicated row.

## 7. What the repair got right, and the limits it now states honestly

**F3, the inferred rate: fixed.** The "about 1 in 3000 reads" claim is withdrawn in both documents.
The reasoning is stated correctly: the repository's own check at `crates/cowfs-vfs-test/src/conformance/concurrency.rs:109` returns `Ok` or `Err` and has no read counter, and its per-rep read count is time-capped, so a rep count from it can never be a rate.
Each figure is now attributed to its own experiment, and the two counting-harness results are kept apart and explicitly not combined:

| experiment | filesystem | reps | reads | tears | stated as |
| --- | --- | --- | --- | --- | --- |
| this revision | tmpfs | 20 | 120000 | 2 | about 1 in 60000 |
| independent reviewer | tmpfs | 20 | 120000 | 17 | about 1 in 7059 |

The documents say plainly that these disagree, that they are separate samples of a low-rate probabilistic event on a shared 4-core host, and that neither is a general rate.
The rep-level counts are also kept distinct from read rates: 11 of 40 tmpfs and 17 of 40 ext4 for the repository check, against 6 reps with 3 tears at this revision.
That is the correct handling, and it matches my own measurements.

**F4, the build side effect: fixed in substance.** The document now states that the earlier `0b47cb02...` was the lock after cargo rewrote it, that it changed on every build, and that the committed lock is now legitimately part of source identity.
Binary digests are now explicitly labelled "Build-specific digests, not reproducible proof of a source revision" and each is attributed to the build it came from, which is the right framing given that my own two builds of the same code produced different digests.
Only the D1 stale hash remains.

**F5, unreachable evidence: fixed.** `docs/verification/evidence/coherence45-repair.md` is now tracked, published, and byte-identical to the committed blob.
It also discloses correctly that the raw per-rep logs and build logs live only on the Linux host under `/home/moonscape/cowfs-ready-wave/task-45-repair/` and the builder's earlier `task-45/out/`, that `.gitignore:10` ignores `/bench/out/`, and that those paths are not reachable from a PR.
That is the honest framing: the tracked file is a curated observation copy, and nothing pretends the raw logs are public.
I did not download the raw log set, which the brief describes as roughly 395 MiB; I derived only the specific figures I needed to check, from the tracked documents and from my own runs.

**F7, the oracle's true strength: now stated explicitly.** The new section "What this oracle does not establish" says the oracle is a membership test and not last-write-wins, that a lost final acknowledged write leaving an older acknowledged value would still pass, that excluding `PREFILL` catches only a block that received no write at all, and that therefore no zero-data-loss, no-lost-write or durability claim is made.
It routes crash and power-loss durability to #96 and to `docs/design.md`'s separate criterion.
This is exactly the correction I asked for, and it is stronger than my own phrasing because it is now in the branch's own document.

**F8, the lying exit code: documented with evidence.** Six reps of the repository's own check on native `ext4`, all six exiting 0, three of them printing a real tear, with the printed offsets shown and one of them matching the original CI report's offset class.
The document states the consequence plainly: any conclusion drawn from that harness's exit code alone is wrong, and every native figure is derived from parsed `FAIL` lines while every counting-harness figure comes from the test's own printed counters.
That matches what I hit myself, where my first pass mis-classified all 40 reps as passes because I trusted the exit code.

**The verdict is scoped correctly.** The document opens with "Verdict, scoped", says the cowfs arms tested did not tear, and immediately adds that this is not a claim that no cowfs production defect exists anywhere and not a claim about macOS, `ext4` durability or any other lane.
That is the wording my prior review required, and it is not weakened anywhere later in the document.

**Closure wording is correct.** The document supplies wording for the coordinator, explicitly not "defect found and fixed", on the grounds that there was no cowfs defect and no production change, and says to keep #45 open until merge.
I agree with that wording and add one constraint: if the coordinator closes #45, it should record the scoped native page-cache cause only.
It should not be recorded as a runtime bug fixed, and it should not be read as exonerating every cowfs concurrency or flush path.
Issue #21's index-integrity work remains open and is not part of #45.

## 8. Two residual limits worth carrying forward, neither a blocker

**The "guard" is documentation, not an enforced check.** There is no reusable verifier in the branch.
A search across all shell and Python files for `VERDICT=` or `running 4 tests` returns nothing, and the repair commit adds no script.
The `VERDICT=FAIL` and `VERDICT=PASS` lines in both documents are literal text, and the `argv:` lines are transcripts, not a tool someone can run.
To the builder's credit, neither document claims CI enforcement; both phrase it as an instruction to assert `running 4 tests` before trusting a green result, which is honest operator guidance.
But the underlying trap is unchanged and still live: I re-confirmed at this head that `--ignored` yields `running 0 tests` and exit 0.
A future edit that reintroduces the flag would again produce a silent green.
If a real guard is wanted, it needs a committed check that asserts the executed test count, or a CI step that does; a comment cannot do that.
Absent that, the correct description is "documented, operator-checked", and it should not be described as enforced.

**CI's real gate for these tests is the `check` job, not `linux-fuse`.** `ci.yml:23` runs `cargo test --workspace` on `ubuntu-latest` with no pipeline, and the four tests are neither `#[ignore]`d nor non-Linux, so they execute there and a failure fails the job.
The `linux-fuse` job pipes `cargo test` into `tee` under GitHub's default `bash -e`, which has no `pipefail`, and its enforcement step greps only for `^FAIL`, while a Rust-level test failure prints `test ... FAILED`.
So a coherence failure would not fail the `linux-fuse` job.
This is pre-existing, unchanged by this PR, and covered by the `check` job.
I did not execute a deliberately failing PR through CI to confirm that end to end, because workflow dispatch and reruns are forbidden here, so treat it as a reading of the workflow text.

## 9. CI at exactly `06f0695`, one snapshot

```
run id        37248979549
head_sha      06f06952944a3db82d13baed66cdb0d672f83f75
conclusion    success
created_at    2026-10-05T00:49:14Z
jobs          linux-fuse            completed success
              check (macos-latest)  completed success
              check (ubuntu-latest) completed success
pr checks     3 passed, 0 failed, 3 total
```

This is the run for the new head.
The earlier green run `37246373869` belongs to `45fcebf` and is not evidence for this delta; I did not treat it as such.
No workflow was dispatched, rerun or polled, and no runner configuration was touched.

## 10. Environment and interruption accounting, unchanged and preserved

Pid `987929` is still in uninterruptible sleep, `wchan request_wait_answer`, command `[coherence-a198a]`, now at 4684 s elapsed.
It has never resolved and I did not signal it, did not clear it, and did not walk or repair anything around it.
No cleanup signal of any kind was sent.
Mount acceptance rep 7 remains recorded `rc=143`, excluded from the pass count, with its row preserved rather than deleted; the evidence file carries it as `rep=7 rc=143 torn_concurrent=? torn_at_rest=?`, which is the correct way to record an interrupted rep that contributed no data.

The three foreign mounts, `task-g5/mnt`, `task-g4/private/mnt` and `task-g4-review/private/mnt`, are all present and were never touched.
The g5 daemon, pid `899604`, is alive at 6651 s and untouched.
Both of my lanes left no mount and no running process behind, and the new fixture is 1.1 GiB, well under the 8 GiB cap, with 55 GiB free, above the 20 GiB floor.
All heavy work ran as one foreground child through the shared lock `/home/moonscape/cowfs-ready-wave/linux-heavy.lock`, which I acquired on every run.

One observation to pass to the coordinator, not a finding against this PR.
`docs/ready-wave-dispatch.md:32` names a shared daemon at pid `15263` and instructs that it never be signalled or restarted.
That pid is **absent** from the host now.
I did not signal it, did not restart it, and did not touch any store, mount or socket.
Something outside this review ended or replaced it.
Whoever tracks that shared daemon should confirm its current identity, because a dispatch instruction pointing at a pid that no longer exists will mislead the next worker.

## 11. Recommendation

Merge after three one-line documentation corrections.
None requires a code change, a re-measurement, or another reviewer.

1. Replace the stale `coherence.rs` hash `88577bfd...` with `559147ca...` at `ready-45.md:136` and `evidence:21`, and add that the code body is byte-identical to the old hash, comments only.
   Label the three mutation-run occurrences with the commit they ran at.
2. Add `--no-deps` to the owned-surface clippy command at `evidence:173` and `ready-45.md:268`, so the printed command produces the rc 0 it claims.
3. Delete the duplicate builder/tmpfs/40-rep row at `ready-45.md:206`.

Optionally, and only if a real guard is wanted rather than a documented one: commit a check that asserts the executed test count for this target, so the `--ignored` trap cannot silently return.
If that is out of scope now, then describe the guard as documented and operator-checked, which is what the documents already do.

What I did not retire, unchanged from my prior review: the at-rest invariant is proven on `tmpfs` and `ext4` at 4 KiB granularity with 3 writers and 3 readers on one 4-core host, probabilistically, so absence of a tear in a sample is not proof of absence in general.
No last-write-wins, zero-data-loss, crash, power-loss or `fsync` durability property is claimed or established, and the documents now say so themselves.
Durability remains owned by #96.