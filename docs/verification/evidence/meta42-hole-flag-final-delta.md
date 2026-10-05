# PR #138 finalization: one stale doc comment, corrected

Scope: the single follow-up this lane was permitted to land on PR #138, which is F1 of
`docs/reviews/pr138-meta42-hole-flag-final.md`.
No behaviour change, no other file edited, no rerun.

| what | value |
|---|---|
| branch | `fix/meta-hole-flag-42` |
| reviewed head this starts from | `99bf7a5efd28a80bc024f040efa2ae6fe0ca6c67` |
| review read, sha256 verified by the coordinator | `docs/reviews/pr138-meta42-hole-flag-final.md`, `b63eb2606abcc2eb23b965061466b9b6f201b36b0fef282dff18fcaf476f4645` |
| verdict acted on | `PASS, scoped`, three of three CI green at `99bf7a5` |
| lease | `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/6/cowfs` |

## What was wrong, and who said so

F1 of the review, quoted:

> **F1, stale doc.** `crates/cowfs-core/src/lib.rs:579-581`.
> `Core::live_blocks` says "`cowfs-meta` yields the all-zero hole ref of a sparse file, which is not a block; this filters it", and "until `ChunkRef` has a hole flag".
> After this change the walk does not yield the hole, so both sentences are now false.
> Three lines to correct, in a file this PR does not touch.

Both sentences were false against this branch's own source, and I read the source before writing a
word:

- `crates/cowfs-meta/src/walk.rs:104-105` filters on the decoded flag,
  `refs.into_iter().filter_map(|c| (!c.hole).then_some(c.id))`;
- `crates/cowfs-meta/src/types.rs:363` is where the flag comes from,
  `hole: id == cowfs_store::HOLE`.

So the walk no longer yields a hole, and "until `ChunkRef` has a hole flag" named a condition this
branch had already satisfied.

## The edit

Six lines of doc comment on `Core::live_blocks`, three of them replacing three.
`6 insertions, 3 deletions`, one file, no code line touched.

The new text says what is true now: the flag exists, the walk filters on it, the filter below is
kept as defence in depth, `Core::live_blocks` remains the public entry point GC should use because it
flushes the snapshot first and maps the walk's errors, and the method says which ids are live rather
than promising that GC reclaims anything.

## What was deliberately not done

The review listed five other follow-ups and this branch touches none of them.

- **F2, `lib.rs:594`,** the `id != file::HOLE` filter in `Core::live_blocks`: left in place.
- **F3, `lib.rs:463`,** the same comparison in `Core::fsck`: left in place.
- **F4, `cowfs-gc/src/lib.rs:709`,** the collector's arm: left in place.
- **`lib.rs:793`, `Core::pinned_blocks`:** its filter reads cached in-memory
  `FileData.chunks.refs` at lines 789-792, not the walk, so those refs still carry holes.
  It is load-bearing, not redundant, and the review says so in as many words. Left in place.
- **`cowfs-gc/src/lib.rs:222`, `note_access`:** a public entry point handed any block id by a reader,
  and defensive by contract. Left in place.
- **F5,** the `ChunkRefError` enum granularity, and **F6,** the untested `hole_refs` split at
  `HOLE_MAX`: recorded as follow-ups in the review, not acted on here. No error-type change, no
  constructor change, no new boundary matrix.

The review's own distinction is the reason: only the three walk-fed comparisons are dead, and a
defence at a trust boundary is not a defect.

This is a documentation slice. It does not claim the hole family is complete, does not claim GC
physically reclaims anything, and adds no promise about reclamation.

## The provenance correction, carried forward

The digest handed to the review, `97564702ed9f08a27a2f9c2a6fed20f1fe34835bd3fb18de890d743a0efd216d`,
is not the canonical record at the reviewed head.
It is the blob at parent commit `37ab0bc`.
The blob at head `99bf7a5` is `b0041044e58284eb2b3cd0dd06ff37368549369a88c0f64b5b7c9a2754a6bd97`, which
differs because `99bf7a5` only corrected the PR number in the record.
The MAIN primary checkout now holds `b0041044…`, and `97564702…` is no longer quoted as current
anywhere in the pull request body.

The review is mirrored into this branch byte for byte at `b63eb260…`, so what a reviewer of the
branch reads is the same bytes the coordinator verified.

## Runtime carry, mechanically checked

| check | result |
|---|---|
| files differing from `99bf7a5` | exactly one: `crates/cowfs-core/src/lib.rs` |
| doc-comment-stripped, blank lines ignored | byte-identical |
| non-blank stripped lines | identical, 0 added, 0 removed |
| every stripped-body difference | 3 added blank lines where doc comments were |
| all code and test blobs | unchanged from `99bf7a5` |

`rustfmt --edition 2021 --check crates/cowfs-core/src/lib.rs` exits 0, standalone, no compile and no
build. 2021 is this workspace's edition, per `Cargo.toml:8`.
No cargo invocation of any kind ran on this branch, no archive was extracted, no target directory was
created, no probe was built, and the shared heavy lane was not taken: there was no heavy work.

## What this does not claim

- **No rerun.** No test, build, suite, clippy or `cargo fmt` result is new here. The runtime evidence
  is the review's, at `99bf7a5`, plus this branch's own recorded runs. The change is three doc-comment
  lines, so there is no new runtime claim to make.
- **Not a hole-family acceptance,** not an fsx acceptance, not a performance or timing acceptance,
  not a power-loss or crash claim. None of those was made before and none is made now.
- **The 811-pass figure is not re-executed here.** The review did not independently re-execute it
  either, and says so.
- **CI must be read at this branch's own new head.** The three green jobs belong to `99bf7a5` and do
  not carry to a new commit. One snapshot on the new head, reported, not polled.
- **`no-mistakes` is not initialized** in this repository, so that pipeline did not run and no claim
  is made about it.
- **No browser step.** `chromium` is not installed, so any browser work would be UNVERIFIED here, and
  this change has no browser surface.
- **`codebase-memory-mcp` graph tools were not used.** The source read here is four lines of one file,
  read directly, which is the pinned read this task asked for.
- **MisakaNet was local-only and was not consulted.** No remote call was made.

## Evidence

`bench/out/meta42-hole-flag-final-delta/` in the lease, gitignored, logs and digests only:

| file | what |
|---|---|
| `logs/source-hash.log` | the blob identities before and after, and the differing-file list |

No lane script and no lock were needed, because nothing heavy ran.
Prior reports and evidence untouched, including `fbc6a078…`, `965d9c73…`, `c4999d45…` and
`ed9609e2…`.
