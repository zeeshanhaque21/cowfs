# PR 136 citation erratum: one number in the previous erratum was wrong

Refs #42.

The previous erratum, `docs/verification/evidence/meta42-operation-time-review-corrections.md`,
sha256 `5c3e172f0ea35715151c6b6a27c9fc76933dd4d25897fac280f1967a6b312625`, is immutable and unchanged.
Its `tx.rs:407` explanation was wrong and is superseded here.
Nothing else in it changes.

The original critic was right at the reviewed head.
`docs/reviews/pr136-meta42-operation-time-final.md`, sha256
`c4999d45af0ea0d78344ab311be378102ec0856931939bd4f96468f0965a8644`, described `tx.rs:407` as a closing
brace, and at `cde59305fe05f4551468965cd648888eb98e9dbf` that is exactly what line 407 is.
The previous erratum disagreed with that on the grounds that the file was at a different revision, which
was not the reason.
Both cited the reviewed head, and the line was read wrong.

## The correction

Line 407 of `crates/cowfs-meta/src/tx.rs`, read at both revisions:

| revision | line 405 | line 407 | line 418 | `rec.ctime = self.now` |
| --- | --- | --- | --- | --- |
| `cde59305fe05f4551468965cd648888eb98e9dbf` | `rec.mtime = self.now;` | `}` | `rec.mtime = t;` | **418** |
| `57232166639ff09b4741719cf492df787d214a17` | *(shifted)* | `rec.size = size;` | *(shifted)* | **421** |

So the three statements are:

At the reviewed head `cde5930`, line 407 is a closing brace and line 405 is `rec.mtime = self.now;`, so
the historical record's `tx.rs:407` did not point at the `rec.ctime` assignment.
The previous erratum's claim that it pointed at `rec.mtime = self.now;` was incorrect; that line is 405.
At this head `5723216`, which adds three doc-comment lines above it, line 407 is `rec.size = size;`,
line 408 is `rec.mtime = self.now;`, and the `rec.ctime = self.now` assignment is at 421.

The substantive correction the previous erratum carried stands unchanged: the historical record cited the
wrong line for `rec.ctime = self.now`, and the right line at the reviewed head is 418.

## Citations are pinned to a revision, and this erratum pins them

Every line number above names the commit it was read at.
No line number in any of these documents should be read without the revision it belongs to, which is the
habit that produced the error being corrected.
The commit pins in use across this record are `93cfef94457a989d031cb6b0a475ac4edbdb85ef` for the base,
`cde59305fe05f4551468965cd648888eb98e9dbf` for the reviewed head, and
`57232166639ff09b4741719cf492df787d214a17` for this head.

## Two statements that are accurate as they stand

The added test `a_created_inode_takes_all_three_times_from_the_stamp` is accurate about the create path:
a `create` under `set_now(T)` yields `atime == mtime == ctime == T`.
It does not, by itself, pin the explicit-override-on-create behaviour.
That behaviour rests on the source, `crates/cowfs-meta/src/tx.rs:412-417`, where `setattr` assigns
`rec.atime` and `rec.mtime` from the caller's `SetAttr` and only then writes `rec.ctime = self.now`.
The existing fixture `set_now_does_not_replace_an_explicit_atime_or_mtime` pins the override for an
inode that already exists.
The create-with-explicit-times case is source-accurate and is not separately pinned by a test, and that
is stated rather than implied.

The scoped `PASS` that justified the comment fix is carried, not re-executed.
`crates/cowfs-meta/src/tx.rs` was shown to be identical to the reviewed head once comments are stripped,
11840 non-comment characters on both sides, and this erratum is documentation only.
No cargo, fmt or clippy run was performed for it, and no runtime result is claimed by it.

## The clock observation is now resolved, and its fix is queued elsewhere

The epoch-bound clarification, `docs/reviews/pr136-clock-observation-clarification.md`, sha256
`965d9c732882aed380df8c5dc4d4365e35a71997d58a621d1be765065f0ba59d`, is published byte-identically by
this change. Its findings, carried as the clarification's:

In both historical raw logs the durable value equalled the stable pre-flush public `ctime` exactly, and
the assertion that had failed compared it against the transient peak of a quarter of a million racing
per-write samples, a different visible epoch, with the peak exceeding the stable value by 7 us and 9 us.
The independent epoch-matched sample, 5 attempts, 977,507 samples, quiesced before the flush, passed with
exact equality after flush and reopen for the durable `ctime` and for the full 8-byte content, and the
peak never exceeded the stable value in any attempt.

The real defect is separate and was demonstrated deterministically rather than statistically.
`crates/cowfs-core/src/io.rs` reads the clock at line 88 and takes the node write lock at line 90, with
nothing between, so a writer holding an older reading can apply last.
Under a forced interleaving the cached `ctime` regressed 3 us after a value had already been returned to
a client, while the content linearized to the older-clock writer's bytes, so `ctime` and content disagree
about which write happened last.
The durable value after reopen matched the final cached value exactly, so the replay was faithful and
this is a cache-layer defect rather than a PR136 replay contract failure.

The fix is queued as an existing #42 timestamp obligation and is not in these owned paths.
It belongs to the cached write path, `io.rs:88` versus `:90`, and it awaits the READY6 hole-flag owner,
who declares a `Core` hole-IO surface and therefore owns the boundary that change lands on.
`crates/cowfs-core/src/inner.rs`, `Inner::op_times` and the final attribute loop are explicitly not the
fix site and not at fault here, and `crates/cowfs-meta/src/tx.rs` needs no change from this finding.
The same read-then-lock shape appears at `io.rs:168`, `io.rs:436` and `ns.rs:176`; the clarification did
not demonstrate any of those and records them as shapes to check, not as findings.

No overlap is claimed with READY6, no cause is guessed beyond the measured boundary, and no new issue is
filed.

## What the runtime `PASS` does and does not cover

The runtime `PASS` on PR136 carries forward. It is not whole-#42 completion.
The atomic rename, the hole flag and the reservation work are untouched by this branch, and the `Core`
integration is likewise untouched.
No claim is made about any of them.

## A hazard recorded about this repository's history

The commit history on this branch contains a negated closing form in an earlier commit message, which
GitHub does not honour, so `closingIssuesReferences` is empty.
That is verified below and reported as a hazard rather than left silent.
History is not rewritten to remove it.
Neutral `Refs #42` only, with no closing form in this body.

## Verification recorded for this change

Documentation only: three newly published files, one of which is this erratum, plus the two already
canonical reports published byte-identically so no link points at an unpublished file.
No production source, no test, no `Cargo.toml`, no `Cargo.lock`, and no build.
All previously committed source and test blobs are unchanged.
No browser step was taken and none is available here, so any browser surface is **UNVERIFIED**.
`no-mistakes` is **not initialized** in this repository, so that pipeline was not run and no claim is made
about it.