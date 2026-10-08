# PR136 corrections delta, final independent review

Reviewed head: `57232166639ff09b4741719cf492df787d214a17`, branch `fix/deferred-operation-time-42`, one commit on the previously reviewed head.
Parent head: `cde59305fe05f4551468965cd648888eb98e9dbf`, verified to be an ancestor.
Base and current `main`, remote and local: `93cfef94457a989d031cb6b0a475ac4edbdb85ef`.
Author's claimed corrections-report digest: `5c3e172f0ea35715151c6b6a27c9fc76933dd4d25897fac280f1967a6b312625`, verified against the blob at the head.

Scope: the four changed paths, the runtime carry of the one production file, the accuracy of the one added test, the preservation of the existing assertions and receipts, and the PR record.
Read-only and source-only.
No cargo, no build, no lane lock, no archive extraction, no deletion.
No checkout, branch change, source edit, reset, stash, commit, push, merge, lease action or issue edit.

## Verdict

**PASS, scoped, with three findings, none blocking.**

The runtime carry is exact: the only production file changed is a doc comment, and stripping every comment from both revisions leaves byte-identical code.
The one added test asserts what the source does.
Every existing assertion and every receipt is preserved.
My earlier scoped proof carries forward only as far as the code is unchanged, which the mechanical comparison confirms.

CI at this head is **not green**: one check completed and succeeded, one is in progress, one is queued.
That is recorded, not waived, and nothing was polled, rerun, dispatched or reconfigured.

The cache-layer defect is **still queued and still open**.
This delta does not touch it, does not claim it, and the PR body still needs to record it as a separate obligation before merge.

| item | result |
| --- | --- |
| production code changed | **comment bytes only**, comment-stripped code identical at 11,840 characters |
| non-comment changed lines in `tx.rs` | **0** of 7 |
| other runtime files touched | **none**: `inner.rs`, `io.rs`, `ns.rs`, `db.rs`, `types.rs`, `swap.rs`, store crate, `Cargo.toml`, `Cargo.lock` |
| core fixture | **untouched**, 4 tests at both heads |
| meta fixture | 5 tests to 6, additive, the new case asserts the created triple |
| existing assertions in the touched test | **all three preserved**, expressions identical, one message reworded |
| the added test's claim | **matches source**: `tx.rs:87-89` builds `atime`, `mtime`, `ctime` all from `self.now` |
| corrections report digest | **verified**, `5c3e172f…` |
| `c4999d45…` mirror at the head | **byte-identical** to the primary checkout copy |
| my clarification `965d9c73…` | **uncommitted**, never in any commit, so no GitHub link resolves for it |
| CI at the exact head | **not green**: `linux-fuse` success, `check (ubuntu-latest)` in progress, `check (macos-latest)` queued |
| `closingIssuesReferences` | null |
| issue #42 | **open**, body digest unchanged at `67caa764…` |
| merge into current `main`, pinned SHAs, read-only | **clean**, tree `946a212f…`, exit 0 |

## Runtime carry, proved mechanically

Four paths changed. Classified by what they are, not by their location:

| path | class |
| --- | --- |
| `crates/cowfs-meta/src/tx.rs` | production source, comment-only |
| `crates/cowfs-meta/tests/operation_time.rs` | test |
| `docs/reviews/pr136-meta42-operation-time-final.md` | document, added mirror |
| `docs/verification/evidence/meta42-operation-time-review-corrections.md` | document, added erratum |

For `tx.rs` I compared the two revisions after removing `//`, `/* */` and doc comments while honouring string and character literals, then removed all whitespace, so neither formatting nor comment wording can hide a change.
Both sides reduce to **11,840 identical characters**.
Separately, of the 7 changed lines in the raw diff, **0 are non-comment lines**.
The old blob digest is `a92c3d76c573fb77`, which is exactly the `tx.rs` digest my first review recorded for `2a06ea9`, and `cde5930` did not touch that file.

That is what carries my earlier runtime evidence forward.
My first review's scoped suites, the 21 private cases, the lock-audit measurement and the 3-green CI at `cde5930` are evidence about code that is provably unchanged here.
They are **not** re-executed here and are not presented as such.

## The added test is accurate

`a_created_inode_takes_all_three_times_from_the_stamp` asserts `atime == T1`, `mtime == T1`, `ctime == T1` after `tx.set_now(T1); tx.create(...)`.
The source, read at the head:

```
tx.rs:87            atime: self.now,
tx.rs:88            mtime: self.now,
tx.rs:89            ctime: self.now,
```

inside `Tx::new_rec`, which every create reaches.
The assertions match the code exactly, and they pin precisely what my first review measured at the seam and what the old comment denied.

The comment's other new clause, that an explicit time in a `setattr` is never replaced including on a create, is source-accurate:

```
tx.rs:415        if let Some(t) = set.atime {
tx.rs:416            rec.atime = t;
tx.rs:418        if let Some(t) = set.mtime {
tx.rs:419            rec.mtime = t;
```

and `Tx::create` at `tx.rs:233` accepts no times at all, so a caller can only give an explicit time on a create by a later `setattr`, which is the path above.
One precision note, not a defect: the added test does not itself pin that override-on-create path, because `create` cannot carry an explicit time.
The clause rests on the two `setattr` assignments and on the pre-existing fixture `set_now_does_not_replace_an_explicit_atime_or_mtime`.
That is adequate, and I am recording it so the coverage is described accurately rather than generously.

## Existing assertions and receipts

The touched test keeps all three of its assertions with identical expressions.
Only the third assertion's message changed, from "ctime is the only field the stamp owns" to "ctime follows the stamp on an existing inode", and that is the sentence the correction is about.
The two time-preservation assertions are untouched.

The core fixture is byte-identical at both heads, 4 tests.
The meta fixture goes from 5 tests to 6, and the added one is a new function rather than a change to an existing case.

Receipts are all preserved.
The corrections report is at the head at the claimed digest.
The mirror of my first review is at the head and is byte-identical to the copy in the primary checkout at `c4999d45…`.
The two immutable evidence files are unchanged at `ed9609e2…` and `fbc6a078…`.
My clarification is unchanged at `965d9c73…`.

The author's own receipts are internally consistent with the source and with what I measured earlier, and I did not execute any of them:

| author's claim | consistency check, without building |
| --- | --- |
| the added test, `--exact`, 1 passed and 5 filtered out | the meta fixture has 6 tests at this head, so 1 of 6 selected is arithmetically right |
| `cargo test -p cowfs-meta --test operation_time`, 6 passed | matches the 6 tests present at this head |
| `cargo test -p cowfs-core --test operation_time`, 4 passed | the core fixture is untouched at 4 tests |
| `cargo test -p cowfs-meta --lib`, 16 passed | matches the 16 lib unit tests my first review measured |
| `cargo fmt --all -- --check`, exit 0 | not verified here, author's claim |
| `cargo clippy -p cowfs-meta --all-targets -D warnings`, exit 0 | not verified here, author's claim, and narrower than the two-crate clippy I ran |
| 643 of 645 tracked files identical to `cde5930` | correct: `cde5930` has 645 tracked files and the delta edits 2 of them, the other 2 changed paths are additions |

The erratum also labels the 12-suite, 18-target, 167-passed and lock-audit rows as the reviewer's numbers, taken from the review rather than inferred, and states that they are not a claim of whole-repository coverage.
That is the right attribution and I am not restating them as the author's execution.

## CI: recorded, not waived

One read-only snapshot at the exact head, and nothing else:

| check | status | conclusion |
| --- | --- | --- |
| `linux-fuse` | completed, success | passed |
| `check (ubuntu-latest)` | in progress | not green |
| `check (macos-latest)` | queued | not green |

`total_count` is 3.
The combined legacy status endpoint reports `pending` with zero legacy contexts, which is not itself informative.
`mergeable_state` is `unstable`, which is consistent with the in-progress and queued checks and is a change from `clean` at the previous head.

So the merge gate this delta needs is **not satisfied yet**, exactly as the author states in the body and in the corrections report: green CI at `cde5930` does not carry to this head.
No poll, no rerun, no dispatch, no workflow edit, no runner change, and no manufactured trigger commit.

## PR record, closing references and history

PR #136 is not a draft, base `main` at `93cfef9`, head `fix/deferred-operation-time-42` at `5723216`, 9 changed files, +1,499 and -1.
`closingIssuesReferences` is null, read again after the new commit.
The body ends with `Refs #42`, which is not a closing form.
The body states twice, in its own words, that #42 stays open and that this does not complete the umbrella, and it lists requests 1, 3 and 4 as untouched and not started.
Issue #42 is `open`, `labels: []`, and its body digest is unchanged at `67caa764…` over the same 36 lines.
The corrections report says the same and adds that the rename half, the hole flag and the reservation work are separate and not addressed.

One thing to record precisely, because it is a hazard rather than a defect today.
The new commit message ends with "Not merged here and nothing here closes #42."
That contains the literal closing form `closes #42` inside a negated sentence.
GitHub's own parser did not link it: `closingIssuesReferences` is null, read from the API after the commit.
So nothing closes today, and I am reporting the shape rather than pretending the wording is harmless.
Re-wording to "Not merged here; #42 remains open" would remove the hazard without changing the meaning.

## Findings

**1. The erratum's own explanation of the `tx.rs:407` line is wrong at the revision it names, and it inverts the review it is answering.**
The erratum says "At the reviewed head `407` is `rec.mtime = self.now;`" and that the reviewer described 407 as a closing brace "which is what it would be at a different revision".
At `cde5930`, line 407 is `            }`, a closing brace, and `rec.mtime = self.now;` is at 405.
At `5723216`, line 407 is `                rec.size = size;` and `rec.mtime = self.now;` is at 408.
So that sentence is true at no revision of this file, and the reviewer's description was correct at the reviewed head rather than at a different one.
The substantive point is unaffected and the erratum says so: line 407 was never the `rec.ctime` assignment.

**2. One citation inside the erratum is stale by three lines relative to the head it ships in.**
The erratum's table row gives `tx.rs:418` as the corrected location.
That is right for `cde5930` and wrong for `5723216`, where the `rec.ctime = self.now` assignment in `setattr` is at 421.
The erratum's own prose four lines below does say 421, and the PR body says "418 at the reviewed head and 421 after this comment fix", so the correct value is present twice and the table cell is the only stale copy.
`docs/v1-core.md:559` and `:561`, and `inner.rs:879-908` and `:863`, all resolve exactly at this head, and `docs/v1-core.md` is untouched by this delta, so those three corrections are durable.

**3. The PR body still needs to record the cache defect as a separate queued obligation.**
The body holds the merge pending the epoch-bound clarification and carries the concurrent observation with "no rate and no causality claimed", which is honest and does not overclaim.
But it does not name the site, does not state that the durable value equals the stable pre-flush cached value, and does not say the open obligation belongs to the cached write path and waits on the lane that owns `crates/cowfs-core/src/io.rs`.
Searched the body for the clock-before-lock site, for the node write lock, and for the owning lane: no hit.
The clarification that establishes those facts is `965d9c73…`, and it is **uncommitted**, so there is no GitHub URL that resolves for it and the body cannot link to it yet.
Before merge the body should state, in the errata section that already supersedes everything below it, that the durable `ctime` equals the stable pre-flush cached value across five epoch-matched attempts with content as well as time, that a writer which reads the clock before taking the node write lock can make the cached `ctime` move backwards after a client has observed a later value, and that this is a separate queued obligation in `crates/cowfs-core/src/io.rs` whose fix waits on that lane's owner.
That is a body update, not a code change, and it costs nothing now.

## Boundaries, restated

The queued cache fix is **not** in this delta and must not be taken from it.
The responsible layer is the cached write path: read the node write lock first, then the clock, so a writer's `ctime` reflects its own application point.
`Inner::op_times` and the final attribute loop in `crates/cowfs-core/src/inner.rs` are faithful to the cached value and are not at fault.
`Tx::set_now` in `crates/cowfs-meta/src/tx.rs` is untouched by the finding.
The same read-then-lock shape appears at `io.rs:168` and `io.rs:436` and at `ns.rs:176` before the parent lock at `ns.rs:206`; those are recorded as the same shape to check, not as measured defects.
The queued fix waits on the `crates/cowfs-core/src/io.rs` owner.
I did not inspect that lane's in-progress edits and make no claim about what it touches.
I did not overlap the concurrent rename reviewer or the hole-flag worker.
One fix attempt's worth of diagnosis exists, so no spike is warranted; if a fix at the clock boundary fails to remove the regression under the forced-interleaving probe, that is the second attempt and the point at which a falsifiable spike is the right instrument.

## Limitations, stated plainly

- No build, no cargo, no test execution and no archive extraction in this task, by instruction.
  Every runtime claim here is a source comparison, and it is exact for comment-only code.
- The author's `fmt` and `clippy` exit codes are their claims and were not re-run.
- My first review's scoped suite results and its 3-green CI at `cde5930` carry forward only over provably unchanged code.
  They are not re-executed evidence and are not presented as such.
- CI at this head is a single read-only snapshot; two of three checks had not finished.
  I did not poll and make no prediction about the outcome.
- The historical concurrent observation remains **unreproduced** at the quiesced-epoch boundary across five attempts, with the cause now established by construction rather than by frequency.
  No rate is claimed.
- No performance, timing, soak or acceptance-threshold measurement.
- No `SIGKILL` or power-loss claim.
- No workspace, daemon, `PathVfs`, FUSE or NFS run.
- `no-mistakes` is **not initialized** in this repository, `.no-mistakes` and `.claude` are both absent, so that pipeline was not run and no claim is made about it.
- No browser step; `chromium` is not installed, so any browser work would be **UNVERIFIED** here, and this change has no browser surface.
- `codebase-memory-mcp` graph tools were not used; the source was read directly from the pinned commits by `git show`, which is the binding this review required.
- MisakaNet was available only as a local stdio server, was not needed, and no remote call was made.

## Evidence

Under `bench/out/meta42-clock-corrections-final-critic/` in the assigned worktree, gitignored, one file:

| what | where |
| --- | --- |
| changed-path classification, comment-stripped `tx.rs` comparison, immutable digests, corrections digest, mirror comparison, fixture counts, pinned merge-tree | `logs/receipts.txt` |

The four immutable inputs were verified before and after this task: `pr136-meta42-operation-time-final.md` at `c4999d45…`, `pr136-clock-observation-clarification.md` at `965d9c73…`, `meta42-operation-time.md` at `ed9609e2…`, `meta42-residual-verification.md` at `fbc6a078…`.
The prior lane's logs and archives were not appended to, pruned or deleted in this task.
The assigned worktree still sits at `016769e7f4076a5c0fc712a65932c546048052f7` with zero tracked-file changes.

## Recommendation

The corrections delta is sound and can stand as it is.
The production change is a comment that now says what the code does, proved by a test that asserts what the code does, and the runtime carry is exact by a mechanical comparison rather than by assertion.
Everything my first review measured still applies to this head.

Three things before merge, none of them code: the erratum's sentence about `tx.rs:407`, its stale table cell, and the PR body's missing record of the queued cache obligation.
The merge gate is CI at this head, which is not green yet, and the author's own hold is the right call until it is.

Nothing here closes anything.
Requests 1, 3 and 4 of issue #42 remain open: atomic snapshot rename, the `ChunkRef` hole flag, inode reservation, and the `Core` integration for the rename.
Whole #42 stays open.